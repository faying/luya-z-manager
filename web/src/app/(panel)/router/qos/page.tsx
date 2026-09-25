"use client";
// QoS (new design), read-only since 2026-09-25.
// What this firmware calls QoS is the vendor "带宽分配" mode (zte_smart_manage:
// Ai / Live / Chat / Game / Video / Nomal). Every mode but Nomal (极速) sorts
// traffic by app, which needs xdpi, and xdpi is off on purpose (≈25 % CPU,
// 2026-09-23). The owner decided not to offer switching it
// (docs/designs/touch-menu-tabs.md «不做»), so the page only says which mode
// the device is in and why nothing here changes it.
// GET /api/router/qos: the vendor getter's fields plus `smart_qos_mode` and
// `xdpi_support` (router.rs). The old switch (router_set_qos_switch) never
// existed on B27; the page no longer writes anything.
import { useTranslation } from "react-i18next";
import { useApi } from "@/lib/hooks/useApi";
import type { RouterQos } from "@/lib/api/schemas/router";
import { Button, Freshness, Group, Row, StatusBlock, type Tone } from "@/components/nd";

const MODE_KEYS: Record<string, [string, string]> = {
  Nomal_mode: ["qos.modeFast", "Fastest (no per-app sorting)"],
  Ai_mode: ["qos.modeAi", "AI"],
  Live_mode: ["qos.modeLive", "Live streaming"],
  Chat_mode: ["qos.modeChat", "Chat"],
  Game_mode: ["qos.modeGame", "Gaming"],
  Video_mode: ["qos.modeVideo", "Video"],
};

export default function QosPage() {
  const { t } = useTranslation();
  const qos = useApi<RouterQos>("/api/router/qos", { refreshInterval: 30000 });
  const mode = typeof qos.data?.smart_qos_mode === "string" ? qos.data.smart_qos_mode : "";
  const xdpiOff = qos.data?.xdpi_support === "0";
  const serviceDown = !!qos.error && !qos.data;
  const unsupported = serviceDown && qos.unsupported;
  const known = MODE_KEYS[mode];
  const modeName = known ? t(known[0], known[1]) : mode;

  let tone: Tone = "neutral";
  let state: string = t("qos.loading", "Reading QoS…");
  let reason: string | null = null;
  if (unsupported) {
    state = t("qos.unsupported", "Not available on this firmware");
    reason = t(
      "qos.unsupportedDesc",
      "The device's firmware doesn't answer the QoS request (the method isn't there), so QoS can't be read or changed from this page."
    );
  } else if (serviceDown) {
    tone = "warn";
    state = t("qos.unavailable", "Unavailable");
    reason = qos.error?.message ?? null;
  } else if (qos.data) {
    tone = "ok";
    state = mode
      ? t("qos.modeState", "Bandwidth mode: {{mode}}", { mode: modeName })
      : t("qos.modeUnknown", "Bandwidth mode unknown");
    reason = t(
      "qos.readOnlyWhy",
      "The other vendor modes (AI, live, chat, gaming, video) decide by recognising apps, which needs xdpi. xdpi is off to save about a quarter of the CPU, so this page only shows the mode and doesn't change it."
    );
  }
  if (qos.data && qos.stale) tone = "stale";

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("qos.title", "QoS")}</h1>
      <p className="nd-body mb-4 max-w-[720px] text-nd-t2">
        {t("qos.descRo", "Which bandwidth mode the device is in. Read-only.")}
      </p>

      <div className="grid max-w-[720px] gap-6">
        <StatusBlock
          tone={tone}
          state={state}
          reason={reason}
          meta={qos.data && qos.stale ? <Freshness stale lastOkAt={qos.lastOkAt} what={t("qos.settingsWord", "Settings")} /> : undefined}
          actions={
            serviceDown && !unsupported ? (
              <Button variant="secondary" size="sm" onPress={() => qos.mutate()}>
                {t("common.retry", "Retry")}
              </Button>
            ) : undefined
          }
        />

        <section>
          <Group title={t("qos.modeTitle", "Bandwidth allocation")} stale={qos.stale}>
            <Row
              label={t("qos.modeRow", "Mode")}
              sub={t("qos.modeSub", "Only matters when several devices fill the link at once.")}
              control={
                !qos.data && !qos.error ? (
                  <span className="nd-skel" style={{ width: 80 }} />
                ) : (
                  <span className="nd-aux">{mode ? modeName : "—"}</span>
                )
              }
            />
            <Row
              label={t("qos.xdpiRow", "App recognition (xdpi)")}
              sub={t("qos.xdpiSub", "Off on purpose: it costs about 25 % CPU.")}
              control={
                <span className="nd-aux">
                  {qos.data ? (xdpiOff ? t("qos.off", "Off") : t("qos.on", "On")) : "—"}
                </span>
              }
            />
          </Group>
        </section>
      </div>
    </>
  );
}
