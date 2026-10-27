//! the parser against /api/netinfo bodies shaped like luyaz-core's
//! netinfo.rs output (serde sorts keys, so `error` comes first in every exit object).
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

use super::*;

fn cells_and_ops(ncells: usize, nops: usize) -> String {
    let cells: Vec<String> = (0..ncells)
        .map(|i| format!(r#"{{"arfcn":"627264","pci":{},"rat":"NR","rsrp":"-{}","rsrq":null,"sinr":null}}"#, 100 + i, 90 + i))
        .collect();
    let ops: Vec<String> = (0..nops)
        .map(|i| format!(r#"{{"country":"日本","name":"Op{i}","plmn":"440{i:02}","rat":"12","raw_name":"X","status":"1"}}"#))
        .collect();
    format!(
        r#""neighbors":{{"cells":[{}],"error":null,"scanned_at":1790250000,"state":"done"}},"network_type":"SA","now":1790250112,"proxy":null,"roaming":true,"roaming_raw":"Roaming","scan":{{"error":null,"finished_at":1,"last_status":"2","operators":[{}],"started_at":0,"state":"done"}},"#,
        cells.join(","),
        ops.join(",")
    )
}

fn p(n: &mut netinfo_t, s: &str) {
    parse(n, s.as_bytes());
}

#[test]
fn full_body() {
    lang::set_en(false);
    let mut n = empty();
    let longerr = "e".repeat(150);
    let mid = cells_and_ops(16, 12);
    let clients_head = r#""chill_running":false,"clients":{"at":1790250000,"list":["#;
    let body = format!(
        r#"{{"apn":{{"auto":[{{"apn":"ctiot","id":"auto109590","in_use":true,"iot":true,"name":"China Telecom","pdp":3,"selected":true}},{{"apn":"ctnet","id":"auto109600","in_use":false,"iot":false,"name":"China Telecom 4G","pdp":3,"selected":false}}],"picked_id":null,"notice":{{"at":1,"text":"old","text_en":"old"}},"in_use":{{"apn":"ctiot","id":"auto109590","in_use":true,"iot":true,"name":"China Telecom","pdp":3,"selected":false}},"manual":[{{"apn":"ctnet","id":"manu1","in_use":false,"name":"CTNET","pdp":3,"selected":true}}],"mode":"auto"}},{clients_head}{{"connected_secs":360,"down_bytes":5000000000,"down_rate":250000,"iface":"wlan1","ip":"192.168.0.109","mac":"02:00:00:00:00:0a","name":"MacBook","signal":-47,"signal_tier":"great","up_bytes":1000,"up_rate":null,"band":"5 GHz","channel":44,"width_mhz":160,"wifi_gen":6,"link_down_mbps":2402,"link_up_mbps":1201}},{{"connected_secs":5,"down_bytes":20,"down_rate":null,"iface":"wlan1","ip":null,"mac":"02:00:00:00:00:0b","name":null,"signal":null,"up_bytes":10,"up_rate":null,"band":null,"wifi_gen":null,"link_down_mbps":null}}]}},"data_connected":true,"direct":{{"error":"{longerr}","fetched_at":1790250000,"geo":"\u65e5\u672c \u5927\u962a\u5e9c","ip":"198.51.100.40","isp":"SoftBank","node":null,"source":"ip-api.com"}},"guard":{{"finished_at":0,"last_result":"","phase":"registering","rat":"12","reason":"","started_at":1,"target":"44020"}},"home_operator":{{"country":"中国","mcc":"460","mnc":"01","name":"中国联通"}},{mid}"scenes":{{"current":"abroad","enabled":true,"guard_takeover":false,"list":[{{"abroad":false,"id":"home","name":"在家","wifi_off":true}},{{"abroad":false,"id":"away","name":"外出","wifi_off":false}},{{"abroad":true,"id":"abroad","name":"国外","wifi_off":false}}],"pin":null}},"selection":{{"checked_at":1,"mode":"manual"}},"serving_operator":{{"country":"日本","mcc":"440","mnc":"20","name":"SoftBank"}}}}"#
    );
    p(&mut n, &body);
    assert!(n.direct.present != 0);
    assert_eq!(n.direct.ip, "198.51.100.40", "direct ip survives a long error before it");
    assert_eq!(n.direct.geo, "日本 大阪府", "\\u escapes decoded");
    assert_eq!(n.direct.err.len(), 79, "error cut, not overflowed");
    assert!(n.proxy.present == 0 && n.proxy.ip.is_empty());
    assert_eq!(n.chill_running, 0);
    assert!(n.home.name == "中国联通" && n.home.mnc == "01");
    assert!(n.serving.name == "SoftBank" && n.serving.mcc == "440");
    assert_eq!(n.roaming, 1);
    assert_eq!(n.selection, "manual");
    assert!(n.guard_phase == "registering" && n.guard_target == "44020");
    assert_eq!(n.ncells as usize, NI_MAX_CELLS);
    assert_eq!(n.cells[0].pci, "100", "numeric pci read as text");
    assert!(n.cells[0].rsrq.is_empty(), "null rsrq → empty");
    assert_eq!(n.cells[NI_MAX_CELLS - 1].rsrp, "-97");
    assert_eq!(n.nops as usize, NI_MAX_OPS);
    assert!(n.ops[3].plmn == "44003" && n.ops[3].rat == "12" && n.ops[3].country == "日本");
    assert_eq!(n.scan_state, "done");
    assert_eq!(n.nbr_at, 1790250000);
    assert!(n.scene_known != 0 && n.scene_enabled != 0 && n.nscenes == 3);
    assert!(n.apn_known != 0 && n.apn_manual == 0);
    assert!(n.apn_in_use.id == "auto109590" && n.apn_in_use.apn == "ctiot" && n.apn_in_use.pdp == 3);
    assert!(n.napns == 1 && n.apns[0].name == "CTNET" && n.apns[0].selected != 0 && n.apns[0].in_use == 0);
    assert!(n.ncands == 2 && n.cands[1].id == "auto109600" && n.cands[0].iot != 0 && n.cands[1].iot == 0);
    assert!(n.apn_in_use.iot != 0);
    assert!(n.apn_picked.is_empty() && n.apn_notice.is_empty(), "picked null; a notice older than 10 min dropped");
    assert!(n.clients_known != 0 && n.nclients == 2);
    assert!(n.clients[0].down == 5000000000 && n.clients[0].down_rate == 250000);
    assert!(n.clients[0].up_rate == -1 && n.clients[1].name.is_empty());
    assert!(n.clients[0].signal == -47 && n.clients[1].signal == 0);
    assert!(n.clients[0].signal_tier == "great" && n.clients[1].signal_tier.is_empty());
    assert!(n.clients[0].band == "5 GHz" && n.clients[0].wifi_gen == 6 && n.clients[0].link_down == 2402 && n.clients[0].link_up == 1201);
    assert!(n.clients[1].band.is_empty() && n.clients[1].wifi_gen == 0 && n.clients[1].link_down == 0 && n.clients[1].link_up == 0);
    assert!(n.scenes[0].name == "在家" && n.scenes[0].wifi_off != 0 && n.scenes[2].abroad != 0);
    assert!(n.scene_current == "abroad" && n.scene_pin.is_empty());

    // every manual profile the web added (up to NI_MAX_APNS)
    let ms: Vec<String> = (0..12)
        .map(|i| format!(r#"{{"apn":"apn{i}","id":"manu{i}","in_use":false,"name":"Web {i}","pdp":1,"selected":{}}}"#, i == 7))
        .collect();
    let tail = r#"],"mode":"manual"},"chill_running":false,"direct":null,"proxy":null}"#;
    p(&mut n, &format!(r#"{{"apn":{{"auto":[],"in_use":null,"manual":[{}{tail}"#, ms.join(",")));
    assert!(n.napns == 10 && NI_MAX_APNS == 10);
    assert!(n.apns[7].name == "Web 7" && n.apns[7].selected != 0);

    // an empty body: nothing known yet
    let head = r#"{"chill_running":false,"direct":null,"guard":{"phase":"idle"},"home_operator":null,"#;
    p(
        &mut n,
        &format!(
            r#"{head}"neighbors":{{"cells":[],"state":"idle"}},"proxy":null,"roaming":null,"scan":{{"operators":[],"state":"idle"}},"selection":{{"mode":null}},"serving_operator":null}}"#
        ),
    );
    assert!(n.direct.present == 0 && n.proxy.present == 0);
    assert_eq!(n.roaming, -1);
    assert!(n.selection.is_empty());
    assert!(n.home.name.is_empty() && n.home.mcc.is_empty());
    assert!(n.ncells == 0 && n.nops == 0);
    assert!(n.scene_known == 0 && n.nscenes == 0);

    // garbage
    p(&mut n, "not json at all");
    assert!(n.direct.present == 0 && n.ncells == 0);
    p(&mut n, r#"{"neighbors":{"cells":[{"pci":"1""#);
    assert_eq!(n.ncells, 0, "a cut body does not crash");
}

#[test]
fn english_twins() {
    let l2 = r#"{"guard":{"phase":"reverted","reason":"手动恢复自动","reason_code":"manual_auto","reason_en":"Back to automatic on request","target":"44020"},"home_operator":{"country":"中国","country_en":"China","country_iso":"CN","mcc":"460","mnc":"01","name":"中国联通","operator_en":"China Unicom"},"neighbors":{"cells":[],"error":"原厂扫描会断网且拿不到数据，已停用","error_en":"Off: the stock scan drops data, finds nothing","state":"unsupported"},"scan":{"error":"没搜到网络","error_en":"No networks found","operators":[{"country":"中国台湾","country_en":"Taiwan","country_iso":"TW","name":"中华电信","operator_en":"Chunghwa Telecom","plmn":"46692","rat":"7","status":"1"},{"country":null,"country_en":null,"country_iso":null,"name":"X","operator_en":"X","plmn":"52098","rat":"7","status":"1"}],"state":"error"},"scenes":{"current":"home","enabled":true,"guard_takeover":false,"list":[{"abroad":false,"does":"关 Wi-Fi","id":"home","name":"在家","when":"附近有 Wi-Fi「A」时","wifi_off":true},{"abroad":false,"does":"开 Wi-Fi","id":"office","name":"办公室","when":"插的是日本的卡时","wifi_off":false}],"list_en":[{"does_en":"Wi-Fi off","id":"home","name_en":"Home","when_en":"When Wi-Fi \"A\" is nearby"},{"does_en":"Wi-Fi on","id":"office","name_en":null,"when_en":"When the SIM is from Japan"}],"pin":null},"serving_operator":{"country":"日本","country_en":"Japan","country_iso":"JP","mcc":"440","mnc":"20","name":"SoftBank","operator_en":"SoftBank"}}"#;
    let old = r#"{"guard":{"phase":"reverted","reason":"注册失败","target":"44020"},"home_operator":{"country":"中国","mcc":"460","mnc":"01","name":"中国联通"},"scan":{"error":"没搜到网络","operators":[],"state":"error"},"scenes":{"current":"home","enabled":true,"list":[{"abroad":false,"does":"关 Wi-Fi","id":"home","name":"在家","when":"w","wifi_off":true}],"pin":null}}"#;
    let mut n = empty();
    lang::set_en(false);
    p(&mut n, l2);
    assert!(n.home.name == "中国联通" && n.home.country == "中国");
    assert!(n.home.country_iso == "CN" && n.serving.country_iso == "JP");
    assert!(n.guard_reason == "手动恢复自动" && n.guard_reason_code == "manual_auto");
    assert!(n.scan_err == "没搜到网络" && n.nbr_err == "原厂扫描会断网且拿不到数据，已停用");
    assert!(n.ops[0].name == "中华电信" && n.ops[0].country == "中国台湾" && n.ops[0].country_iso == "TW");
    assert!(n.nscenes == 2 && n.scenes[0].name == "在家" && n.scenes[0].when == "附近有 Wi-Fi「A」时");

    lang::set_en(true);
    p(&mut n, l2);
    assert!(n.home.name == "China Unicom" && n.serving.name == "SoftBank");
    assert!(n.home.country == "China" && n.serving.country == "Japan");
    assert!(n.guard_reason == "Back to automatic on request" && n.guard_reason_code == "manual_auto");
    assert_eq!(n.scan_err, "No networks found");
    assert_eq!(n.nbr_err, "Off: the stock scan drops data, finds nothing");
    assert!(n.ops[0].name == "Chunghwa Telecom" && n.ops[0].country == "Taiwan");
    assert!(n.ops[1].name == "X" && n.ops[1].country.is_empty() && n.ops[1].country_iso.is_empty());
    assert_eq!(n.scenes[0].name, "Home");
    assert!(n.scenes[0].when == "When Wi-Fi \"A\" is nearby" && n.scenes[0].does == "Wi-Fi off");
    assert!(n.scenes[1].name == "办公室" && n.scenes[1].when == "When the SIM is from Japan");
    assert!(n.scenes[0].id == "home" && n.scene_current == "home");
    p(&mut n, old);
    assert!(n.home.name == "中国联通" && n.guard_reason == "注册失败" && n.scan_err == "没搜到网络");
    assert!(n.guard_reason_code.is_empty() && n.home.country_iso.is_empty());
    assert!(n.nscenes == 1 && n.scenes[0].name == "在家" && n.scenes[0].when == "w");

    // the longest twins still leave the fields after them
    let zh = "长".repeat(60);
    let en = "e".repeat(120);
    let nm = "N".repeat(47);
    let lb = format!(
        r#"{{"guard":{{"finished_at":1790250000,"last_result":"{zh}","phase":"revert_failed","rat":"12","reason":"{zh}","reason_code":"auto_retry","reason_en":"{en}","started_at":1790240000,"target":"44020"}},"home_operator":{{"country":"阿联酋","country_en":"United Arab Emirates","country_iso":"AE","mcc":"424","mnc":"02","name":"{nm}","operator_en":"{nm}"}}}}"#
    );
    p(&mut n, &lb);
    assert_eq!(n.guard_reason.len(), 120);
    assert!(n.guard_target == "44020" && n.guard_reason_code == "auto_retry");
    assert!(n.home.mnc == "02" && n.home.country == "United Arab Emirates");
    lang::set_en(false);
    p(&mut n, &lb);
    assert!(n.guard_reason == zh && n.guard_target == "44020");
    assert!(n.home.country == "阿联酋" && n.home.country_iso == "AE");

    // exit geo_en / isp_en
    let eb = r#"{"direct":{"geo":"日本 东京都","geo_en":"Japan","ip":"1.2.3.4","isp":"电信","isp_en":"China Telecom"},"proxy":{"geo":"广东 深圳","ip":"5.6.7.8","isp":"某某宽带"}}"#;
    lang::set_en(true);
    p(&mut n, eb);
    assert!(n.direct.geo == "Japan" && n.direct.isp == "China Telecom");
    assert!(n.proxy.geo == "广东 深圳" && n.proxy.isp == "某某宽带");
    lang::set_en(false);
    p(&mut n, eb);
    assert!(n.direct.geo == "日本 东京都" && n.direct.isp == "电信");
}
