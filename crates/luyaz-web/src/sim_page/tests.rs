use super::*;
use serde_json::json;

fn form(pin: &str, old: &str, new: &str, puk: &str, nck: &str) -> Form {
    Form {
        pin: pin.into(),
        oldpin: old.into(),
        newpin: new.into(),
        puk: puk.into(),
        nck: nck.into(),
    }
}

fn card(states: &str, pin_status: &str, pin_left: &str, puk_left: &str, nck: Option<u32>) -> Card {
    Card {
        info: parse_info(&json!({
            "sim_states": states, "modem_main_state": states, "pin_status": pin_status,
            "sim_imsi": "460000000000001", "sim_iccid": "89860000000000000001",
            "pinnumber": pin_left, "puknumber": puk_left,
        })),
        nck,
    }
}

#[test]
fn the_old_checks_with_the_old_words() {
    let l = Lang::En;
    assert_eq!(check(Kind::VerifyPin, &form("12", "", "", "", ""), l).err().unwrap(), "PIN must be at least 4 digits");
    assert_eq!(check(Kind::PinOn, &form("12a4", "", "", "", ""), l).err().unwrap(), "PIN can only contain digits");
    assert_eq!(check(Kind::PinOff, &form("123456789", "", "", "", ""), l).err().unwrap(), "PIN is at most 8 digits");
    assert!(check(Kind::VerifyPin, &form("1234", "", "", "", ""), l).is_ok());
    assert_eq!(check(Kind::VerifyPuk, &form("", "", "1234", "1234567", ""), l).err().unwrap(), "PUK must be at least 8 digits");
    assert_eq!(check(Kind::VerifyPuk, &form("", "", "12", "12345678", ""), l).err().unwrap(), "New PIN must be at least 4 digits");
    assert!(check(Kind::VerifyPuk, &form("", "", "1234", "12345678", ""), l).is_ok());
    assert_eq!(check(Kind::ChangePin, &form("", "12", "1234", "", ""), l).err().unwrap(), "Current PIN must be at least 4 digits");
    assert_eq!(check(Kind::Nck, &form("", "", "", "", "  "), l).err().unwrap(), "Unlock code is required");
    assert_eq!(check(Kind::PinOn, &form("12", "", "", "", ""), Lang::Zh).err().unwrap(), "PIN 至少需要 4 位数字");
    assert_eq!(check(Kind::VerifyPuk, &form("", "", "1234", "123", ""), Lang::Zh).err().unwrap(), "PUK 至少需要 8 位数字");
    assert_eq!(check(Kind::Nck, &form("", "", "", "", ""), Lang::Zh).err().unwrap(), "需要解锁码");
}

#[test]
fn the_body_of_each_write_is_the_agents_shape() {
    let a = check(Kind::VerifyPuk, &form("", "", "4321", "12345678", ""), Lang::En).ok().unwrap();
    assert_eq!(request(&a), ("/api/sim/pin/verify", json!({"pin_num":"4321","puk_num":"12345678","pin_encode_flag":"0"})));
    let a = check(Kind::VerifyPin, &form("1234", "", "", "", ""), Lang::En).ok().unwrap();
    assert_eq!(request(&a).1, json!({"pin_num":"1234","puk_num":"","pin_encode_flag":"0"}));
    let a = check(Kind::PinOn, &form("1234", "", "", "", ""), Lang::En).ok().unwrap();
    assert_eq!(request(&a), ("/api/sim/pin/mode", json!({"pin_num_m":"1234","pin_mode":"1","pin_encode_flag":"0"})));
    let a = check(Kind::PinOff, &form("1234", "", "", "", ""), Lang::En).ok().unwrap();
    assert_eq!(request(&a).1["pin_mode"], "0");
    let a = check(Kind::ChangePin, &form("", "1234", "4321", "", ""), Lang::En).ok().unwrap();
    assert_eq!(request(&a), ("/api/sim/pin/change", json!({"pin_num":"1234","new_pin_num":"4321","pin_encode_flag":"0"})));
    let a = check(Kind::Nck, &form("", "", "", "", " 1234567890123456 "), Lang::En).ok().unwrap();
    assert_eq!(request(&a), ("/api/sim/unlock", json!({"nck":"1234567890123456"})));
}

