use super::*;

fn fits(s: &Say) {
    assert!(s.detail.len() <= DETAIL_MAX, "{}", s.detail);
    assert!(s.detail_en.len() <= DETAIL_MAX, "{}", s.detail_en);
    assert!(
        s.detail_en
            .chars()
            .all(|c| !('\u{4e00}'..='\u{9fff}').contains(&c))
    );
}

#[test]
fn hot_explains_and_says_when_drain_is_unread() {
    let a = hot_drain::Advice::Explain {
        batt_dc: 410,
        batt_normal_dc: 380,
        drain_mw: 920,
        drain_normal_mw: 400,
        alongside: vec![],
        unread: vec![],
        ambient: "unread",
    };
    let s = hot(&hot_drain::Evidence::default(), &a).unwrap();
    assert!(s.detail.contains("41.0 °C") && s.detail.contains("38.0 °C"));
    assert!(s.detail_en.contains("2.3×"), "{}", s.detail_en);
    fits(&s);
    let e = hot_drain::Evidence {
        batt_dc: Some(410),
        batt_normal_dc: Some(380),
        ..Default::default()
    };
    for why in ["drain_unqualified", "no_drain_baseline"] {
        let s = hot(&e, &hot_drain::Advice::Unknown { why }).unwrap();
        assert!(s.detail.contains("判断不了"));
        fits(&s);
    }
    for a in [
        hot_drain::Advice::Unknown {
            why: "no_temperature",
        },
        hot_drain::Advice::None { why: "not_hot" },
    ] {
        assert_eq!(hot(&e, &a), None);
    }
    // hot unread but no temperatures to quote: nothing
    let a = hot_drain::Advice::Unknown {
        why: "drain_unqualified",
    };
    assert_eq!(hot(&hot_drain::Evidence::default(), &a), None);
}

#[test]
fn idle_only_on_suggest() {
    let s = idle(&long_idle::Advice::Suggest {
        hours: 3,
        step: "chill_idle_pause",
    })
    .unwrap();
    assert!(s.detail.starts_with("已经 3 小时"));
    fits(&s);
    assert_eq!(
        idle(&long_idle::Advice::None {
            why: "already_saving"
        }),
        None
    );
}

#[test]
fn slow_suggests_and_explains() {
    let s = slow(&slow_signal::Advice::Suggest {
        busy_s: 150,
        step: "deep_check",
    })
    .unwrap();
    assert!(s.detail_en.contains("2 min"));
    fits(&s);
    let s = slow(&slow_signal::Advice::Explain {
        proven: vec!["wifi"],
        possible: vec![],
    })
    .unwrap();
    fits(&s);
    assert_eq!(
        slow(&slow_signal::Advice::Unknown { why: "no_check" }),
        None
    );
}
