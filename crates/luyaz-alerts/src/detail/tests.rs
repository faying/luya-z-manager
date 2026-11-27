use super::*;

fn zh(kind: &str, text: &str) -> Option<String> {
    detail_zh(kind, text)
}

fn some(kind: &str, text: &str) -> String {
    zh(kind, text).unwrap_or_else(|| panic!("{kind}: {text:?} not worded"))
}

#[test]
fn holes_take_the_shortest_run_and_are_never_empty() {
    assert_eq!(
        caps("{} failing {}m", "chill failing 5m"),
        Some(vec!["chill", "5"])
    );
    assert_eq!(caps("{} failing {}m", " failing 5m"), None);
    assert_eq!(caps("{} failing {}m", "chill failing m"), None);
    assert_eq!(caps("a {}", "a "), None);
    assert_eq!(
        caps("x {} y", "x 1 y z"),
        None,
        "the last literal ends the text"
    );
    assert_eq!(caps("{} {}", "a b c"), Some(vec!["a", "b c"]));
}

/// Every shape a producer writes, as the queue keeps it (see the producers' file:line in each
/// kind's arm). A producer that changes its English falls back to the raw text here first.
#[test]
fn every_known_shape_reads_in_chinese() {
    let cases: &[(&str, &str, &str)] = &[
        (
            "core-crash",
            "luyaz-core exited, status unknown",
            "luyaz-core：退出原因不明",
        ),
        (
            "web-crash",
            "luyaz-web killed by SIGKILL",
            "luyaz-web：被 SIGKILL 结束",
        ),
        (
            "sentinel-crash",
            "luyaz-sentinel exit 101",
            "luyaz-sentinel：退出码 101",
        ),
        (
            "ui-crash",
            "[3f2a1b0c-12] luyaz-ui killed by signal 6",
            "luyaz-ui：被信号 6 结束",
        ),
        (
            "ui-gave-up",
            "3 launches did not stay up; vendor UI on screen",
            "连续 3 次启动都没撑住，屏幕换回原厂界面",
        ),
        (
            "screen-hold-expired",
            "screen held for ship past its deadline; sentinel took it back",
            "",
        ),
        (
            "screen-two-uis",
            "vendor UI started next to ours; ours stopped",
            "",
        ),
        (
            "ui-theme-paused",
            "automatic light/dark switching paused until reboot (3 switches within an hour)",
            "",
        ),
        (
            "wifi-takeover",
            "agent gone (no heartbeat for 95s), Wi-Fi was off; turning it on",
            "数据服务失联（95 秒没有心跳），Wi-Fi 原本关着，已重新打开",
        ),
        (
            "wifi-takeover",
            "agent gone (heartbeat file gone), Wi-Fi was off; turning it on",
            "数据服务失联（心跳文件不见了），Wi-Fi 原本关着，已重新打开",
        ),
        (
            "agent-silent",
            "no heartbeat since boot",
            "开机后一直没有心跳",
        ),
        (
            "wifi-restore-failed",
            "could not turn Wi-Fi on; retrying",
            "",
        ),
        (
            "dropbear-down",
            "SSH (:2222) keeps going away; restart limit reached",
            "",
        ),
        (
            "dropbear-restarted",
            "SSH (:2222) was not listening; started it again",
            "",
        ),
        (
            "sentinel-silent",
            "luyaz-sentinel heartbeat 180s old",
            "哨兵心跳已 180 秒没更新",
        ),
        (
            "screen-empty",
            "no UI on screen for 60s",
            "屏幕上 60 秒没有界面",
        ),
        (
            "sms-failed",
            "alert 90 (sim-changed) not sent",
            "第 90 条没发出去：SIM 卡换了（卡号/IMSI 变了）",
        ),
        (
            "sms-held",
            "alert SMS held 10 min: the device clock stepped and is not trusted yet",
            "已暂缓 10 分钟，等设备时间重新可信",
        ),
        (
            "home-exit-down",
            "home exit silent 15 min; Rest via home turned off",
            "家里出口 15 分钟没回应，「其余都走家里」已关",
        ),
        (
            "home-exit-check-failed",
            "check failed (home_no_answer); turned back off",
            "自检没过（家里 Mac 没回应），已关回去",
        ),
        (
            "lan-new-device",
            "[02005e00-21250] iPhone 02:00:5e:00:53:02",
            "\u{0}",
        ),
        (
            "sim-changed",
            "[822fee2e-11] SIM identity changed (epoch 1)",
            "",
        ),
        (
            "sim-stalled",
            "[822fee2e-12] SIM change stuck without data (epoch 1)",
            "",
        ),
        (
            "esim-failed",
            "[822fee2e-13] eSIM switch failed: profile did not come up",
            "原因：profile did not come up",
        ),
        (
            "esim-rebooted",
            "Rebooted automatically to finish an eSIM switch",
            "",
        ),
        (
            "wifi-post-stuck",
            "zte_post_wifi.sh alive 300s, enabled 2 vs hostapd_cli 1",
            "原厂 Wi-Fi 脚本已跑 300 秒；配置里开着 2 个，hostapd_cli 看到 1 个",
        ),
        (
            "no-service-long",
            "no service 320s, no stock recovery seen",
            "无服务 320 秒，原厂没有自己恢复",
        ),
        (
            "ops-unsaved",
            "wifi: the change went ahead; its rollback record could not be saved (op-12)",
            "wifi：改动已生效，但回滚记录没存下（op-12）",
        ),
        (
            "wifi-5g-down",
            "5 GHz Wi-Fi is still down 5 minutes later",
            "",
        ),
        (
            "wifi-5g-fallback",
            "5 GHz Wi-Fi stopped; moved to channel 36 at 80 MHz",
            "5 GHz Wi-Fi 停了，已换到 36 信道（80 MHz）",
        ),
        (
            "chill-direct-kept",
            "CHILL is back on but still All direct (since 11-10 09:12, by web); switch to Proxy on the CHILL page if not",
            "CHILL 重新打开后仍是全部直连（从 11-10 09:12 起，由 web 改的）；不是有意的话，到 CHILL 页切回代理",
        ),
        (
            "chill-direct-kept",
            "CHILL is back on but still All direct (since ?); switch to Proxy",
            "CHILL 重新打开后仍是全部直连（从 ? 起）；不是有意的话，到 CHILL 页切回代理",
        ),
        (
            "ship-auto-rollback",
            "crash loop after core ship: rolledback",
            "换上 core 后反复崩溃：已退回旧版",
        ),
        (
            "ship-auto-rollback",
            "reboot loop goes on after 2 rollbacks: needs a person",
            "退回 2 次后仍在重启循环，需要人来处理",
        ),
        (
            "ship-auto-rollback",
            "reboot loop after touch ship: rollback incomplete",
            "换上 touch 后反复重启：没退回完",
        ),
        (
            "ship-auto-rollback",
            "reboot loop after touch ship: back to the old version",
            "换上 touch 后反复重启：已退回旧版",
        ),
        (
            "ship-conn-rollback",
            "conn lost: ssh after lweb ship: rolledback",
            "换上 lweb 后连不上（ssh）：已退回旧版",
        ),
        (
            "ship-prev-damaged",
            "rollback copy damaged after lweb ship 20261105-000001-lweb: /data/luyaz/ota07t/x",
            "换 lweb（20261105-000001-lweb）后，回滚用的旧版副本损坏：/data/luyaz/ota07t/x",
        ),
        (
            "tailscale-gave-up",
            "tailscaled died 5 times; restarts paused for up to 1800s",
            "tailscaled 退出了 5 次，暂停重启最多 1800 秒",
        ),
        (
            "sms-forward-stuck",
            "SMS 7 not forwarded to rule 2, given up: timeout",
            "短信 7 没能按规则 2 转发，已放弃：timeout",
        ),
        (
            "heat-protect",
            "[34d373b7-9899371] battery 45 C, chip 54 C; charging paused",
            "电池 45 °C、芯片 54 °C：暂停充电，凉下来自动恢复",
        ),
        (
            "tailnet-poller",
            "[34d373b7-1] polls :9090 over Tailscale for 10 min, ~124 pkts/min: 100.64.0.2 (mini)",
            "100.64.0.2（mini） 经 Tailscale 持续访问 :9090，已 10 分钟（约 124 包/分钟），息屏约多耗 0.3–0.6 W",
        ),
        // regress (open, close, escalation, the 3-day question)
        (
            "regress-egress",
            "[rg-egress-chill-1794679637-open] chill failing 5m",
            "CHILL 不通已 5 分钟",
        ),
        (
            "regress-egress",
            "[rg-egress-chill-1794679637-close] chill through 3 checks",
            "已恢复：CHILL 连续 3 次检查都通了",
        ),
        (
            "regress-egress",
            "[rg-egress-tailscale-1-open] tailscale failing 12m",
            "Tailscale 不通已 12 分钟",
        ),
        ("regress-cellular", "no service 6m", "无服务 6 分钟"),
        (
            "regress-cellular",
            "registered, no data route 5m",
            "已注册但没有数据路由，5 分钟",
        ),
        (
            "regress-cellular",
            "registered with a data route 3 checks",
            "已注册且有数据路由，连续 3 次检查",
        ),
        (
            "regress-core-down",
            "[rg-core-down-14db6353-25793-open] /healthz no answer: refused for 10 min; attribution unknown",
            "/healthz 连不上（拒绝连接），已 10 分钟，原因不明",
        ),
        (
            "regress-core-down",
            "/healthz no answer: timed out for 12 min; attribution unknown",
            "/healthz 没回应（timed out），已 12 分钟，原因不明",
        ),
        (
            "regress-core-down",
            "/healthz 503 busy for 10 min; attribution unknown",
            "/healthz 回的是 503 busy，已 10 分钟，原因不明",
        ),
        (
            "regress-core-down",
            "core healthy again",
            "数据服务恢复正常",
        ),
        (
            "regress-core-down",
            "core healthy again after 1 sentinel restart(s)",
            "哨兵重启它 1 次后，数据服务恢复正常",
        ),
        (
            "regress-recorder",
            "long/open.json not written for 30 min",
            "long/open.json 已 30 分钟没写",
        ),
        (
            "regress-recorder",
            "open.json written again",
            "open.json 又开始写了",
        ),
        (
            "regress-disk",
            "/data 12/400 MB, 900/25000 inodes free; /data/x 80M",
            "/data 剩 12/400 MB，inode 剩 900/25000；最大的：/data/x 80M",
        ),
        (
            "regress-disk",
            "/data 50/400 MB, 900/25000 inodes free again",
            "恢复到：/data 剩 50/400 MB，inode 剩 900/25000",
        ),
        (
            "regress-mem-abs",
            "heap 90MiB > 80MiB 15m; anon 40MiB",
            "堆内存 90MiB 超过上限 80MiB 已 15 分钟；匿名内存 40MiB",
        ),
        (
            "regress-mem-abs",
            "anon 130MiB > 120MiB 30m; heap 30MiB",
            "匿名内存 130MiB 超过上限 120MiB 已 30 分钟；堆内存 30MiB",
        ),
        (
            "regress-mem-abs",
            "heap 40MiB anon 50MiB under 80% 30m",
            "堆内存 40MiB、匿名内存 50MiB，低于上限的 80% 已 30 分钟",
        ),
        (
            "regress-mem",
            "heap 30MiB > 1.5x base 18MiB 15m",
            "堆内存 30MiB 超过平时 18MiB 的 1.5 倍，已 15 分钟",
        ),
        (
            "regress-mem",
            "anon 80MiB > 2x base 35MiB 30m",
            "匿名内存 80MiB 超过平时 35MiB 的 2 倍，已 30 分钟",
        ),
        (
            "regress-mem",
            "heap 20MiB anon 30MiB back under 80% 30m",
            "堆内存 20MiB、匿名内存 30MiB，回到 80% 以下已 30 分钟",
        ),
        (
            "regress-mem",
            "open 3d; new normal? build 1.98.0>1.98.4 at 31MiB; regress accept regr",
            "已经 3 天，可能是新常态（版本 1.98.0>1.98.4，现在 31MiB）；确认的话运行 regress accept regress-mem",
        ),
        (
            "regress-power",
            "open 3d; new normal? build 1.98.4 at 640mW; regress accept regress-power",
            "已经 3 天，可能是新常态（版本 1.98.4，现在 640mW）；确认的话运行 regress accept regress-power",
        ),
        ("regress-heat", "heat care critical", "温控进入最高档"),
        (
            "regress-heat",
            "heat care out of critical",
            "温控离开最高档",
        ),
        (
            "regress-blind",
            "power could not be judged for 3 days",
            "功耗已经 3 天没法判断",
        ),
        (
            "regress-blind",
            "egress can be judged again",
            "出口又能判断了",
        ),
        (
            "regress-power-abs",
            "home standby 900mW > 800mW 2h",
            "国内待机 900 mW，超过上限 800 mW 已 2 小时",
        ),
        (
            "regress-power-abs",
            "abroad standby 600mW 2h",
            "国外待机回到 600 mW，已 2 小时",
        ),
        (
            "regress-power",
            "home standby 700mW > 600mW (base 450) 2h",
            "国内待机 700 mW，超过 600 mW（平时 450 mW）已 2 小时",
        ),
        (
            "regress-reboot",
            "code 1185; prev build 1.98.3; ship core ",
            "原因码 1185；之前的版本 1.98.3；之前刚换过 core",
        ),
        (
            "regress-reboot",
            "code unknown; ship touch before reboot?",
            "原因码未知；重启前可能刚换过 touch",
        ),
        (
            "regress-reboot",
            "#2 code 1134; prev build 1.98",
            "第 2 次：原因码 1134；之前的版本 1.98",
        ),
    ];
    for (kind, text, want) in cases {
        if *want == "\u{0}" {
            assert_eq!(zh(kind, text), None, "{kind}: {text:?} stays as stored");
        } else {
            assert_eq!(&some(kind, text), want, "{kind}: {text:?}");
        }
    }
}

