use super::*;

fn who() -> Caller {
    Caller { token: None, peer: None, sid: None }
}

fn full(l: Lang) -> Poll {
    let mut p = Poll::new(who(), l);
    p.status = Some(false);
    p.params = Some(Params {
        lte_timer: "60".into(),
        nrsa_timer: "60".into(),
        lte_max: "8".into(),
        nrsa_max: "8".into(),
    });
    p
}

fn leaf(p: &Poll, id: &str) -> String {
    p.leaves().into_iter().find(|(k, _)| k == id).unwrap().1
}

fn b27() -> Fail {
    Fail::Refused(
        503,
        "ubus call zte_nwinfo_api nwinfo_get_stc_white_list_par failed: Command failed (Method not found)".into(),
        String::new(),
    )
}

#[test]
fn the_old_is_on() {
    use serde_json::json;
    for (v, want) in [
        (json!(true), true),
        (json!(false), false),
        (json!(1), true),
        (json!(0), false),
        (json!("1"), true),
        (json!("0"), false),
        (json!("true"), true),
        (json!("Enabled"), true),
        (json!("off"), false),
        (json!(null), false),
    ] {
        assert_eq!(is_on(&v), Some(want), "{v}");
    }
}

#[test]
fn method_not_found_is_unsupported_other_errors_are_not() {
    assert!(unsupported(&b27()));
    assert!(!unsupported(&Fail::Refused(503, "service unavailable".into(), String::new())));
    assert!(!unsupported(&Fail::Refused(500, "Method not found".into(), String::new())));
    assert!(!unsupported(&Fail::Unreachable));
}

#[test]
fn the_three_states_of_the_page() {
    let l = Lang::En;
    // working
    let p = full(l);
    assert!(leaf(&p, "stc-status").contains("Disabled") && leaf(&p, "stc-status").contains("nd-status--neutral"));
    assert!(leaf(&p, "stc-switch").contains("Current State: Disabled") && leaf(&p, "stc-switch").contains("data-on:click"));
    assert!(leaf(&p, "stc-params").contains("data-signals__ifmissing") && !leaf(&p, "stc-params").contains(" disabled"));
    assert!(!leaf(&p, "stc-resetbtn").contains(" disabled"));
    let mut on = full(l);
    on.status = Some(true);
    assert!(leaf(&on, "stc-status").contains("nd-status--ok") && leaf(&on, "stc-switch").contains("data-selected"));

    // B27: no STC at all, everything locked, reset too, no retry button
    let mut p = Poll::new(who(), l);
    p.status_fail = Some(b27());
    p.params_fail = Some(b27());
    assert!(p.unsupported() && !p.down());
    assert!(leaf(&p, "stc-status").contains("Not available on this firmware") && !leaf(&p, "stc-status").contains("Retry"));
    assert!(!leaf(&p, "stc-switch").contains("data-on:click"));
    assert!(leaf(&p, "stc-params").contains("readonly") && !leaf(&p, "stc-params").contains("data-on:input"));
    assert!(leaf(&p, "stc-params").contains(" disabled>") || leaf(&p, "stc-params").matches("disabled").count() >= 5);
    assert!(leaf(&p, "stc-resetbtn").contains(" disabled"));

    // service down: switch and parameters locked, the reset stays, Retry is offered
    let mut p = Poll::new(who(), l);
    p.status_fail = Some(Fail::Refused(503, "unavailable".into(), String::new()));
    p.params_fail = Some(Fail::Unreachable);
    assert!(p.down() && !p.unsupported());
    assert!(leaf(&p, "stc-status").contains("STC service unavailable") && leaf(&p, "stc-status").contains("Retry"));
    assert!(!leaf(&p, "stc-switch").contains("data-on:click"));
    assert!(!leaf(&p, "stc-resetbtn").contains(" disabled"));

    // a failed re-read with values shown: greyed, said, locked
    let mut p = full(l);
    p.status_fail = Some(Fail::Unreachable);
    assert!(p.stale());
    let s = leaf(&p, "stc-status");
    assert!(s.contains("nd-status--stale") && s.contains("Refresh before changing"), "{s}");
    assert!(leaf(&p, "stc-switch").contains("nd-stale") && !leaf(&p, "stc-switch").contains("data-on:click"));
    assert!(leaf(&p, "stc-params").contains("disabled"));
}

