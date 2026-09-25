"use client";
// Component sheet for the new design. Exists only under `next dev`
// (pageExtensions "dev.tsx"); never exported to web/out.
import { useState } from "react";
import { ChatCircleText as MessageSquare, HardDrive as Gauge, WifiHigh as Wifi } from "@phosphor-icons/react";
import { I18nProvider } from "@/components/nd/shell/I18nProvider";
import {
  Button, ConfirmDialog, ConfirmInline, ConsoleBand, Freshness, Group, GroupTitle, ModuleCard,
  Readout, ReadoutWall, Row, Segmented, StatusBlock, Switch, ToastProvider, useToast,
  useConfirmInline,
} from "@/components/nd";
import { applyTheme, readThemeChoice, type ThemeChoice } from "@/lib/theme";

function Sheet() {
  const toast = useToast();
  const [theme, setTheme] = useState<ThemeChoice>(() => readThemeChoice());
  const [wifi, setWifi] = useState(true);
  const [ci, setCi] = useState(false);
  const inline = useConfirmInline(ci);
  const [dlg, setDlg] = useState<null | "lock" | "reset">(null);

  return (
    <main className="nd min-h-dvh bg-nd-bg px-4 py-6 sm:px-8">
      <div className="mx-auto grid max-w-[1120px] gap-8">
        <header className="flex flex-wrap items-center justify-between gap-4">
          <h1 className="nd-title">/dev-kit · 新设计组件</h1>
          <Segmented
            label="主题"
            value={theme}
            onChange={(v) => { setTheme(v); applyTheme(v); }}
            options={[{ id: "system", label: "跟系统" }, { id: "light", label: "浅色" }, { id: "dark", label: "深色" }]}
          />
        </header>

        <section className="grid gap-3">
          <GroupTitle>状态块</GroupTitle>
          <StatusBlock tone="ok" state="信号良好" meta="5G SA · 中华电信 · 3 个 NR 载波" />
          <StatusBlock tone="warn" state="信号偏弱" reason="RSRP -112 dBm" actions={<Button variant="secondary" size="sm">去锁频 ›</Button>} />
          <StatusBlock tone="bad" state="设备连接中断" reason="数字停在 14:32 · 正在重试" actions={<Button variant="secondary" size="sm">立即重试</Button>} />
          <StatusBlock tone="neutral" state="正在连接设备…" />
          <StatusBlock tone="stale" state="信号与网速暂未更新" reason={<Freshness stale lastOkAt={STALE_AT} />} />
        </section>

        <section className="grid gap-3">
          <GroupTitle>读数墙</GroupTitle>
          <ReadoutWall label="关键读数">
            <Readout wide hero label="聚合带宽 · MHz" value="240" unit="MHz" sub="3 个 NR 载波" />
            <Readout label="RSRP" value="-95" unit="dBm" sub="RSRQ -11 · 良好" />
            <Readout label="SINR" value="18.5" unit="dB" sub="良好" />
            <Readout label="下行" value="312" unit="Mbps" sub="↓" />
            <Readout label="上行" value="42.1" unit="Mbps" sub="↑" />
            <Readout label="温度" value={undefined} unit="°C" sub="加载中" />
            <Readout label="电量" value={null} sub="没有数据" stale />
          </ReadoutWall>
        </section>

        <section className="grid gap-6 lg:grid-cols-2">
          <div>
            <Group title="分组列表">
              <Row icon={Wifi} label="Wi-Fi" value="U60-5G · 36 · 160 MHz" href="/router/wifi" />
              <Row icon={MessageSquare} label="短信" sub="2 条未读" href="/sms" />
              <Row icon={Gauge} label="IMEI" value="490154203237518" mono />
              <Row label="Wi-Fi 开关" control={<Switch label="Wi-Fi 开关" isSelected={wifi} onChange={setWifi} />} />
              <Row label="禁用的开关" control={<Switch label="禁用" isSelected={false} onChange={() => {}} isDisabled />} />
            </Group>
          </div>
          <div className="grid gap-3">
            <ModuleCard href="/router/scenario" title="情景">
              <p className="nd-body">在家 · 14:05 切换</p>
            </ModuleCard>
          </div>
        </section>

        <section className="grid gap-3">
          <GroupTitle>按钮与确认</GroupTitle>
          <div className="flex flex-wrap gap-2">
            <Button>主按钮</Button>
            <Button variant="secondary">次按钮</Button>
            <Button variant="confirm">确认：关闭 Wi-Fi</Button>
            <Button variant="danger">恢复出厂</Button>
            <Button variant="ghost">文字按钮</Button>
            <Button pending>提交中</Button>
            <Button isDisabled>不可用</Button>
            <Button variant="secondary" onPress={() => toast.show("bad", "没保存：设备 10 秒没有回应 · 重试")}>失败提示</Button>
          </div>
          <div className="max-w-[560px]">
            <Button variant="secondary" onPress={() => setCi((o) => !o)} {...inline.triggerProps}>全部直连</Button>
            <ConfirmInline
              id={inline.id}
              open={ci}
              actionLabel="全部直连"
              consequence="全屋流量改为直连，AI 和 VoWiFi 也直连。随时可以再打开。"
              onConfirm={() => { setCi(false); toast.show("ok", "已改为全部直连"); }}
              onCancel={() => setCi(false)}
            />
          </div>
          <div className="flex flex-wrap gap-2">
            <Button variant="secondary" onPress={() => setDlg("lock")}>第三档：锁频</Button>
            <Button variant="secondary" onPress={() => setDlg("reset")}>第三档：恢复出厂</Button>
          </div>
          <ConfirmDialog
            open={dlg === "lock"}
            onOpenChange={(o) => !o && setDlg(null)}
            title="锁定 NR 频段 n78？"
            what="只用 n78 连网，其他频段不再使用。"
            downtime="网络会断开约 30 秒。"
            recovery="在锁频页点「恢复默认」即可解除。"
            actionLabel="锁定 n78"
            cutsUplink
            onConfirm={() => setDlg(null)}
          />
          <ConfirmDialog
            open={dlg === "reset"}
            onOpenChange={(o) => !o && setDlg(null)}
            title="恢复出厂设置？"
            what="清除全部设置，包括 Wi-Fi、APN 和本项目装的程序。不能撤销。"
            downtime="设备重启约 90 秒。"
            recovery="需要重新装机。"
            actionLabel="恢复出厂"
            typeToConfirm="恢复出厂"
            danger
            cutsUplink
            onConfirm={() => setDlg(null)}
          />
        </section>

        <section className="grid gap-3">
          <GroupTitle>深色带</GroupTitle>
          <ConsoleBand label="AT 终端">
            <div>&gt; AT+CSQ</div>
            <div className="nd-console__muted">+CSQ: 24,99</div>
            <div className="nd-console__muted">OK</div>
          </ConsoleBand>
        </section>
      </div>
    </main>
  );
}

// Dev kit only: a fixed "3 minutes ago" for the stale sample.
const STALE_AT = Date.now() - 180000;

export default function DevKit() {
  return (
    <I18nProvider>
      <ToastProvider>
        <Sheet />
      </ToastProvider>
    </I18nProvider>
  );
}
