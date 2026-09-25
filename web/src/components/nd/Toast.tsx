"use client";
import { createContext, useCallback, useContext, useMemo, useRef, useState, type ReactNode } from "react";
import { X } from "@phosphor-icons/react";
import { useTranslation } from "react-i18next";
import { StatusMark } from "./StatusMark";

type ToastKind = "ok" | "bad";
type Item = { id: number; kind: ToastKind; text: ReactNode };

const Ctx = createContext<{ show: (kind: ToastKind, text: ReactNode) => void } | null>(null);

/**
 * Result bar for writes (design doc §5.1). Success leaves after 3 s;
 * failure stays until closed and should say the reason and next step.
 * Phones: 8px above the floating tab bar. Desktop: bottom right.
 */
export function ToastProvider({ children }: { children: ReactNode }) {
  const { t } = useTranslation();
  const [items, setItems] = useState<Item[]>([]);
  const next = useRef(1);
  const close = useCallback((id: number) => setItems((xs) => xs.filter((x) => x.id !== id)), []);
  const show = useCallback(
    (kind: ToastKind, text: ReactNode) => {
      const id = next.current++;
      setItems((xs) => [...xs.slice(-2), { id, kind, text }]);
      if (kind === "ok") setTimeout(() => close(id), 3000);
    },
    [close],
  );
  const value = useMemo(() => ({ show }), [show]);
  return (
    <Ctx.Provider value={value}>
      {children}
      <div className="nd nd-toasts">
        {items.map((it) => (
          <div key={it.id} className="nd-toast" role={it.kind === "ok" ? "status" : "alert"}>
            <div className="nd-toast__body">
              <StatusMark tone={it.kind === "ok" ? "ok" : "bad"}>{it.text}</StatusMark>
            </div>
            {it.kind === "bad" && (
              <button
                type="button"
                className="nd-btn nd-btn--ghost nd-btn--icon"
                aria-label={t("nd.close", "Close")}
                onClick={() => close(it.id)}
              >
                <X size={18} weight="bold" aria-hidden />
              </button>
            )}
          </div>
        ))}
      </div>
    </Ctx.Provider>
  );
}

export function useToast() {
  const c = useContext(Ctx);
  if (!c) throw new Error("useToast must be used inside <ToastProvider>");
  return c;
}
