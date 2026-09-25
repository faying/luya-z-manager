"use client";
// Hub pages for the Charts / Functions / System anchors: grouped rows for
// every route in that anchor (lib/routes.ts). A row shows a current value
// only when data the shell already polls has one (no extra polling here).
import { useTranslation } from "react-i18next";
import type { ReactNode } from "react";
import { GROUPS, ROUTES, type Anchor, type RouteGroup } from "@/lib/routes";
import { Group, Row } from "../Group";

export function HubGroups({ anchor, values = {} }: { anchor: Exclude<Anchor, "home">; values?: Record<string, ReactNode> }) {
  const { t } = useTranslation();
  const groups = (Object.keys(GROUPS) as RouteGroup[]).filter((g) => GROUPS[g].anchor === anchor);
  return (
    <div className="mt-6 grid gap-2">
      {groups.map((g) => (
        <Group key={g} title={t(GROUPS[g].tKey, GROUPS[g].fallback)}>
          {ROUTES.filter((r) => r.group === g && !r.hidden).map((r) => (
            <Row key={r.href} icon={r.icon} label={t(r.tKey, r.fallback)} value={values[r.href]} href={r.href} />
          ))}
        </Group>
      ))}
    </div>
  );
}

export function HubTitle({ children }: { children: ReactNode }) {
  return <h1 className="nd-title mb-4 mt-2">{children}</h1>;
}
