// New zh strings for /router/signal-detect (merged into zh by the coordinator).
export const zh = {
  sigdetect: {
    quality: "基带报告的质量：",
    idle: "没在检测",
    idleReason: "按「开始检测」测量当前的信号质量。",
    running: "正在检测…",
    runningPct: "正在检测 · {{pct}}%",
    runningReason: "调制解调器正在测量信号质量。可以切到别的标签页，进度会继续查。",
    progressErr: "读不到进度：{{e}}。每 2 秒会再试一次。",
    progressWhat: "进度",
    stoppedReason: "已经测到的结果在下面。",
    statusLabel: "检测状态",
    startConsequence: "调制解调器开始测量信号质量，可能要几分钟，随时可以停止。",
    stopConsequence: "马上结束这次测量，并读取已经测到的结果。",
    stepStop: "停止",
    stepResults: "读取结果",
    resultsErr: "没读到结果：{{e}}",
    loadingResults: "正在读取结果…",
    resultsPending: "检测完成后，结果会显示在这里。",
    noResultYet: "还没有结果 · 按「开始检测」",
    noResultsNext: "可以再检测一次。",
  },
} as const;
