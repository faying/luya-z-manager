//! 写操作的处理（Phase 2a，D12）：`POST /control` 解析完请求体之后的全部——启动等待、E4 事务动作、
//! 旧请求排队、交给执行者、跨进程写锁、写操作日志、成功后让相关块立即重读。
//! HTTP 那层（解析、校验请求体、旧接口计数）在 `server.rs` 的 `control`；动作本身在 `crate::control`、`crate::ops`。
use super::{App, CONTROL_START_WAIT, Stage};
use crate::executor;
use crate::ops::{Source, txn::Who};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// `/control` 的一个请求（已确认是 JSON 对象、有 action、params 是对象）。
pub(super) async fn handle(
    app: App,
    action: String,
    body: Value,
    peer: Option<std::net::SocketAddr>,
) -> Response {
    // Phase 4 影子：做成了的充电写——agent 的（auto）记给 charge_policy 比对，手动的（web/screen）
    // 记成 override（agent 也是写成功了才设）。
    let watch = (action == "power.direct_supply.set").then(|| body.clone());
    let response = handle_inner(app, action.clone(), body, peer).await;
    if let Some(body) = watch
        && response.status().is_success()
    {
        crate::plugin::charge::observe_control(&action, &body);
    }
    response
}

async fn handle_inner(
    app: App,
    action: String,
    body: Value,
    peer: Option<std::net::SocketAddr>,
) -> Response {
    // P1-3：启动中（第一轮、事务恢复还没做完）先等，最多 20 秒；还没好就照队列满回 503 `busy`。
    if !app.wait_stage(Stage::Ready, CONTROL_START_WAIT).await {
        return control_busy(&action);
    }
    if let Err(e) = check_approved_by(&body) {
        return invalid_parameter(&action, &e);
    }
    // 第二期（D7、O1）：有效批准者是 mcp 的写先过 Agent 的关（白名单、敏感、按层限速），不论走不走事务
    let pass = match agent_gate(&action, &body).await {
        Ok(p) => p,
        Err(r) => return r,
    };
    let response = gated(app, action.clone(), body, peer, pass.as_ref()).await;
    if let Some(p) = &pass
        && p.fresh
    {
        let g = crate::ops::agent::gate();
        // 没动设备（参数错、忙、被拒绝、执行者满）：位置还回去；动了就算这一次
        if response.status().is_client_error()
            || response.status() == StatusCode::SERVICE_UNAVAILABLE
        {
            g.release(&p.ticket);
        } else if p.single {
            g.done(&p.ticket);
        }
        // 不经事务的写（apn.add/delete/set_pdp_type、改不在用的 APN；D11）没有事务结束那一行：这里记
        if crate::ops::spec::find(&action).is_none()
            || p.plain.load(std::sync::atomic::Ordering::Relaxed)
        {
            crate::ops::agent::event(&format!(
                "agent {action}: HTTP {} (no trial, no rollback)",
                response.status().as_u16()
            ));
        }
    }
    response
}

/// Agent 的一次放行（[`agent_gate`]）。
struct AgentPass {
    ticket: crate::ops::agent::Ticket,
    /// CHILL 切之前就确定不通（D10）：照做、不带退回。
    rollback: Option<bool>,
    /// 这次请求新占的位置（不是带着票来的后续一步）。
    fresh: bool,
    /// 一步就完（不是 `apn_use` 那样的几步）：做完票就作废。
    single: bool,
    /// 描述表里的动作这次不算事务（改的不是正在用的 APN）：照原来的 /control 做了。
    plain: std::sync::atomic::AtomicBool,
}

