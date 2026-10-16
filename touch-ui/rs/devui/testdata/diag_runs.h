/*
 * diag_runs.h - zte-agent diagnosis runs (GET /api/diagnose "data") in the
 * shape deep_diag.rs sends them, texts as its judge_* functions write them.
 * Used by the render fixtures (diagnose-* scenes) and tests/diag_view_test.c.
 *
 * SPDX-License-Identifier: MIT
 */
#ifndef RT_DIAG_RUNS_H
#define RT_DIAG_RUNS_H

/* 3 of 6 done: Wi-Fi, signal, cap filled; the link probe going */
static const char k_diag_running[] =
    "{\"id\":7,\"state\":\"running\",\"asked_at\":1790250100,\"started_at\":1790250101,\"finished_at\":null,"
    "\"step\":3,\"steps\":5,\"layers\":["
    "{\"id\":\"wifi\",\"level\":\"ok\",\"detail\":\"最差：iPad · -52 dBm · 433 Mbps\",\"detail_en\":\"Worst: iPad · -52 dBm · 433 Mbps\",\"counted\":true},"
    "{\"id\":\"signal\",\"level\":\"ok\",\"detail\":\"信号强 · 干扰小 · RSRP -87 · SINR 17.7\",\"detail_en\":\"Signal strong · noise low · RSRP -87 · SINR 17.7\",\"counted\":true},"
    "{\"id\":\"limit\",\"level\":\"ok\",\"detail\":\"没有限速 · QCI 9\",\"detail_en\":\"No cap · QCI 9\",\"counted\":true},"
    "{\"id\":\"link\",\"level\":\"running\",\"detail\":\"\",\"detail_en\":\"\",\"counted\":true},"
    "{\"id\":\"crowd\",\"level\":\"pending\",\"detail\":\"\",\"detail_en\":\"\",\"counted\":true}"
    "],\"main\":null,\"key\":{\"plmn\":\"46011\",\"cell\":23960174596,\"ch\":627264,\"hour\":10},"
    "\"from\":\"touch\",\"feedback\":null,\"speed\":null,\"age_s\":null}";

/* done: cell load the main cause, the link a second warn, two rows can't
 * tell (one of them not counted), a speed row */
static const char k_diag_result[] =
    "{\"id\":7,\"state\":\"done\",\"asked_at\":1790249500,\"started_at\":1790249501,\"finished_at\":1790249511,"
    "\"step\":4,\"steps\":4,\"layers\":["
    "{\"id\":\"wifi\",\"level\":\"na\",\"detail\":\"没有设备连着\",\"detail_en\":\"No devices on Wi-Fi\",\"counted\":false},"
    "{\"id\":\"signal\",\"level\":\"ok\",\"detail\":\"信号强 · 干扰小 · RSRP -88 · SINR 8.0\",\"detail_en\":\"Signal strong · noise low · RSRP -88 · SINR 8.0\",\"counted\":true},"
    "{\"id\":\"limit\",\"level\":\"na\",\"detail\":\"QoS 读不到\",\"detail_en\":\"QoS unavailable\",\"counted\":true},"
    "{\"id\":\"link\",\"level\":\"warn\",\"detail\":\"延迟 210 ms · 丢包 0/10\",\"detail_en\":\"210 ms · lost 0/10\",\"counted\":true},"
    "{\"id\":\"crowd\",\"level\":\"warn\",\"detail\":\"疑似拥挤 · RSRQ -18 · 延迟是平时的 3.0 倍\",\"detail_en\":\"Likely busy · RSRQ -18 · latency 3.0× usual\",\"counted\":true}"
    "],\"main\":{\"layer\":\"link\",\"level\":\"warn\",\"text\":\"蜂窝链路不稳\",\"text_en\":\"Cellular link unstable\","
    "\"action\":\"过几分钟再试，或换个地方\",\"action_en\":\"Try again in a few minutes, or move\",\"action_to\":\"\",\"more\":1},"
    "\"key\":{\"plmn\":\"46011\",\"cell\":23960174596,\"ch\":627264,\"hour\":10},"
    "\"from\":\"touch\",\"feedback\":null,"
    "\"speed\":{\"id\":\"speed\",\"level\":\"info\",\"detail\":\"直连 ↓ 86 Mbps\",\"detail_en\":\"Direct ↓ 86 Mbps\",\"counted\":true},"
    "\"age_s\":12}";

/* done: weak signal, the action leads to Placement */
static const char k_diag_weak[] =
    "{\"id\":8,\"state\":\"done\",\"asked_at\":1790249500,\"started_at\":1790249501,\"finished_at\":1790249511,"
    "\"step\":5,\"steps\":5,\"layers\":["
    "{\"id\":\"wifi\",\"level\":\"ok\",\"detail\":\"iPad · -48 dBm · 866 Mbps\",\"detail_en\":\"iPad · -48 dBm · 866 Mbps\",\"counted\":true},"
    "{\"id\":\"signal\",\"level\":\"bad\",\"detail\":\"信号弱 · RSRP -112 · SINR -2.5\",\"detail_en\":\"Signal weak · RSRP -112 · SINR -2.5\",\"counted\":true},"
    "{\"id\":\"limit\",\"level\":\"ok\",\"detail\":\"没有限速 · QCI 9\",\"detail_en\":\"No cap · QCI 9\",\"counted\":true},"
    "{\"id\":\"link\",\"level\":\"ok\",\"detail\":\"延迟 48 ms · 丢包 0/10\",\"detail_en\":\"48 ms · lost 0/10\",\"counted\":true},"
    "{\"id\":\"crowd\",\"level\":\"na\",\"detail\":\"历史不够\",\"detail_en\":\"Not enough history\",\"counted\":true}"
    "],\"main\":{\"layer\":\"signal\",\"level\":\"bad\",\"text\":\"信号弱\",\"text_en\":\"Weak signal\","
    "\"action\":\"固定位置时用摆放模式；在路上只能等\",\"action_en\":\"Use Placement if you're staying put; on the move, wait\","
    "\"action_to\":\"placement\",\"more\":0},"
    "\"key\":{\"plmn\":\"46011\",\"cell\":null,\"ch\":null,\"hour\":10},"
    "\"from\":\"touch\",\"feedback\":true,\"speed\":null,\"age_s\":30}";

#endif