/// core 1.98.4 and older wrote these four in Chinese; the queue kept only the ASCII scraps.
#[test]
fn scraps_of_the_old_chinese_texts_are_rebuilt_or_hidden() {
    assert_eq!(
        some("heat-protect", "[34d373b7-9899371]  45 C 54 C"),
        "电池 45 °C、芯片 54 °C"
    );
    assert_eq!(
        some("heat-protect", "[34d373b7-9899372]  45 C 54 CCHILL "),
        "电池 45 °C、芯片 54 °C"
    );
    assert_eq!(
        some(
            "tailnet-poller",
            "[34d373b7-179467873700] 192.168.50.88  Tailscale  :9090 10  124 / 0.30.6 W"
        ),
        "192.168.50.88 经 Tailscale 持续访问 :9090，已 10 分钟，息屏约多耗 0.3–0.6 W"
    );
    assert_eq!(some("home-exit-down", " 15 "), "");
    assert_eq!(some("home-exit-check-failed", "Mac "), "");
    assert_eq!(some("home-exit-check-failed", "Tailscale"), "");
}

#[test]
fn unknown_shapes_and_kinds_stay_as_stored() {
    assert_eq!(zh("regress-egress", "chill flapping"), None);
    assert_eq!(zh("something-new", "whatever it says"), None);
    assert_eq!(zh("lan-new-device", "aa:bb:cc:dd:ee:ff"), None);
    assert_eq!(
        zh("core-crash", "[3f2a1b0c-12] "),
        Some(String::new()),
        "nothing after the id"
    );
}

/// A detail that only repeats its label adds nothing.
#[test]
fn a_detail_that_is_its_label_is_hidden() {
    assert_eq!(
        zh(
            "esim-rebooted",
            crate::kind_label_en("esim-rebooted").as_str()
        ),
        Some(String::new())
    );
}
