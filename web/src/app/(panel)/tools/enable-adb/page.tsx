"use client";

import { useState } from "react";
import { useTranslation } from "react-i18next";
import { apiFetch } from "@/lib/api/client";
import { ApiError } from "@/lib/api/types";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { Terminal, ShieldAlert, CheckCircle2 } from "lucide-react";

export default function EnableADBPage() {
  const { t } = useTranslation();
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const handleEnable = async () => {
    if (
      !window.confirm(
        t(
          "adb.confirmEnable",
          "Enable ADB Debug USB Mode?\n\nThis will expose ADB over USB, allowing full shell access to the device. Only do this if you have physical access and trust your environment."
        )
      )
    ) {
      return;
    }

    setLoading(true);
    setError(null);
    setSuccess(false);

    try {
      await apiFetch("/api/usb/mode", {
        method: "PUT",
        body: { mode: "debug" },
      });
      setSuccess(true);
    } catch (e) {
      setError(e instanceof ApiError ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  return (
    <>
      <PageHeader
        title={t("adb.title", "Enable ADB")}
        description={t("adb.desc", "Switch USB to Android Debug Bridge mode.")}
      />

      <SectionCard className="max-w-md">
        <div className="flex flex-col items-center gap-6 py-4 text-center">
          <div className="rounded-full bg-warning/15 p-4">
            <Terminal className="h-8 w-8 text-warning" />
          </div>

          <div>
            <h3 className="font-display text-lg font-semibold">
              {t("adb.cardTitle", "Enable ADB Debug USB Mode")}
            </h3>
            <p className="mt-2 text-sm text-text-dim">
              {t(
                "adb.cardDesc",
                "Switches the USB port to ADB debug mode, exposing a full shell over USB."
              )}
            </p>
          </div>

          <div className="flex w-full items-start gap-2 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-left text-sm text-warning">
            <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0" />
            <span>
              {t(
                "adb.warning",
                "This exposes ADB over USB. Anyone with physical access to the USB port will have root shell access. Disable when not in use."
              )}
            </span>
          </div>

          {error && <ErrorBanner message={error} />}

          {success && (
            <div className="flex w-full items-center gap-2 rounded-md border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">
              <CheckCircle2 className="h-4 w-4 shrink-0" />
              {t("adb.successPrefix", "ADB debug mode enabled. Connect via USB and run")}{" "}
              <code className="font-mono text-xs">adb devices</code>.
            </div>
          )}

          <Button
            variant={success ? "outline" : "primary"}
            onClick={handleEnable}
            loading={loading}
            disabled={loading}
            className="w-full"
          >
            <Terminal className="h-4 w-4" />
            {success
              ? t("adb.reEnable", "Re-enable ADB")
              : t("adb.cardTitle", "Enable ADB Debug USB Mode")}
          </Button>
        </div>
      </SectionCard>
    </>
  );
}
