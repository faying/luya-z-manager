// New keys for /sms/compose (new design). Merged into zh.ts by the coordinator.
export const zh = {
  smscompose: {
    accepted: "设备已接受：发给 {{to}} 的短信",
    descNd: "用设备里的 SIM 卡经移动网络发送，运营商可能按条收费。",
    errBadNumber: "号码只能是数字，可以用 + 开头，3 到 20 位。",
    rejected: "设备没有发出去（result {{code}}）",
    sendAction: "发给 {{to}}",
    sendConsequence: "把 {{chars}} 个字符发给 {{to}}，分 {{parts}} 条短信经移动网络发出，运营商可能按条收费。发出后没法撤回。",
    stepSend: "发送短信",
  },
} as const;