#[test]
fn only_what_fits_the_card_is_sent() {
    let l = Lang::En;
    let ready = card("modem_init_complete", "0", "3", "10", Some(5)).info;
    let on = card("modem_init_complete", "1", "3", "10", Some(5)).info;
    let pin = card("modem_waitpin", "1", "2", "10", None).info;
    let puk = card("modem_waitpuk", "1", "0", "9", None).info;
    assert!(fits(Kind::PinOn, &ready, l).is_ok() && fits(Kind::PinOff, &ready, l).is_err());
    assert!(fits(Kind::PinOff, &on, l).is_ok() && fits(Kind::PinOn, &on, l).is_err());
    assert!(fits(Kind::ChangePin, &ready, l).is_ok());
    assert!(fits(Kind::VerifyPin, &pin, l).is_ok() && fits(Kind::VerifyPin, &ready, l).is_err());
    assert!(fits(Kind::VerifyPuk, &puk, l).is_ok() && fits(Kind::VerifyPuk, &pin, l).is_err());
    assert!(fits(Kind::PinOn, &pin, l).is_err() && fits(Kind::ChangePin, &puk, l).is_err());
    assert!(fits(Kind::Nck, &puk, l).is_ok());
    // the states are matched case-insensitively and in either field
    assert!(parse_info(&json!({"sim_states":"WAIT PIN"})).pin_locked());
    assert!(parse_info(&json!({"modem_main_state":"modem_waitpuk"})).puk_locked());
}

#[test]
fn every_dialog_is_tier_three_and_quotes_the_attempts() {
    let l = Lang::En;
    let c = card("modem_init_complete", "0", "3", "10", Some(5));
    for k in [Kind::VerifyPin, Kind::VerifyPuk, Kind::PinOn, Kind::PinOff, Kind::ChangePin, Kind::Nck] {
        let d = confirm(k, &c, l);
        assert_eq!(d.tier, Tier::Dialog, "{k:?}");
        let want = match k.uses() {
            Counter::Pin => "3 PIN attempt(s) left.",
            Counter::Puk => "10 PUK attempt(s) left.",
            Counter::Nck => "5 unlock attempt(s) left.",
        };
        assert!(d.what.contains(want), "{k:?}: {}", d.what);
    }
    assert!(confirm(Kind::VerifyPuk, &c, l).danger && confirm(Kind::Nck, &c, l).danger);
    assert!(!confirm(Kind::VerifyPin, &c, l).danger);
    assert!(confirm(Kind::PinOn, &c, l).cuts_uplink && !confirm(Kind::PinOff, &c, l).cuts_uplink);
    assert!(confirm(Kind::PinOn, &c, l).what.contains("Tailscale"));
    // the card did not say: said so; one left: said so
    let none = card("modem_waitpin", "1", "", "", None);
    assert!(confirm(Kind::VerifyPin, &none, l).what.contains("did not report"));
    let last = card("modem_waitpin", "1", "1", "10", None);
    assert!(confirm(Kind::VerifyPin, &last, l).what.contains("This is the last attempt."));
    assert!(confirm(Kind::VerifyPin, &last, Lang::Zh).what.contains("这是最后一次机会。"));
}

#[test]
fn after_a_try_the_attempts_left_are_named() {
    let l = Lang::En;
    let before = card("modem_init_complete", "1", "3", "10", Some(5));
    let after = card("modem_init_complete", "1", "2", "10", Some(5));
    assert_eq!(after_words(Kind::PinOn, &after, &before, l), "2 PIN attempt(s) left");
    assert_eq!(after_words(Kind::PinOn, &after, &before, Lang::Zh), "还剩 2 次 PIN 尝试机会");
    // the last PIN try went: the SIM is PUK-locked now, and the PUK count is the one to watch
    let puk = card("modem_waitpuk", "1", "0", "10", None);
    let s = after_words(Kind::VerifyPin, &puk, &card("modem_waitpin", "1", "1", "10", None), l);
    assert!(s.contains("locked by PUK") && s.contains("10 PUK attempt(s) left"), "{s}");
    // a PUK on its last try
    let dead = card("modem_waitpuk", "1", "0", "0", None);
    let s = after_words(Kind::VerifyPuk, &dead, &puk, l);
    assert!(s.contains("replace the SIM"), "{s}");
    assert_eq!(after_words(Kind::Nck, &card("x", "0", "3", "10", Some(4)), &before, l), "4 unlock attempt(s) left");
}

