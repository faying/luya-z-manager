//! An alert's detail in Chinese, for the screens that speak it.
//!
//! The queue keeps printable ASCII only (`append_locked`, alert-lib.sh), so every producer writes
//! its detail in English: short, with the numbers that matter. [`detail_zh`] words the known
//! shapes in Chinese when they are read. The stored text never changes: the MCP agent, the SMS
//! hint (`sms::regress_hint_zh`) and rows already in the inbox all read the English.
//!
//! `None` = not a shape known here: show the text as stored (names, MACs and paths read the same
//! in either language). `Some("")` = nothing to add to the kind's label: hide the line.

use crate::text_body;

/// The detail of one alert (`text` as queued, id and all) in Chinese; see the module note.
pub fn detail_zh(kind: &str, text: &str) -> Option<String> {
    let body = text_body(text).trim();
    if body.is_empty() {
        return Some(String::new());
    }
    if body == crate::kind_label_en(kind) {
        return Some(String::new());
    }
    if kind.starts_with("regress-") {
        // a reboot row's 2nd/4th/8th alert: `#n <detail>`
        if let Some((n, rest)) = body
            .strip_prefix('#')
            .and_then(|r| r.split_once(' '))
            .filter(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            let inner = regress(kind, rest.trim()).unwrap_or_else(|| rest.trim().to_string());
            return Some(format!("第 {n} 次：{inner}"));
        }
        // a close sits under the open's label (「出口不通 5 分钟以上」): say it is over
        let closed = crate::text_id(text).is_some_and(|id| id.ends_with("-close"));
        return regress(kind, body).map(|d| match closed && !d.is_empty() {
            true if !d.contains("恢复") && !d.contains("不再跟踪") => format!("已恢复：{d}"),
            _ => d,
        });
    }
    point(kind, body)
}

/// `pat` with `{}` holes against `s`: the pieces between the literals, or `None`. A hole takes
/// the shortest run up to the next literal; the last hole (with no literal after it) takes the
/// rest. Holes are never empty.
fn caps<'a>(pat: &str, s: &'a str) -> Option<Vec<&'a str>> {
    let lits: Vec<&str> = pat.split("{}").collect();
    let mut rest = s.strip_prefix(lits[0])?;
    let mut out = Vec::new();
    for (i, lit) in lits.iter().enumerate().skip(1) {
        let last = i == lits.len() - 1;
        if last && lit.is_empty() {
            if rest.is_empty() {
                return None;
            }
            out.push(rest);
            rest = "";
        } else {
            let at = if last {
                // the final literal ends the text
                rest.strip_suffix(lit).map(|h| h.len()).filter(|&n| n > 0)?
            } else {
                rest.get(1..)?.find(lit)? + 1
            };
            out.push(&rest[..at]);
            rest = &rest[at + lit.len()..];
        }
    }
    rest.is_empty().then_some(out)
}

/// How a supervised program ended (`exit 1`, `killed by SIGABRT`, …), in Chinese when `zh`;
/// the crash-log list words it too.
pub fn exit_why(why: &str, zh: bool) -> String {
    if !zh {
        return why.to_string();
    }
    if why == "exited, status unknown" {
        return "退出原因不明".into();
    }
    if let Some(c) = why.strip_prefix("exit ") {
        return format!("退出码 {c}");
    }
    if let Some(n) = why.strip_prefix("killed by signal ") {
        return format!("被信号 {n} 结束");
    }
    if let Some(s) = why.strip_prefix("killed by ") {
        return format!("被 {s} 结束");
    }
    why.to_string()
}

fn heartbeat_why(why: &str) -> String {
    if let Some(n) = caps("no heartbeat for {}s", why) {
        return format!("{} 秒没有心跳", n[0]);
    }
    match why {
        "heartbeat file gone" => "心跳文件不见了".into(),
        "no heartbeat since boot" => "开机后一直没有心跳".into(),
        _ => why.to_string(),
    }
}

fn ship_phase(p: &str) -> &str {
    match p {
        "rolledback" => "已退回旧版",
        "failed" => "退回失败",
        "aborted" => "已中止",
        "done" => "已完成",
        _ => p,
    }
}

fn home_fail(code: &str) -> &str {
    match code {
        "core" => "代理没重启成功",
        "home_no_answer" => "家里 Mac 没回应",
        "tailscale_down" => "Tailscale 没连上家里",
        "direct_down" => "直连测试不通",
        _ => code,
    }
}

