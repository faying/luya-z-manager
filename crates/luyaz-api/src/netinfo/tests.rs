use super::*;

#[test]
fn apn_in_use_is_the_dialled_profile() {
    // Recorded on the owner's device 2026-09-25: auto mode, and the manual
    // list still marks CTNET as the manual choice.
    let mode = json!({"apn_mode": 0});
    let dialled = json!({"profilename":"China Telecom","wanapn":"ctiot","pdpType":3,"profileId":"auto109590","isEnable":false});
    let auto = json!({"apnListArray":[
        {"profilename":"China Telecom","wanapn":"ctiot","pdpType":3,"profileId":"auto109590","isEnable":true},
        {"profilename":"China Telecom 4G","wanapn":"ctnet","pdpType":3,"profileId":"auto109600","isEnable":false}]});
    let manual = json!({"apnListArray":[{"profilename":"CTNET","wanapn":"ctnet","pdpType":3,"profileId":"manu1","isEnable":true}]});
    let v = apn_view(&mode, &dialled, &auto, &manual);
    assert_eq!(v["mode"], "auto");
    assert_eq!(v["in_use"]["name"], "China Telecom");
    assert_eq!(v["in_use"]["apn"], "ctiot");
    assert_eq!(v["auto"][0]["in_use"], true);
    assert_eq!(
        (v["auto"][0]["iot"].as_bool(), v["auto"][1]["iot"].as_bool()),
        (Some(true), Some(false))
    );
    assert_eq!(v["manual"][0]["selected"], true);
    assert_eq!(v["manual"][0]["in_use"], false);
    let v = apn_view(
        &json!({"apn_mode":"1"}),
        &json!({"profileId":"manu1","profilename":"CTNET"}),
        &auto,
        &manual,
    );
    assert_eq!(
        (v["mode"].as_str(), v["manual"][0]["in_use"].as_bool()),
        (Some("manual"), Some(true))
    );
    assert!(apn_view(&mode, &Value::Null, &Value::Null, &Value::Null)["in_use"].is_null());
}

#[test]
fn geo_ipip_json() {
    let g = parse_geo(
        r#"{"ret":"ok","data":{"ip":"1.2.3.4","location":["中国","北京","北京","","联通"]}}"#,
    )
    .unwrap();
    assert_eq!(
        g,
        Geo {
            ip: "1.2.3.4".into(),
            geo: "中国 北京".into(),
            isp: "联通".into()
        }
    );
}

#[test]
fn geo_ipip_text() {
    let g = parse_geo("当前 IP：1.2.3.4  来自于：中国 广东 深圳  电信\n").unwrap();
    assert_eq!(g.ip, "1.2.3.4");
    assert_eq!(g.geo, "中国 广东 深圳");
    assert_eq!(g.isp, "电信");
}

