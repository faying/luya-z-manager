// Pure helpers for the Diagnose page (/tools/diagnose). Same content and
// states as the touch screen's 网络诊断 page (docs/designs/slow-diagnosis.md
// §12.3, §12.5, §12.8 decision 11A); the agent (deep_diag.rs) does the
// judging and words the details in both languages, this only maps them to
// symbols, words and tones.
import type { Tone } from "@/components/nd/tone";
import type { DiagGet, DiagLayer, DiagLevel, DiagMain, DiagRun } from "@/lib/api/schemas/tools";
import type { Lang } from "@/lib/i18n/config";
import { pick } from "@/lib/i18n/pick";
import type { T } from "@/lib/signalWords";

/** Layer order of the result card (deep_diag.rs `Run::new`; proxy only while the proxy service runs). */
export const LAYER_ORDER = ["wifi", "signal", "limit", "link", "crowd", "proxy"] as const;

/** A finished run is shown again for this long (deep_diag.rs KEEP_SECS). */
export const KEEP_SECS = 600;

/** Row names (glossary §10; the touch screen uses the same words). */
export function layerName(t: T, id: string): string {
  switch (id) {
    case "wifi": return "Wi-Fi";
    case "signal": return t("diagnose.layer.signal", "Signal");
    case "limit": return t("diagnose.layer.limit", "Speed cap");
    case "link": return t("diagnose.layer.link", "Cellular link");
    case "crowd": return t("diagnose.layer.crowd", "Cell load");
    case "proxy": return t("diagnose.layer.proxy", "Proxy");
    case "speed": return t("diagnose.layer.speed", "Speed");
    default: return id;
  }
}

export interface RowView {
  /** null = no symbol (pending, running, info: plain text). */
  tone: Tone | null;
  /** The word after the symbol ("正常", "疑点", "差", "测不了 · 超时"), or the plain value. */
  word: string;
  /** Numbers next to the word (ok / warn / bad). */
  detail: string;
  /** Grey row: not counted, can't tell, or still waiting. */
  muted: boolean;
}

/** One row's right side: symbol + word + value (§12.3). */
export function rowView(t: T, l: Pick<DiagLayer, "level" | "detail" | "detail_en" | "counted">, lang: Lang): RowView {
  const detail = pick(l.detail, l.detail_en, lang);
  const muted = l.counted === false;
  switch (l.level) {
    case "pending":
      return { tone: null, word: t("diagnose.pending", "Waiting"), detail: "", muted: true };
    case "running":
      return { tone: null, word: t("diagnose.running", "Testing…"), detail: "", muted: false };
    case "ok":
      return { tone: muted ? "neutral" : "ok", word: t("diagnose.ok", "OK"), detail, muted };
    case "warn":
      return { tone: "warn", word: t("diagnose.warn", "Suspect"), detail, muted };
    case "bad":
      return { tone: "bad", word: t("diagnose.bad", "Poor"), detail, muted };
    case "na":
      return {
        tone: "neutral",
        word: detail ? t("diagnose.naWhy", "Can't check · {{why}}", { why: detail }) : t("diagnose.na", "Can't check"),
        detail: "",
        muted: true,
      };
    case "info":
    default:
      return { tone: null, word: detail, detail: "", muted };
  }
}

/** "3/6": the agent's counts, else counted rows that are no longer pending/running. */
export function progress(run: Pick<DiagRun, "layers"> & Partial<Pick<DiagRun, "step" | "steps">>): { step: number; steps: number } {
  const layers = Array.isArray(run.layers) ? run.layers : [];
  const counted = layers.filter((l) => l.counted !== false);
  const steps = typeof run.steps === "number" && run.steps > 0 ? run.steps : counted.length;
  const step =
    typeof run.step === "number" ? run.step : counted.filter((l) => l.level !== "pending" && l.level !== "running").length;
  return { step: Math.min(step, steps), steps };
}

export interface HeadView {
  tone: Tone;
  text: string;
  /** The one thing to do (may be ""). */
  action: string;
  /** "另有 N 处疑点 / +N more", or "". */
  more: string;
  /** Where the action leads: "proxy" → the proxy page; "placement" is a touch-screen page (text only). */
  to: DiagMain["action_to"];
}