/// 有效批准者是 mcp 时：只读动作放过；白名单外拒绝（含 `op.keep`/`op.revert`）；APN 层看敏感（读漫游和卡号）；
/// 带着有效的票（`agent_ticket`）是同一次操作的后续一步，不再计数；否则按层占位。
async fn agent_gate(action: &str, body: &Value) -> Result<Option<AgentPass>, Response> {
    use crate::ops::agent::{self, Layer, Refusal};
    let Some(who) = who_of(body) else {
        return Ok(None);
    };
    if who.approved_by != Source::Mcp || agent::read_only(action) {
        return Ok(None);
    }
    let now = agent::now_unix();
    let refuse = |r: Refusal| {
        let (status, code, message, extra) = r.reply(now);
        let mut v = json!({"ok": false, "action": action,
            "error": {"code": code, "message": message}});
        if let (Some(e), Some(m)) = (v["error"].as_object_mut(), extra.as_object()) {
            e.extend(m.clone());
        }
        eprintln!("ops: agent {action} refused: {code}");
        Err((StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN), Json(v)).into_response())
    };
    let Some(layer) = agent::layer_of(action) else {
        return refuse(Refusal::NotAllowed);
    };
    let g = agent::gate();
    if let Some(id) = body.get("agent_ticket").and_then(Value::as_str) {
        return match g.redeem(id, layer, now) {
            Some(ticket) => Ok(Some(AgentPass {
                ticket,
                rollback: None,
                fresh: false,
                single: false,
                plain: Default::default(),
            })),
            None => refuse(Refusal::NotAllowed),
        };
    }
    let mut rollback = None;
    let iccid = if layer == Layer::Apn {
        match crate::ops::agent::apn_sensitivity().await {
            Ok(iccid) => Some(iccid),
            Err(why) => return refuse(Refusal::Sensitive(why)),
        }
    } else {
        // O10：别的 CHILL 工作在跑（含核心重启前起的 chill.sh）就不动；D10：切之前先真路径测一次
        if crate::ops::chill::busy() {
            return refuse(Refusal::Busy("another CHILL job is running".into()));
        }
        if crate::ops::chill::idle_paused() {
            // 暂停时改了 mihomo 会重启一下又停，试用必然测不通、到点退回
            return refuse(Refusal::Off(
                "CHILL is paused while nobody uses the device (screen off, no clients); it resumes on use".into(),
            ));
        }
        match crate::ops::chill::probe().await {
            crate::ops::chill::Probe::Up => {}
            crate::ops::chill::Probe::Down => {
                eprintln!("ops: agent {action}: CHILL does not get through now; no rollback");
                rollback = Some(false);
            }
            crate::ops::chill::Probe::Unknown if !crate::ops::chill::up().await => {
                return refuse(Refusal::Off(
                    "CHILL is not running (mihomo does not answer); the owner turns it on".into(),
                ));
            }
            crate::ops::chill::Probe::Unknown => {
                return refuse(Refusal::Busy(
                    "cannot tell whether CHILL gets through now (mihomo or 127.0.0.1:7894 not answering)".into(),
                ));
            }
        }
        None
    };
    match g.reserve(layer, iccid, now) {
        Ok(ticket) => Ok(Some(AgentPass {
            ticket,
            rollback,
            fresh: true,
            single: true,
            plain: Default::default(),
        })),
        Err(r) => refuse(r),
    }
}

async fn gated(
    app: App,
    action: String,
    body: Value,
    peer: Option<std::net::SocketAddr>,
    pass: Option<&AgentPass>,
) -> Response {
    if let Some(response) = ops_route(&app, &action, &body, pass).await {
        return response;
    }
    let params = body.get("params").cloned().unwrap_or_else(|| json!({}));
    let safety = crate::ops::spec::is_safety(&action, &params);
    let has_source = body.get("source").is_some();
    // 没有 source 的旧请求：锁被占时（描述表里的动作）回成功、排队；同一项当覆盖；关数据马上插队（D14）。
    let legacy_item = (!has_source)
        .then(|| crate::ops::spec::find(&action))
        .flatten()
        .filter(|s| s.target(&params).is_ok())
        .map(|s| s.item);
    if has_source {
        if safety {
            app.inner.ops.preempt();
        }
    } else if app.inner.ops.legacy_gate(legacy_item, safety) == crate::ops::LegacyGate::Queue {
        let item = legacy_item.expect("only described actions queue");
        app.inner.ops.enqueue_legacy(item, &action, params);
        return legacy_queued(&action);
    }
    if matches!(action.as_str(), "device.reboot" | "device.poweroff") {
        app.inner.ops.clear_legacy(&action);
    }
    match run_control_from(app.clone(), action.clone(), body.clone(), peer).await {
        Ok(response) => response,
        // 执行者队列满（V2-25）。关数据（新旧客户端一样，D14）作为内部任务马上做；
        // 旧请求不回 503：描述表里的动作回成功、排队。
        Err(executor::Busy) if safety => {
            let exec = app.inner.exec.clone();
            exec.task(control_job(app, action, body, None))
                .await
                .expect("no Wi-Fi lock asked for")
        }
        Err(executor::Busy) => match legacy_item {
            Some(item) => {
                app.inner.ops.enqueue_legacy(item, &action, params);
                legacy_queued(&action)
            }
            None => control_busy(&action),
        },
    }
}

/// T22（D23）：顶层 `approved_by`（可选）。要有 `source`；和来源不同时只能是 screen 或 web。
/// 来源本身认不出来时不在这里报，交给原来的处理（各自的错误照旧）。
fn check_approved_by(body: &Value) -> Result<(), String> {
    let Some(by) = body.get("approved_by") else {
        return Ok(());
    };
    let Some(by) = by.as_str().and_then(Source::parse) else {
        return Err("approved_by must be a known source".into());
    };
    let Some(source) = body.get("source") else {
        return Err("approved_by needs a top-level source".into());
    };
    match source.as_str().and_then(Source::parse) {
        Some(source) => Who::new(source, Some(by)).map(|_| ()),
        None => Ok(()),
    }
}

/// 请求的身份（来源 + 批准者）；没有或认不出 `source` 为 None。`approved_by` 已由 [`check_approved_by`] 校验过。
fn who_of(body: &Value) -> Option<Who> {
    let source = body
        .get("source")
        .and_then(Value::as_str)
        .and_then(Source::parse)?;
    let by = body
        .get("approved_by")
        .and_then(Value::as_str)
        .and_then(Source::parse);
    Some(Who::new(source, by).unwrap_or_else(|_| source.into()))
}

