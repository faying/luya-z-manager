// New keys for /login (new design). Merged into zh.ts by the coordinator.
export const zh = {
  login: {
    loading: "正在加载…",
    noReply: "设备没有回应。检查网络，或「高级」里的 Agent 地址，再试一次。",
    failedWith: "登录失败：{{msg}}",
    agentUrlHelp: "只有这个页面不是由设备本身提供时才需要填。保存在本浏览器里。",
    statusTitle: "设备状态",
    unreachable: "连不上设备",
    unreachableNext: "检查网络（或「高级」里的 Agent 地址）后重试。",
  },
} as const;
