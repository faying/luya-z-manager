"use client";
import { Modal } from "@heroui/react";
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "./Button";
import { isRemoteAccess } from "@/lib/api/remote";

/**
 * Tier-3 confirm (design doc §3.1). Says what will happen, how long the
 * device is away and how to recover. Default focus is Cancel. Irreversible
 * actions (factory reset, eSIM delete) require typing a confirm word.
 * Over Tailscale, actions that can cut the uplink get the remote banner.
 */
export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  what,
  downtime,
  recovery,
  actionLabel,
  onConfirm,
  cutsUplink = false,
  typeToConfirm,
  danger = false,
  pending,
}: {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  title: ReactNode;
  what: ReactNode;
  downtime?: ReactNode;
  recovery?: ReactNode;
  actionLabel: string;
  onConfirm: () => void;
  cutsUplink?: boolean;
  typeToConfirm?: string;
  danger?: boolean;
  pending?: boolean;
}) {
  const { t } = useTranslation();
  const [typed, setTyped] = useState("");
  const remote = cutsUplink && isRemoteAccess();
  const blocked = typeToConfirm !== undefined && typed.trim() !== typeToConfirm;

  return (
    <Modal.Backdrop
      isOpen={open}
      onOpenChange={(o) => {
        if (!o) setTyped("");
        onOpenChange(o);
      }}
    >
      <Modal.Container placement="center">
        <Modal.Dialog className="nd nd-dialog">
          <Modal.Header>
            <Modal.Heading className="nd-title">{title}</Modal.Heading>
          </Modal.Header>
          <Modal.Body>
            {remote && (
              <p className="nd-dialog__remote" role="alert">
                {t("nd.remoteWarning", "You are connected remotely over Tailscale. If this step cuts the connection, someone has to be at the device to recover it.")}
              </p>
            )}
            <dl className="nd-dialog__facts">
              <div>
                <dt>{t("nd.whatHappens", "What happens")}</dt>
                <dd>{what}</dd>
              </div>
              {downtime && (
                <div>
                  <dt>{t("nd.downtime", "How long it is offline")}</dt>
                  <dd>{downtime}</dd>
                </div>
              )}
              {recovery && (
                <div>
                  <dt>{t("nd.recovery", "How to recover")}</dt>
                  <dd>{recovery}</dd>
                </div>
              )}
            </dl>
            {typeToConfirm !== undefined && (
              <label className="mt-4 block">
                <span className="nd-aux block mb-2">
                  {t("nd.typeToConfirm", "Type “{{word}}” to confirm", { word: typeToConfirm })}
                </span>
                <input
                  className="nd-field"
                  value={typed}
                  onChange={(e) => setTyped(e.target.value)}
                  autoComplete="off"
                  autoCapitalize="off"
                  spellCheck={false}
                />
              </label>
            )}
          </Modal.Body>
          <Modal.Footer className="flex flex-wrap justify-end gap-2">
            <Button variant="secondary" slot="close" autoFocus>
              {t("common.cancel", "Cancel")}
            </Button>
            <Button variant={danger ? "danger" : "confirm"} onPress={onConfirm} isDisabled={blocked} pending={pending}>
              {actionLabel}
            </Button>
          </Modal.Footer>
        </Modal.Dialog>
      </Modal.Container>
    </Modal.Backdrop>
  );
}
