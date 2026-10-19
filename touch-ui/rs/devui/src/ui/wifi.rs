//! Wi-Fi (src/ui_parts/wifi.c): the hotspot, its switches, the connected devices and DHCP.
//! The list and the values are drawn by refresh_wifi (cellular.rs, as in C).
// SPDX-License-Identifier: MIT

use super::cellular::{nz, toggle_row, WIFI_CLI_H, WIFI_MAX_CLI};
use super::network::NH_WIFI;
use super::{Ui, VIEW_H};
use crate::kit::{self, CARD_W, MARGIN, PAD, ROW_H, W};
use crate::lang::{n_, tr};
use crate::lv::{self, sys, Obj};
use crate::theme::{t, uf};

const WSW_MASTER: usize = 0;
const WSW_24: usize = 1;
const WSW_5: usize = 2;
const WSW_PSM: usize = 3;
const WSW_NFC: usize = 4;

#[derive(Default, Clone)]
pub struct Wifi {
    pub scroll: Obj,
    pub ssid: Obj,
    pub pass: Obj,
    pub enc: Obj,
    pub state: Obj,
    pub sw: [Obj; 5],
    pub sw_st: [Obj; 5],
    pub cli_sec: Obj,
    pub cli_n: Obj,
    pub cli_card: Obj,
    pub cli_empty: Obj,
    pub cli: [Obj; WIFI_MAX_CLI],
    pub cli_sep: [Obj; WIFI_MAX_CLI],
    pub cli_name: [Obj; WIFI_MAX_CLI],
    pub cli_mac: [Obj; WIFI_MAX_CLI],
    pub cli_ip: [Obj; WIFI_MAX_CLI],
    /// 连上以来的总流量 (netinfo)
    pub cli_tot: [Obj; WIFI_MAX_CLI],
    pub dhcp_sec: Obj,
    pub dhcp_card: Obj,
    pub gw: Obj,
    pub pool: Obj,
    pub lease: Obj,
}

unsafe extern "C" fn wifi_sw_cb(e: *mut sys::lv_event_t) {
    // SAFETY: LVGL's event
    let (id, sw) = unsafe { (lv::event_user(e), lv::event_target(e)) };
    super::with(|u| u.wifi_sw(id, sw.has_state(lv::LV_STATE_CHECKED)));
}

impl Ui {
    fn wifi_sw(&mut self, id: usize, on: bool) {
        let now = lv::tick_get();
        match id {
            WSW_MASTER | WSW_24 | WSW_5 => {
                // the same lasting switch as the web page (E4): the main AP interfaces' uci
                // `disabled`, written and reloaded by core (wifi.apply)
                let w24 = if id == WSW_5 { self.aux.w24 == 1 } else { on };
                let w5 = if id == WSW_24 { self.aux.w5 == 1 } else { on };
                self.aux.w24 = w24 as i32;
                self.aux.w5 = w5 as i32;
                self.aux.hold_w24 = nz(now);
                self.aux.hold_w5 = nz(now);
                // both off: the guests too — one left on alone is a layout the firmware cannot run,
                // and the core refuses it (luyaz_wifi::layout); best_effort: a guest section may be
                // missing. 2.4 GHz off with 5 GHz on is refused by the core; the switch comes back.
                let guests = if !w24 && !w5 {
                    ",\"wireless.guest_2g.disabled\":\"1\",\"wireless.guest_5g.disabled\":\"1\""
                } else {
                    ""
                };
                let params = format!(
                    "{{\"set\":{{\"wireless.main_2g.disabled\":\"{}\",\"wireless.main_5g.disabled\":\"{}\"{guests}}},\"reload\":true,\"best_effort\":{}}}",
                    !w24 as i32, !w5 as i32, !w24 && !w5
                );
                // the emergency script calls this write wifi.radio
                self.data_control("wifi.apply", &params, Some(&format!("ap_2g={} ap_5g={}", w24 as i32, w5 as i32)));
            }
            WSW_PSM => {
                // through core (E4): the hotplug script keeps it across an ifup
                self.aux.psm = on as i32;
                self.data_control("wifi.power_save", &format!("{{\"enabled\":{}}}", on as i32), None);
            }
            WSW_NFC => {
                let fb = format!("enabled={} flag=2", on as i32);
                self.data_control("nfc.set", &format!("{{\"enabled\":{},\"flag\":2}}", on as i32), Some(&fb));
            }
            _ => {}
        }
    }

