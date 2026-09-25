"use client";
// In-place login when the session expires mid-use (design doc §5.1, R2):
// the page, its forms and any waiting write stay mounted underneath.
// A new token fires LOGIN_EVENT: reads resend, tier-1 writes resend, tier
// 2/3 writes offer "resubmit" (writeOp.ts). Not dismissable by clicking
// outside; the only other way out is signing out.
import { Modal } from "@heroui/react";
import { useState, type FormEvent } from "react";
import { useTranslation } from "react-i18next";
import { useAuth } from "@/lib/hooks/useAuth";
import { ApiError } from "@/lib/api/types";
import { Button } from "../Button";

export function LoginDialog() {
  const { t } = useTranslation();
  const { expired, login, logout } = useAuth();
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (!password) return;
    setBusy(true);
    setErr(null);
    try {
      await login(password);
      setPassword("");
    } catch (x) {
      setErr(
        x instanceof ApiError && x.status === 401
          ? t("nd.wrongPassword", "Wrong password.")
          : t("nd.loginNoReply", "The device did not answer. Check the connection and try again."),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal.Backdrop isOpen={expired} isDismissable={false} isKeyboardDismissDisabled>
      <Modal.Container placement="center">
        <Modal.Dialog className="nd nd-dialog w-full max-w-[420px]" aria-label={t("nd.sessionExpired", "Session expired")}>
          <form onSubmit={submit}>
            <Modal.Header>
              <Modal.Heading className="nd-title">{t("nd.sessionExpired", "Session expired")}</Modal.Heading>
            </Modal.Header>
            <Modal.Body>
              <p className="nd-body text-nd-t2">
                {t("nd.sessionExpiredBody", "Sign in again to carry on. This page and what you typed are still here.")}
              </p>
              <label className="mt-4 block">
                <span className="nd-aux mb-2 block">{t("login.password", "Agent password")}</span>
                <input
                  className="nd-field"
                  type="password"
                  autoComplete="current-password"
                  autoFocus
                  value={password}
                  onChange={(e) => setPassword(e.target.value)}
                />
              </label>
              {err && (
                <p className="nd-error mt-2" role="alert">
                  {err}
                </p>
              )}
            </Modal.Body>
            <Modal.Footer className="flex flex-wrap justify-end gap-2">
              <Button variant="secondary" onPress={logout}>
                {t("common.signOut", "Sign out")}
              </Button>
              <Button type="submit" pending={busy} isDisabled={!password}>
                {t("nd.signIn", "Sign in")}
              </Button>
            </Modal.Footer>
          </form>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
}
