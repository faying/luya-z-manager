// Call status, USSD and SIM Toolkit endpoints (telephony.rs). The real agent
// drives all of these with AT commands (fixed sleeps; see the schema file);
// the mock keeps short delays so loading states are visible.
//
// Persona: China Mobile home SIM roaming in Taiwan. USSD replies are UCS2
// (DCS 72) Chinese text; STK menu is a plausible China Mobile one.

import type { Ctx, Reply, Route } from "../lib.ts";
import { ok, fail, bodyField } from "../lib.ts";
import { shared } from "../shared.ts";
import type {
  CallStatus,
  UssdResponse,
  StkItem,
  StkMenu,
  StkSelectResult,
} from "../../../src/lib/api/schemas/telephony.ts";

const USSD_DELAY = 1500;
const STK_DELAY = 1200;

function needJson(ctx: Ctx): Reply | null {
  return ctx.body === undefined ? fail("invalid JSON", 400) : null;
}

/** Keep digits and `*#+` like the agent (telephony.rs:393, 428). */
function cleanCode(v: unknown): string {
  return typeof v === "string" ? [...v].filter((c) => /[0-9*#+]/.test(c)).join("") : "";
}

function ucs2Hex(s: string): string {
  let out = "";
  for (const ch of s) {
    const cp = ch.codePointAt(0) ?? 0;
    out += (cp > 0xffff ? 0x3f : cp).toString(16).toUpperCase().padStart(4, "0");
  }
  return out;
}

function ussdReply(text: string, status: 0 | 1 | 2): UssdResponse {
  return { response: text, raw_response: ucs2Hex(text), status, dcs: 72, session_active: status === 1 };
}

/** The no-+CUSD fallback: AT reply with control chars removed, status -1 (telephony.rs:402-410, 433-441). */
function ussdFallback(atLine: string, reply: string): UssdResponse {
  const text = `${atLine}${reply}`;
  return { response: text, raw_response: text, status: -1, dcs: 15, session_active: false };
}

const MENU_TEXT =
  "尊敬的客户，您的话费余额为86.50元。回复1查询国际漫游流量，回复2查询当前套餐，回复0退出。";

const ussd = { active: false };

const STK_ROOT: StkMenu = {
  supported: true,
  title: "中国移动",
  items: [
    { id: 1, label: "国际漫游" },
    { id: 2, label: "话费查询" },
    { id: 3, label: "移动服务" },
  ],
  source: "at_cusatd",
} satisfies StkMenu;

const STK_ROAMING: StkItem[] = [
  { id: 11, label: "漫游资费" },
  { id: 12, label: "流量包订购" },
];

const STK_DISPLAY: Record<number, string> = {
  2: "+CUSATE: 0\"您的话费余额为86.50元\"OK",
  3: "+CUSATE: 0OK",
  11: "+CUSATE: 0\"台湾地区：流量 1GB/天\"OK",
  12: "+CUSATE: 0\"已发送订购短信，请留意回复\"OK",
};

export const routes: Route[] = [
  {
    method: "GET",
    path: "/api/call/status",
    handler: () => ({ data: { calls: [] } satisfies CallStatus, delayMs: 300 }),
  },
  {
    method: "POST",
    path: "/api/ussd/send",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const raw = bodyField(ctx.body, "code");
      if (typeof raw !== "string" || raw === "") return fail("missing code", 400);
      const code = cleanCode(raw);
      // Modem answers ERROR → 500 "USSD failed" (telephony.rs:397-398).
      if (code === "" || shared.airplane || ctx.has("nosignal")) {
        return { status: 500, error: "USSD failed", delayMs: USSD_DELAY };
      }
      ussd.active = true;
      return { data: ussdReply(MENU_TEXT, 1), delayMs: USSD_DELAY };
    },
  },
  {
    method: "POST",
    path: "/api/ussd/respond",
    handler: (ctx) => {
      const bad = needJson(ctx);
      if (bad) return bad;
      const raw = bodyField(ctx.body, "reply");
      if (typeof raw !== "string" || raw === "") return fail("missing reply", 400);
      const reply = cleanCode(raw);
      const atLine = `AT+CUSD=1,"${reply}",15`;
      // respond never checks ERROR (telephony.rs:430-441): no session / radio
      // off / fakesuccess all come back 200 ok with "ERROR" as the text.
      if (!ussd.active || shared.airplane || ctx.has("nosignal") || ctx.has("fakesuccess")) {
        ussd.active = false;
        return { data: ussdFallback(atLine, "ERROR"), delayMs: USSD_DELAY };
      }
      let data: UssdResponse;
      if (reply === "1") data = ussdReply("本月国际漫游流量已使用1.2GB，剩余2.8GB，有效期至2026年9月30日。", 0);
      else if (reply === "2") data = ussdReply("您当前的套餐为：全球通畅享套餐，月费128元，含国内流量40GB。", 0);
      else if (reply === "0") data = ussdReply("感谢使用，再见。", 2);
      else data = ussdReply(`输入有误。${MENU_TEXT}`, 1);
      ussd.active = data.session_active;
      return { data, delayMs: USSD_DELAY };
    },
  },
  {
    method: "POST",
    path: "/api/ussd/cancel",
    handler: () => {
      ussd.active = false;
      return { data: { status: "ok" }, delayMs: 300 };
    },
  },
  {
    // Real agent: AT+CUAD (3.3 s) + AT+CUSATD=1 (5.3 s) ≈ 8.6 s, past the
    // client's 8 s GET timeout. The mock answers quickly.
    method: "GET",
    path: "/api/stk/menu",
    handler: () => ({ data: structuredClone(STK_ROOT), delayMs: STK_DELAY }),
  },
  {
    method: "POST",
    path: "/api/stk/select",
    handler: (ctx) => {
      // STK support is checked before the body is parsed (telephony.rs:525-531);
      // this SIM supports STK, so body validation follows.
      const bad = needJson(ctx);
      if (bad) return bad;
      const id = bodyField(ctx.body, "item_id");
      if (typeof id !== "number" || !Number.isInteger(id) || id < 0) return fail("missing item_id", 400);
      const itemId = id & 0xff; // `as u8`
      let data: StkSelectResult;
      if (itemId === 1) {
        data = { type: "menu", title: "国际漫游", items: structuredClone(STK_ROAMING), source: "at_cusate" };
      } else if (STK_DISPLAY[itemId] !== undefined) {
        data = { type: "display", data: STK_DISPLAY[itemId] };
      } else {
        return { status: 500, error: "item selection failed", delayMs: STK_DELAY };
      }
      return { data, delayMs: STK_DELAY };
    },
  },
];
