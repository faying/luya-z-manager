# Hardware interfaces (U60 Pro / MU5250 front panel)

**English** · [中文](HARDWARE.zh-CN.md)

Results of probing standard Linux interfaces on a running device, written down to make the code easier to read. **No vendor programs or assets are copied into this repo.**

## Screen

- Node: `/dev/dri/card0` (standard DRM/KMS).
- Panel: about 320×480, **RGB565 (16-bit)**.
- Refresh: command-mode panel; after writing the dumb buffer, pixels are pushed with `DRM_IOCTL_MODE_DIRTYFB` (`vrefresh=1`, not continuous scanout).
- Mounting orientation: rotated 180° relative to the framebuffer scan order, so drawing is flipped (`DEVUI_ROTATE_180`).
- connector / crtc: enumerated automatically at runtime with `GETRESOURCES` / `GETCONNECTOR` / `GETENCODER`. The measured values (connector 31, crtc 34) are only a fallback in `include/devui_config.h`.
- Only one program can hold `/dev/dri/card0` at a time; two UIs fighting over the screen trigger a whole-device reboot.

## Touch

- Node: one of the `/dev/input/event*` devices (measured: `event3`), found automatically at startup by scanning for a device with `ABS_MT_POSITION_X/Y` or `ABS_X/Y`.
- The raw coordinate range is read with `EVIOCGABS` and scaled to screen pixels; the 180° rotation applies to touch coordinates too (`DEVUI_TOUCH_ROTATE_180`).

## Device data

The device runs OpenWrt (ZWRT). Signal, Wi-Fi, clients, battery, SMS and other state live in **ubus** (the `zwrt_*` namespace) and **uci**.
The UI does not read them directly; it reads the `/v2/state` that the core (`luyaz-core`) has put together. This project does not link against the vendor's `libzte_*.so`.

## Clean-room principle

The stock `zte_topsw_devui`, `libzte_*.so`, fonts and images are used only for local interface analysis and are excluded in `.gitignore`. This project is an independent implementation based on public Linux/OpenWrt interfaces and is not affiliated with ZTE.
