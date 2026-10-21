//! The page table: path, page family, titles, search aliases (DESIGN.md §5 "页面表只在一处定义").
//! Navigation, ⌘K and the colour family all come from here (the old Next.js web's `routes.ts`,
//! which this table used to match, was removed in Phase 10 X13).

use crate::i18n::Lang;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Anchor {
    Home,
    Charts,
    Functions,
    System,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Charts,
    Network,
    WifiLan,
    Services,
    Messages,
    Device,
    Maintenance,
    Tools,
}

pub struct Route {
    pub href: &'static str,
    pub en: &'static str,
    pub zh: &'static str,
    pub icon: &'static str,
    pub group: Group,
    pub aliases: &'static str,
    /// Only in search (reached from its parent page).
    pub hidden: bool,
}

impl Route {
    pub fn title(&self, l: Lang) -> &'static str {
        l.pick(self.en, self.zh)
    }
}

impl Group {
    pub const ALL: [Group; 8] = [
        Group::Charts,
        Group::Network,
        Group::WifiLan,
        Group::Services,
        Group::Messages,
        Group::Device,
        Group::Maintenance,
        Group::Tools,
    ];
    pub fn anchor(self) -> Anchor {
        match self {
            Group::Charts => Anchor::Charts,
            Group::Network | Group::WifiLan | Group::Services | Group::Messages => {
                Anchor::Functions
            }
            Group::Device | Group::Maintenance | Group::Tools => Anchor::System,
        }
    }
    /// `data-family` value (nd.css block colours).
    pub fn family(self) -> &'static str {
        match self {
            Group::Charts => "charts",
            Group::Network => "network",
            Group::WifiLan => "wifiLan",
            Group::Services => "services",
            Group::Messages => "messages",
            Group::Device => "device",
            Group::Maintenance => "maintenance",
            Group::Tools => "tools",
        }
    }
    pub fn title(self, l: Lang) -> &'static str {
        match self {
            Group::Charts => l.pick("Charts", "图表"),
            Group::Network => l.pick("Network & bands", "网络与频段"),
            Group::WifiLan => l.pick("Wi-Fi & LAN", "Wi-Fi 与局域网"),
            Group::Services => l.pick("Services", "服务"),
            Group::Messages => l.pick("Messages", "消息"),
            Group::Device => l.pick("Device", "设备"),
            Group::Maintenance => l.pick("Maintenance", "维护"),
            Group::Tools => l.pick("Tools", "工具"),
        }
    }
    pub fn routes(self) -> impl Iterator<Item = &'static Route> {
        ROUTES.iter().filter(move |r| r.group == self && !r.hidden)
    }
}

impl Anchor {
    pub const ALL: [Anchor; 4] = [
        Anchor::Home,
        Anchor::Charts,
        Anchor::Functions,
        Anchor::System,
    ];
    pub fn href(self) -> &'static str {
        match self {
            Anchor::Home => "/",
            Anchor::Charts => "/charts",
            Anchor::Functions => "/functions",
            Anchor::System => "/system",
        }
    }
    pub fn icon(self) -> &'static str {
        match self {
            Anchor::Home => "House",
            Anchor::Charts => "ChartLine",
            Anchor::Functions => "SquaresFour",
            Anchor::System => "GearSix",
        }
    }
    pub fn title(self, l: Lang) -> &'static str {
        match self {
            Anchor::Home => l.pick("Home", "首页"),
            Anchor::Charts => l.pick("Charts", "图表"),
            Anchor::Functions => l.pick("Functions", "功能"),
            Anchor::System => l.pick("System", "系统"),
        }
    }
    pub fn groups(self) -> impl Iterator<Item = Group> {
        Group::ALL.into_iter().filter(move |g| g.anchor() == self)
    }
    pub fn from_hub(path: &str) -> Option<Anchor> {
        Anchor::ALL
            .into_iter()
            .find(|a| *a != Anchor::Home && a.href() == path)
    }
}

macro_rules! r {
    ($href:literal, $en:literal, $zh:literal, $icon:literal, $g:ident, $aliases:literal) => {
        Route {
            href: $href,
            en: $en,
            zh: $zh,
            icon: $icon,
            group: Group::$g,
            aliases: $aliases,
            hidden: false,
        }
    };
    ($href:literal, $en:literal, $zh:literal, $icon:literal, $g:ident, $aliases:literal, hidden) => {
        Route {
            href: $href,
            en: $en,
            zh: $zh,
            icon: $icon,
            group: Group::$g,
            aliases: $aliases,
            hidden: true,
        }
    };
}

