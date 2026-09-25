// New keys for /services/tailscale (new design). Merged into zh.ts by the coordinator.
export const zh = {
  ts: {
    errorReason: "Tailscale：{{e}}",
    loadFailed: "读不到状态：{{msg}} · 点「重试」",
    stoppedReason: "设备上的 tailscaled 没有在运行。",
    authReason: "打开下面的登录链接，把这台路由器接入 Tailscale。",
    stUnknown: "未知",
    peersCapped: "只显示前 {{n}} 个",
    noPeers: "这个 Tailscale 网络里还没有别的设备",
    noPeersNext: "在另一台设备上登录 Tailscale，它就会出现在这里。",
    handshake: "握手 {{when}}",
    refreshLog: "刷新日志",
    logFailed: "读不到日志：{{msg}}",
  },
} as const;