/** Status block of a finished run (§12.3): main cause + one action; nothing found → green. */
export function headView(t: T, main: DiagMain | null | undefined, lang: Lang): HeadView {
  if (!main || !main.layer) {
    return {
      tone: "ok",
      text: (main && pick(main.text, main.text_en, lang)) || t("diagnose.noProblem", "No problem found"),
      action:
        (main && pick(main.action, main.action_en, lang)) ||
        t("diagnose.noProblemAction", "The site itself may be slow; you can also add a speed test"),
      more: "",
      to: "",
    };
  }
  const tone: Tone = main.level === "bad" ? "bad" : main.level === "warn" ? "warn" : "neutral";
  return {
    tone,
    text: pick(main.text, main.text_en, lang),
    action: pick(main.action, main.action_en, lang),
    more: main.more > 0 ? t("diagnose.more", "+{{n}} more", { n: main.more }) : "",
    to: main.action_to ?? "",
  };
}

/** A real run, not `{state:"idle"}` (or a payload missing its pieces). */
export function isRun(d: DiagGet | null | undefined): d is DiagRun {
  return !!d && (d.state === "waiting" || d.state === "running" || d.state === "done") && Array.isArray((d as DiagRun).layers);
}

/** Still going (waiting or running), or its speed row is being measured. */
export function isLive(d: DiagGet | null | undefined): boolean {
  if (!isRun(d)) return false;
  return d.state !== "done" || d.speed?.level === "running";
}

/** SWR refresh interval for GET /api/diagnose: 1 s while live, else off. */
export function pollMs(d: DiagGet | null | undefined, expecting = false): number {
  return expecting || isLive(d) ? 1000 : 0;
}

/** What a waiting run waits for, in words. */
export function waitingFor(t: T, w: DiagRun["waiting_for"]): string {
  switch (w) {
    case "scan": return t("diagnose.waitScan", "Searching for networks");
    case "register": return t("diagnose.waitRegister", "Registering on a network");
    case "speedtest": return t("diagnose.waitSpeedtest", "Speed test running");
    default: return "";
  }
}

/** Rows in display order: the agent's order, unknown ids last. */
export function orderedLayers(layers: DiagLayer[] | null | undefined): DiagLayer[] {
  const ls = Array.isArray(layers) ? [...layers] : [];
  const rank = (id: string) => {
    const i = (LAYER_ORDER as readonly string[]).indexOf(id);
    return i < 0 ? LAYER_ORDER.length : i;
  };
  return ls.sort((a, b) => rank(a.id) - rank(b.id));
}

/** Level for the speed row while it is being measured or after (none = no row). */
export function speedLevel(run: DiagRun | null | undefined): DiagLevel | null {
  return run?.speed?.level ?? null;
}

/** datad verdicts (`story.state`) that get 「查原因 →」 on home (§12.2: slow kinds, nodata, stall). */
export const DIAGNOSE_VERDICTS: ReadonlySet<string> = new Set(["limit", "weak", "noise", "crowd", "narrow", "nodata", "stall"]);

/**
 * Whether home offers 「查原因 →」. With datad's verdict (agent ≥ 10-02) it
 * alone decides; without one (null: datad silent, absent: older agent) the
 * web's own signal state does — weak signal or no service. Never while the
 * page is still connecting or the numbers stopped.
 */
export function wantsDiagnose(verdict: string | null | undefined, sig: string): boolean {
  if (sig === "loading" || sig === "stale") return false;
  if (typeof verdict === "string" && verdict) return DIAGNOSE_VERDICTS.has(verdict);
  return sig === "weak" || sig === "none";
}

/**
 * Entered with an in-app link or menu (client-side navigation), not a
 * typed URL, a bookmark or a reload: the page the browser hard-loaded
 * (the navigation timing entry) is a different URL from the current one.
 */
export function arrivedInApp(loadedUrl: string | null | undefined, currentUrl: string): boolean {
  if (!loadedUrl) return false;
  const norm = (u: string) => {
    try {
      const x = new URL(u);
      return x.origin + (x.pathname.replace(/\/+$/, "") || "/");
    } catch {
      return u;
    }
  };
  return norm(loadedUrl) !== norm(currentUrl);
}