#[test]
fn the_dialogs_keep_their_tiers() {
    let l = Lang::En;
    let on = toggle_confirm(true, l);
    assert_eq!(on.tier, Tier::Dialog);
    assert!(on.cuts_uplink && !on.danger);
    assert!(on.downtime.as_deref().unwrap().contains("30 seconds") && on.recovery.is_some());
    assert!(on.what.contains("no mobile service"));
    assert!(toggle_confirm(false, l).what.contains("picks cells normally"));
    let r = reset_confirm(l);
    assert_eq!(r.tier, Tier::Dialog);
    assert!(r.danger && r.cuts_uplink);
    assert_eq!(r.what, "Clear the STC whitelist? The collected cells are gone for good.");
    let f = Four { lt: "60".into(), nt: "70".into(), lm: "8".into(), nm: "9".into() };
    let c = params_confirm(&f, l);
    assert_eq!(c.tier, Tier::Inline);
    assert!(c.what.contains("LTE 60 s / 8 cells") && c.what.contains("NR-SA 70 s / 9 cells"), "{}", c.what);
    assert_eq!(params_confirm(&f, Lang::Zh).what, "白名单采集改用新的定时器和上限：LTE 60 秒 / 8 个小区，NR-SA 70 秒 / 9 个小区。");
}

#[test]
fn parameters_are_whole_numbers_and_what_the_confirm_names_is_what_goes() {
    let l = Lang::En;
    let ok = |a: &str, b: &str, c: &str, d: &str| check_four(&Four { lt: a.into(), nt: b.into(), lm: c.into(), nm: d.into() }, l);
    assert_eq!(ok(" 060 ", "70", "8", "9").unwrap(), Four { lt: "60".into(), nt: "70".into(), lm: "8".into(), nm: "9".into() });
    assert_eq!(ok("", "70", "8", "9").unwrap_err(), "LTE Collect Timer must be a whole number");
    assert_eq!(ok("60", "7x", "8", "9").unwrap_err(), "NR-SA Collect Timer must be a whole number");
    assert!(ok("60", "70", "-1", "9").is_err() && ok("60", "70", "8", "1.5").is_err());
    assert_eq!(
        check_four(&Four { lt: "".into(), nt: "1".into(), lm: "1".into(), nm: "1".into() }, Lang::Zh).unwrap_err(),
        "LTE 采集定时器要填整数"
    );
    let f = Four { lt: "60".into(), nt: "70".into(), lm: "8".into(), nm: "9".into() };
    assert_eq!(set_url(&f), "/router/stc/params/set?lt=60&nt=70&lm=8&nm=9");
    let p = Params { lte_timer: "60".into(), nrsa_timer: "70".into(), lte_max: "8".into(), nrsa_max: "9".into() };
    assert!(same(&p, &f) && !same(&Params::default(), &f));
}

#[test]
fn the_english_page_has_no_chinese() {
    let l = Lang::En;
    let mut all: Vec<String> = Vec::new();
    let mut ps = vec![full(l), Poll::new(who(), l)];
    let mut b = Poll::new(who(), l);
    b.status_fail = Some(b27());
    b.params_fail = Some(b27());
    ps.push(b);
    let mut d = Poll::new(who(), l);
    d.status_fail = Some(Fail::Unreachable);
    ps.push(d);
    let mut s = full(l);
    s.status_fail = Some(Fail::Unreachable);
    ps.push(s);
    for p in ps {
        all.extend(p.leaves().into_iter().map(|(_, h)| h));
    }
    for (w, c) in [
        (Which::Toggle, toggle_confirm(true, l)),
        (Which::Toggle, toggle_confirm(false, l)),
        (Which::Reset, reset_confirm(l)),
        (Which::Params, params_confirm(&Four { lt: "1".into(), nt: "2".into(), lm: "3".into(), nm: "4".into() }, l)),
    ] {
        all.push(area(w, l, Via::Tailnet, "/x", &Phase::Ask(c)));
    }
    for o in [busy(l), cant_read(&Fail::SignedOut, l), unsupported_note(l), Outcome::Unknown, Outcome::Accepted] {
        all.push(area(Which::Reset, l, Via::Local, "", &Phase::Done(o)));
    }
    let html = all.join("\n");
    assert!(
        !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3000}'..='\u{303f}').contains(&c)
            || ('\u{ff00}'..='\u{ffef}').contains(&c)),
        "{html}"
    );
    let css = include_str!("../../static/dev/stc.css");
    assert!(css.contains("font-size: 16px"));
}