fn heat_action(a: &str) -> Option<&'static str> {
    Some(match a {
        "charging paused, CHILL to eco" => "暂停充电、CHILL 降到省电档，凉下来自动恢复",
        "charging paused" => "暂停充电，凉下来自动恢复",
        "CHILL to eco" => "CHILL 降到省电档，凉下来自动恢复",
        "move it somewhere airy" => "放到通风的地方",
        _ => return None,
    })
}

/// The kinds that are not regress incidents.
fn point(kind: &str, b: &str) -> Option<String> {
    let c = |pat: &str| caps(pat, b);
    Some(match kind {
        "core-crash" | "web-crash" | "sentinel-crash" | "ui-crash" => {
            let (prog, why) = b.split_once(' ')?;
            format!("{prog}：{}", exit_why(why, true))
        }
        "ui-gave-up" => {
            let n = c("{} launches did not stay up; vendor UI on screen")?;
            format!("连续 {} 次启动都没撑住，屏幕换回原厂界面", n[0])
        }
        "screen-hold-expired"
        | "screen-two-uis"
        | "ui-theme-paused"
        | "wifi-restore-failed"
        | "dropbear-down"
        | "dropbear-restarted"
        | "esim-rebooted"
        | "wifi-5g-down"
        | "sim-changed"
        | "sim-stalled" => {
            // the label already says it (sim-*: the epoch is for the logs)
            let known = [
                "screen held for ship past its deadline; sentinel took it back",
                "vendor UI started next to ours; ours stopped",
                "automatic light/dark switching paused until reboot (3 switches within an hour)",
                "could not turn Wi-Fi on; retrying",
                "SSH (:2222) keeps going away; restart limit reached",
                "SSH (:2222) was not listening; started it again",
                "5 GHz Wi-Fi is still down 5 minutes later",
            ];
            let sim = c("SIM identity changed (epoch {})")
                .or_else(|| c("SIM change stuck without data (epoch {})"));
            if !known.contains(&b) && sim.is_none() {
                return None;
            }
            String::new()
        }
        "wifi-takeover" => {
            let w = c("agent gone ({}), Wi-Fi was off; turning it on")?;
            format!(
                "数据服务失联（{}），Wi-Fi 原本关着，已重新打开",
                heartbeat_why(w[0])
            )
        }
        "agent-silent" => heartbeat_why(b),
        "sentinel-silent" => {
            let n = c("luyaz-sentinel heartbeat {}s old")?;
            format!("哨兵心跳已 {} 秒没更新", n[0])
        }
        "screen-empty" => {
            let n = c("no UI on screen for {}s")?;
            format!("屏幕上 {} 秒没有界面", n[0])
        }
        "sms-failed" => {
            let n = c("alert {} ({}) not sent")?;
            format!("第 {} 条没发出去：{}", n[0], crate::kind_label(n[1]))
        }
        "sim-standby-changed" => {
            let (slot, what) = b.strip_prefix("slot ")?.split_once(": ")?;
            let own = what.strip_suffix(" (switched on the eSIM page)");
            let w = own.unwrap_or(what);
            let said = if w == "card out" {
                "卡拔出了".to_string()
            } else if let Some(t) = caps("card in (...{})", w) {
                format!("插入了一张卡（…{}）", t[0])
            } else if let Some(t) = caps("another card (...{})", w) {
                format!("换成了另一张卡（…{}）", t[0])
            } else {
                format!("eSIM 换成了 profile …{}", caps("profile now ...{}", w)?[0])
            };
            let by = if own.is_some() { "（你在 eSIM 页切的）" } else { "" };
            format!("卡槽 {slot}：{said}{by}")
        }
        "voice-card-followed" => {
            let n = c("voice card moved to slot {}")?;
            format!("电话卡现在在卡槽 {}，电话设置已跟过去", n[0])
        }
        "voice-card-missing" => {
            let n = c("voice card not in either slot; slot {} takes calls")?;
            format!("设定的电话卡两个卡槽都没有，现在用卡槽 {} 接电话", n[0])
        }
        "sms-held" => {
            let n = c("alert SMS held {} min: the device clock stepped and is not trusted yet")?;
            format!("已暂缓 {} 分钟，等设备时间重新可信", n[0])
        }
        "home-exit-down" => {
            if b == "home exit silent 15 min; Rest via home turned off" {
                "家里出口 15 分钟没回应，「其余都走家里」已关".into()
            } else {
                // core 1.98.4 and older wrote Chinese, which the queue dropped: only scraps left
                String::new()
            }
        }
        "home-exit-check-failed" => match c("check failed ({}); turned back off") {
            Some(n) => format!("自检没过（{}），已关回去", home_fail(n[0])),
            None => String::new(),
        },
        "tailnet-poller" => poller(b),
        "heat-protect" => heat(b),
        "esim-failed" => {
            let n = c("eSIM switch failed: {}")?;
            format!("原因：{}", n[0])
        }
        "wifi-post-stuck" => {
            let n = c("zte_post_wifi.sh alive {}s, enabled {} vs hostapd_cli {}")?;
            format!(
                "原厂 Wi-Fi 脚本已跑 {} 秒；配置里开着 {} 个，hostapd_cli 看到 {} 个",
                n[0], n[1], n[2]
            )
        }
        "no-service-long" => {
            let n = c("no service {}s, no stock recovery seen")?;
            format!("无服务 {} 秒，原厂没有自己恢复", n[0])
        }
        "ops-unsaved" => {
            let n = c("{}: the change went ahead; its rollback record could not be saved ({})")?;
            format!("{}：改动已生效，但回滚记录没存下（{}）", n[0], n[1])
        }
        "wifi-5g-fallback" => {
            let n = c("5 GHz Wi-Fi stopped; moved to channel {} at 80 MHz")?;
            format!("5 GHz Wi-Fi 停了，已换到 {} 信道（80 MHz）", n[0])
        }
        "chill-direct-kept" => {
            // cut at 120 characters: only the head is sure to be there
            let rest = b.strip_prefix("CHILL is back on but still All direct (since ")?;
            let (inside, _) = rest.split_once(')')?;
            let since = match inside.split_once(", by ") {
                Some((s, by)) => format!("从 {s} 起，由 {by} 改的"),
                None => format!("从 {inside} 起"),
            };
            format!("CHILL 重新打开后仍是全部直连（{since}）；不是有意的话，到 CHILL 页切回代理")
        }
        "ship-auto-rollback" => {
            if let Some(n) = c("crash loop after {} ship: {}") {
                format!("换上 {} 后反复崩溃：{}", n[0], ship_phase(n[1]))
            } else if let Some(n) = c("reboot loop goes on after {} rollbacks: needs a person") {
                format!("退回 {} 次后仍在重启循环，需要人来处理", n[0])
            } else if let Some(n) = c("reboot loop after {} ship: rollback incomplete") {
                format!("换上 {} 后反复重启：没退回完", n[0])
            } else {
                let n = c("reboot loop after {} ship: back to the old version")?;
                format!("换上 {} 后反复重启：已退回旧版", n[0])
            }
        }
        "ship-conn-rollback" => {
            let n = c("conn lost: {} after {} ship: {}")?;
            format!("换上 {} 后连不上（{}）：{}", n[1], n[0], ship_phase(n[2]))
        }
        "ship-prev-damaged" => {
            let n = c("rollback copy damaged after {} ship {}: {}")?;
            format!("换 {}（{}）后，回滚用的旧版副本损坏：{}", n[0], n[1], n[2])
        }
        "tailscale-gave-up" => {
            let n = c("tailscaled died {} times; restarts paused for up to {}s")?;
            format!("tailscaled 退出了 {} 次，暂停重启最多 {} 秒", n[0], n[1])
        }
        "sms-forward-stuck" => {
            let n = c("SMS {} not forwarded to rule {}, given up: {}")?;
            format!("短信 {} 没能按规则 {} 转发，已放弃：{}", n[0], n[1], n[2])
        }
        _ => return None,
    })
}