/// 排进旧请求队列的旧请求：回和今天一样形状的成功（原厂设置调用回 `{"result":"success"}`）。
fn legacy_queued(action: &str) -> Response {
    control_ok(action, json!({"result":"success"}))
}

/// 整个处理过程作为一个控制任务交给执行者（V2-24）：排队中的满 8 个立即 `Busy`（V2-25），
/// 否则挂到做完才回复（和原来一样；请求方断开了任务也做完）。
pub(super) async fn run_control(
    app: App,
    action: String,
    body: Value,
) -> Result<Response, executor::Busy> {
    run_control_from(app, action, body, None).await
}

/// `run_control`，带上调用方的地址（D3：看调用方自己是不是拿着 Wi-Fi 锁）。
/// 写 `wireless` 的动作（`wifi_lock::ACTIONS`）先写锁、后 Wi-Fi 锁，等锁在执行者外（`ops/wifi_lock.rs`）。
pub(super) async fn run_control_from(
    app: App,
    action: String,
    body: Value,
    peer: Option<std::net::SocketAddr>,
) -> Result<Response, executor::Busy> {
    let exec = app.inner.exec.clone();
    let cfg = crate::ops::wifi_lock::Cfg::from_env();
    let path = match &cfg.path {
        Some(p) if crate::ops::wifi_lock::covers_action(&action) => p.clone(),
        _ => {
            return exec
                .control(control_job(app, action, body, None))
                .await
                .map(|r| r.expect("no Wi-Fi lock asked for"));
        }
    };
    // 等锁可能很久（guard、agent 拿着时几十秒），触屏 5 秒就关连接。和执行者里的任务一样，
    // 请求方断开了也要做完（否则这次写悄悄没了、也不记账）：另起一个任务，这里只等它。
    tokio::spawn(wifi_control(app, action, body, peer, cfg, path))
        .await
        .expect("Wi-Fi control task panicked")
}

/// [`run_control_from`] 里写 `wireless` 的那条路（D3）。
async fn wifi_control(
    app: App,
    action: String,
    body: Value,
    peer: Option<std::net::SocketAddr>,
    cfg: crate::ops::wifi_lock::Cfg,
    path: String,
) -> Result<Response, executor::Busy> {
    let exec = app.inner.exec.clone();
    let who = who_of(&body).unwrap_or_else(|| Source::Legacy.into());
    let record = |t: &crate::ops::wifi_lock::Timeout| {
        eprintln!(
            "ops: {action}: Wi-Fi lock {path} still held after {} ms (pid {:?}), attempt {}{}",
            t.waited_ms,
            t.holder,
            t.attempt,
            if t.last { ", giving up" } else { ", retrying" }
        );
        let mut line = json!({
            "action": action,
            "source": who.source,
            "result": "lock_wait",
            "reason": "wifi_lock",
            "waited_ms": t.waited_ms,
            "holder": t.holder,
            "attempt": t.attempt,
        });
        who.tag(&mut line);
        app.inner.ops.record().append(line);
    };
    let done = crate::ops::wifi_lock::run(
        &cfg,
        &path,
        caller_of(peer),
        |mode| {
            let (app, action, body, exec, path) = (
                app.clone(),
                action.clone(),
                body.clone(),
                exec.clone(),
                path.clone(),
            );
            async move {
                match exec
                    .control(control_job(app, action, body, Some((path, mode))))
                    .await
                {
                    Err(busy) => Some(Err(busy)),
                    Ok(None) => None,
                    Ok(Some(r)) => Some(Ok(r)),
                }
            }
        },
        record,
    )
    .await;
    if let Some(r) = done {
        return r;
    }
    let mut line = json!({
        "action": action,
        "source": who.source,
        "result": "failed",
        "reason": "wifi_lock",
        "status": 503,
    });
    who.tag(&mut line);
    app.inner.ops.record().append(line);
    Ok((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"ok":false,"action":action,"error":{
            "code":"busy",
            "message":"Wi-Fi is being changed by another writer; try again",
        }})),
    )
        .into_response())
}

/// 调用方是谁（只在 Wi-Fi 锁被占时查）：本机连接按端口在 /proc 里找进程（`peer_pid::find_pid`）。
async fn caller_of(peer: Option<std::net::SocketAddr>) -> crate::ops::wifi_lock::Caller {
    use crate::ops::wifi_lock::Caller;
    let Some(peer) = peer else {
        return Caller::Unknown;
    };
    let ip = match peer.ip() {
        std::net::IpAddr::V6(v6) => v6
            .to_ipv4_mapped()
            .map_or(std::net::IpAddr::V6(v6), std::net::IpAddr::V4),
        ip => ip,
    };
    if !ip.is_loopback() {
        return Caller::Remote;
    }
    let port = peer.port();
    match crate::state::blocking(move || crate::peer_pid::find_pid(port)).await {
        Some(pid) => Caller::Pid(pid),
        None => Caller::Unknown,
    }
}

