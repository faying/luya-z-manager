// New keys for /alerts (new design). Merged into zh.ts by the coordinator.
export const zh = {
  alerts: {
    reading: "正在读取告警…",
    unreadable: "读不到告警",
    noneState: "没有告警",
    noneReason: "设备一切正常。",
    allRead: "没有新告警",
    listWord: "列表",
    refreshFailed: "列表刷新失败，下面是上一次读到的。",
    smsNumberBad: "只能是数字，开头可以带 +，3 到 20 个字符。",
    turnSmsOff: "关闭短信告警",
    saveNumberAction: "保存号码",
    cNumberOff: "不再发告警短信。告警照样记在这里和触屏上。",
    cNumberOn: "以后的告警会用本机 SIM 发短信到 {{n}}。",
    abroadOnAction: "在国外也发送",
    abroadOffAction: "在国外不发送",
    cAbroadOn: "设备在国外时也会发告警短信，每条都可能产生漫游费。",
    cAbroadOff: "设备在国外时，告警只记在这里，不发短信。",
    settingsWord: "设置",
    refreshToEdit: "，刷新后再改。",
  },
} as const;
