"use client";
// Tab strip: react-aria Tabs (role=tablist/tab/tabpanel, aria-selected,
// arrow keys) dressed in the Segmented look.
import { Tab, TabList, TabPanel, Tabs as AriaTabs, type Key } from "react-aria-components";
import type { ReactNode } from "react";

export type TabDef<K extends string> = { id: K; label: ReactNode; content: ReactNode };

export function NdTabs<K extends string>({
  label,
  tabs,
  value,
  onChange,
}: {
  /** Accessible name of the tab list. */
  label: string;
  tabs: TabDef<K>[];
  value: K;
  onChange: (k: K) => void;
}) {
  return (
    <AriaTabs selectedKey={value} onSelectionChange={(k: Key) => onChange(String(k) as K)}>
      <div className="nd-seg mb-4">
        <TabList aria-label={label} className="nd-seg__track">
          {tabs.map((t) => (
            <Tab key={t.id} id={t.id} className="nd-seg__item inline-flex items-center gap-1.5">
              {t.label}
            </Tab>
          ))}
        </TabList>
      </div>
      {tabs.map((t) => (
        <TabPanel key={t.id} id={t.id}>
          {t.content}
        </TabPanel>
      ))}
    </AriaTabs>
  );
}
