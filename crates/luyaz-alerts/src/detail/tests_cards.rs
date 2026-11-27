//! The dual-slot card events (luyaz-core card_watch.rs, D2/D3) in Chinese.

use super::detail_zh;

#[test]
fn the_card_events_read_in_chinese() {
    let d = |k, t| detail_zh(k, t);
    assert_eq!(
        d("sim-standby-changed", "slot 2: card in (...2222)").as_deref(),
        Some("卡槽 2：插入了一张卡（…2222）")
    );
    assert_eq!(
        d("sim-standby-changed", "slot 1: card out").as_deref(),
        Some("卡槽 1：卡拔出了")
    );
    assert_eq!(
        d("sim-standby-changed", "slot 2: another card (...3333)").as_deref(),
        Some("卡槽 2：换成了另一张卡（…3333）")
    );
    assert_eq!(
        d(
            "sim-standby-changed",
            "slot 2: profile now ...5555 (switched on the eSIM page)"
        )
        .as_deref(),
        Some("卡槽 2：eSIM 换成了 profile …5555（你在 eSIM 页切的）")
    );
    assert_eq!(d("sim-standby-changed", "slot 2: something else"), None);
    assert_eq!(
        d("voice-card-followed", "voice card moved to slot 2").as_deref(),
        Some("电话卡现在在卡槽 2，电话设置已跟过去")
    );
    assert_eq!(
        d(
            "voice-card-missing",
            "voice card not in either slot; slot 1 takes calls"
        )
        .as_deref(),
        Some("设定的电话卡两个卡槽都没有，现在用卡槽 1 接电话")
    );
}

/// The bodies go through the queue (ASCII only, ids `<boot>-<digits>`) and still read back.
#[test]
fn the_card_events_survive_the_queue() {
    let dir = std::env::temp_dir().join(format!("alerts-cards-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let a = crate::Alerts::at(&dir);
    let body = "slot 2: profile now ...5555 (switched on the eSIM page)";
    let (_, new) = a
        .add_once("0123abcd-52123456", "sim-standby-changed", body, 0, 1)
        .unwrap();
    assert!(new);
    let e = a.events().into_iter().last().unwrap();
    let text = e.text.strip_prefix("[0123abcd-52123456] ").unwrap();
    assert_eq!(text, body);
    assert_eq!(
        detail_zh("sim-standby-changed", text).as_deref(),
        Some("卡槽 2：eSIM 换成了 profile …5555（你在 eSIM 页切的）")
    );
    let _ = std::fs::remove_dir_all(&dir);
}
