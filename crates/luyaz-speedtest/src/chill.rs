//! The CHILL leg of a paired run (R5): HTTP CONNECT through CHILL's mixed listener 7894, which
//! template.yaml binds to 🚀 节点选择 (the main exit). Never started for a test, never 7895 (AI).
//! Only a region label leaves this module, never a node's name or server address.
use serde_json::Value;
use std::time::Duration;

/// What the run does about CHILL, decided at admission.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Plan {
    /// not asked
    Off,
    /// asked and CHILL's listener answers
    Run,
    /// asked, but nothing listens on 7894 (idle direct, off)
    NotRunning,
}
pub(crate) struct Env<'a> {
    pub plan: Plan,
    pub proxy: &'a str,
    pub controller: &'a str,
    pub context: &'a crate::Context,
    /// the cell at admission
    pub cell: Option<String>,
}
const MAIN_GROUP: &str = "🚀 节点选择";

/// CHILL runs = something accepts on its listener (a local connect, so fast).
pub(crate) fn running(addr: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(mut addrs) = addr.to_socket_addrs() else {
        return false;
    };
    addrs.any(|a| std::net::TcpStream::connect_timeout(&a, Duration::from_millis(500)).is_ok())
}

/// The main exit as mihomo's controller has it: the group's `now`, and the `now` of that
/// group (to notice a node change; compared only, never shown).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Exit {
    pub region: String,
    inner: Option<String>,
}
fn now_of(controller: &str, group: &str) -> Option<String> {
    let mut u = url::Url::parse(controller).ok()?;
    u.path_segments_mut()
        .ok()?
        .pop_if_empty()
        .push("proxies")
        .push(group);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .proxy(None)
        .timeout_global(Some(Duration::from_secs(1)))
        .build()
        .into();
    let v: Value = agent
        .get(u.as_str())
        .call()
        .ok()?
        .into_body()
        .with_config()
        .limit(64 * 1024)
        .read_json()
        .ok()?;
    v.get("now")?.as_str().map(str::to_string)
}
pub(crate) fn exit(controller: &str) -> Option<Exit> {
    let now = now_of(controller, MAIN_GROUP)?;
    if now == "DIRECT" {
        return Some(Exit {
            region: "直连".into(),
            inner: None,
        });
    }
    let inner = now_of(controller, &now);
    Some(Exit {
        region: region_label(&now)?,
        inner,
    })
}
/// 「🇯🇵 日本」→「日本」: the group's name without its flag; at most 16 characters.
fn region_label(group: &str) -> Option<String> {
    let s: String = group
        .trim_start_matches(|c: char| !c.is_alphanumeric())
        .trim()
        .chars()
        .take(16)
        .collect();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    #[test]
    fn region_labels_drop_the_flag() {
        assert_eq!(super::region_label("🇯🇵 日本").as_deref(), Some("日本"));
        assert_eq!(
            super::region_label("🎯 手动节点").as_deref(),
            Some("手动节点")
        );
        assert_eq!(super::region_label("🇺🇸"), None);
    }
}
