"use client";

import { useState, useRef, useCallback } from "react";
import { useTranslation } from "react-i18next";
import { PageHeader, SectionCard, ErrorBanner } from "@/components/admin/StatCard";
import { Button } from "@/components/admin/Button";
import { Upload, FileText, Download } from "lucide-react";

// Known ZXHN config encryption magic
const ZXHN_MAGIC = "ZXHN";

// Known ZTE config keys (reproduced from iOS ConfigToolViewModel / ConfigConstants)
const KNOWN_KEYS: { descKey: string; description: string; hex: string }[] = [
  { descKey: "keyDefault", description: "Default (Telstra/generic)", hex: "***REMOVED***" },
  { descKey: "keyOptus", description: "Optus / ZTE global", hex: "***REMOVED***" },
  { descKey: "keyVodafone", description: "Vodafone AU", hex: "***REMOVED***" },
  { descKey: "keyTpg", description: "TPG/iiNet AU", hex: "***REMOVED***" },
  { descKey: "keyOpen", description: "Open/community key", hex: "***REMOVED***" },
];

interface ConfigHeader {
  magic: string;
  payloadType: string;
  signature: string;
  payloadOffset: number;
  fileSize: number;
}

function parseHeader(data: Uint8Array): ConfigHeader | null {
  if (data.length < 16) return null;
  const decoder = new TextDecoder("ascii");
  const magic = decoder.decode(data.subarray(0, 4));
  if (!magic.startsWith("ZXHN") && !magic.startsWith("ZXWL")) return null;

  // Read payload type from byte 4
  const typeMap: Record<number, string> = { 0: "plain", 1: "ECB", 2: "CBC", 3: "CBC-new" };
  const payloadType = typeMap[data[4]] ?? `unknown(${data[4]})`;

  // Signature: bytes 8..24 (16 bytes, null-terminated ASCII)
  const sigBytes = data.subarray(8, 24);
  const nullIdx = sigBytes.indexOf(0);
  const signature = decoder.decode(nullIdx >= 0 ? sigBytes.subarray(0, nullIdx) : sigBytes).trim();

  // Payload offset: 4-byte LE at byte 4 or hardcoded header size
  // Heuristic: header is commonly 32 or 64 bytes
  const payloadOffset = (data[28] | (data[29] << 8) | (data[30] << 16) | (data[31] << 24)) || 64;

  return { magic, payloadType, signature, payloadOffset, fileSize: data.length };
}

function toHex(data: Uint8Array, maxBytes = 256): string {
  const slice = data.slice(0, maxBytes);
  const lines: string[] = [];
  for (let i = 0; i < slice.length; i += 16) {
    const chunk = slice.slice(i, i + 16);
    const hex = Array.from(chunk).map((b) => b.toString(16).padStart(2, "0")).join(" ");
    const ascii = Array.from(chunk).map((b) => (b >= 32 && b < 127 ? String.fromCharCode(b) : ".")).join("");
    lines.push(`${i.toString(16).padStart(6, "0")}  ${hex.padEnd(48)}  ${ascii}`);
  }
  if (data.length > maxBytes) lines.push(`... (${data.length - maxBytes} more bytes)`);
  return lines.join("\n");
}