/// `polls :{port} over Tailscale for {min} min, ~{pkts} pkts/min: {who}` (who may be cut short);
/// core 1.98.4 and older
/// wrote it in Chinese and the queue kept `{who-scraps}  Tailscale  :{port} {min}  {pkts} / …`.
fn poller(b: &str) -> String {
    let tail = "，息屏约多耗 0.3–0.6 W";
    if let Some(n) = caps("polls :{} over Tailscale for {} min, ~{} pkts/min: {}", b) {
        let who = if let Some(p) = n[3].strip_suffix(" (via gateway)") {
            format!("{p}（经网关转发）")
        } else if let Some((peer, name)) = n[3].split_once(" (") {
            match name.strip_suffix(')') {
                Some(name) => format!("{peer}（{name}）"),
                None => format!("{peer}（{name}…）"),
            }
        } else {
            n[3].to_string()
        };
        return format!(
            "{who} 经 Tailscale 持续访问 :{}，已 {} 分钟（约 {} 包/分钟）{tail}",
            n[0], n[1], n[2]
        );
    }
    if let Some((who, rest)) = b.split_once("Tailscale") {
        let mut t = rest.split_whitespace();
        let port = t.next().and_then(|p| p.strip_prefix(':'));
        let min = t.next().filter(|m| m.bytes().all(|c| c.is_ascii_digit()));
        if let (Some(port), Some(min)) = (port, min) {
            return format!(
                "{} 经 Tailscale 持续访问 :{port}，已 {min} 分钟{tail}",
                who.trim()
            );
        }
    }
    String::new()
}