#[test]
fn a_pin_appears_in_no_reply() {
    // Everything the server can say about a try, with a recognizable PIN, PUK and code typed:
    // none of it may carry them.
    let secrets = ["4711", "98765432", "1357902468135790"];
    let f = || form("4711", "4711", "4711", "98765432", "1357902468135790");
    let c = card("modem_waitpin", "1", "2", "10", Some(5));
    let mut all = Vec::new();
    for l in [Lang::En, Lang::Zh] {
        for k in [Kind::VerifyPin, Kind::VerifyPuk, Kind::PinOn, Kind::PinOff, Kind::ChangePin, Kind::Nck] {
            all.push(area(l, Via::Tailnet, Some(k), &Phase::Ask(confirm(k, &c, l))));
            all.push(area(l, Via::Tailnet, Some(k), &Phase::Pending(l.pick("Sending…", "提交中…").into())));
            for o in [
                Outcome::Applied(applied(k, l).into()),
                Outcome::AcceptedSaid(said(k, l).into()),
                Outcome::Failed(format!("{} · {}", wrong(k, l), after_words(k, &c, &c, l))),
                Outcome::Accepted,
                Outcome::Unknown,
                busy(l),
                cant_read(&Fail::Unreachable, l),
            ] {
                all.push(area(l, Via::Tailnet, Some(k), &Phase::Done(o)));
            }
            let a = check(k, &f(), l).ok().unwrap();
            all.push(request(&a).0.to_owned());
        }
        all.push(reset(true));
        all.push(reset(false));
        // a refusal that quotes nothing of the body
        all.push(Fail::Refused(503, "ubus call zwrt_zte_mdm.api sim_verify_pin_puk failed: Unknown error".into(), String::new()).text(l));
        // the forms and leaves, with the card in every state
        let mut p = Poll::new(
            Caller { token: None, peer: None, sid: None },
            l,
        );
        p.info = Some(c.info.clone());
        p.nck = Some(Some(5));
        all.extend(p.leaves().into_iter().map(|(_, h)| h));
        all.push(panels(l, &|_| String::new()));
        // validation errors name no digits
        for k in [Kind::VerifyPin, Kind::VerifyPuk, Kind::ChangePin, Kind::Nck] {
            if let Err(e) = check(k, &form("4711x", "47", "47", "9876543", ""), l) {
                all.push(e);
            }
        }
    }
    let html = all.join("\n");
    for s in secrets {
        assert!(!html.contains(s), "{s} in the output");
    }
}

#[test]
fn the_forms_keep_secrets_out_of_urls_and_are_numeric_and_big_enough() {
    let html = panels(Lang::En, &|id| format!("<p id=\"{id}\"></p>"));
    for k in ["verify-pin", "verify-puk", "pin-on", "pin-off", "change-pin", "nck"] {
        assert!(html.contains(&format!("@post('/router/sim/ask?k={k}')")), "{k}");
        assert!(html.contains(&format!("id=\"sim-t-{k}\"")), "tries next to {k}");
    }
    assert!(!html.contains("pin=") && !html.contains("puk=") && !html.contains("nck="), "no value in a URL");
    assert_eq!(html.matches("type=\"password\"").count(), 8);
    assert_eq!(html.matches("inputmode=\"numeric\"").count(), 7);
    assert!(html.contains("data-bind:pin") && html.contains("data-bind:puk") && html.contains("data-bind:nck"));
    assert!(html.contains("for=\"sim-f-vp\""), "labels are linked");
    let css = include_str!("../../static/dev/sim.css");
    assert!(css.contains("font-size: 16px"));
}