/// 一个控制任务。`wifi` = 写 `wireless` 的动作要的 Wi-Fi 锁（路径、怎么拿）：拿着写锁之后再拿；
/// 别人拿着（`Mode::Own`）就两把都放、返回 None（什么都没做，调用方在执行者外等一会再来）。
async fn control_job(
    app: App,
    action: String,
    body: Value,
    wifi: Option<(String, crate::ops::wifi_lock::Mode)>,
) -> Option<Response> {
    let marker = app.inner.exec.clone();
    let writes = (crate::control::ACTIONS.contains(&action.as_str())
        || crate::control::E4_ACTIONS.contains(&action.as_str()))
        && !read_only(&action);
    if writes && let Some(response) = other_write_gate(&app, &action, &body) {
        return Some(response);
    }
    // D29：会改设备的动作在执行者里拿着跨进程写锁做（和应急直写脚本互斥）。
    let _lock = if writes {
        Some(crate::ops::write_lock::acquire().await)
    } else {
        None
    };
    // D3：第二把（Wi-Fi 锁）在写锁之后拿，先于写锁放（变量后声明、先丢）。
    let _wifi = match &wifi {
        Some((path, mode)) => {
            let write_held = _lock.as_ref().is_none_or(|l| l.held());
            crate::ops::wifi_lock::second(path, *mode, write_held).ok()?
        }
        None => None,
    };
    let journal = writes.then(|| JournalWrite::new(&action, &body)).flatten();
    let ends = ends_device(&action, &body);
    // 重启、关机、恢复出厂：先把这一行写进闪存再做（之后没机会了）。
    if let Some(j) = &journal
        && ends
    {
        j.line(&app, "requested", None);
        app.inner.ops.record().flush().await;
    }
    // 账本：下次开机记成「有人要的」（B31 上手动重启和基带崩溃同一个原因码 1134）
    if ends {
        let why = body["source"].as_str().unwrap_or("control");
        if let Err(e) = crate::longterm::boot::plan(why) {
            eprintln!("[control] planned-reboot marker: {e}");
        }
    }
    // A remembered screen choice is automation, even though its transport says screen.
    // Missing/legacy sources and internal callers cannot authorize an unknown USB write.
    let usb_human = usb_human(&body);
    let response = executor::usb_human_scope(usb_human, control_task(app.clone(), &action, body)).await;
    if let Some(j) = &journal
        && !ends
    {
        let ok = response.status().is_success();
        j.line(
            &app,
            if ok { "ok" } else { "failed" },
            (!ok).then_some(response.status().as_u16()),
        );
        if ok {
            j.owner(&app);
        }
    }
    // V2-27：成功后相关块下一轮立即读（没有映射就全部块），不另起一轮采集。
    if response.status().is_success() && !read_only(&action) {
        crate::state::invalidate_cache();
        marker.mark_immediate(blocks_for_action(&action));
    }
    Some(response)
}

fn usb_human(body: &Value) -> bool {
    who_of(body).is_some_and(|who| who.approved_by.can_confirm())
        && body["params"]["remembered"] != true
}

/// D40：事务在确认或退回中时，影响上网的写（安全类写除外，照旧插队）按来源处理：用户的（screen、web、
/// 没有 source 的旧请求）照做并取消这次自动退回（other_change）；自动来源的回 409 `op_busy`，不做、不记账。
/// 在执行者里、真要做之前判断（执行者队列满回 503 的请求不会取消事务）。
fn other_write_gate(app: &App, action: &str, body: &Value) -> Option<Response> {
    let empty = json!({});
    let params = body.get("params").unwrap_or(&empty);
    if crate::ops::spec::is_safety(action, params)
        || !crate::ops::spec::affects_network(action, params)
    {
        return None;
    }
    // 用户与否按批准者判断（T22）
    let who = who_of(body).unwrap_or_else(|| Source::Legacy.into());
    let op = app.inner.ops.other_write(who).err()?;
    Some(op_busy(action, op))
}

/// D40 的 409：`{"ok":false,"action":…,"error":{"code":"op_busy","message":…,"op":{op_id,item,phase}}}`。
fn op_busy(action: &str, op: Value) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({"ok":false,"action":action,"error":{
            "code":"op_busy",
            "message":"a change is being confirmed; try again after it ends",
            "op":op,
        }})),
    )
        .into_response()
}

/// 不走事务的写记一行流水账（T5）：旧请求直接执行的、带来源但不在描述表里的。
/// `sms.mark_read` 太频繁，不记；短信动作只记动作名（`record::redact`）。
struct JournalWrite {
    action: String,
    who: Who,
    params: Value,
    /// 描述表里的动作：改的项和目标值（成功后记 owners）。
    item: Option<(&'static str, Value)>,
}

impl JournalWrite {
    fn new(action: &str, body: &Value) -> Option<Self> {
        if action == "sms.mark_read" {
            return None;
        }
        let empty = json!({});
        let params = body.get("params").unwrap_or(&empty);
        let who = who_of(body).unwrap_or_else(|| Source::Legacy.into());
        // 带 source 的描述表动作走到这里只会是「不算事务」的（改不在用的 APN 等）：不占这一项，
        // 免得挡住上一次真改动的撤销；旧请求照旧占（它们按原来的处理写进去）。
        let item = (body.get("source").is_none())
            .then(|| crate::ops::spec::find(action))
            .flatten()
            .and_then(|s| s.target(params).ok().map(|v| (s.item, v)))
            .filter(|(_, v)| v.get("modify").is_none());
        Some(Self {
            action: action.to_owned(),
            who,
            params: crate::ops::record::redact(action, params),
            item,
        })
    }