pub static ROUTES: &[Route] = &[
    r!(
        "/signal",
        "Signal",
        "信号",
        "CellSignalFull",
        Charts,
        "信号 xinhao xh rsrp sinr 趋势"
    ),
    r!(
        "/router/signal-detect",
        "Signal Detect",
        "信号检测",
        "Broadcast",
        Charts,
        "信号检测 信号探测 xinhaojiance detect"
    ),
    r!(
        "/tools/speedtest",
        "Speed Test",
        "测速",
        "Gauge",
        Charts,
        "测速 cesu cs speed"
    ),
    r!(
        "/tools/diagnose",
        "Diagnose",
        "网络诊断",
        "Stethoscope",
        Charts,
        "网络诊断 诊断 为什么慢 查原因 慢 卡顿 zhenduan zd diagnose diagnosis slow"
    ),
    r!(
        "/trends",
        "Last 5 Minutes",
        "近 5 分钟",
        "ChartLine",
        Charts,
        "近5分钟 曲线 网速 内存 电池曲线 jin5fenzhong trend trends graph cpu"
    ),
    r!(
        "/usage",
        "Data Usage",
        "流量",
        "SimCard",
        Charts,
        "流量 liuliang 用量 每天 按卡 usage data traffic 流量统计"
    ),
    r!(
        "/clients",
        "Clients",
        "已连设备",
        "Users",
        Charts,
        "在线设备 已连设备 zaixianshebei 终端 clients devices"
    ),
    r!(
        "/router/mobile-network",
        "Mobile Network",
        "移动网络",
        "Network",
        Network,
        "移动网络 yidongwangluo 运营商 飞行模式 mobile carrier airplane"
    ),
    r!(
        "/router/network-mode",
        "Network Mode",
        "网络模式",
        "Path",
        Network,
        "选网 网络模式 xuanwang 5g sa nsa mode"
    ),
    r!(
        "/router/apn",
        "APN",
        "APN",
        "Globe",
        Network,
        "apn 接入点 jierudian jrd"
    ),
    r!(
        "/router/sim",
        "SIM / PIN",
        "SIM / PIN",
        "LockSimple",
        Network,
        "sim pin 卡 ka"
    ),
    r!(
        "/router/dualsim",
        "Dual SIM",
        "双卡",
        "SimCard",
        Network,
        "dual sim 双卡 双卡切换 主卡 内置卡 shuangka",
        hidden
    ),
    r!(
        "/router/esim",
        "eSIM",
        "eSIM",
        "SimCard",
        Network,
        "esim e卡 profile lpac 换卡"
    ),
    r!(
        "/router/qci",
        "QCI / Bearers",
        "QCI / 承载",
        "SlidersHorizontal",
        Network,
        "qci 承载 chengzai bearer"
    ),
    r!(
        "/bandlock",
        "Band Lock",
        "频段锁定",
        "CellTower",
        Network,
        "锁频 suopin sp band bandlock 频段 pinduan"
    ),
    r!(
        "/router/celllock",
        "Cell Lock",
        "小区锁定",
        "Crosshair",
        Network,
        "锁小区 suoxiaoqu sxq celllock cell pci"
    ),
    r!(
        "/router/stc",
        "STC",
        "STC",
        "FadersHorizontal",
        Network,
        "stc 白名单"
    ),
    r!(
        "/router/wifi",
        "Wi-Fi",
        "Wi-Fi",
        "WifiHigh",
        WifiLan,
        "wifi 无线 wuxian ssid 密码"
    ),
    r!(
        "/router/wifi-guest",
        "Guest Wi-Fi",
        "访客 Wi-Fi",
        "UserPlus",
        WifiLan,
        "访客 fangke guest"
    ),
    r!(
        "/router/lan",
        "LAN / DHCP",
        "局域网 / DHCP",
        "Plugs",
        WifiLan,
        "局域网 juyuwang lan dhcp ip"
    ),
    r!(
        "/router/dns",
        "DNS",
        "DNS",
        "HardDrives",
        WifiLan,
        "dns doh 域名"
    ),
    r!(
        "/router/firewall",
        "Firewall",
        "防火墙",
        "Shield",
        WifiLan,
        "防火墙 fanghuoqiang firewall 端口转发 dmz"
    ),
    r!(
        "/router/qos",
        "QoS",
        "QoS",
        "Speedometer",
        WifiLan,
        "qos 限速 xiansu"
    ),
    r!(
        "/router/vpn",
        "VPN Passthrough",
        "VPN 穿透",
        "ShieldCheck",
        WifiLan,
        "vpn 穿透 直通"
    ),
    r!(
        "/router/telemetry",
        "Telemetry Block",
        "遥测拦截",
        "EyeSlash",
        WifiLan,
        "遥测 yaoce telemetry 拦截"
    ),
    r!(
        "/services/chill",
        "CHILL",
        "CHILL",
        "Waves",
        Services,
        "chill 代理 daili 节点 jiedian 梯子 出口 mihomo proxy node"
    ),
    r!(
        "/services/tailscale",
        "Tailscale",
        "Tailscale",
        "Cloud",
        Services,
        "tailscale 远程 yuancheng ts"
    ),
    r!(
        "/router/scenario",
        "Scenarios",
        "情景模式",
        "MapPin",
        Services,
        "情景 qingjing scenario 场景"
    ),
    r!(
        "/sms",
        "SMS",
        "短信",
        "ChatCircleText",
        Messages,
        "短信 duanxin dx sms 消息"
    ),
    r!(
        "/sms/compose",
        "New SMS",
        "写短信",
        "NotePencil",
        Messages,
        "发短信 写短信 compose",
        hidden
    ),
    r!(
        "/sms/forward",
        "SMS Forward",
        "短信转发",
        "ArrowBendUpRight",
        Messages,
        "短信转发 zhuanfa forward telegram"
    ),
    r!(
        "/router/stk",
        "STK / USSD",
        "STK / USSD",
        "DeviceMobile",
        Messages,
        "stk ussd 菜单"
    ),
    r!(
        "/device-info",
        "Device Info",
        "设备信息",
        "HardDrive",
        Device,
        "设备信息 shebeixinxi imei iccid 版本 固件"
    ),
    r!(
        "/health",
        "Health",
        "健康",
        "Heartbeat",
        Device,
        "健康 jiankang health 体检 doctor"
    ),
    r!(
        "/alerts",
        "Alerts",
        "告警",
        "Bell",
        Device,
        "告警 gaojing alerts 警告"
    ),
    r!(
        "/changes",
        "Change log",
        "改动记录",
        "ClockCounterClockwise",
        Device,
        "改动记录 改动 gaidong 撤销 流水账 changes change log undo history"
    ),
    r!(
        "/router/device",
        "Device Control",
        "设备控制",
        "Power",
        Maintenance,
        "设备控制 重启 chongqi reboot 恢复出厂 关机"
    ),
    r!(
        "/scheduler",
        "Scheduler",
        "计划任务",
        "CalendarBlank",
        Maintenance,
        // /router/schedule (定时重启) folded in here (5a, user 10-13); its old address redirects
        "计划任务 jihua cron scheduler 定时重启 dingshi reboot"
    ),
    r!(
        "/usb",
        "USB Mode",
        "USB 模式",
        "Usb",
        Maintenance,
        "usb rndis"
    ),
    r!(
        "/config",
        "Config Tool",
        "配置工具",
        "FileText",
        Maintenance,
        "配置 peizhi config 备份"
    ),
    r!(
        "/settings",
        "Settings",
        "设置",
        "Gear",
        Maintenance,
        "设置 shezhi settings 深色 浅色 语言 theme"
    ),
    r!(
        "/tools/at",
        "AT Terminal",
        "AT 终端",
        "TerminalWindow",
        Tools,
        "at 终端 zhongduan terminal"
    ),
    r!(
        "/tools/cpu",
        "CPU & Memory",
        "CPU 与内存",
        "Cpu",
        Tools,
        "cpu 内存 neicun 频率 memory 占用"
    ),
    r!(
        "/tools/processes",
        "Processes",
        "进程",
        "Cpu",
        Tools,
        "进程 jincheng processes top"
    ),
    r!(
        "/tools/agent",
        "AI Agent Permissions",
        "AI 代理权限",
        "ShieldCheck",
        Tools,
        "ai agent 代理 daili 权限 quanxian token mcp"
    ),
    r!(
        "/tools/enable-adb",
        "Enable ADB",
        "启用 ADB",
        "Bug",
        Tools,
        "adb 调试"
    ),
];

