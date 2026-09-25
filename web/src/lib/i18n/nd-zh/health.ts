// New keys for /health (new design). Merged into zh.ts by the coordinator.
export const zh = {
  health: {
    reading: "正在读取设备检查结果…",
    unreadable: "读不到设备检查结果",
    couldNotRun: "检查没能运行",
    notYetState: "还没检查过",
    runCheck: "运行检查",
    runConsequence: "现在在设备上运行一遍健康检查脚本（doctor.sh），最长约 20 秒。",
    checkDone: "检查完成",
    checkDoneAt: "{{time}} 检查完成",
    noChecksYet: "还没有结果。点「现在检查」运行一遍。",
    noChecks: "这次检查没有返回任何项目。",
    showLogOf: "查看 {{program}} {{time}} 的崩溃日志",
    hideLogOf: "收起 {{program}} {{time}} 的崩溃日志",
    logOf: "{{program}} 的崩溃日志",
    levelOk: "正常",
    levelWarn: "需要注意",
    levelBad: "异常",
  },
} as const;