    fn line(&self, app: &App, result: &str, status: Option<u16>) {
        let mut line = json!({
            "action": self.action,
            "item": self.item.as_ref().map(|i| i.0),
            "source": self.who.source,
            "params": self.params,
            "result": result,
            "status": status,
        });
        self.who.tag(&mut line);
        app.inner.ops.record().append(line);
    }

    fn owner(&self, app: &App) {
        if let Some((item, value)) = &self.item {
            app.inner
                .ops
                .record()
                .set_owner(item, self.who, false, value, None);
        }
    }
}

/// `journal.append`（只记账，T5）：eSIM、CHILL 这类不经 `/control` 的改动由 agent 补记。
/// 要顶层 `source`；`params.result` 必填，`skipped` 按 D27 合并。不受事务锁、不进执行者。
fn journal_append(app: &App, action: &str, body: &Value, params: &Value) -> Response {
    let Some(who) = who_of(body) else {
        return invalid_parameter(action, "journal.append needs a known top-level source");
    };
    let Some(result) = params
        .get("result")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return invalid_parameter(action, "missing parameter: result");
    };
    let Some(what) = params
        .get("item")
        .or_else(|| params.get("action"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    else {
        return invalid_parameter(action, "missing parameter: item or action");
    };
    let record = app.inner.ops.record();
    if result == "skipped" {
        let reason = params.get("reason").and_then(Value::as_str).unwrap_or("");
        record.skip(who.source.as_str(), what, reason);
    } else {
        let mut line = crate::ops::record::redact(what, params);
        line["source"] = json!(who.source);
        who.tag(&mut line);
        line["journal_append"] = json!(true);
        record.append(line);
    }
    control_ok(action, json!({"recorded":true}))
}

/// E4：`op.status` / `op.revert` / `op.keep`，以及带 source 的、描述表里的写（走事务）。
/// 其他请求返回 None，照原来的处理。
async fn ops_route(
    app: &App,
    action: &str,
    body: &Value,
    pass: Option<&AgentPass>,
) -> Option<Response> {
    let ops = &app.inner.ops;
    let empty = json!({});
    let params = body.get("params").unwrap_or(&empty);
    let op_id = params.get("op_id").and_then(Value::as_str);
    match action {
        "op.status" => return Some(control_ok(action, ops.status(op_id))),
        "journal.append" => return Some(journal_append(app, action, body, params)),
        "journal.list" => {
            let limit = params
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 500) as usize;
            // 先等前面接受的行落盘，刚记的也读得到。
            ops.record().flush().await;
            // E4 T8c（V2-41）：每行加上界面直接显示的字段
            let mut entries = ops.record().list(limit);
            crate::ops::journal_view::decorate(&mut entries);
            return Some(control_ok(
                action,
                json!({"entries": entries, "owners": ops.record().owners()}),
            ));
        }
        "op.ack" => {
            let Some(op_id) = op_id else {
                return Some(invalid_parameter(action, "missing parameter: op_id"));
            };
            // 「知道了」是用户点的：只认触屏和网页（V2-37）；按批准者看（T22）。
            let who = who_of(body).filter(|w| w.approved_by.can_confirm());
            let Some(who) = who else {
                return Some(invalid_parameter(
                    action,
                    "op.ack needs source screen or web",
                ));
            };
            return Some(match ops.ack(op_id, who) {
                Ok(v) => control_ok(action, v),
                Err(e) => op_error(action, StatusCode::CONFLICT, "invalid_state", &e, None),
            });
        }
        // D40：agent 里不经 datad 的用户写（eSIM 切换、AT 终端）之前发，取消进行中的自动退回。
        // DD18：「自动退回已打开」的提示点「知道了」。都只认触屏和网页。
        "op.interrupt" | "op.notice_ack" => {
            let what = params
                .get("what")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty());
            if action == "op.interrupt" && what.is_none() {
                return Some(invalid_parameter(action, "missing parameter: what"));
            }
            let who = who_of(body).filter(|w| w.approved_by.can_confirm());
            let Some(who) = who else {
                return Some(invalid_parameter(
                    action,
                    &format!("{action} needs source screen or web"),
                ));
            };
            return Some(control_ok(
                action,
                match what {
                    Some(what) if action == "op.interrupt" => ops.interrupt(who, what),
                    _ => ops.notice_ack(who),
                },
            ));
        }
        "op.revert" | "op.keep" => {
            let Some(op_id) = op_id else {
                return Some(invalid_parameter(action, "missing parameter: op_id"));
            };
            let r = if action == "op.revert" {
                ops.revert(op_id)
            } else {
                ops.keep(op_id)
            };
            return Some(match r {
                Ok(v) => control_ok(action, v),
                Err(e) => op_error(action, StatusCode::CONFLICT, "invalid_state", &e, None),
            });
        }
        _ => {}
    }
    // 搜网会话和它的步骤只给带来源的新客户端（D17）
    let session_action =
        action.starts_with("netselect.") || crate::ops::engine::SESSION_OPTIONAL.contains(&action);
    // Wi-Fi 设置（第四批 4-0b）只走事务：没有来源的旧请求不排队（排队的旧请求做的是原来的 /control，这里没有）
    let wifi_settings =
        crate::ops::spec::find(action).is_some_and(|s| crate::ops::spec::is_wifi(s.item));
    if (session_action || wifi_settings) && body.get("source").is_none() {
        return Some(op_error(
            action,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "source required",
            None,
        ));
    }
    body.get("source")?;
    let Some(who) = who_of(body) else {
        return Some(op_error(
            action,
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "unknown source",
            None,
        ));
    };
    let session_err = |e: crate::ops::engine::SessionError| match e {
        crate::ops::engine::SessionError::Busy(doing) => op_error(
            action,
            StatusCode::CONFLICT,
            "busy",
            "another change is in progress",
            Some(("doing", doing)),
        ),
        crate::ops::engine::SessionError::Gone => op_error(
            action,
            StatusCode::CONFLICT,
            "invalid_state",
            "no such session (ended or expired)",
            None,
        ),
    };
    let session_id = |v: &Value| v.get("session").and_then(Value::as_str).map(str::to_owned);
    match action {
        "netselect.session.open" => {
            return Some(match ops.session_open(who) {
                Ok(v) => control_ok(action, v),
                Err(e) => session_err(e),
            });
        }
        "netselect.session.renew" | "netselect.session.close" => {
            let Some(id) = session_id(params) else {
                return Some(invalid_parameter(action, "missing parameter: session"));
            };
            let r = if action.ends_with("renew") {
                ops.session_renew(&id)
            } else {
                let result = params.get("result").and_then(Value::as_str);
                ops.session_close(&id, result)
                    .map(|()| json!({"session": id}))
            };
            return Some(match r {
                Ok(v) => control_ok(action, v),
                Err(e) => session_err(e),
            });
        }
        _ => {}
    }
    // 关数据 / 关漫游（D14）在会话期间也照做：在国外这是最要紧的写
    if !crate::ops::spec::is_safety(action, params)
        && let Err(e) = ops.session_gate(action, session_id(body).as_deref())
    {
        return Some(session_err(e));
    }
    let spec = crate::ops::spec::find(action)?;
    let target = match spec.target(params) {
        Ok(v) => v,
        Err(e) => return Some(invalid_parameter(action, &e)),
    };
    let op_id = match body.get("op_id") {
        None => None,
        Some(Value::String(s)) if crate::ops::engine::valid_op_id(s) => Some(s.clone()),
        Some(_) => {
            return Some(op_error(
                action,
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "op_id must be 1-64 letters, digits, '.', '_' or '-'",
                None,
            ));
        }
    };
    let undo = body.get("undo").and_then(Value::as_bool).unwrap_or(false);
    let request = crate::ops::Request {
        spec,
        target,
        params: params.clone(),
        source: who.source,
        approved_by: who.approved_by,
        op_id,
        undo,
        rollback: pass.and_then(|p| p.rollback),
        expect_iccid: pass.and_then(|p| p.ticket.iccid.clone()),
    };
    let submitted = if crate::ops::spec::is_wifi(spec.item) {
        // Wi-Fi 设置：提交之前拿 Wi-Fi 锁（和 agent 一样最多等 5 秒），拿不到就是忙，事务还没开始。
        // 拿锁、提交、放掉没用上的锁在另一个任务里做完：请求方断开（触屏 5 秒就关）也不会把锁留在交接处。
        let task = tokio::spawn(wifi_submit(ops.clone(), request));
        match task.await.expect("Wi-Fi submit task panicked") {
            Err(e) => {
                eprintln!("ops: {action}: {e}");
                return Some(op_error(
                    action,
                    StatusCode::SERVICE_UNAVAILABLE,
                    "busy",
                    "Wi-Fi is being changed by another writer; try again",
                    None,
                ));
            }
            // 没有要改的：不是事务，也不用写（原来的 /control 没有这个动作）
            Ok(crate::ops::Submit::Plain) => {
                return Some(control_ok(
                    action,
                    json!({"status": "ok", "note": "no changes"}),
                ));
            }
            Ok(s) => s,
        }
    } else {
        ops.submit(request).await
    };
    // 改的不是正在用的 APN：不是事务，照原来的 /control 做
    if matches!(submitted, crate::ops::Submit::Plain) {
        if action == "apn.use" {
            // 已经是这样（原来的 /control 没有这个动作）
            return Some(control_ok(
                action,
                json!({"status": "ok", "note": "no changes"}),
            ));
        }
        if let Some(p) = pass {
            p.plain.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        return None;
    }
    Some(match submitted {
        crate::ops::Submit::Plain => unreachable!("handled above"),
        crate::ops::Submit::Refused(e) => {
            op_error(action, StatusCode::CONFLICT, "refused", &e, None)
        }
        crate::ops::Submit::Existing(op) => (
            StatusCode::OK,
            Json(json!({"ok":true,"action":action,"op":op})),
        )
            .into_response(),
        crate::ops::Submit::Busy(doing) => op_error(
            action,
            StatusCode::CONFLICT,
            "busy",
            "another change is in progress",
            Some(("doing", doing)),
        ),
        crate::ops::Submit::NoCapture(e) => control_failed(action, e),
        crate::ops::Submit::Applied { result: Ok(v), op } => (
            StatusCode::OK,
            Json(json!({"ok":true,"action":action,"result":v,"op":op})),
        )
            .into_response(),
        crate::ops::Submit::Applied { result: Err(e), op } => op_error(
            action,
            StatusCode::BAD_GATEWAY,
            "device_call_failed",
            &e,
            Some(("op", op)),
        ),
    })
}

/// Wi-Fi 设置的提交：拿 Wi-Fi 锁、按交接号交给这次写（`ops::wifi_dev`），提交完放掉没用上的。
async fn wifi_submit(
    ops: crate::ops::Engine<crate::ops::UbusDevice>,
    mut req: crate::ops::Request,
) -> Result<crate::ops::Submit, String> {
    let token = ops.device().wifi.take_for_request().await?;
    req.params[crate::ops::wifi_dev::LOCK_TOKEN] = json!(token);
    let submitted = ops.submit(req).await;
    ops.device().wifi.release(token);
    Ok(submitted)
}

fn op_error(
    action: &str,
    status: StatusCode,
    code: &str,
    message: &str,
    extra: Option<(&str, Value)>,
) -> Response {
    let mut body = json!({"ok":false,"action":action,"error":{"code":code,"message":message}});
    if let Some((k, v)) = extra {
        body[k] = v;
    }
    (status, Json(body)).into_response()
}

/// 做完设备就没了（重启、关机、恢复出厂）：流水账先记 requested 并落盘。
fn ends_device(action: &str, body: &Value) -> bool {
    if matches!(action, "device.reboot" | "device.poweroff") {
        return true;
    }
    let p = &body["params"];
    action == "vendor.call"
        && matches!(
            (p["object"].as_str(), p["method"].as_str()),
            (Some("zwrt_mc.device.manager"), Some("device_reset"))
                | (Some("system"), Some("reboot"))
        )
}

/// 只读、不改设备的动作：不清慢数据缓存、不标块（`sms.list_after` 翻页时每页一次，清缓存会让下一轮全部重读）。
/// `state.set_interval` 只改本进程执行者的采样间隔（`Executor::set_interval_ms`，一个原子数 + 唤醒），
/// 不碰设备；触屏每次亮灭屏都调，以前每次都清全部缓存、全量重读（audit-2026-10-06 附录 A）。
fn read_only(action: &str) -> bool {
    matches!(action, "sms.list_after" | "state.set_interval")
}

/// `/control` 动作 → 它会改变的块。没列出的动作算「没有映射」，成功后全部块立即读（R12）。
fn blocks_for_action(action: &str) -> Option<&'static [&'static str]> {
    match action {
        "power.direct_supply.set" => Some(&["charger"]),
        "usb.attach_mode" => Some(&["typec", "powerbank", "charger"]),
        "sms.list_after" => Some(&[]),
        _ => None,
    }
}

