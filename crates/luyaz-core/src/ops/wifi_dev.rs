//! Wi-Fi 设置事务的设备一侧（第四批 4-0b，值和写法在 `ops::wifi`）：Wi-Fi 锁、读回、写、重载和确认。
//!
//! - **锁**：和 agent 一样，从第一个 `uci set` 拿着 Wi-Fi 锁（`luyaz_wifi`，`/tmp/luyaz-wifi.lock`，进程内
//!   标志 + flock）直到重载确认完。`/control` 在提交之前拿（最多等 5 秒，拿不到回 503 `busy`，事务还没开始），
//!   用请求参数里的交接号（[`LOCK_TOKEN`]，不落盘）交给这次写；退回、撤销以外没有交接的写（自动退回、
//!   重启后续跑）自己拿，最多等 150 秒（在阻塞线程里等，不占执行者）。顺序是先 Wi-Fi 锁、后写锁
//!   （和 guard、agent 经 `/control` 写一样，`wifi_lock.rs` 说明了为什么不会死锁）。
//! - **写**：一个执行者任务里拿写锁，要动原厂总开关就先动（`wifi.set_module`：`Some(true)` 先开、
//!   `Some(false)` 就是总开关关本身），再 `wifi.apply {set, reload:false}`（访客 best_effort）。
//!   只动原厂总开关、没有 uci 要写时不 reload——开关自己已生效（B11 实测约 2 秒关 / 十几秒开），
//!   确认在线程里等原厂开关、保存频段和运行布局一致。只改了发射功率：iw 热改、不重载。模块目标开且有冷写时，
//!   锁交给系统线程重载和轮询，做完放锁。
//! - **确认**（`Confirm::Wifi`）：读回对上目标，且 `Reading::registered` = AP 已按 uci 起来/关掉：
//!   跟踪中的写以后台验证结果为准，只有未跟踪的写才当场核对布局。不看蜂窝或 DNS，SIM 记空（换卡不影响 Wi-Fi）。

use super::{
    apn::Vault,
    engine::WriteError,
    txn::{self, Reading, SimId},
    wifi::{self, Kind},
    write_lock,
};
use crate::control::Outcome;
use crate::executor::Executor;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

/// 请求参数里的 Wi-Fi 锁交接号（`/control` 加的，只在内存里）。
pub const LOCK_TOKEN: &str = "_wifi_lock";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Verify {
    /// 没有在跟的重载。
    Idle,
    Running,
    Done(bool),
}

pub struct WifiDev {
    vault: Vault,
    handoff: Mutex<Option<(u64, luyaz_wifi::WifiLock)>>,
    next: AtomicU64,
    verify: Arc<Mutex<Verify>>,
}

fn lock_path() -> String {
    super::wifi_lock::Cfg::from_env()
        .path
        .unwrap_or_else(|| luyaz_wifi::LOCK_FILE.into())
}

fn guard<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 无需重载的验证线程。
struct UciHost;

impl luyaz_wifi::Host for UciHost {
    fn wireless(&self) -> Option<wifi::Uci> {
        crate::state::wifi_map_sync()
    }
    fn write_aps(&self, _: bool, _: bool) -> Result<Option<String>, String> {
        Err("not used here".into())
    }
    fn vendor_on(&self) -> Result<(), String> {
        Err("not used here".into())
    }
    fn reload(&self) -> Result<Value, String> {
        Err("not used here".into())
    }
}

/// 重载线程用：`zwrt_wlan reload` 作为执行者任务（拿写锁）在运行时上做，线程只等它。
struct ReloadHost {
    handle: tokio::runtime::Handle,
    exec: Executor,
}

impl luyaz_wifi::Host for ReloadHost {
    fn wireless(&self) -> Option<wifi::Uci> {
        crate::state::wifi_map_sync()
    }
    fn write_aps(&self, _: bool, _: bool) -> Result<Option<String>, String> {
        Err("not used here".into())
    }
    fn vendor_on(&self) -> Result<(), String> {
        Err("not used here".into())
    }
    fn reload(&self) -> Result<Value, String> {
        let exec = self.exec.clone();
        self.handle
            .block_on(self.handle.spawn(async move {
                // D7：拿不到写锁就不重载
                write_lock::run(&exec, async {
                    crate::state::ubus("zwrt_wlan", "reload", json!({})).await
                })
                .await
                .map_err(|e| e.to_string())
            }))
            .map_err(|e| e.to_string())?
            .and_then(|r| r)
    }
}

