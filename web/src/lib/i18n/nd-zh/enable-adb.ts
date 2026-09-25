// New zh strings for /tools/enable-adb (merged into zh by the coordinator).
export const zh = {
  adb: {
    reading: "正在读取 USB 模式…",
    isOn: "ADB 已开启",
    isOnReason: "USB 处于调试模式：任何接上数据线的人都能拿到 root shell。用完请把 USB 切到别的模式。",
    isOff: "ADB 未开启",
    currentMode: "USB 模式：{{mode}}",
    modeUnknown: "设备没报告 USB 模式",
    modeUnknownReason: "设备没有说明当前是哪种 USB 模式。",
    unread: "读不到 USB 模式",
    successSuffix: "。",
    offHint: "要关闭 ADB，在",
    usbPage: "USB 模式页面",
    offHintEnd: "选别的模式。",
    confirmTitle: "启用 ADB 调试 USB 模式？",
    confirmWhat: "这将通过 USB 暴露 ADB，允许对设备进行完整的 shell 访问。仅在你拥有物理访问权限并信任所处环境时才执行此操作。",
    confirmDowntime: "如果你是通过 USB 数据线（USB 网络）打开这个页面的，这条连接会断开。Wi-Fi 和移动网络不受影响。",
    confirmRecovery: "改用 Wi-Fi 连接，在「USB 模式」页面把 USB 切回别的模式。",
    confirmAction: "启用 ADB",
  },
} as const;