/// `battery {b} C, chip {c} C; {action}`; core 1.98.4 and older wrote it in Chinese and the queue
/// kept ` {b} C {c} C` (plus `CHILL` scraps).
fn heat(b: &str) -> String {
    let head = |bt: &str, ch: &str| format!("电池 {bt} °C、芯片 {ch} °C");
    if let Some(n) = caps("battery {} C, chip {} C; {}", b) {
        return match heat_action(n[2]) {
            Some(a) => format!("{}：{a}", head(n[0], n[1])),
            None => head(n[0], n[1]),
        };
    }
    let t: Vec<&str> = b.split_whitespace().collect();
    let vals: Vec<&str> = t
        .windows(2)
        .filter(|w| w[1].starts_with('C') && (w[0] == "?" || w[0].parse::<i32>().is_ok()))
        .map(|w| w[0])
        .collect();
    match vals.as_slice() {
        [bt, ch, ..] => head(bt, ch),
        _ => String::new(),
    }
}

fn cell(c: &str) -> &str {
    match c {
        "home" => "国内",
        "abroad" => "国外",
        _ => c,
    }
}

fn egress(name: &str) -> &str {
    match name {
        "chill" => "CHILL",
        "tailscale" => "Tailscale",
        _ => name,
    }
}

fn mem_word(w: &str) -> &str {
    match w {
        "heap" => "堆内存",
        "anon" => "匿名内存",
        _ => w,
    }
}

/// Regress details (`longterm/regress`, the sentinel's three), cut to 72 characters by `clip`.
fn regress(kind: &str, b: &str) -> Option<String> {
    if let Some(a) = ask(kind, b) {
        return Some(a);
    }
    let c = |pat: &str| caps(pat, b);
    Some(match kind {
        "regress-egress" => {
            if let Some(n) = c("{} failing {}m") {
                format!("{} 不通已 {} 分钟", egress(n[0]), n[1])
            } else {
                let n = c("{} through 3 checks")?;
                format!("{} 连续 3 次检查都通了", egress(n[0]))
            }
        }
        "regress-cellular" => {
            if let Some(n) = c("no service {}m") {
                format!("无服务 {} 分钟", n[0])
            } else if let Some(n) = c("registered, no data route {}m") {
                format!("已注册但没有数据路由，{} 分钟", n[0])
            } else if b == "registered with a data route 3 checks" {
                "已注册且有数据路由，连续 3 次检查".into()
            } else {
                return None;
            }
        }
        "regress-core-down" => {
            if b == "core healthy again" {
                "数据服务恢复正常".into()
            } else if let Some(n) = c("core healthy again after {} sentinel restart(s)") {
                format!("哨兵重启它 {} 次后，数据服务恢复正常", n[0])
            } else {
                let n = c("/healthz {} for {} min; attribution unknown")?;
                let what = if n[0] == "no answer: refused" {
                    "连不上（拒绝连接）".to_string()
                } else if let Some(e) = n[0].strip_prefix("no answer: ") {
                    format!("没回应（{e}）")
                } else {
                    format!("回的是 {}", n[0])
                };
                format!("/healthz {what}，已 {} 分钟，原因不明", n[1])
            }
        }
        "regress-recorder" => {
            if b == "open.json written again" {
                "open.json 又开始写了".into()
            } else {
                let n = c("long/open.json not written for {} min")?;
                format!("long/open.json 已 {} 分钟没写", n[0])
            }
        }
        "regress-disk" => {
            let (room, again) = match b.strip_suffix(" again") {
                Some(r) => (r, true),
                None => (b, false),
            };
            let (head, top) = match room.split_once("; ") {
                Some((h, t)) => (h, Some(t)),
                None => (room, None),
            };
            let n = caps("{} {}/{} MB, {}/{} inodes free", head)?;
            let mut s = format!(
                "{} 剩 {}/{} MB，inode 剩 {}/{}",
                n[0], n[1], n[2], n[3], n[4]
            );
            if let Some(t) = top {
                s.push_str(&format!("；最大的：{t}"));
            }
            if again {
                s = format!("恢复到：{s}");
            }
            s
        }
        "regress-mem-abs" | "regress-mem" => mem(b)?,
        "regress-heat" => match b {
            "heat care critical" => "温控进入最高档".into(),
            "heat care out of critical" => "温控离开最高档".into(),
            _ => return None,
        },
        "regress-blind" => {
            let class = |c: &str| {
                match c {
                    "cellular" => "蜂窝",
                    "egress" => "出口",
                    "power" => "功耗",
                    _ => "",
                }
                .to_string()
            };
            if let Some(n) = c("{} could not be judged for 3 days") {
                format!("{}已经 3 天没法判断", class(n[0]))
            } else {
                let n = c("{} can be judged again")?;
                format!("{}又能判断了", class(n[0]))
            }
        }
        "regress-power-abs" | "regress-power" => {
            if let Some(n) = c("{} standby {}mW > {}mW (base {}) 2h") {
                format!(
                    "{}待机 {} mW，超过 {} mW（平时 {} mW）已 2 小时",
                    cell(n[0]),
                    n[1],
                    n[2],
                    n[3]
                )
            } else if let Some(n) = c("{} standby {}mW > {}mW 2h") {
                format!(
                    "{}待机 {} mW，超过上限 {} mW 已 2 小时",
                    cell(n[0]),
                    n[1],
                    n[2]
                )
            } else {
                let n = c("{} standby {}mW 2h")?;
                format!("{}待机回到 {} mW，已 2 小时", cell(n[0]), n[1])
            }
        }
        "regress-reboot" => reboot(b)?,
        _ => return None,
    })
}