fn fail(message: impl Into<String>) -> WriteError {
    WriteError {
        message: message.into(),
        timed_out: false,
    }
}

fn check_expect(params: &Value, current: &Value) -> Result<(), String> {
    if let Some(expect) = params.get("expect")
        && !txn::matches(expect, current)
    {
        return Err(if params.get("restore").is_some() {
            "Wi-Fi settings changed since; not undoing"
        } else {
            "Wi-Fi settings changed since the form was opened"
        }
        .into());
    }
    Ok(())
}

async fn write_settings<F>(
    set: Value,
    vendor: Option<bool>,
    guest: bool,
    mut execute: impl FnMut(&'static str, Value) -> F,
) -> Result<Value, String>
where
    F: Future<Output = Outcome>,
{
    let mut out = json!({"status": "ok"});
    if let Some(on) = vendor {
        match execute("wifi.set_module", json!({"enabled": on as i32})).await {
            Outcome::Ok(_) => out["vendor_switch"] = json!(if on { "turned_on" } else { "turned_off" }),
            Outcome::Failed(e) | Outcome::Invalid(e) => return Err(e),
            Outcome::CapabilityDenied(e) => return Err(e.to_string()),
            Outcome::NotHandled => return Err("wifi.set_module not handled".into()),
        }
    }
    if set.as_object().is_some_and(|s| !s.is_empty()) {
        let params = json!({"set": set, "reload": false, "best_effort": guest});
        match execute("wifi.apply", params).await {
            Outcome::Ok(v) => {
                out["committed"] = v["committed"].clone();
                out["skipped"] = v["skipped"].clone();
            }
            Outcome::Failed(e) | Outcome::Invalid(e) => return Err(e),
            Outcome::CapabilityDenied(e) => return Err(e.to_string()),
            Outcome::NotHandled => return Err("wifi.apply not handled".into()),
        }
    }
    Ok(out)
}

impl WifiDev {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            vault: Vault::open_as(dir, wifi::VAULT_FILE, wifi::VAULT_MAX),
            handoff: Mutex::new(None),
            next: AtomicU64::new(0),
            verify: Arc::new(Mutex::new(Verify::Idle)),
        }
    }

    /// `/control`：提交之前拿 Wi-Fi 锁（agent 的 `HTTP_WAIT`，5 秒），留给这次写；回交接号。
    pub async fn take_for_request(&self) -> Result<u64, String> {
        let path = lock_path();
        let lk =
            crate::state::blocking(move || luyaz_wifi::lock_at(&path, luyaz_wifi::HTTP_WAIT)).await?;
        let token = self.next.fetch_add(1, Ordering::SeqCst) + 1;
        *guard(&self.handoff) = Some((token, lk));
        Ok(token)
    }

    /// 提交完：这次没用上（没变、忙、拒绝……）就放掉。
    pub fn release(&self, token: u64) {
        let mut h = guard(&self.handoff);
        if h.as_ref().is_some_and(|(t, _)| *t == token) {
            *h = None;
        }
    }

    async fn lock_for(&self, params: &Value) -> Result<luyaz_wifi::WifiLock, String> {
        if let Some(token) = params.get(LOCK_TOKEN).and_then(Value::as_u64) {
            let mut h = guard(&self.handoff);
            if h.as_ref().is_some_and(|(t, _)| *t == token) {
                return Ok(h.take().expect("checked").1);
            }
        }
        let path = lock_path();
        crate::state::blocking(move || luyaz_wifi::lock_at(&path, luyaz_wifi::ENGINE_WAIT)).await
    }

    /// 放进保管箱（已经有就不再写闪存）。
    fn keep(&self, key: &str) {
        let content = json!({ "wifi_key": key });
        if self.vault.get(&self.vault.fp(&content)).is_none() {
            self.vault.keep("wifi", &content);
        }
    }

    fn lookup(&self, fp: &str) -> Option<String> {
        self.vault
            .get(fp)?
            .get("wifi_key")?
            .as_str()
            .map(str::to_owned)
    }

    async fn now(&self, kind: Kind) -> Result<(wifi::Uci, Value), String> {
        let m = crate::state::blocking(crate::state::wifi_map_sync)
            .await
            .ok_or("cannot read Wi-Fi settings")?;
        let v = wifi::value(kind, &m, &self.vault).ok_or("cannot read Wi-Fi settings")?;
        Ok((m, Value::Object(v)))
    }

    /// AP 是不是已经按 uci 起来/关掉（见模块说明）。
    async fn aps_ready(&self, cfg: Option<wifi::Uci>) -> bool {
        let Some(cfg) = cfg else { return false };
        let v = *guard(&self.verify);
        match v {
            Verify::Running => false,
            Verify::Done(ok) => ok,
            Verify::Idle => {
                crate::state::blocking(move || luyaz_wifi::settled_layout(&cfg) == Some(true)).await
            }
        }
    }

    pub async fn read(&self, kind: Kind) -> Result<Reading, String> {
        let m = crate::state::blocking(crate::state::wifi_map_sync).await;
        let value = m
            .as_ref()
            .and_then(|m| wifi::value(kind, m, &self.vault))
            .map(Value::Object);
        Ok(Reading {
            value,
            // Wi-Fi 项：AP 已经按 uci 起来/关掉（不是蜂窝注册）
            registered: self.aps_ready(m).await,
            sim: SimId {
                iccid: String::new(),
                slot: 0,
            },
            data: None,
            probe: None,
        })
    }

    /// 提交时定目标：新写按请求体算（agent 的写法套到现状上，只留变了的键）；撤销（`{restore, expect}`）
    /// 先核对现状还是那次改完的样子。没变 = `None`（不算事务）。要用的旧密码不在保管箱就拒绝。
    pub async fn resolve(
        &self,
        kind: Kind,
        target: Value,
        params: &Value,
    ) -> Result<Option<Value>, String> {
        let (m, current) = self.now(kind).await?;
        // The HTTP route hands this request the Wi-Fi lock before resolve.
        check_expect(params, &current)?;
        let cur = current.as_object().expect("flattened Wi-Fi settings");
        for k in wifi::current_keys(kind, &m) {
            self.keep(k);
        }
        let restore = params.get("restore").is_some();
        let (want, plan) = if restore {
            let want = target.as_object().cloned().unwrap_or_default();
            // Include both saved switch values when interpreting a historical split.
            let plan = wifi::plan_for(kind, &want, &m, cur, |fp| self.lookup(fp))?;
            (want, plan)
        } else {
            let mut body = params.as_object().cloned().unwrap_or_default();
            body.remove(LOCK_TOKEN);
            let plan = match kind {
                Kind::Main => wifi::main_plan(&body, &m)?,
                Kind::Guest => wifi::guest_plan(&body, &m)?,
            };
            for k in wifi::new_keys(&plan) {
                self.keep(k);
            }
            let want = wifi::value(kind, &wifi::simulate(&m, &plan), &self.vault)
                .ok_or("cannot read Wi-Fi settings")?;
            (want, plan)
        };
        let mut t = wifi::changed(cur, &want);
        if restore {
            t.retain(|_, value| !value.is_null());
        }
        if let Some(on) = plan.vendor {
            t.insert("on".into(), json!(on));
            t.insert("module_on".into(), json!(on));
            for key in ["band_2g.radio_on", "band_5g.radio_on"] {
                if let Some(saved) = cur.get(key).and_then(Value::as_bool) {
                    t.insert(key.into(), json!(saved));
                } else {
                    t.remove(key);
                }
            }
        }
        if t.is_empty() {
            return Ok(None);
        }
        // 写得出来（退回、撤销要的旧密码还在）、写完的 AP 布局固件跑得起来才接
        let plan = if restore { plan } else {
            wifi::plan_for(kind, &t, &m, cur, |fp| self.lookup(fp))?
        };
        if let Some(why) = luyaz_wifi::layout::refusal(&m, &wifi::simulate(&m, &plan)) {
            return Err(why.into());
        }
        Ok(Some(Value::Object(t)))
    }

    /// 写成 `value`（新写、退回、撤销都是值）。重载在后台线程里做，这里不等。
    pub async fn write(
        &self,
        exec: &Executor,
        kind: Kind,
        value: &Value,
        params: &Value,
    ) -> Result<Value, WriteError> {
        let lk = self.lock_for(params).await.map_err(fail)?;
        // 拿着锁了：之前那次的确认结果不算这次的
        *guard(&self.verify) = Verify::Idle;
        let (m, now) = self.now(kind).await.map_err(fail)?;
        let want = value.as_object().cloned().unwrap_or_default();
        let now = now.as_object().expect("flattened Wi-Fi settings");
        let plan = wifi::plan_for(kind, &want, &m, now, |fp| self.lookup(fp)).map_err(fail)?;
        if plan.is_empty() {
            // 已经是这样：不重载，确认时当场看一眼
            *guard(&self.verify) = Verify::Idle;
            return Ok(json!({"status": "ok", "note": "no changes"}));
        }
        let switches = luyaz_wifi::switches::read(|k| m.get(k).map(String::as_str));
        let module_on = plan.vendor.or(switches.module) == Some(true);
        let hot = module_on.then(|| plan.hot_txpower()).flatten();
        let reload = module_on && hot.is_none() && !plan.set.is_empty();
        let expected = reload.then(|| wifi::simulate(&m, &plan));
        let (set, vendor, guest) = (plan.set_json(), plan.vendor, kind == Kind::Guest);
        // D7：拿不到写锁就什么都不写（Wi-Fi 锁已经拿着，lk 随返回放掉）
        let r = write_lock::run(exec, async move {
            write_settings(set, vendor, guest, |action, params| async move {
                crate::control::execute(action, &params).await
            })
            .await
        })
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r);
        crate::state::invalidate_cache();
        exec.mark_immediate(None);
        let mut out = r.map_err(|e| {
            *guard(&self.verify) = Verify::Done(false);
            fail(e)
        })?;
        if let Some(tx) = hot {
            // agent：只改发射功率时 iw 热改（limit = 百分比 × 30），不重载
            for (iface, pct) in tx {
                let limit = (pct * 30).to_string();
                let args = ["dev", iface, "set", "txpower", "limit", limit.as_str()];
                if let Err(e) = crate::command::run("iw", args, Duration::from_secs(5)).await {
                    eprintln!("ops: iw txpower on {iface}: {e}");
                }
            }
            out["hot"] = json!(true);
        }
        *guard(&self.verify) = Verify::Running;
        let verify = self.verify.clone();
        let handle = tokio::runtime::Handle::current();
        let exec = exec.clone();
        let spawned = std::thread::Builder::new()
            .name("wifi-txn".into())
            .spawn(move || {
                let host = ReloadHost {
                    handle: handle.clone(),
                    exec: exec.clone(),
                };
                let data = if let Some(expected) = &expected {
                    let error = luyaz_wifi::reload(&host, &lk).err();
                    let mut data = luyaz_wifi::verify_written(&UciHost, expected);
                    if let Some(e) = error {
                        data["reload_error"] = json!(e);
                    }
                    data
                } else if let Some(wanted) = vendor {
                    luyaz_wifi::verify_master(&UciHost, wanted, switches.radios)
                } else {
                    luyaz_wifi::verify_no_reload(&UciHost)
                };
                let ok = data["verified"].as_bool() == Some(true);
                eprintln!("ops: Wi-Fi change: reload finished {data}");
                *guard(&verify) = Verify::Done(ok);
                drop(lk);
                crate::engines::wifi_dfs::layout_changed();
                handle.spawn(async move {
                    crate::state::invalidate_cache();
                    exec.mark_immediate(None);
                });
            });
        if let Err(e) = spawned {
            eprintln!("ops: Wi-Fi change: could not start the reload thread: {e}");
            *guard(&self.verify) = Verify::Done(false);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wifi_save_conflict_is_scoped_to_edited_fields() {
        let current = json!({"guest.ssid": "Guest", "guest.isolate": true});
        let mut params = json!({"guest_ssid": "New name", "expect": {"guest.ssid": "Guest"}});
        assert_eq!(check_expect(&params, &current), Ok(()));
        params["expect"]["guest.ssid"] = json!("Another name");
        assert_eq!(
            check_expect(&params, &current).unwrap_err(),
            "Wi-Fi settings changed since the form was opened"
        );
        params["restore"] = json!({"guest.ssid": "New name"});
        assert_eq!(
            check_expect(&params, &current).unwrap_err(),
            "Wi-Fi settings changed since; not undoing"
        );
    }

    #[tokio::test]
    async fn wifi_vendor_failure_stops_the_following_uci_write() {
        let mut calls = Vec::new();
        let result = write_settings(
            json!({"wireless.main_5g.ssid": "Renamed"}),
            Some(true),
            false,
            |action, _| {
                calls.push(action.to_owned());
                std::future::ready(Outcome::Failed("vendor refused".into()))
            },
        )
        .await;
        assert_eq!(result.unwrap_err(), "vendor refused");
        assert_eq!(calls, ["wifi.set_module"]);
    }
}
