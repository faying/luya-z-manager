"use client";
// Config tool (new design). Runs entirely in the browser: reads a file the
// user picks, parses the ZXHN header and shows a hex preview. It calls no
// device endpoint and uploads nothing, so there are no write tiers here
// (controls-inventory §/config: every control is 「—（本地）」).
//
// Order: pick a file → error / header → hex preview + download → known keys.
import { useRef, useState, type DragEvent } from "react";
import { useTranslation } from "react-i18next";
import { DownloadSimple, FileArrowUp } from "@phosphor-icons/react";
import { Button, Group, GroupTitle, Row, StatusMark } from "@/components/nd";

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

  // Payload offset: 4-byte LE at byte 28, or the common 64-byte header size.
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
  const [reading, setReading] = useState(false);
  const [dragging, setDragging] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  function loadFile(file: File) {
    setFileName(file.name);
    setHeader(null);
    setParseError(null);
    setFileData(null);
    setReading(true);
    const reader = new FileReader();
    reader.onload = (e) => {
      setReading(false);
      const bytes = new Uint8Array(e.target?.result as ArrayBuffer);
      setFileData(bytes);
      const parsed = parseHeader(bytes);
      if (!parsed) setParseError(t("config.errNotRecognized", "Not a recognized ZXHN config file (magic header not found)."));
      else setHeader(parsed);
    };
    reader.onerror = () => {
      setReading(false);
      setParseError(t("config.errRead", "The browser couldn't read this file. Pick it again."));
    };
    reader.readAsArrayBuffer(file);
  }

  const onDrop = (e: DragEvent) => {
    e.preventDefault();
    setDragging(false);
    const file = e.dataTransfer.files[0];
    if (file) loadFile(file);
  };

  function downloadHex() {
    if (!fileData || !fileName) return;
    const text = toHex(fileData, fileData.length);
    const blob = new Blob([text], { type: "text/plain" });
    const url = URL.createObjectURL(blob);
    const a = document.createElement("a");
    a.href = url;
    a.download = fileName + ".hex.txt";
    a.click();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
  }

  const bytes = (n: number) => t("config.bytes", "{{n}} bytes", { n });

  return (
    <>
      <h1 className="nd-title mb-4 mt-2">{t("config.title", "Config Tool")}</h1>

      <div className="grid max-w-[720px] gap-6">
        <p className="nd-body -mt-2 text-nd-t2">
          {t("config.desc", "Inspect ZTE ZXHN router config files (.zxhn).")} {t("config.localOnly", "The file stays in your browser; nothing is sent to the device.")}
        </p>

        {/* ── pick a file ── */}
        <section aria-labelledby="config-file">
          <GroupTitle id="config-file">{t("config.fileTitle", "Config file")}</GroupTitle>
          <div
            onDragOver={(e) => {
              e.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={onDrop}
            className={`nd-group grid justify-items-center gap-3 border-2 border-dashed p-6 text-center ${dragging ? "border-nd-accT" : "border-transparent"}`}
          >
            <FileArrowUp size={24} weight="bold" className="text-nd-t3" aria-hidden />
            <p className="nd-body text-nd-t2">
              {t("config.dropBefore", "Drag & drop a")} <code className="nd-mono">.zxhn</code> {t("config.dropAfterNd", "file here, or")}
            </p>
            <Button onPress={() => inputRef.current?.click()}>{t("config.chooseFile", "Choose config file")}</Button>
            {fileName && (
              <p className="nd-mono break-all" aria-live="polite">
                {fileName}
              </p>
            )}
            {reading && <p className="nd-aux" role="status">{t("config.reading", "Reading the file…")}</p>}
          </div>
          <input
            ref={inputRef}
            type="file"
            className="hidden"
            tabIndex={-1}
            aria-hidden
            accept=".zxhn,.bin,.cfg,.conf"
            onChange={(e) => {
              const f = e.target.files?.[0];
              if (f) loadFile(f);
              e.target.value = "";
            }}
          />
        </section>

        {parseError && (
          <p role="alert">
            <StatusMark tone="bad">{parseError}</StatusMark>
          </p>
        )}

        {/* ── header ── */}
        {header && (
          <Group title={t("config.fileHeader", "File Header")}>
            <Row label={t("config.magic", "Magic")} value={header.magic} mono />
            <Row label={t("config.payloadType", "Payload type")} value={header.payloadType} mono />
            <Row label={t("config.signature", "Signature")} value={header.signature || "—"} mono />
            <Row label={t("config.payloadOffset", "Payload offset")} value={bytes(header.payloadOffset)} mono />
            <Row label={t("config.fileSize", "File size")} value={bytes(header.fileSize)} mono />
          </Group>
        )}

        {/* ── hex preview ── */}
        {fileData && (
          <section aria-labelledby="config-hex">
            <GroupTitle id="config-hex">{t("config.hexPreview", "Hex Preview (first 256 bytes)")}</GroupTitle>
            <div className="nd-group grid gap-3 p-4">
              <pre className="nd-mono overflow-x-auto text-[12px] leading-5 text-nd-t2">{toHex(fileData)}</pre>
              <div>
                <Button variant="secondary" onPress={downloadHex}>
                  <DownloadSimple size={20} weight="bold" aria-hidden />
                  {t("config.downloadHexDump", "Download hex dump")}
                </Button>
              </div>
            </div>
          </section>
        )}

        {/* ── known keys ── */}
        <section aria-labelledby="config-keys">
          <GroupTitle id="config-keys">{t("config.knownKeys", "***REMOVED***")}</GroupTitle>
          <p className="nd-aux mb-2">
            {t("config.knownKeysDesc", "***REMOVED***. Full decrypt/re-encrypt is available in the iOS companion app.")}
          </p>
          <div className="nd-group">
            {KNOWN_KEYS.map((k) => (
              <Row
                key={k.hex}
                label={t(`config.${k.descKey}`, k.description)}
                sub={<span className="nd-mono break-all">{k.hex}</span>}
              />
            ))}
          </div>
          <p className="nd-aux mt-2">
            {t("config.cryptoNoteBefore", "Full AES-ECB/CBC decrypt, zlib decompress, XML view, and re-encryption are implemented in the iOS app (")}
            <code className="nd-mono">***REMOVED***</code>
            {t("config.cryptoNoteAfter", "). Web-side decryption requires SubtleCrypto integration — planned for a future update.")}
          </p>
        </section>
      </div>
    </>
  );
}