/// V2-25：控制队列满。
pub(crate) fn control_busy(action: &str) -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"ok":false,"action":action,"error":{"code":"busy","message":"control queue full"}})),
    )
        .into_response()
}

async fn control_task(app: App, action: &str, body: Value) -> Response {
    // Whatever this action changes, the cached slow-moving state may now be
    // stale (state.rs `invalidate_cache`); it is cleared again after a
    // successful action (the task runs on the executor, so no round can refill
    // the cache with pre-action values in between).
    if !read_only(action) {
        crate::state::invalidate_cache();
    }
    match crate::control::execute(action, body.get("params").unwrap_or(&json!({}))).await {
        crate::control::Outcome::Ok(value) => {
            return control_ok(action, value);
        }
        crate::control::Outcome::Invalid(error) => return invalid_parameter(action, &error),
        crate::control::Outcome::CapabilityDenied(error) => return (
            StatusCode::from_u16(error.status()).expect("fixed capability refusal status"),
            Json(json!({"ok":false, "action":action, "error":error})),
        ).into_response(),
        crate::control::Outcome::Failed(error) => return control_failed(action, error),
        crate::control::Outcome::NotHandled => {}
    }
    if action == "state.set_interval" {
        let milliseconds = body
            .get("params")
            .and_then(|value| value.get("milliseconds"))
            .and_then(Value::as_u64);
        let Some(milliseconds) = milliseconds.filter(|value| (500..=5000).contains(value)) else {
            return (StatusCode::BAD_REQUEST,Json(json!({"ok":false,"action":action,"error":{"code":"invalid_parameter","message":"milliseconds must be between 500 and 5000"}}))).into_response();
        };
        app.inner.exec.set_interval_ms(milliseconds);
        return control_ok(action, json!({"sample_interval_ms":milliseconds}));
    }
    (
        StatusCode::NOT_FOUND,
        Json(json!({"ok":false,"action":action,"error":{"code":"unknown_action","message":"unsupported control action"}})),
    )
        .into_response()
}