export default function ConfigPage() {
  const { t } = useTranslation();
  const [fileName, setFileName] = useState<string | null>(null);
  const [fileData, setFileData] = useState<Uint8Array | null>(null);
  const [header, setHeader] = useState<ConfigHeader | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);
  const [dragging, setDragging] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  function loadFile(file: File) {
    setFileName(file.name);
    setHeader(null);
    setParseError(null);
    setFileData(null);
    const reader = new FileReader();
    reader.onload = (e) => {
      const buf = e.target?.result as ArrayBuffer;
      const bytes = new Uint8Array(buf);
      setFileData(bytes);
      const parsed = parseHeader(bytes);
      if (!parsed) {
        setParseError(t("config.errNotRecognized", "Not a recognized ZXHN config file (magic header not found)."));
      } else {
        setHeader(parsed);
      }
    };
    reader.readAsArrayBuffer(file);
  }

  const onDrop = useCallback((e: React.DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const file = e.dataTransfer.files[0];
    if (file) loadFile(file);
  }, []);

  function downloadHex() {
    if (!fileData || !fileName) return;
    const text = toHex(fileData, fileData.length);
    const blob = new Blob([text], { type: "text/plain" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = fileName + ".hex.txt";
    a.click();
  }

  return (
    <>
      <PageHeader title={t("config.title", "Config Tool")} description={t("config.desc", "Inspect ZTE ZXHN router config files (.zxhn).")} />

      <div className="grid gap-4">
        {/* Drop zone */}
        <SectionCard>
          <div
            onDragOver={(e) => { e.preventDefault(); setDragging(true); }}
            onDragLeave={() => setDragging(false)}
            onDrop={onDrop}
            onClick={() => inputRef.current?.click()}
            className={`flex cursor-pointer flex-col items-center justify-center gap-3 rounded-lg border-2 border-dashed py-10 transition ${dragging ? "border-accent bg-accent/5" : "border-border hover:border-accent/60"}`}
          >
            <Upload size={28} className="text-text-dim" />
            <p className="text-sm text-text-dim">{t("config.dropBefore", "Drag & drop a")} <code className="text-text">.zxhn</code> {t("config.dropAfter", "file, or click to browse")}</p>
            {fileName && <p className="text-xs text-accent">{fileName}</p>}
          </div>
          <input
            ref={inputRef}
            type="file"
            className="hidden"
            accept=".zxhn,.bin,.cfg,.conf"
            onChange={(e) => { const f = e.target.files?.[0]; if (f) loadFile(f); }}
          />
        </SectionCard>

        {parseError && <ErrorBanner message={parseError} />}

        {/* File header info */}
        {header && (
          <SectionCard title={t("config.fileHeader", "File Header")}>
            <div className="space-y-1 font-mono text-sm">
              <div className="flex gap-3"><span className="w-32 text-text-dim">{t("config.magic", "Magic")}</span><span>{header.magic}</span></div>
              <div className="flex gap-3"><span className="w-32 text-text-dim">{t("config.payloadType", "Payload type")}</span><span>{header.payloadType}</span></div>
              <div className="flex gap-3"><span className="w-32 text-text-dim">{t("config.signature", "Signature")}</span><span>{header.signature || "—"}</span></div>
              <div className="flex gap-3"><span className="w-32 text-text-dim">{t("config.payloadOffset", "Payload offset")}</span><span>{t("config.bytes", "{{n}} bytes", { n: header.payloadOffset })}</span></div>
              <div className="flex gap-3"><span className="w-32 text-text-dim">{t("config.fileSize", "File size")}</span><span>{t("config.bytes", "{{n}} bytes", { n: header.fileSize })}</span></div>
            </div>
          </SectionCard>
        )}

        {/* Hex preview */}
        {fileData && (
          <SectionCard title={t("config.hexPreview", "Hex Preview (first 256 bytes)")}>
            <pre className="overflow-x-auto rounded-md bg-bg-elevated p-3 font-mono text-[11px] leading-relaxed text-text-dim">
              {toHex(fileData)}
            </pre>
            <div className="mt-3 flex gap-2">
              <Button size="sm" variant="outline" onClick={downloadHex}>
                <Download size={13} /> {t("config.downloadHexDump", "Download hex dump")}
              </Button>
            </div>
          </SectionCard>
        )}

        {/* Known keys */}
        <SectionCard title={t("config.knownKeys", "***REMOVED***")}>
          <p className="mb-3 text-sm text-text-dim">
            {t("config.knownKeysDesc", "***REMOVED***. Full decrypt/re-encrypt is available in the iOS companion app.")}
          </p>
          <div className="overflow-x-auto">
            <table className="w-full text-sm">
              <thead>
                <tr className="border-b border-border/60 text-left text-xs text-text-dim">
                  <th className="pb-2 pr-4">{t("config.colDescription", "Description")}</th>
                  <th className="pb-2">{t("config.colKeyHex", "Key (hex)")}</th>
                </tr>
              </thead>
              <tbody>
                {KNOWN_KEYS.map((k) => (
                  <tr key={k.hex} className="border-b border-border/40 last:border-0">
                    <td className="py-2 pr-4 text-text-dim">{t(`config.${k.descKey}`, k.description)}</td>
                    <td className="py-2 font-mono text-xs">{k.hex}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
          <div className="mt-4 flex items-start gap-2 rounded-md border border-accent/20 bg-accent/5 px-3 py-2 text-xs text-text-dim">
            <FileText size={14} className="mt-0.5 shrink-0 text-accent" />
            <span>
              {t("config.cryptoNoteBefore", "Full AES-ECB/CBC decrypt, zlib decompress, XML view, and re-encryption are implemented in the iOS app (")}<code>***REMOVED***</code>{t("config.cryptoNoteAfter", "). Web-side decryption requires SubtleCrypto integration — planned for a future update.")}
            </span>
          </div>
        </SectionCard>
      </div>
    </>
  );
}
