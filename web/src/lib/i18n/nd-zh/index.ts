// Chinese strings added while migrating pages to the new design, one file
// per page. They are deep-merged over zh.ts (a key here wins), so each page
// can add keys without rewriting a namespace that zh.ts or zh-pages.ts
// already fills.
import { zh as clients } from "./clients";
import { zh as homeMode } from "./home-mode";
import { zh as login } from "./login";
import { zh as scenario } from "./scenario";
import { zh as signalDetect } from "./signal-detect";
import { zh as speedtest } from "./speedtest";
import { zh as tailscale } from "./tailscale";
import { zh as mobileNetwork } from "./mobile-network";
import { zh as networkMode } from "./network-mode";
import { zh as qci } from "./qci";
import { zh as celllock } from "./celllock";
import { zh as dns } from "./dns";
import { zh as firewall } from "./firewall";
import { zh as qos } from "./qos";
import { zh as apn } from "./apn";
import { zh as sim } from "./sim";
import { zh as esim } from "./esim";
import { zh as stc } from "./stc";
import { zh as wifi } from "./wifi";
import { zh as wifiGuest } from "./wifi-guest";
import { zh as lan } from "./lan";
import { zh as vpn } from "./vpn";
import { zh as smsList } from "./sms";
import { zh as smsCompose } from "./sms-compose";
import { zh as smsForward } from "./sms-forward";
import { zh as stk } from "./stk";
import { zh as deviceInfo } from "./device-info";
import { zh as health } from "./health";
import { zh as alerts } from "./alerts";
import { zh as settings } from "./settings";
import { zh as device } from "./device";
import { zh as schedule } from "./schedule";
import { zh as scheduler } from "./scheduler";
import { zh as usb } from "./usb";
import { zh as telemetry } from "./telemetry";
import { zh as at } from "./at";
import { zh as processes } from "./processes";
import { zh as enableAdb } from "./enable-adb";
import { zh as config } from "./config";
import { zh as netinfo } from "./netinfo";

type Dict = { [k: string]: string | Dict };

export const ND_ZH: Dict[] = [clients, homeMode, login, scenario, signalDetect, speedtest, tailscale, mobileNetwork, networkMode, qci, celllock, dns, firewall, qos, apn, sim, esim, stc, wifi, wifiGuest, lan, vpn, smsList, smsCompose, smsForward, stk, deviceInfo, health, alerts, settings, device, schedule, scheduler, usb, telemetry, at, processes, enableAdb, config, netinfo] as unknown as Dict[];

export function deepMerge(base: Dict, ...layers: Dict[]): Dict {
  const out: Dict = { ...base };
  for (const layer of layers) {
    for (const [k, v] of Object.entries(layer)) {
      const cur = out[k];
      out[k] = typeof v === "object" && typeof cur === "object" ? deepMerge(cur, v) : v;
    }
  }
  return out;
}
