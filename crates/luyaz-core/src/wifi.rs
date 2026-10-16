use crate::state;
use serde_json::{Value, json};
use std::sync::OnceLock;

/// Radio (`wifi-device`) section behind each band's main AP, as the firmware
/// names it in `wireless.main_<band>.device` (seen on this MU5250 under B31,
/// 10-04: `wifi0`/`wifi1`).
/// Fallback when the option is unreadable: the names every MU525x so far uses.
const RADIO_FALLBACK: [&str; 2] = ["wifi0", "wifi1"];
/// Read once, kept for the process: the radio sections never change at run
/// time. Only a real answer is kept, so a ubus hiccup at start doesn't pin the
/// fallback.
static RADIO_SECTIONS: [OnceLock<String>; 2] = [OnceLock::new(), OnceLock::new()];

/// `uci get` reply (ubus `{"value": …}`) → a usable uci section name, or None.
fn radio_name(reply: Option<&Value>) -> Option<String> {
    let value = reply?.get("value")?.as_str()?.trim();
    (!value.is_empty()
        && value.len() <= 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'))
    .then(|| value.to_owned())
}

fn band_index(band: &str) -> usize {
    usize::from(band != "2g")
}

/// The radio section of `band` (`"2g"`/`"5g"`). Read through ubus `uci get`
/// (sees uncommitted /tmp/.uci changes, no fork), cached once found.
pub async fn radio_section(band: &str) -> String {
    let index = band_index(band);
    if let Some(name) = RADIO_SECTIONS[index].get() {
        return name.clone();
    }
    let ap = if index == 0 { "main_2g" } else { "main_5g" };
    let reply = state::ubus(
        "uci",
        "get",
        json!({"config":"wireless","section":ap,"option":"device"}),
    )
    .await
    .ok();
    match radio_name(reply.as_ref()) {
        Some(name) => RADIO_SECTIONS[index].get_or_init(|| name).clone(),
        None => RADIO_FALLBACK[index].to_owned(),
    }
}

/// Both radio sections, 2.4 GHz first.
pub async fn radio_sections() -> [String; 2] {
    [radio_section("2g").await, radio_section("5g").await]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radio_names_from_uci_device_or_fallback() {
        assert_eq!(
            radio_name(Some(&json!({"value":"wifi1"}))),
            Some("wifi1".into())
        );
        assert_eq!(
            radio_name(Some(&json!({"value":" radio0\n"}))),
            Some("radio0".into())
        );
        for bad in [
            json!({"value":""}),
            json!({"value":"wifi0;reboot"}),
            json!({"value":"a.b"}),
            json!({"value":1}),
            json!({"result":"success"}),
            json!({"value":"x".repeat(33)}),
        ] {
            assert_eq!(radio_name(Some(&bad)), None, "{bad}");
        }
        assert_eq!(radio_name(None), None);
    }
}