    #[allow(clippy::needless_range_loop)]
    pub(super) fn build_wifi(&mut self, tile: Obj) {
        const SW_NAME: [&str; 5] = [n_("WiFi 总开关"), n_("2.4G 频段"), n_("5G 频段"), n_("节能模式"), n_("NFC 碰一碰")];
        let th = t();
        let f = uf();
        let mut y = 4;
        let sc = kit::scroll(tile, 0, VIEW_H, 1000);
        let w = &mut self.wifi;
        w.scroll = sc;

        kit::section(sc, y, tr("热点"));
        y += 20;
        let ap = kit::card(sc, MARGIN, y, CARD_W, 3 * ROW_H);
        w.ssid = kit::label_w(ap, f.cj17b, th.t1, PAD, 10, 200, false, "");
        w.state = kit::label_r(ap, f.cj13, th.ok_t, CARD_W - PAD, 12, "");
        w.enc = kit::row(ap, ROW_H, tr("加密"), false);
        w.pass = kit::row(ap, 2 * ROW_H, tr("密码"), false);
        y += 3 * ROW_H + 10;

        kit::section(sc, y, tr("开关"));
        y += 20;
        let sws = kit::card(sc, MARGIN, y, CARD_W, 5 * ROW_H);
        for i in 0..5 {
            w.sw[i] = toggle_row(sws, i as i32 * ROW_H, tr(SW_NAME[i]), i == 0, &mut w.sw_st[i], Some(wifi_sw_cb), i);
        }
        y += 5 * ROW_H + 10;

        w.cli_sec = kit::section(sc, y, tr("已连接设备"));
        w.cli_n = kit::label_r(sc, f.cj12, th.t3, W - MARGIN - 6, y, "");
        y += 20;
        let cc = kit::card(sc, MARGIN, y, CARD_W, WIFI_MAX_CLI as i32 * WIFI_CLI_H);
        w.cli_card = cc;
        w.cli_empty = kit::label(cc, f.cj14, th.t3, PAD, 11, tr("还没有设备连上"));
        for i in 0..WIFI_MAX_CLI {
            let ry = i as i32 * WIFI_CLI_H;
            let r = kit::bx(cc, 0, ry, CARD_W, WIFI_CLI_H, th.card, 0);
            r.set_style_bg_opa(lv::OPA_TRANSP, 0);
            w.cli_sep[i] = if i > 0 { kit::sep(r, 0) } else { Obj::NULL };
            w.cli_name[i] = kit::label_w(r, f.cj14, th.t1, PAD, 8, 160, false, "");
            w.cli_mac[i] = kit::label_w(r, f.cj12, th.t3, PAD, 29, CARD_W - 2 * PAD, false, "");
            w.cli_ip[i] = kit::label_r(r, f.n15, th.t1, CARD_W - PAD, 7, "");
            w.cli_tot[i] = kit::label(r, f.cj12, th.t3, PAD, 47, "");
            kit::show(r, false);
            w.cli[i] = r;
        }
        y += WIFI_MAX_CLI as i32 * WIFI_CLI_H + 10;

        w.dhcp_sec = kit::section(sc, y, "DHCP");
        y += 20;
        let dc = kit::card(sc, MARGIN, y, CARD_W, 3 * ROW_H);
        w.dhcp_card = dc;
        w.gw = kit::row(dc, 0, tr("网关"), true);
        w.pool = kit::row(dc, ROW_H, tr("地址池"), false);
        w.lease = kit::row(dc, 2 * ROW_H, tr("租期"), false);
        self.net.nh_scroll[NH_WIFI] = sc;
        // the page ends here (net_reflow sets the scroll range from it)
        self.net.nh_base[NH_WIFI] = y + 3 * ROW_H + 10;
        sc.scroll_to_y(0);
    }
}