#[test]
fn the_english_page_has_no_chinese() {
    let l = Lang::En;
    let c = card("modem_waitpin", "1", "2", "10", Some(5));
    let mut all = vec![panels(l, &|_| String::new())];
    let mut p = Poll::new(Caller { token: None, peer: None, sid: None }, l);
    all.extend(p.leaves().into_iter().map(|(_, h)| h));
    p.info = Some(c.info.clone());
    p.nck = Some(Some(5));
    all.extend(p.leaves().into_iter().map(|(_, h)| h));
    p.info_fail = Some(Fail::Unreachable);
    all.extend(p.leaves().into_iter().map(|(_, h)| h));
    for k in [Kind::VerifyPin, Kind::VerifyPuk, Kind::PinOn, Kind::PinOff, Kind::ChangePin, Kind::Nck] {
        all.push(area(l, Via::Tailnet, Some(k), &Phase::Ask(confirm(k, &c, l))));
        all.push(applied(k, l).into());
        all.push(said(k, l).into());
        all.push(wrong(k, l).into());
        all.push(after_words(k, &c, &c, l));
        all.push(fits(k, &c.info, l).err().unwrap_or_default());
        all.push(check(k, &form("", "", "", "", ""), l).err().unwrap_or_default());
    }
    all.push(busy(l).text(l));
    all.push(cant_read(&Fail::SignedOut, l).text(l));
    let html = all.join("\n");
    assert!(
        !html.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)
            || ('\u{3000}'..='\u{303f}').contains(&c)
            || ('\u{ff00}'..='\u{ffef}').contains(&c)),
        "{html}"
    );
}

#[test]
fn the_status_follows_the_card() {
    let l = Lang::En;
    let mut p = Poll::new(Caller { token: None, peer: None, sid: None }, l);
    let status = |p: &Poll| p.leaves().into_iter().find(|(k, _)| k == "sim-status").unwrap().1;
    p.info = Some(card("modem_init_complete", "0", "3", "10", None).info);
    assert!(status(&p).contains("SIM ready") && status(&p).contains("PIN lock is off."));
    p.info = Some(card("modem_waitpin", "1", "2", "10", None).info);
    let s = status(&p);
    assert!(s.contains("SIM locked by PIN") && s.contains("2 PIN attempt(s) left") && s.contains("nd-status--bad"), "{s}");
    p.info = Some(card("modem_waitpuk", "1", "0", "9", None).info);
    assert!(status(&p).contains("SIM locked by PUK") && status(&p).contains("9 PUK attempt(s) left"));
    // a re-read that failed: greyed, said, and no button that writes
    p.info_fail = Some(Fail::Unreachable);
    let all = p.leaves();
    let get = |id: &str| all.iter().find(|(k, _)| k == id).unwrap().1.clone();
    assert!(get("sim-status").contains("nd-status--stale") && get("sim-status").contains("Refresh before changing"));
    assert!(get("sim-actions").contains(" disabled") && get("sim-info").contains("nd-stale"));
}

#[test]
fn only_the_buttons_that_fit_are_there() {
    let l = Lang::En;
    let mut p = Poll::new(Caller { token: None, peer: None, sid: None }, l);
    let actions = |p: &Poll| p.leaves().into_iter().find(|(k, _)| k == "sim-actions").unwrap().1;
    p.info = Some(card("modem_init_complete", "0", "3", "10", None).info);
    let a = actions(&p);
    assert!(a.contains("Enable PIN Lock") && a.contains("Change PIN") && a.contains("Network Unlock") && !a.contains("Verify P"));
    p.info = Some(card("modem_init_complete", "1", "3", "10", None).info);
    assert!(actions(&p).contains("Disable PIN Lock"));
    p.info = Some(card("modem_waitpin", "1", "2", "10", None).info);
    let a = actions(&p);
    assert!(a.contains("Verify PIN") && !a.contains("Change PIN") && !a.contains("PIN Lock") && a.contains("Network Unlock"));
    p.info = Some(card("modem_waitpuk", "1", "0", "9", None).info);
    assert!(actions(&p).contains("Verify PUK"));
}
