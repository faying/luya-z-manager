//! the core's `/control` answers as the agent's HTTP replies (moved from zte-agent `core_write.rs`,
//! Phase 4 step 8d): what a write came back with ([`Reply`]), how the agent answers the page for it
//! ([`Reply::into_http`]) and how its own loops read it ([`Reply::into_result`], [`retry_after`]).
//! u60d's touch compatibility layer sends its writes to its own `/control` and answers the screen
//! with the same mapping, so the screen sees the same status and words from either.

use serde_json::{Value, json};
use std::time::Duration;

/// the core's answer to one write.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Written (200). `op` is the transaction state for described actions.
    Done { result: Value, op: Option<Value> },
    /// Written by the emergency script while core was gone.
    Fallback,
    /// The vendor call failed, or core could not read the current value
    /// first (502 and the like). `op` as above.
    Failed { error: String, op: Option<Value> },
    /// The request itself was refused (400): bad parameters.
    Invalid(String),
    /// Another change is in progress (409 busy, with what it is), or the core's
    /// queue is full (503 busy).
    Busy { doing: Option<Value> },
    /// A network-mode transaction is confirming and this write is from an
    /// automatic source (409 `op_busy`, D40): not a failure, try again once
    /// it ends. `op` is the core's `{op_id, item, phase}`.
    OpBusy { message: String, op: Option<Value> },
    /// No answer from core. `maybe_done`: the request was sent and may have
    /// been carried out.
    Unreachable { error: String, maybe_done: bool },
}

/// the core's HTTP reply → [`Reply`].
pub fn parse(status: u16, reply: &Value) -> Reply {
    let op = reply.get("op").filter(|v| !v.is_null()).cloned();
    let code = reply["error"]["code"].as_str().unwrap_or("");
    let message = reply["error"]["message"]
        .as_str()
        .or_else(|| reply["error"].as_str())
        .unwrap_or("failed")
        .to_string();
    match status {
        200 if reply["ok"] == true => Reply::Done {
            result: reply.get("result").cloned().unwrap_or(Value::Null),
            op,
        },
        409 | 503 if code == "busy" => Reply::Busy {
            doing: reply.get("doing").filter(|v| !v.is_null()).cloned(),
        },
        409 if code == "op_busy" => Reply::OpBusy {
            message,
            op: reply["error"].get("op").filter(|v| !v.is_null()).cloned(),
        },
        400 => Reply::Invalid(message),
        _ => Reply::Failed { error: message, op },
    }
}

pub const DATAD_DOWN: &str = "数据服务没响应，写操作暂停；可用触屏";
pub const DATAD_DOWN_EN: &str =
    "The data service is not responding; changes are paused. Use the touch screen";
pub const NO_ANSWER: &str = "数据服务没回答，不知道改没改成；稍后看一下当前设置";
pub const NO_ANSWER_EN: &str = "The data service did not answer; it may or may not have changed. Check the setting in a moment";

impl Reply {
    pub fn ok(&self) -> bool {
        matches!(self, Reply::Done { .. } | Reply::Fallback)
    }

