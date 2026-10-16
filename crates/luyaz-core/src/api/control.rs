//! The touch layer's writes (Phase 4 step 8d): to u60d's own `/control`, over loopback, as the agent
//! sends them to datad (`zte-agent/src/datad_write.rs`). Going through `/control` keeps everything
//! the agent's writes get there: the source on every write, the journal, the search session's gate
//! (D17) and `op_busy` while a network-mode change confirms (D40). The answers are mapped by
//! u60-api's `control::Reply`, the agent's own mapping.
//!
//! No emergency-script fallback: when u60d's own `/control` does not answer, u60d itself is the
//! problem, and the screen gets the "data service down" reply.

use serde_json::{Value, json};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use luyaz_api::control::{Reply, parse};

/// A write may wait up to 15 s for the cross-process write lock, then up to 8 s for the vendor call.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(30);
/// Opening, renewing and closing a session do not queue behind the executor.
const SESSION_CALL: Duration = Duration::from_secs(5);

/// Where `/control` is and how to ask it.
pub struct Client {
    url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl Client {
    pub fn new(url: String, token: Option<String>) -> Client {
        Client {
            url,
            token,
            http: reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap_or_default(),
        }
    }

    /// The URL and token, for a blocking caller (the scenario engine's thread).
    pub fn target(&self) -> (String, Option<String>) {
        (self.url.clone(), self.token.clone())
    }

    /// (status, body), or the [`Reply::Unreachable`] it came to.
    async fn post_raw(&self, body: &Value, timeout: Duration) -> Result<(u16, Value), Reply> {
        let mut req = self
            .http
            .post(&self.url)
            .timeout(timeout)
            .header("Content-Type", "application/json")
            .body(body.to_string());
        if let Some(t) = &self.token {
            req = req.header("Authorization", format!("Bearer {t}"));
        }
        let resp = req.send().await.map_err(|e| Reply::Unreachable {
            error: e.to_string(),
            maybe_done: !e.is_connect(),
        })?;
        let status = resp.status().as_u16();
        let b = resp.bytes().await.map_err(|e| Reply::Unreachable {
            error: e.to_string(),
            maybe_done: true,
        })?;
        Ok((status, serde_json::from_slice(&b).unwrap_or(Value::Null)))
    }

    async fn post(&self, body: &Value, timeout: Duration) -> Reply {
        match self.post_raw(body, timeout).await {
            Ok((status, v)) => parse(status, &v),
            Err(r) => r,
        }
    }

    /// [`Client::send`] with its own timeout (a short one for `op.interrupt`).
    pub async fn send_with(
        &self,
        source: &str,
        session: Option<&str>,
        action: &str,
        params: &Value,
        timeout: Duration,
    ) -> Reply {
        let mut body = json!({"action": action, "source": source, "params": params});
        if let Some(id) = session {
            body["session"] = json!(id);
        }
        self.post(&body, timeout).await
    }

    /// One write as `source`, inside `session` when there is one.
    pub async fn send(
        &self,
        source: &str,
        session: Option<&str>,
        action: &str,
        params: &Value,
    ) -> Reply {
        let mut body = json!({"action": action, "source": source, "params": params});
        if let Some(id) = session {
            body["session"] = json!(id);
        }
        self.post(&body, TIMEOUT).await
    }
}

/// Who writes and in which session: what the agent keeps per thread (`with_source`, `Session::run`).
#[derive(Clone)]
pub struct Writer {
    pub client: Arc<Client>,
    pub source: &'static str,
    pub session: Option<String>,
}

impl Writer {
    pub fn new(client: Arc<Client>, source: &'static str) -> Writer {
        Writer {
            client,
            source,
            session: None,
        }
    }

    pub async fn send(&self, action: &str, params: &Value) -> Reply {
        self.client
            .send(self.source, self.session.as_deref(), action, params)
            .await
    }
}

/// A search session (D17): while it is open u60d refuses other connectivity writes. Renewed every
/// `renew` by a task of its own (taken back 60 s after the last renew); closed by [`Session::close`]
/// on every way out, or, as a backstop, when dropped.
pub struct Session {
    id: String,
    source: &'static str,
    client: Arc<Client>,
    alive: Arc<AtomicBool>,
    result: Mutex<String>,
    closed: AtomicBool,
}

impl Session {
    pub async fn open(
        client: Arc<Client>,
        source: &'static str,
        renew: Duration,
    ) -> Result<Session, Reply> {
        let body = json!({"action": "netselect.session.open", "source": source, "params": {}});
        let id = match client.post(&body, SESSION_CALL).await {
            Reply::Done { result, .. } => match result["session"].as_str() {
                Some(id) => id.to_string(),
                None => {
                    return Err(Reply::Failed {
                        error: "session.open: no session id".into(),
                        op: None,
                    });
                }
            },
            other => return Err(other),
        };
        let alive = Arc::new(AtomicBool::new(true));
        let (a, c, rid) = (alive.clone(), client.clone(), id.clone());
        tokio::spawn(async move {
            let tick = renew.min(Duration::from_millis(500));
            let mut waited = Duration::ZERO;
            while a.load(Ordering::Relaxed) {
                tokio::time::sleep(tick).await;
                waited += tick;
                if waited < renew || !a.load(Ordering::Relaxed) {
                    continue;
                }
                waited = Duration::ZERO;
                let body = json!({"action": "netselect.session.renew", "source": source, "params": {"session": rid}});
                if let Ok((409, v)) = c.post_raw(&body, SESSION_CALL).await
                    && v["error"]["code"] == "invalid_state"
                {
                    // it ended on u60d's side
                    return;
                }
            }
        });
        Ok(Session {
            id,
            source,
            client,
            alive,
            result: Mutex::new("done".into()),
            closed: AtomicBool::new(false),
        })
    }

    /// A writer for the steps inside the session, as its source.
    pub fn writer(&self) -> Writer {
        Writer {
            client: self.client.clone(),
            source: self.source,
            session: Some(self.id.clone()),
        }
    }

    /// What the journal line says when the session closes.
    pub fn set_result(&self, r: &str) {
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = r.to_string();
    }

    fn close_body(&self) -> Value {
        let result = self
            .result
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        json!({"action": "netselect.session.close", "source": self.source, "params": {"session": self.id, "result": result}})
    }

    pub async fn close(self) {
        self.alive.store(false, Ordering::Relaxed);
        self.closed.store(true, Ordering::Relaxed);
        let body = self.close_body();
        let _ = self.client.post(&body, SESSION_CALL).await;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.alive.store(false, Ordering::Relaxed);
        if self.closed.load(Ordering::Relaxed) {
            return;
        }
        // a way out that missed close(): close it anyway, or the next action waits out 60 s
        let (client, body) = (self.client.clone(), self.close_body());
        if let Ok(h) = tokio::runtime::Handle::try_current() {
            h.spawn(async move {
                let _ = client.post(&body, SESSION_CALL).await;
            });
        }
    }
}
