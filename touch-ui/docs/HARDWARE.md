# Front-panel hardware (U60 Pro / MU5250)

**English** · [中文](HARDWARE.zh-CN.md)

What the touch UI talks to, as measured on a running device through ordinary
Linux interfaces. The code that uses it lives in `rs/plat/src/`.

## Display — `drm.rs`

| | |
|---|---|
| Device | `/dev/dri/card0`, plain DRM/KMS with a dumb buffer |
| Size, format | about 320 × 480, 16-bit RGB565 |
| Updates | command-mode panel: draw into the buffer, then flush it with `DRM_IOCTL_MODE_DIRTYFB` (the mode reports `vrefresh=1`; nothing scans out on its own) |
| Orientation | mounted upside down relative to scan order, so the UI renders rotated 180° |
| Connector / CRTC | looked up at start-up from the DRM resources; nothing hard-coded |

Only one process may own `card0`. If two UIs open it at once the firmware ends
up rebooting the whole device, which is why the screen has a single owner
(luyaz-sentinel).

## Touch — `touch.rs`

At start-up the UI picks the `/dev/input/event*` node that reports multi-touch
(or single-touch) X/Y axes; on the devices measured so far that was `event3`.
The axis ranges come from `EVIOCGABS`, raw points are scaled to screen pixels,
and the same 180° turn as the display is applied.
`tools/touchsim.c` writes events into the same node for tests.

## Other inputs — `key.rs`, `backlight.rs`

The power key arrives as `KEY_POWER` (116) on its own input node; the backlight is
the sysfs LED `/sys/class/leds/led:lcd` (`brightness`, `max_brightness`).

## Where device data comes from

The UI never reads ubus or uci itself. luyaz-core collects modem, Wi-Fi,
battery and SMS state and serves it as `/v2/state` and `/v2/events`; the UI
only consumes that.

## What is not in this repository

No ZTE program, library, font or image is included or linked. The stock UI was
only observed from outside (which device nodes it opens) to learn the
interfaces above. Luya-Z is not affiliated with ZTE.
