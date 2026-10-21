# 前面板硬件（U60 Pro / MU5250）

[English](HARDWARE.md) · **中文**

触屏界面用到的硬件接口，都是在运行中的设备上通过普通 Linux 接口实测的。用到它们的代码在 `rs/plat/src/`。

## 屏幕：`drm.rs`

| | |
|---|---|
| 设备 | `/dev/dri/card0`，普通 DRM/KMS + dumb buffer |
| 尺寸、格式 | 约 320 × 480，16 位 RGB565 |
| 刷新 | 命令模式面板：画进缓冲区后用 `DRM_IOCTL_MODE_DIRTYFB` 推上去（模式里 `vrefresh=1`，不会自己扫描输出） |
| 方向 | 相对扫描顺序装反了，所以界面整体旋转 180° 画 |
| connector / CRTC | 启动时从 DRM 资源里查，不写死 |

`card0` 同时只能有一个进程占用。两个界面同时打开它，固件最后会让整机重启，所以屏幕只有一个主人（luyaz-sentinel）。

## 触摸：`touch.rs`

启动时在 `/dev/input/event*` 里挑报告多点（或单点）X/Y 轴的节点；目前测过的设备上是 `event3`。轴的范围用 `EVIOCGABS` 读，原始坐标换算成屏幕像素，并和屏幕一样转 180°。
测试时用 `tools/touchsim.c` 往同一个节点写事件。

## 其他输入：`key.rs`、`backlight.rs`

电源键在单独的输入节点上，报 `KEY_POWER`（116）；背光是 sysfs 的 LED `/sys/class/leds/led:lcd`（`brightness`、`max_brightness`）。

## 设备数据从哪来

界面自己不读 ubus 或 uci。luyaz-core 汇总基带、Wi-Fi、电池、短信等状态，提供 `/v2/state` 和 `/v2/events`，界面只读这些。

## 本仓库不包含的东西

不包含、也不链接任何中兴的程序、库、字体或图片。原厂界面只从外部观察过（它打开哪些设备节点），用来了解上面这些接口。Luya-Z 与中兴没有关系。
