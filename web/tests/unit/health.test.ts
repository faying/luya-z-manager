import { describe, expect, it } from "vitest";
import { healthRows } from "@/lib/health";
import type { HealthCheck } from "@/lib/api/schemas/system";

const base: HealthCheck[] = [
  { level: "ok", id: "boot-sync", label: "开机同步", detail: "sync success" },
  { level: "warn", id: "sms", label: "短信告警", detail: "没配置号码：后台挂了你不会知道" },
  { level: "ok", id: "standby", label: "待机", detail: "正常" },
];

describe("health rows: the device manifest row", () => {
  it("a mismatch is shown first, with the warn tone and doctor's words", () => {
    const rows = healthRows([
      ...base,
      { level: "warn", id: "manifest", label: "清单", detail: "不一致的是 zwrt-datad（清单 6bbf6ea6，实际 af3c8ad8）" },
    ]);
    expect(rows.map((r) => r.id)).toEqual(["manifest", "boot-sync", "sms", "standby"]);
    expect(rows[0]).toEqual({
      id: "manifest",
      label: "清单",
      detail: "不一致的是 zwrt-datad（清单 6bbf6ea6，实际 af3c8ad8）",
      tone: "warn",
    });
  });

  it("consistent: ok tone, still first", () => {
    const rows = healthRows([...base, { level: "ok", id: "manifest", label: "清单", detail: "一致（12 项）" }]);
    expect(rows[0].tone).toBe("ok");
    expect(rows[0].detail).toBe("一致（12 项）");
  });

  it("an older doctor without the row: rows unchanged", () => {
    expect(healthRows(base).map((r) => [r.id, r.tone])).toEqual([
      ["boot-sync", "ok"],
      ["sms", "warn"],
      ["standby", "ok"],
    ]);
  });
});
