"use client";
// Wi-Fi power save and NFC tap-to-join (audit C, 10-04): the two switches the
// touch screen has on its Wi-Fi tab. Each applies on its own (tier 1: no
// connection drops) and is read back; they are not part of the page's one
// big form. Both go through datad (power save: wifi.power_save; NFC: nfc.set)
// and land in the change log. Older agents answer 404: the group stays away.
import { useRef } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { useApi } from "@/lib/hooks/useApi";
import { useWriteOp } from "@/lib/api/writeOp";
import { GroupTitle, Help, OpResult, Row, Switch } from "@/components/nd";

interface PowerSave {
  /** Live (iw) when Wi-Fi is up, else the saved choice; null = unknown. */
  enabled: boolean | null;
  live: boolean | null;
  saved: boolean | null;
}
interface Nfc {
  supported: boolean;
  enabled: boolean | null;
}

export function WifiExtras() {
  const { t } = useTranslation();
  const psm = useApi<PowerSave>("/api/wifi/power-save", { refreshInterval: 30000 });
  const nfc = useApi<Nfc>("/api/nfc", { refreshInterval: 30000 });

  const psmWant = useRef(false);
  const psmOp = useWriteOp({
    tier: 1,
    steps: [{ label: t("wifi.psm", "Wi-Fi power save"), run: () => apiFetch("/api/wifi/power-save", { method: "PUT", body: { enabled: psmWant.current } }) }],
    verify: async () => {
      const d = await apiFetch<PowerSave>("/api/wifi/power-save");
      await psm.mutate(d, { revalidate: false });
      return d.enabled === psmWant.current;
    },
  });
  const nfcWant = useRef(false);
  const nfcOp = useWriteOp({
    tier: 1,
    steps: [{ label: t("wifi.nfc", "NFC tap to join"), run: () => apiFetch("/api/nfc", { method: "PUT", body: { enabled: nfcWant.current } }) }],
    verify: async () => {
      const d = await apiFetch<Nfc>("/api/nfc");
      await nfc.mutate(d, { revalidate: false });
      return d.enabled === nfcWant.current;
    },
  });

  // an older agent (404) has neither: no group
  if (!psm.data && !nfc.data) return null;
  const busy = psmOp.busy || nfcOp.busy;
  const p = psm.data;
  const n = nfc.data;
  return (
    <section data-testid="wifi-extras">
      <GroupTitle>{t("wifi.extrasTitle", "Power save and NFC")}</GroupTitle>
      <div className="nd-group">
        {p && (
          <Row
            label={
              <span className="flex items-center">
                {t("wifi.psm", "Wi-Fi power save")}
                <Help
                  label={t("wifi.psm", "Wi-Fi power save")}
                  text={t("wifi.helpPsm", "Lets the radios doze between packets. Saves a little battery; some devices see slower replies (games, calls). Applies at once and stays after a restart.")}
                />
              </span>
            }
            sub={
              p.enabled == null
                ? t("wifi.psmUnknown", "Not read yet (Wi-Fi off and never set here)")
                : p.live == null
                  ? t("wifi.psmSaved", "Saved; applies when Wi-Fi is on")
                  : undefined
            }
            control={
              <Switch
                label={t("wifi.psm", "Wi-Fi power save")}
                isSelected={p.enabled === true}
                isDisabled={busy || psm.stale}
                onChange={(on) => {
                  psmWant.current = on;
                  psmOp.start();
                }}
              />
            }
          />
        )}
        {n && n.supported && (
          <Row
            label={
              <span className="flex items-center">
                {t("wifi.nfc", "NFC tap to join")}
                <Help label={t("wifi.nfc", "NFC tap to join")} text={t("wifi.helpNfc", "A phone touched to the device joins this Wi-Fi without typing the password.")} />
              </span>
            }
            control={
              <Switch
                label={t("wifi.nfc", "NFC tap to join")}
                isSelected={n.enabled === true}
                isDisabled={busy || nfc.stale}
                onChange={(on) => {
                  nfcWant.current = on;
                  nfcOp.start();
                }}
              />
            }
          />
        )}
      </div>
      <div className="mt-2 grid gap-1 px-1">
        {psmOp.phase !== "idle" && <OpResult op={psmOp} />}
        {nfcOp.phase !== "idle" && <OpResult op={nfcOp} />}
      </div>
    </section>
  );
}
