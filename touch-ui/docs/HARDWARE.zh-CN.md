# 硬件接口（U60 Pro / MU5250 前面板）

[English](HARDWARE.md) · **中文**

在运行中的设备上探测标准 Linux 接口得到的结果，记下来方便读代码。**仓库里没有复制任何厂商程序或资源。**

## 屏幕

- 节点：`/dev/dri/card0`（标准 DRM/KMS）。
- 面板：约 320×480，**RGB565（16 位）**。
- 刷新方式：命令模式面板，写完 dumb buffer 后用 `DRM_IOCTL_MODE_DIRTYFB` 推像素（`vrefresh=1`，不是连续扫描）。
- 安装方向：相对帧缓冲扫描顺序转了 180°，所以翻转绘制（`DEVUI_ROTATE_180`）。
- connector / crtc：运行时用 `GETRESOURCES` / `GETCONNECTOR` / `GETENCODER` 自动枚举。实测值（connector 31、crtc 34）只作为 `include/devui_config.h` 里的兜底。
- 同一时间只能有一个程序持有 `/dev/dri/card0`，两个界面抢屏幕会触发整机重启。

## 触摸

- 节点：某个 `/dev/input/event*`（实测 `event3`），启动时扫描带 `ABS_MT_POSITION_X/Y` 或 `ABS_X/Y` 的设备自动找到。
- 原始坐标范围用 `EVIOCGABS` 读出后缩放到屏幕像素；180° 旋转同样作用在触摸坐标上（`DEVUI_TOUCH_ROTATE_180`）。

## 设备数据

设备跑的是 OpenWrt（ZWRT）。信号、Wi-Fi、客户端、电池、短信等状态在 **ubus**（`zwrt_*` 命名空间）和 **uci** 里。
界面不直接读它们，而是读核心（`luyaz-core`）整理好的 `/v2/state`；本项目不链接厂商的 `libzte_*.so`。

## 净室原则

原厂的 `zte_topsw_devui`、`libzte_*.so`、字体、图片只用于本地分析接口，已在 `.gitignore` 里排除。本项目是基于公开 Linux/OpenWrt 接口的独立实现，和中兴没有关系。