    /// The agent's usual HTTP answer: 200 `{"ok":true,"data":…}` (+ `op`),
    /// 409 busy, 400 invalid, 503 for a failed vendor call (as before: the web
    /// page reads "method not found" there as "not supported") and for core
    /// not answering.
    pub fn into_http(self) -> (u16, Value) {
        match self {
            Reply::Done { result, op } => {
                let mut v = json!({"ok": true, "data": result});
                if let Some(op) = op {
                    v["op"] = op;
                }
                (200, v)
            }
            Reply::Fallback => (200, json!({"ok": true, "data": null, "fallback": true})),
            Reply::Failed { error, op } => {
                let mut v = json!({"ok": false, "error": error});
                if let Some(op) = op {
                    v["op"] = op;
                }
                (503, v)
            }
            Reply::Invalid(e) => (400, json!({"ok": false, "error": e})),
            Reply::Busy { doing } => {
                let what = doing
                    .as_ref()
                    .and_then(|d| d["action"].as_str())
                    .unwrap_or("");
                // the core's own sentence when it has one (V2-39: what, who, how long)
                let say = |k: &str| {
                    doing
                        .as_ref()
                        .and_then(|d| d[k].as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                };
                (
                    409,
                    json!({
                        "ok": false,
                        "error": say("say_zh").unwrap_or_else(|| if what.is_empty() { "设备正在改别的设置，等它结束再试".to_string() } else { format!("设备正在改别的设置（{what}），等它结束再试") }),
                        "error_en": say("say_en").unwrap_or_else(|| if what.is_empty() { "Another change is in progress; try again when it finishes".to_string() } else { format!("Another change is in progress ({what}); try again when it finishes") }),
                        "busy": "core",
                        "doing": doing,
                    }),
                )
            }
            Reply::OpBusy { message, op } => (
                409,
                json!({
                    "ok": false,
                    "error": OP_BUSY_ZH,
                    "error_en": OP_BUSY_EN,
                    "detail": message,
                    "busy": "op",
                    "op": op,
                }),
            ),
            Reply::Unreachable { error, maybe_done } => {
                let (zh, en) = if maybe_done {
                    (NO_ANSWER, NO_ANSWER_EN)
                } else {
                    (DATAD_DOWN, DATAD_DOWN_EN)
                };
                (
                    503,
                    json!({"ok": false, "error": zh, "error_en": en, "detail": error, "core": "down"}),
                )
            }
        }
    }

    /// For the agent's own loops: `Ok` or the error text.
    pub fn into_result(self) -> Result<Value, String> {
        match self {
            Reply::Done { result, .. } => Ok(result),
            Reply::Fallback => Ok(Value::Null),
            Reply::Failed { error, .. } | Reply::Invalid(error) => Err(error),
            Reply::Busy { doing } => Err(format!(
                "core busy{}",
                doing
                    .and_then(|d| d["action"].as_str().map(|a| format!(" ({a})")))
                    .unwrap_or_default()
            )),
            Reply::OpBusy { message, .. } => Err(format!("{OP_BUSY_ERR}: {message}")),
            Reply::Unreachable { error, .. } => Err(format!("core: {error}")),
        }
    }

    /// core held this automatic write off while a transaction confirms (D40).
    /// The agent's loops use [`retry_after`] on `into_result`; this is for a
    /// caller that keeps the reply.
    pub fn is_op_busy(&self) -> bool {
        matches!(self, Reply::OpBusy { .. })
    }
}

pub const OP_BUSY_ZH: &str = "正在确认刚换的网络模式，等它结束再改";
pub const OP_BUSY_EN: &str = "Checking the new network mode; try again when it finishes";

/// How [`Reply::into_result`] starts the error for [`Reply::OpBusy`].
pub const OP_BUSY_ERR: &str = "core op_busy";

/// An [`Reply::into_result`] error that is an op_busy, not a failure.
pub fn is_op_busy_err(e: &str) -> bool {
    e.starts_with(OP_BUSY_ERR)
}

/// How long an automatic writer waits before trying again after op_busy: a
/// transaction confirms for up to 120 s, so a few looks are enough and none
/// of them is a busy loop.
pub const OP_BUSY_RETRY: Duration = Duration::from_secs(10);

/// For an automatic writer's loop: `Some(wait)` = core said op_busy, try
/// again after `wait` without counting it as a failed attempt; `None` = the
/// result stands (success or a real failure).
pub fn retry_after<T>(r: &Result<T, String>) -> Option<Duration> {
    match r {
        Err(e) if is_op_busy_err(e) => Some(OP_BUSY_RETRY),
        _ => None,
    }
}

/// The data service gone, worded for the touch screen itself (the web's [`DATAD_DOWN`] sends the
/// owner to the screen).
pub const DATAD_DOWN_SCREEN: &str = "数据服务没响应，写操作暂停";
pub const DATAD_DOWN_SCREEN_EN: &str = "The data service is not responding; changes are paused";

impl Reply {
    /// [`Reply::into_http`] for the touch screen: the same, except the "data service down" words.
    pub fn into_http_screen(self) -> (u16, Value) {
        let down = matches!(
            self,
            Reply::Unreachable {
                maybe_done: false,
                ..
            }
        );
        let (s, mut v) = self.into_http();
        if down {
            v["error"] = json!(DATAD_DOWN_SCREEN);
            v["error_en"] = json!(DATAD_DOWN_SCREEN_EN);
        }
        (s, v)
    }
}

#[cfg(test)]
mod tests;