/// Longest-prefix match, so /sms/forward is not also /sms.
pub fn route_for(path: &str) -> Option<&'static Route> {
    let p = if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    };
    ROUTES
        .iter()
        .filter(|r| p == r.href || p.starts_with(&format!("{}/", r.href)))
        .max_by_key(|r| r.href.len())
}

/// Which bottom-tab / rail anchor a path belongs to.
pub fn anchor_for(path: &str) -> Anchor {
    if path == "/" {
        return Anchor::Home;
    }
    Anchor::from_hub(path)
        .or_else(|| route_for(path).map(|r| r.group.anchor()))
        .unwrap_or(Anchor::Home)
}

/// `data-family` for a path.
pub fn family_for(path: &str) -> &'static str {
    if path == "/" {
        return "home";
    }
    match route_for(path) {
        Some(r) => r.group.family(),
        None => match anchor_for(path) {
            Anchor::Charts => "charts",
            Anchor::Functions => "functions",
            Anchor::System => "system",
            Anchor::Home => "home",
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_route_has_an_icon() {
        // (it was also checked against the old web's routes.ts until that was removed, X13)
        for r in ROUTES {
            assert!(
                !crate::icons::path(r.icon, false).is_empty(),
                "icon {}",
                r.icon
            );
        }
    }

    #[test]
    fn prefix_and_family() {
        assert_eq!(route_for("/sms/forward").unwrap().href, "/sms/forward");
        assert_eq!(route_for("/sms/xyz").unwrap().href, "/sms");
        assert_eq!(route_for("/signal/").unwrap().href, "/signal");
        assert_eq!(family_for("/router/wifi"), "wifiLan");
        assert_eq!(family_for("/functions"), "functions");
        assert_eq!(anchor_for("/tools/at"), Anchor::System);
        assert_eq!(anchor_for("/charts"), Anchor::Charts);
    }
}