fn control_ok(action: &str, value: Value) -> Response {
    (
        StatusCode::OK,
        Json(json!({"ok":true,"action":action,"result":value})),
    )
        .into_response()
}

fn control_failed(action: &str, error: String) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({"ok":false,"action":action,"error":{"code":"device_call_failed","message":error}})),
    )
        .into_response()
}

fn invalid_parameter(action: &str, message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"ok":false,"action":action,"error":{"code":"invalid_parameter","message":message}})),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[test]
    fn usb_manual_authority_never_follows_transport_alone() {
        for source in [
            "auto",
            "scenario",
            "scheduler",
            "guard",
            "legacy",
            "mcp",
            "",
        ] {
            assert!(
                !super::usb_human(&serde_json::json!({"source":source,"params":{}})),
                "{source}"
            );
        }
        for source in ["web", "screen"] {
            assert!(super::usb_human(
                &serde_json::json!({"source":source,"params":{}})
            ));
            assert!(!super::usb_human(
                &serde_json::json!({"source":source,"params":{"remembered":true}})
            ));
        }
        assert!(super::usb_human(
            &serde_json::json!({"source":"mcp","approved_by":"web","params":{}})
        ));
        assert!(!super::usb_human(&serde_json::json!({"params":{}})));
    }

    use super::*;

    #[test]
    fn set_interval_is_read_only_and_not_a_device_write() {
        assert!(read_only("state.set_interval"));
        assert!(read_only("sms.list_after"));
        assert!(!read_only("device.reboot"));
        assert!(!read_only("power.direct_supply.set"));
        // 不在会改设备的动作表里：不拿写锁、不记流水账
        assert!(!crate::control::ACTIONS.contains(&"state.set_interval"));
        assert!(!crate::control::E4_ACTIONS.contains(&"state.set_interval"));
    }

    #[test]
    fn approved_by_is_checked_against_the_source() {
        let ok = |b: Value| check_approved_by(&b).is_ok();
        // 没有：和以前一样
        assert!(ok(json!({"action":"network.set_mode","source":"web"})));
        assert!(ok(json!({"action":"network.set_mode"})));
        // 人确认了 MCP 的提议
        assert!(ok(json!({"source":"mcp","approved_by":"screen"})));
        assert!(ok(json!({"source":"mcp","approved_by":"web"})));
        // Agent 自己批准（第二期 D7：放不放行看动作，见 agent_gate）；别的来源不能写 mcp 当批准者
        assert!(ok(json!({"source":"mcp","approved_by":"mcp"})));
        assert!(!ok(json!({"source":"web","approved_by":"mcp"})));
        // 批准者只能是触屏、网页，或者就是来源
        assert!(!ok(json!({"source":"mcp","approved_by":"guard"})));
        assert!(!ok(json!({"source":"mcp","approved_by":"scenario"})));
        assert!(!ok(json!({"source":"mcp","approved_by":"nobody"})));
        assert!(!ok(json!({"source":"mcp","approved_by":1})));
        assert!(!ok(json!({"approved_by":"screen"})));
        // 来源认不出来：交给原来的处理报错
        assert!(ok(json!({"source":"nobody","approved_by":"screen"})));
        let who = who_of(&json!({"source":"mcp","approved_by":"screen"})).unwrap();
        assert_eq!((who.source, who.approved_by), (Source::Mcp, Source::Screen));
        assert_eq!(
            who_of(&json!({"source":"web"})).unwrap(),
            Who::from(Source::Web)
        );
        assert!(who_of(&json!({})).is_none());
    }

    /// 第二期 D7、O1：有效批准者是 mcp（不带 approved_by 也算）时白名单外一律 403，包括对进行中事务的保留/退回；
    /// 只读的放过；人确认过的（approved_by=screen）不过这道关。
    #[tokio::test]
    async fn agent_writes_outside_the_whitelist_are_refused() {
        for (action, body) in [
            ("esim.switch", json!({"source":"mcp"})),
            ("sim.set_slot", json!({"source":"mcp","approved_by":"mcp"})),
            ("network.set_mode", json!({"source":"mcp"})),
            ("device.reboot", json!({"source":"mcp"})),
            ("op.keep", json!({"source":"mcp"})),
            ("op.revert", json!({"source":"mcp"})),
            ("wifi.settings", json!({"source":"mcp"})),
        ] {
            match agent_gate(action, &body).await {
                Err(r) => assert_eq!(r.status(), StatusCode::FORBIDDEN, "{action}"),
                Ok(_) => panic!("{action} let through"),
            }
        }
        // 编的票不算
        let r = agent_gate("apn.add", &json!({"source":"mcp","agent_ticket":"0000"})).await;
        assert!(r.is_err());
        for (action, body) in [
            ("op.status", json!({"source":"mcp"})),
            ("journal.list", json!({"source":"mcp"})),
            ("network.set_mode", json!({"source":"mcp","approved_by":"screen"})),
            ("network.set_mode", json!({"source":"web"})),
            ("network.set_mode", json!({})),
        ] {
            assert!(matches!(agent_gate(action, &body).await, Ok(None)), "{action} {body}");
        }
    }
}