#[test]
fn geo_ip_api() {
    let g = parse_geo(r#"{"status":"success","country":"日本","regionName":"东京都","city":"东京","isp":"KDDI","org":"","query":"5.6.7.8"}"#).unwrap();
    assert_eq!(
        g,
        Geo {
            ip: "5.6.7.8".into(),
            geo: "日本 东京都".into(),
            isp: "KDDI".into()
        }
    );
    assert!(parse_geo(r#"{"status":"fail","query":"x"}"#).is_none());
}

#[test]
fn geo_ipinfo() {
    let g = parse_geo(r#"{"ip":"9.9.9.9","city":"Taipei","region":"Taiwan","country":"TW","org":"AS3462 Chunghwa Telecom"}"#).unwrap();
    assert_eq!(g.geo, "中国台湾 Taiwan Taipei");
    assert_eq!(g.isp, "Chunghwa Telecom");
    assert!(parse_geo("<html>").is_none());
}

#[test]
fn geo_ping0() {
    let g = parse_geo("188.253.116.74\n中国 台湾 台北\nAS38136\nAkari Networks Limited\n").unwrap();
    assert_eq!(
        g,
        Geo {
            ip: "188.253.116.74".into(),
            geo: "中国 台湾 台北".into(),
            isp: "Akari Networks Limited".into()
        }
    );
    let g = parse_geo("1.2.3.4\n中国 广东 深圳 — 电信\nAS4134\nCHINANET\n").unwrap();
    assert_eq!((g.geo.as_str(), g.isp.as_str()), ("中国 广东 深圳", "电信"));
}

#[test]
fn geo_cip_cc() {
    let body = "IP\t: 116.232.29.205\n地址\t: 中国 上海 上海\n运营商\t: 电信\n\n数据二\t: 中国 上海 上海 | 电信\n\nURL\t: http://www.cip.cc/116.232.29.205\n";
    let g = parse_geo(body).unwrap();
    assert_eq!(
        g,
        Geo {
            ip: "116.232.29.205".into(),
            geo: "中国 上海".into(),
            isp: "电信".into()
        }
    );
    assert!(parse_geo("IP\t: nope\n").is_none());
}

#[test]
fn geo_myip_la() {
    let g = parse_geo(r#"{"ip":"188.253.116.74","location":{"city":"台北市","country_code":"TW","country_name":"中国","latitude":"25.03","province":"台湾"}}"#).unwrap();
    assert_eq!(
        g,
        Geo {
            ip: "188.253.116.74".into(),
            geo: "中国 台湾 台北市".into(),
            isp: String::new()
        }
    );
}

#[test]
fn geo_ip_sb_and_ipapi_co_use_the_country_code() {
    let g = parse_geo(r#"{"region":"Taipei City","isp":"Akari Networks","city":"Taipei","ip":"188.253.116.74","country":"Taiwan","country_code":"TW"}"#).unwrap();
    assert_eq!(g.geo, "中国台湾 Taipei City");
    assert_eq!(g.isp, "Akari Networks");
    let g = parse_geo(r#"{"ip":"1.1.1.1","city":"Tokyo","region":"Tokyo","country":"JP","country_name":"Japan","org":"AS13335 CLOUDFLARENET"}"#).unwrap();
    assert!(g.geo.starts_with("日本"), "{}", g.geo);
    assert_eq!(g.isp, "CLOUDFLARENET");
}

#[test]
fn geo_and_isp_in_english() {
    for (zh, en) in [
        ("日本 东京都", "Japan"),
        ("中国 台湾 台北", "Taiwan"),
        ("中国台湾 Taipei City", "Taiwan, Taipei City"),
        ("中国台湾 Taiwan Taipei", "Taiwan, Taipei"),
        ("中国 广东 深圳", "China"),
        ("中国香港", "Hong Kong"),
        ("美国 加利福尼亚州 洛杉矶", "United States"),
        ("广东 深圳", ""),
        ("", ""),
    ] {
        assert_eq!(geo_en(zh), en, "{zh}");
    }
    for (zh, en) in [
        ("电信", "China Telecom"),
        ("中国联通", "China Unicom"),
        ("移动", "China Mobile"),
        ("KDDI", "KDDI"),
        ("台湾大哥大", "Taiwan Mobile"),
        ("某某宽带", ""),
    ] {
        assert_eq!(isp_en(zh), en, "{zh}");
    }
    // every country the MCC table knows can be named back in English
    for mcc in 200..=999u16 {
        if let Some(iso) = mcc_iso(mcc) {
            let (zh, en) = iso_names(iso).unwrap();
            assert_eq!(place_en(zh), Some(en), "{iso}");
        }
    }
    let j = Lookup {
        geo: Geo {
            ip: "1.2.3.4".into(),
            geo: "日本 东京都".into(),
            isp: "电信".into(),
        },
        ..Lookup::default()
    }
    .json();
    assert_eq!(
        (j["geo_en"].as_str(), j["isp_en"].as_str()),
        (Some("Japan"), Some("China Telecom"))
    );
    let j = Lookup {
        geo: Geo {
            ip: "1.2.3.4".into(),
            geo: "广东 深圳".into(),
            isp: "某某宽带".into(),
        },
        ..Lookup::default()
    }
    .json();
    assert!(j["geo_en"].is_null() && j["isp_en"].is_null());
}

#[test]
fn imsi_mnc_length_by_country() {
    assert_eq!(
        plmn_from_imsi("460011234567890"),
        Some(Plmn {
            mcc: 460,
            mnc: 1,
            mnc_digits: 2
        })
    );
    assert_eq!(
        plmn_from_imsi("310260123456789"),
        Some(Plmn {
            mcc: 310,
            mnc: 260,
            mnc_digits: 3
        })
    );
    assert_eq!(plmn_from_imsi("44010"), None);
    assert_eq!(plmn_from_imsi("4401x123456"), None);
    let p = plmn_from_imsi("310260123456789").unwrap();
    assert_eq!(plmn_json(&p, "")["name"], "T-Mobile");
    assert_eq!(plmn_json(&p, "")["mnc"], "260");
    let p = plmn_from_imsi("454000123456789").unwrap();
    assert_eq!(plmn_json(&p, "")["mnc"], "00");
    assert_eq!(plmn_json(&p, "")["country"], "中国香港");
}

#[test]
fn plmn_strings() {
    assert!(same_plmn("46001", "46001"));
    assert!(same_plmn("46692", "466092"));
    assert!(!same_plmn("46001", "46000"));
    assert!(!same_plmn("46001", ""));
    let p = plmn_from_str("44020").unwrap();
    assert_eq!(plmn_json(&p, "x")["name"], "SoftBank");
    let p = plmn_from_str("99999").unwrap();
    assert_eq!(plmn_json(&p, "Hint")["name"], "Hint");
    assert_eq!(serving_plmn(&json!({"rmcc":"460","rmnc":"01"})), "46001");
    assert_eq!(serving_plmn(&json!({"rplmn_num":"46011"})), "46011");
}

#[test]
fn connected_is_not_disconnected() {
    assert!(data_connected(
        &json!({"connect_status":"ipv4_ipv6_connected"})
    ));
    assert!(!data_connected(&json!({"connect_status":"disconnected"})));
    assert!(!data_connected(&json!({})));
}

#[test]
fn cops_mode() {
    assert_eq!(
        parse_cops_mode("\r\n+COPS: 0,0,\"CHN-UNICOM\",7\r\n\r\nOK\r\n"),
        Some("auto")
    );
    assert_eq!(
        parse_cops_mode("+COPS: 1,2,\"46001\",12\r\nOK"),
        Some("manual")
    );
    assert_eq!(parse_cops_mode("+COPS: 0\r\nOK"), Some("auto"));
    assert_eq!(parse_cops_mode("ERROR"), None);
}

fn o<'a>(result: &'a str, serving: &'a str, connected: bool) -> Obs<'a> {
    Obs {
        result,
        before: "",
        serving,
        connected,
    }
}

#[test]
fn slot_due_and_backoff() {
    let mut s = Slot::default();
    assert!(s.due("rmnet 10.0.0.1", 1000));
    s.store(
        "rmnet 10.0.0.1",
        1000,
        Ok((
            Geo {
                ip: "1.1.1.1".into(),
                ..Geo::default()
            },
            "h".into(),
        )),
        None,
    );
    assert!(!s.due("rmnet 10.0.0.1", 1000 + LOOKUP_MAX_AGE - 1));
    assert!(s.due("rmnet 10.0.0.1", 1000 + LOOKUP_MAX_AGE));
    assert!(s.due("rmnet 10.0.0.2", 1001));
    // A failure keeps the old answer for the same exit and backs off.
    s.store("rmnet 10.0.0.1", 2000, Err("timeout".into()), None);
    assert_eq!(s.value.as_ref().unwrap().geo.ip, "1.1.1.1");
    assert!(!s.due("rmnet 10.0.0.1", 2000 + LOOKUP_RETRY - 1));
    assert!(s.due("rmnet 10.0.0.1", 2000 + LOOKUP_RETRY));
    // A failure on a new exit drops the old, now wrong, address.
    s.store("rmnet 10.0.0.9", 3000, Err("timeout".into()), None);
    assert_eq!(s.value.as_ref().unwrap().geo.ip, "");
}

#[test]
fn config_parsing() {
    let c = Config::parse("");
    assert_eq!(c.proxy_port, DEFAULT_PROXY_PORT);
    assert!(c.enabled);
    let c = Config::parse("direct=https://a.example/x, ftp://no\nproxy_port=7000\nlookups=0\n");
    assert_eq!(c.direct, vec!["https://a.example/x".to_string()]);
    assert_eq!(c.proxy_port, 7000);
    assert!(!c.enabled);
}

#[test]
fn operator_list() {
    let v = json!({"operators":[
        {"m_mcc_mnc":"46001","m_oper_name":"CHN-UNICOM","m_rat":"12","m_status":"2"},
        {"m_mcc_mnc":"46000","m_oper_name":"CHINA MOBILE","m_rat":"7","m_status":"1"},
        {"m_mcc_mnc":"99999","m_oper_name":"Test","m_rat":7,"m_status":"3"}
    ]});
    let mut ops = Vec::new();
    collect_operators(&v, &mut ops);
    assert_eq!(ops.len(), 3);
    assert_eq!(ops[0]["name"], "中国联通");
    assert_eq!(ops[0]["raw_name"], "CHN-UNICOM");
    assert_eq!(ops[0]["rat"], "12");
    assert_eq!(ops[2]["name"], "Test");
    assert_eq!(ops[2]["rat"], "7");
    assert_eq!(ops[2]["country"], Value::Null);
    assert_eq!(scan_finished("done"), Some(true));
    assert_eq!(scan_finished("2"), Some(true));
    assert_eq!(scan_finished("fail"), Some(false));
    assert_eq!(scan_finished("1"), None);
    assert_eq!(scan_finished("manual_selecting"), None);
    assert_eq!(scan_finished("manual_selected"), Some(true));
    assert_eq!(scan_finished(""), None);
    // the device's own answer, verbatim
    let mut real = Vec::new();
    collect_operators(
        &json!({"m_netselect_contents": "2,CT,46011,11;1,CT,46011,7;3,CMCC,46000,11;3,UNICOM,46001,2;"}),
        &mut real,
    );
    assert_eq!(real.len(), 4);
    assert_eq!(
        (
            real[0]["plmn"].as_str(),
            real[0]["status"].as_str(),
            real[0]["rat"].as_str()
        ),
        (Some("46011"), Some("2"), Some("11"))
    );
    assert_eq!(real[3]["name"], "中国联通");
    assert_eq!(real[3]["rat"], "2");
}

#[test]
fn measured_netinfo_shape() {
    // 2026-09-25, the device on China Telecom SA
    let net = json!({"rmcc": 460, "rmnc": 11, "net_select_mode": "auto_select", "network_provider": "CT"});
    assert_eq!(serving_plmn(&net), "46011");
    assert_eq!(serving_plmn(&json!({"rmcc": 460, "rmnc": 1})), "46001");
    assert_eq!(serving_plmn(&json!({"rmcc": 310, "rmnc": 260})), "310260");
    assert_eq!(serving_plmn(&json!({"rmcc": 0, "rmnc": 0})), "");
    assert_eq!(selection_from_netinfo(&net), Some("auto"));
    assert_eq!(
        selection_from_netinfo(&json!({"net_select_mode": "manual_select"})),
        Some("manual")
    );
    assert_eq!(selection_from_netinfo(&json!({})), None);
}

#[test]
fn stations_and_leases() {
    let dev = "phy#1\n\tInterface wlan1\n\t\tifindex 9\n\t\ttype AP\nphy#0\n\tInterface wlan0\n\t\ttype managed\n";
    assert_eq!(ap_ifaces(dev), vec!["wlan1".to_string()]);
    let dump = "Station 02:00:00:00:00:0A (on wlan1)\n\tinactive time:\t120 ms\n\trx bytes:\t1000\n\ttx bytes:\t50000\n\tsignal:  \t-47 [-47, -49] dBm\n\tconnected time:\t360 seconds\nStation 02:00:00:00:00:0b (on wlan1)\n\trx bytes:\t10\n\ttx bytes:\t20\n";
    let st = parse_station_dump(dump, "wlan1");
    assert_eq!(st.len(), 2);
    assert_eq!(st[0].mac, "02:00:00:00:00:0a");
    assert_eq!((st[0].down, st[0].up), (50000, 1000));
    assert_eq!(st[0].signal, Some(-47));
    assert_eq!(station_wifi_json(&st[0])["signal_tier"], "great");
    assert!(station_wifi_json(&st[1])["signal_tier"].is_null());
    assert_eq!(
        [-55, -56, -67, -68, -75, -76].map(signal_tier),
        ["great", "good", "good", "fair", "fair", "weak"]
    );
    assert_eq!(st[0].connected_secs, 360);
    let leases = parse_leases(
        "1790300000 02:00:00:00:00:0a 192.168.0.109 laptop 01:aa\n1790300000 02:00:00:00:00:0b 192.168.0.110 * *\n",
    );
    assert_eq!(
        leases[0],
        (
            "02:00:00:00:00:0a".into(),
            "192.168.0.109".into(),
            "laptop".into()
        )
    );
    assert_eq!(leases[1].2, "");
    assert_eq!(rate(Some((1000, 100)), 21000, 110), Some(2000));
    assert_eq!(rate(Some((5000, 100)), 10, 110), None);
    assert_eq!(rate(None, 10, 110), None);
}

#[test]
fn band_and_link() {
    // Recorded on the owner's device 2026-09-25 (MACs made up).
    let ap5 = parse_ap_info(
        "\tssid U-Cafe\n\ttype AP\n\tchannel 44 (5220 MHz), width: 160 MHz, center1: 5250 MHz\n",
    );
    assert_eq!(
        ap5,
        ApInfo {
            band: "5 GHz",
            channel: Some(44),
            width_mhz: Some(160)
        }
    );
    let ap2 = parse_ap_info("\tchannel 11 (2462 MHz), width: 40 MHz, center1: 2452 MHz\n");
    assert_eq!(
        (ap2.band, ap2.channel, ap2.width_mhz),
        ("2.4 GHz", Some(11), Some(40))
    );
    assert_eq!(parse_ap_info("\ttype AP\n").band, "");
    let dump = "Station 02:00:00:00:00:0c (on wlan2)\n\tsignal:  \t-39 dBm\n\ttx bitrate:\t2401.9 MBit/s 160MHz HE-MCS 11 HE-NSS 2 HE-GI 0 HE-DCM 0\n\trx bitrate:\t1200.9 MBit/s 160MHz HE-MCS 11 HE-NSS 1 HE-GI 0 HE-DCM 0\n";
    let st = parse_station_dump(dump, "wlan2");
    assert_eq!(
        (st[0].link_down_mbps, st[0].link_up_mbps, st[0].wifi_gen),
        (Some(2402), Some(1201), Some(6))
    );
    assert_eq!(
        parse_bitrate("866.7 MBit/s VHT-MCS 9 80MHz short GI VHT-NSS 2"),
        (Some(867), Some(5))
    );
    assert_eq!(
        parse_bitrate("144.4 MBit/s MCS 15 short GI"),
        (Some(144), Some(4))
    );
    assert_eq!(parse_bitrate("54.0 MBit/s"), (Some(54), None));
}

#[test]
fn place_join() {
    assert_eq!(join_place(&["中国", "北京", "北京"]), "中国 北京");
    assert_eq!(join_place(&["日本", "东京都", "东京"]), "日本 东京都");
    assert_eq!(join_place(&["", "", ""]), "");
}

// ── L2: English and codes next to the Chinese ──────────────────────────

#[test]
fn operators_in_the_table_get_the_glossary_english() {
    // docs/ui-glossary.md §8, copied exactly; the core's four must match.
    let table: &[(&str, &str)] = &[
        ("46000", "China Mobile"),
        ("46002", "China Mobile"),
        ("46004", "China Mobile"),
        ("46007", "China Mobile"),
        ("46008", "China Mobile"),
        ("46001", "China Unicom"),
        ("46006", "China Unicom"),
        ("46009", "China Unicom"),
        ("46003", "China Telecom"),
        ("46005", "China Telecom"),
        ("46011", "China Telecom"),
        ("46015", "China Broadnet"),
        ("45403", "3 HK"),
        ("45404", "3 HK"),
        ("45412", "China Mobile HK"),
        ("45413", "China Mobile HK"),
        ("45407", "China Unicom HK"),
        ("45500", "SmarTone Macau"),
        ("45502", "China Telecom Macau"),
        ("45507", "China Telecom Macau"),
        ("45503", "3 Macau"),
        ("45505", "3 Macau"),
        ("46601", "FarEasTone"),
        ("46605", "FarEasTone"),
        ("46611", "Chunghwa Telecom"),
        ("46692", "Chunghwa Telecom"),
        ("46689", "Taiwan Mobile"),
        ("46693", "Taiwan Mobile"),
        ("46697", "Taiwan Mobile"),
        ("44011", "Rakuten Mobile"),
        // already English: as it is
        ("44020", "SoftBank"),
        ("310260", "T-Mobile"),
        ("45501", "CTM"),
    ];
    for (plmn, en) in table {
        let j = plmn_json(&plmn_from_str(plmn).unwrap(), "broadcast");
        assert_eq!(j["operator_en"], *en, "{plmn}");
        assert_ne!(j["name"], "broadcast", "{plmn} is in the table");
    }
    let j = plmn_json(&plmn_from_str("46001").unwrap(), "");
    assert_eq!(
        (j["name"].as_str(), j["country"].as_str()),
        (Some("中国联通"), Some("中国"))
    );
    assert_eq!(
        (j["country_en"].as_str(), j["country_iso"].as_str()),
        (Some("China"), Some("CN"))
    );
}

#[test]
fn operators_outside_the_table_use_the_broadcast_name_in_both() {
    let p = plmn_from_str("26207").unwrap();
    let j = plmn_json(&p, "o2 - de");
    assert_eq!(
        (j["name"].as_str(), j["operator_en"].as_str()),
        (Some("o2 - de"), Some("o2 - de"))
    );
    assert_eq!(
        (j["country_en"].as_str(), j["country_iso"].as_str()),
        (Some("Germany"), Some("DE"))
    );
    // no name at all: both null
    let j = plmn_json(&plmn_from_str("99999").unwrap(), "");
    assert!(j["name"].is_null() && j["operator_en"].is_null());
    assert!(j["country"].is_null() && j["country_en"].is_null() && j["country_iso"].is_null());
    // a search result: table name, else the network's own
    let mut ops = Vec::new();
    collect_operators(
        &json!([{"m_mcc_mnc":"46692","m_oper_name":"Chunghwa"}, {"m_mcc_mnc":"52098","m_oper_name":"X"}]),
        &mut ops,
    );
    assert_eq!(
        (ops[0]["name"].as_str(), ops[0]["operator_en"].as_str()),
        (Some("中华电信"), Some("Chunghwa Telecom"))
    );
    assert_eq!(ops[0]["country_iso"], "TW");
    assert_eq!(
        (ops[1]["name"].as_str(), ops[1]["operator_en"].as_str()),
        (Some("X"), Some("X"))
    );
    let op = operator_from_record("2,CMHK,45412,7").unwrap();
    assert_eq!(
        (op["operator_en"].as_str(), op["country_en"].as_str()),
        (Some("China Mobile HK"), Some("Hong Kong"))
    );
}

#[test]
fn every_table_entry_has_english_without_cjk() {
    use crate::health::has_cjk;
    for mcc in 0..1000u16 {
        if let Some((zh, en, iso)) = country_info(mcc) {
            assert!(
                !zh.is_empty() && !en.is_empty() && !has_cjk(en),
                "{mcc}: {en}"
            );
            assert_eq!(
                country(mcc),
                Some(zh),
                "{mcc}: the Chinese name is unchanged"
            );
            match iso {
                Some(c) => assert!(
                    c.len() == 2 && c.bytes().all(|b| b.is_ascii_uppercase()),
                    "{mcc}: {c}"
                ),
                None => assert_eq!(mcc, 901),
            }
        }
        for mnc in 0..1000u16 {
            if let Some(zh) = operator(mcc, mnc) {
                let en = operator_en(zh);
                assert!(!has_cjk(en), "{mcc}-{mnc}: {en}");
            }
        }
    }
    for (mcc, iso, en) in [
        (454, "HK", "Hong Kong"),
        (455, "MO", "Macau"),
        (466, "TW", "Taiwan"),
        (440, "JP", "Japan"),
        (450, "KR", "South Korea"),
        (310, "US", "United States"),
        (234, "GB", "United Kingdom"),
        (460, "CN", "China"),
    ] {
        assert_eq!(
            country_info(mcc).map(|c| (c.1, c.2)),
            Some((en, Some(iso))),
            "{mcc}"
        );
    }
    assert_eq!(country_info(901), Some(("国际", "International", None)));
    // every MCC with an ISO code has its names (none silently dropped)
    for mcc in 0..1000u16 {
        if mcc_iso(mcc).is_some() {
            assert!(country_info(mcc).is_some(), "{mcc}");
        }
    }
    // the IP-lookup path translates the same 23 codes as before, no more
    assert_eq!(country_by_code("HK"), Some("中国香港"));
    assert_eq!(country_by_code("IT"), None);
    assert_eq!(country_by_code("XX"), None);
    assert!(GEO_CODES.iter().all(|c| iso_names(c).is_some()));
}

#[test]
fn guard_reasons_have_a_code_and_english() {
    use crate::health::has_cjk;
    let whys = [
        "注册失败",
        "已注册但数据没通",
        "超时没注册上",
        "手动恢复自动",
        "重启前没做完",
        "重启前还没拨上",
    ];
    let mut all = Vec::new();
    for w in whys {
        assert_ne!(why_info(w).0, "other", "{w} has no code");
        all.push(Reason::why(w));
        all.push(Reason::auto_retry(w, 3, AUTO_NOT_YET, 120));
        all.push(Reason::auto_retry(w, 1, "AT port busy", 30));
        all.push(Reason::auto_no_data(w));
        all.push(Reason::auto_held(w));
    }
    all.push(Reason::auto_no_data(""));
    all.push(Reason::redial_retry(2, 120));
    for r in &all {
        assert!(
            !r.en.is_empty() && !has_cjk(&r.en) && !r.en.ends_with('.'),
            "{r:?}"
        );
        // the code says exactly which message: the web shows its own line
        // for manual_auto and the reason text for anything else
        assert_eq!(r.code == "manual_auto", r.zh == "手动恢复自动", "{r:?}");
    }
    assert_eq!(
        Reason::why("手动恢复自动").en,
        "Back to automatic on request"
    );
    assert_eq!(
        Reason::auto_no_data("手动恢复自动").zh,
        "手动恢复自动；已回到自动选网，但数据还没拨上"
    );
    assert_eq!(
        Reason::auto_no_data("手动恢复自动").en,
        "Back to automatic on request; automatic again, but no data yet"
    );
    assert_eq!(
        Reason::auto_retry("注册失败", 2, AUTO_NOT_YET, 60).zh,
        "注册失败；第 2 次恢复自动没成功（模组还没回到自动），60 秒后再试"
    );
    // what decide() hands over maps too
    for v in [
        decide(
            "46001",
            &Obs {
                result: "fail",
                before: "",
                serving: "46000",
                connected: true,
            },
            99,
        ),
        decide(
            "46001",
            &Obs {
                result: "",
                before: "",
                serving: "46000",
                connected: true,
            },
            GUARD_TIMEOUT,
        ),
    ] {
        let Verdict::Revert(w) = v else {
            panic!("{v:?}")
        };
        assert_ne!(why_info(w).0, "other");
    }
}

#[test]
fn guard_json_sends_null_until_there_is_a_reason() {
    let mut g = Guard::default();
    let j = g.json();
    assert!(j["reason_code"].is_null() && j["reason_en"].is_null());
    assert_eq!(j["reason"], "");
    g.set_reason(Reason::why("手动恢复自动"));
    let j = g.json();
    assert_eq!(
        (j["reason"].as_str(), j["reason_code"].as_str()),
        (Some("手动恢复自动"), Some("manual_auto"))
    );
}

#[test]
fn user_facing_errors_have_english() {
    use crate::health::has_cjk;
    let mut zh: Vec<String> = [OpKind::Scanning, OpKind::Registering, OpKind::Reverting]
        .iter()
        .map(|k| {
            claim(
                &mut Ops {
                    kind: *k,
                    ..Ops::default()
                },
                OpKind::Registering,
            )
            .unwrap_err()
            .to_string()
        })
        .collect();
    zh.extend(
        [
            "正在搜索网络，搜完再操作",
            "没有这个手动 APN",
            "上一次 APN 切换还没做完",
            "没搜到网络",
            "没有可用的查询地址",
            NBR_UNSUPPORTED,
            "写不了保护标记，没有注册：No space left on device (os error 28)",
            "写不了保护标记：Read-only file system",
            "模组报告搜索失败（manual_fail）",
            "ip.sb: 看不懂返回",
        ]
        .map(String::from),
    );
    for z in &zh {
        let en = error_en(z).unwrap_or_else(|| panic!("{z} has no English"));
        assert!(
            !has_cjk(&en) && !en.ends_with('.') && !en.contains("Please"),
            "{z} -> {en}"
        );
    }
    assert_eq!(
        error_en("模组报告搜索失败（3）").as_deref(),
        Some("Modem reported the search failed (3)")
    );
    assert_eq!(
        error_en("ubus call failed: timeout").as_deref(),
        Some("ubus call failed: timeout"),
        "English passes through"
    );
    // clipped mid-pattern, or Chinese we have no English for: null, never Chinese
    let clipped = clip(format!("模组报告搜索失败（{}）", "s".repeat(80)));
    assert!(clipped.ends_with('…'));
    assert_eq!(error_en(&clipped), None);
    assert_eq!(error_en("某个新的中文错误"), None);
    let s = Scan {
        error: "没搜到网络".into(),
        ..Scan::default()
    };
    assert_eq!(s.json()["error_en"], "No networks found");
    assert!(Scan::default().json()["error_en"].is_null());
    let l = Lookup {
        error: Some("ip.sb: 看不懂返回".into()),
        ..Lookup::default()
    };
    assert_eq!(l.json()["error_en"], "ip.sb: unreadable reply");
    assert!(Lookup::default().json()["error_en"].is_null());
}

/// Old touch screens read these with fixed buffers that truncate silently
/// (touch-ui netinfo.c: operator `o[256]`, guard `obj[768]`, scan list
/// `arr[6144]`; L2 review O1). Worst cases must leave a quarter spare.
#[test]
fn new_fields_fit_the_old_touch_screen() {
    let strip = |v: &Value| {
        let mut v = v.clone();
        v.as_object_mut()
            .unwrap()
            .retain(|k, _| !k.ends_with("_en") && !k.ends_with("_iso") && k != "reason_code");
        v.to_string().len()
    };
    // A long broadcast name (the table's longest is shorter) on the
    // longest-named country.
    // Keys come out sorted, so everything after "name" is new (operator_en):
    // truncation there costs the old screen nothing. What it reads must end
    // well inside the buffer, and the whole object should fit.
    let op = plmn_json(&plmn_from_str("42402").unwrap(), &"N".repeat(32));
    let text = op.to_string();
    eprintln!(
        "operator: {} bytes before L2, {} after",
        strip(&op),
        text.len()
    );
    let name_end = text.find(r#","operator_en""#).unwrap();
    assert!(name_end < 256 * 3 / 4 && text.len() < 256, "{text}");
    let op = plmn_json(&plmn_from_str("45500").unwrap(), "");
    assert!(op.to_string().len() < 256 * 3 / 4, "{op}");

    let mut g = Guard {
        phase: "revert_failed",
        target: "460011".into(),
        rat: "11".into(),
        started_at: 1_758_600_000,
        finished_at: 1_758_600_000,
        // what register() stores when the ubus call errors
        last_result: clip(format!("调用出错：{}", "e".repeat(100))),
        ..Guard::default()
    };
    // The longest reason: errors in it come from the AT port (English) or
    // are AUTO_NOT_YET; try both.
    g.set_reason(Reason::auto_retry(
        "已注册但数据没通",
        99,
        &"e".repeat(100),
        300,
    ));
    let j = g.json();
    eprintln!(
        "guard: {} bytes before L2, {} after",
        strip(&j),
        j.to_string().len()
    );
    assert!(
        j.to_string().len() < 768 * 9 / 10,
        "{} bytes",
        j.to_string().len()
    );
    g.set_reason(Reason::auto_retry(
        "已注册但数据没通",
        99,
        AUTO_NOT_YET,
        300,
    ));
    let j = g.json();
    eprintln!(
        "guard: {} bytes before L2, {} after",
        strip(&j),
        j.to_string().len()
    );
    assert!(
        j.to_string().len() < 768 * 9 / 10,
        "{} bytes",
        j.to_string().len()
    );

    let rec = format!("2,{},52098,11", "N".repeat(32));
    let ops: Vec<Value> = (0..SCAN_MAX_OPS)
        .map(|_| operator_from_record(&rec).unwrap())
        .collect();
    let size = serde_json::to_string(&ops).unwrap().len();
    eprintln!("scan list: {size} bytes");
    assert!(size < 6144 * 3 / 4, "{size} bytes");
    for o in &ops {
        assert!(o.to_string().len() < 768 * 3 / 4);
    }
}

#[test]
fn guard_decisions() {
    // Where the modem is decides success, whatever the string says.
    assert_eq!(decide("46001", &o("weird", "46001", true), 9), Verdict::Ok);
    assert_eq!(
        decide("46001", &o("success", "46000", true), 9),
        Verdict::Wait
    );
    // A stale "fail" right after the call is ignored…
    assert_eq!(decide("46001", &o("fail", "46000", true), 3), Verdict::Wait);
    // …a later one reverts.
    assert_eq!(
        decide("46001", &o("fail", "46000", true), 9),
        Verdict::Revert("注册失败")
    );
    // The modem's own words (stock web): manual_fail reverts…
    assert_eq!(
        decide("46001", &o("manual_fail", "46000", false), 9),
        Verdict::Revert("注册失败")
    );
    // …"0" is not an answer yet, and manual_success still needs data.
    assert_eq!(decide("46001", &o("0", "46000", false), 9), Verdict::Wait);
    assert_eq!(
        decide("46001", &o("manual_success", "46001", false), 9),
        Verdict::Wait
    );
    assert_eq!(
        decide("46001", &o("manual_success", "46001", true), 9),
        Verdict::Ok
    );
    // The previous attempt's manual_fail, still there, is not this one's…
    let stale = Obs {
        result: "manual_fail",
        before: "manual_fail",
        serving: "46000",
        connected: false,
    };
    assert_eq!(decide("46001", &stale, 20), Verdict::Wait);
    // …until half the deadline has passed with no new answer.
    assert_eq!(
        decide("46001", &stale, GUARD_TIMEOUT / 2),
        Verdict::Revert("注册失败")
    );
    // Registered but no data at the deadline is still a revert.
    assert_eq!(
        decide("46001", &o("1", "46001", false), GUARD_TIMEOUT),
        Verdict::Revert("已注册但数据没通")
    );
    assert_eq!(
        decide("46001", &o("", "46000", true), GUARD_TIMEOUT),
        Verdict::Revert("超时没注册上")
    );
    assert_eq!(
        decide("46001", &o("", "", false), GUARD_TIMEOUT - 1),
        Verdict::Wait
    );
}

#[test]
fn string_records() {
    let mut ops = Vec::new();
    collect_operators(
        &json!({"m_netselect_contents": "2,China Telecom,CT,46011,12;1,China Mobile,CMCC,46000,7;"}),
        &mut ops,
    );
    assert_eq!(ops.len(), 2);
    assert_eq!(ops[0]["plmn"], "46011");
    assert_eq!(ops[0]["name"], "中国电信");
    assert_eq!(ops[0]["status"], "2");
    assert_eq!(ops[0]["rat"], "12");
    let mut none = Vec::new();
    collect_operators(&json!({"m_netselect_contents": ""}), &mut none);
    assert!(none.is_empty());
}

#[test]
fn one_operation_at_a_time() {
    let mut o = Ops::default();
    assert!(claim(&mut o, OpKind::Scanning).is_ok());
    assert_eq!(
        claim(&mut o, OpKind::Registering),
        Err("正在搜索网络，等搜完再操作")
    );
    assert_eq!(
        claim(&mut o, OpKind::Reverting),
        Err("正在搜索网络，等搜完再操作")
    );
    o.kind = OpKind::Registering;
    assert!(claim(&mut o, OpKind::Scanning).is_err());
    o.kind = OpKind::Idle;
    assert!(claim(&mut o, OpKind::Reverting).is_ok());
    assert_eq!(o.kind, OpKind::Reverting);
}
