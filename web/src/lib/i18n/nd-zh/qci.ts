// New zh strings for /router/qci (merged into zh by the coordinator).
export const zh = {
  qci: {
    reading: "正在读取承载…",
    readingReason: "要经 AT 口询问调制解调器，大约 12 秒。",
    unreadable: "读不到承载",
    noContextsNext: "现在没有移动数据连接。检查移动数据和信号。",
    nActive: "{{n}} 个承载在用",
    refreshFailed: "最近一次刷新失败：{{e}}。显示的是上一次的读数。",
    noDataYet: "还没有数据 · 按上面的「重试」。",
    rawLabel: "AT+CGCONTRDP 原始输出",
    rawNotYet: "还没有输出。",
  },
} as const;
