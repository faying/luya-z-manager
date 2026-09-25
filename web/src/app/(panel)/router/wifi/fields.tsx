"use client";
// Form rows shared by /router/wifi and /router/wifi-guest (new design).
// nd has Group/Row but no labelled input rows yet; these stay local to the
// two Wi-Fi pages until another page needs them.
import { useState, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { Eye, EyeSlash } from "@phosphor-icons/react";
import { Button, Help } from "@/components/nd";

/** One labelled row. `stacked` puts the control under the label (text
 *  inputs); otherwise it sits on the right from 640 px up. */
export function FieldRow({
  id,
  label,
  help,
  sub,
  srPrefix,
  stacked = false,
  children,
}: {
  id: string;
  label: string;
  /** Read before the label by screen readers only (e.g. "2.4 GHz"), so two
   *  bands' fields don't share one accessible name. */
  srPrefix?: string;
  help?: string;
  sub?: ReactNode;
  stacked?: boolean;
  children: ReactNode;
}) {
  return (
    <div className={`nd-row ${stacked ? "flex-col items-stretch gap-2" : "flex-wrap"}`}>
      <span className="nd-row__text">
        <span className="flex items-center">
          <label htmlFor={id} className="nd-row__label">
            {srPrefix && <span className="sr-only">{srPrefix} </span>}
            {label}
          </label>
          {help && <Help text={help} label={label} />}
        </span>
        {sub && <span className="nd-row__sub block">{sub}</span>}
      </span>
      <div className={stacked ? "" : "w-full sm:w-56"}>{children}</div>
    </div>
  );
}

export type Opt = { value: string; label: string };

/** Native select in nd-field style. A value the list doesn't know (an
 *  empty uci value, a region outside the list) is put first as-is, so an
 *  untouched field is sent back exactly as it was read. */
export function SelectField({
  id,
  value,
  onChange,
  options,
  disabled,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  options: Opt[];
  disabled?: boolean;
}) {
  const opts = options.some((o) => o.value === value) ? options : [{ value, label: value || "—" }, ...options];
  return (
    <select id={id} className="nd-field" value={value} disabled={disabled} onChange={(e) => onChange(e.target.value)}>
      {opts.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

export function TextField({
  id,
  value,
  onChange,
  disabled,
  placeholder,
  invalid,
  describedBy,
  mono = true,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
  placeholder?: string;
  invalid?: boolean;
  describedBy?: string;
  mono?: boolean;
}) {
  return (
    <input
      id={id}
      className={`nd-field${mono ? " nd-mono" : ""}`}
      value={value}
      disabled={disabled}
      placeholder={placeholder}
      aria-invalid={invalid || undefined}
      aria-describedby={describedBy}
      autoComplete="off"
      autoCapitalize="off"
      autoCorrect="off"
      spellCheck={false}
      onChange={(e) => onChange(e.target.value)}
    />
  );
}

/** Password input with a show/hide button whose name says which password. */
export function PasswordField({
  id,
  value,
  onChange,
  disabled,
  placeholder,
  whose,
  showLabel,
  hideLabel,
  invalid,
  describedBy,
}: {
  id: string;
  value: string;
  onChange: (v: string) => void;
  disabled?: boolean;
  placeholder?: string;
  /** e.g. "2.4 GHz" — goes into the button's accessible name. */
  whose: string;
  /** Override the composed names (e.g. 「显示访客密码」). */
  showLabel?: string;
  hideLabel?: string;
  invalid?: boolean;
  describedBy?: string;
}) {
  const { t } = useTranslation();
  const [show, setShow] = useState(false);
  return (
    <div className="flex items-center gap-2">
      <input
        id={id}
        type={show ? "text" : "password"}
        className="nd-field nd-mono flex-1"
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        aria-invalid={invalid || undefined}
        aria-describedby={describedBy}
        autoComplete="new-password"
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
        onChange={(e) => onChange(e.target.value)}
      />
      <Button
        variant="ghost"
        iconOnly
        aria-label={
          show
            ? (hideLabel ?? t("wifi.hidePasswordOf", "Hide {{whose}} password", { whose }))
            : (showLabel ?? t("wifi.showPasswordOf", "Show {{whose}} password", { whose }))
        }
        aria-pressed={show}
        onPress={() => setShow((s) => !s)}
      >
        {show ? <EyeSlash size={20} weight="bold" aria-hidden /> : <Eye size={20} weight="bold" aria-hidden />}
      </Button>
    </div>
  );
}

/** Characters zte-agent's sanitize_uci_value (wifi.rs:70) silently strips
 *  from every Wi-Fi value. A password containing one would be saved
 *  without it, so the page refuses them instead. */
export const STRIPPED_CHARS = /['";$`\\|<>&]/;

/** Both pages send every field on every apply, so a stored SSID or key
 *  with one of these characters would be rewritten without it by an
 *  unrelated change. Check them all, changed or not. */
export const hasStripped = (s: string) => STRIPPED_CHARS.test(s);

/** SSID length: 1–32 bytes (check changed fields only). */
export function ssidProblem(s: string): "empty" | "long" | null {
  if (s.length === 0) return "empty";
  if (new TextEncoder().encode(s).length > 32) return "long";
  return null;
}

/** WPA passphrase: 8–63 printable ASCII (or 64 hex). Not checked when the
 *  network is open (check changed fields only). */
export function keyProblem(k: string, encryption: string): "short" | "long" | "ascii" | null {
  if (encryption === "none") return null;
  if (/^[0-9a-fA-F]{64}$/.test(k)) return null;
  if (!/^[\x20-\x7e]*$/.test(k)) return "ascii";
  if (k.length < 8) return "short";
  if (k.length > 63) return "long";
  return null;
}

/** uci flags: "" (unset) means "0". */
export const flag = (v: string | undefined) => (v === "1" ? "1" : "0");
