//! /tools/agent (AI 代理权限, R18-P0-03): what the AI agent may read and change, read-only.
//!
//! ```text
//!  on open ─▶ agent.token (names + read/act, never the token itself)
//!          ─▶ agent.allow / agent.act-allow (sources)          ─▶ one page, no polling
//!          ─▶ the core's agent-limit.json (last change per layer)
//! ```
//! - Nothing here writes: permissions are changed in the files on the device (user decision
//!   10-20: a read-only page).
//! - The scopes, the limit and the rollback words mirror `luyaz-core` `ops/agent.rs` (`Layer`,
//!   `LIMIT_SECS`), `ops/spec.rs` (every `apn.*` and `chill.*` op is `Unsupported`: no automatic
//!   rollback) and `agent_act.rs` (`PAGES`).

use std::path::Path;
use std::time::UNIX_EPOCH;

use askama::Template;
use axum::extract::State;
use axum::response::Response;
use serde_json::Value;

use crate::agent_api::{Perm, parse_allow, parse_tokens_perm};
use crate::i18n::Lang;
use crate::pages::Shared;

pub const PATH: &str = "/tools/agent";
/// The core's agent limit state (`ops/agent.rs` `LIMIT_FILE` in the ops dir).
const LIMIT_FILE: &str = "/data/luyaz/core/ops/agent-limit.json";
/// `ops/agent.rs` `LIMIT_SECS`: one change per scope per this many minutes.
const LIMIT_MIN: u64 = 30;
/// Always allowed to read, besides `agent.allow` (`agent_api::source_ok`).
const BUILT_IN: &str = "127.0.0.1 · 100.64.0.0/10 · fd7a:115c:a1e0::/48";

/// What was read from the device's files; `None` = the file is missing.
#[derive(Default)]
pub struct Files {
    pub tokens: Option<String>,
    pub allow: Option<String>,
    pub act_allow: Option<String>,
    pub limit: Option<String>,
    /// Newest modification time of the three permission files (device clock).
    pub changed: Option<i64>,
}

pub struct Token {
    pub name: String,
    pub act: bool,
}

pub struct Scope {
    pub label: &'static str,
    pub href: &'static str,
    /// "MM-DD HH:mm" of the agent's last change in this scope.
    pub last: Option<String>,
}

pub struct View {
    pub l: Lang,
    pub on: bool,
    pub tokens: Vec<Token>,
    pub read_from: Vec<String>,
    pub act_from: Vec<String>,
    pub can_act: bool,
    pub scopes: Vec<Scope>,
    pub limit_min: u64,
    pub built_in: &'static str,
    pub changed: Option<String>,
}

fn cidrs(text: &str) -> Vec<String> {
    parse_allow(text)
        .into_iter()
        .map(|(ip, n)| {
            let max = if ip.is_ipv4() { 32 } else { 128 };
            if n == max {
                ip.to_string()
            } else {
                format!("{ip}/{n}")
            }
        })
        .collect()
}

pub fn view(f: &Files, l: Lang) -> View {
    let tokens: Vec<Token> = f
        .tokens
        .as_deref()
        .map(parse_tokens_perm)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, _, p)| Token {
            name,
            act: p == Perm::Act,
        })
        .collect();
    let act_from = f.act_allow.as_deref().map(cidrs).unwrap_or_default();
    let can_act = tokens.iter().any(|t| t.act) && !act_from.is_empty();
    let limit: Value = f
        .limit
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or(Value::Null);
    let scopes = [("APN", "apn", "/router/apn"), ("CHILL", "chill", "/services/chill")]
        .into_iter()
        .map(|(label, key, href)| Scope {
            label,
            href,
            last: limit[key]
                .as_i64()
                .filter(|&t| t > 0)
                .map(crate::alerts_page::mdhm),
        })
        .collect();
    View {
        l,
        on: !tokens.is_empty(),
        tokens,
        read_from: f.allow.as_deref().map(cidrs).unwrap_or_default(),
        act_from,
        can_act,
        scopes,
        limit_min: LIMIT_MIN,
        built_in: BUILT_IN,
        changed: f.changed.map(crate::alerts_page::mdhm),
    }
}

fn read(p: &Path) -> (Option<String>, Option<i64>) {
    let text = std::fs::read_to_string(p).ok();
    let mtime = std::fs::metadata(p)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .and_then(|d| i64::try_from(d.as_secs()).ok());
    (text, mtime)
}

fn read_files() -> Files {
    let (tok, allow) = crate::agent_api::files();
    let (tokens, t1) = read(&tok);
    let (allow, t2) = read(&allow);
    let (act_allow, t3) = read(&crate::agent_act::act_allow());
    Files {
        tokens,
        allow,
        act_allow,
        limit: std::fs::read_to_string(LIMIT_FILE).ok(),
        changed: [t1, t2, t3].into_iter().flatten().max(),
    }
}

#[derive(Template)]
#[template(path = "agent_perms.html")]
struct Page {
    shell: crate::shell::Shell,
    l: Lang,
    v: View,
}

pub async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(&headers, PATH).live(st, &ctx, None));
    let l = ctx.lang;
    let html = Page {
        shell,
        l,
        v: view(&read_files(), l),
    }
    .render()
    .unwrap();
    crate::assets::maybe_gzip(
        &headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOK: &str = "hermes 0123456789abcdef0123\nhermes-act fedcba9876543210fedc act\n# x\n";

    #[test]
    fn names_and_rights_but_never_the_token() {
        let f = Files {
            tokens: Some(TOK.into()),
            act_allow: Some("192.168.50.88/32\n".into()),
            limit: Some(r#"{"apn":1792483200}"#.into()),
            changed: Some(1_792_483_200),
            ..Default::default()
        };
        let v = view(&f, Lang::Zh);
        assert!(v.on && v.can_act);
        let names: Vec<_> = v.tokens.iter().map(|t| (t.name.as_str(), t.act)).collect();
        assert_eq!(names, [("hermes", false), ("hermes-act", true)]);
        assert_eq!(v.act_from, ["192.168.50.88"]);
        assert!(v.scopes[0].last.is_some() && v.scopes[1].last.is_none());
        let html = Page {
            shell: crate::shell::Shell::new(&Default::default(), PATH),
            l: Lang::Zh,
            v,
        }
        .render()
        .unwrap();
        assert!(!html.contains("0123456789abcdef"), "a token leaked");
        assert!(html.contains("hermes-act") && html.contains("192.168.50.88"));
        assert!(html.contains("不能自动退回") && !html.contains("核对不通过"));
    }

    #[test]
    fn act_needs_both_an_act_token_and_a_source() {
        let only_tok = Files {
            tokens: Some(TOK.into()),
            ..Default::default()
        };
        assert!(!view(&only_tok, Lang::En).can_act);
        let read_only = Files {
            tokens: Some("hermes 0123456789abcdef0123\n".into()),
            act_allow: Some("192.168.50.88\n".into()),
            ..Default::default()
        };
        assert!(!view(&read_only, Lang::En).can_act);
        let none = view(&Files::default(), Lang::En);
        assert!(!none.on && none.changed.is_none());
    }
}