/// `heap {X} > {L} 15m; anon {Y}` and the other memory shapes (sizes already carry `MiB`).
fn mem(b: &str) -> Option<String> {
    let c = |pat: &str| caps(pat, b);
    if let Some(n) = c("{} {} > {} {}m; {} {}") {
        return Some(format!(
            "{} {} 超过上限 {} 已 {} 分钟；{} {}",
            mem_word(n[0]),
            n[1],
            n[2],
            n[3],
            mem_word(n[4]),
            n[5]
        ));
    }
    if let Some(n) = c("heap {} > 1.5x base {} 15m") {
        return Some(format!(
            "堆内存 {} 超过平时 {} 的 1.5 倍，已 15 分钟",
            n[0], n[1]
        ));
    }
    if let Some(n) = c("anon {} > 2x base {} 30m") {
        return Some(format!(
            "匿名内存 {} 超过平时 {} 的 2 倍，已 30 分钟",
            n[0], n[1]
        ));
    }
    if let Some(n) = c("heap {} anon {} back under 80% 30m") {
        return Some(format!(
            "堆内存 {}、匿名内存 {}，回到 80% 以下已 30 分钟",
            n[0], n[1]
        ));
    }
    let n = c("heap {} anon {} under 80% 30m")?;
    Some(format!(
        "堆内存 {}、匿名内存 {}，低于上限的 80% 已 30 分钟",
        n[0], n[1]
    ))
}

/// `code 1185; prev build 1.98.0; ship core ` piece by piece (any piece may be cut short).
fn reboot(b: &str) -> Option<String> {
    let mut out = Vec::new();
    for p in b.split("; ") {
        let p = p.trim();
        let w = if p == "code unknown" {
            "原因码未知".to_string()
        } else if let Some(c) = p.strip_prefix("code ") {
            format!("原因码 {c}")
        } else if let Some(v) = p.strip_prefix("prev build ") {
            format!("之前的版本 {v}")
        } else if let Some(s) = p.strip_prefix("ship ") {
            match s.strip_suffix(" before reboot?") {
                Some(comp) => format!("重启前可能刚换过 {comp}"),
                None => format!("之前刚换过 {}", s.trim()),
            }
        } else if out.is_empty() {
            return None;
        } else {
            p.to_string()
        };
        out.push(w);
    }
    Some(out.join("；"))
}

/// The 3-day question on a memory or power incident: `open 3d; new normal? build …`.
fn ask(kind: &str, b: &str) -> Option<String> {
    let rest = b.strip_prefix("open 3d; new normal? ")?;
    let (build, accept) = match rest.split_once("; ") {
        Some((x, y)) => (x, Some(y)),
        None => (rest, None),
    };
    let build = build.strip_prefix("build ").unwrap_or(build);
    let (build, level) = match build.split_once(" at ") {
        Some((x, y)) => (x, format!("，现在 {y}")),
        None => (build, String::new()),
    };
    let mut s = format!("已经 3 天，可能是新常态（版本 {build}{level}）");
    // the kind may be cut short by the 72-character clip: name it whole
    if accept.is_some_and(|a| a.starts_with("regress accept ")) {
        s.push_str(&format!("；确认的话运行 regress accept {kind}"));
    }
    Some(s)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "detail/tests_cards.rs"]
mod tests_cards;
