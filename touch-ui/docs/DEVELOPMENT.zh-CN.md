# 开发说明

[English](DEVELOPMENT.md) · **中文**

当前的触屏界面是 `rs/` 里的 **Rust 版**（Rust + LVGL v9.5 + FreeType；设备上的程序叫 `luyaz-ui`）。C/LVGL 版（`src/`、`include/`、`make`、`scripts/build-docker.sh`）
2026-10-15 已删除，留在 git tag `legacy-c-touch-ui`；更早的 litehtml 版在 tag `legacy-litehtml`。

## 代码结构

| 文件 | 作用 |
|---|---|
| `rs/devui/src/main.rs`、`lib.rs` | 入口：DRM、触摸、按键、背光，主循环 |
| `rs/devui/src/ui/*.rs`、`kit.rs`、`theme.rs` | 5 个标签和各子页面的布局（一页一个文件）、通用控件、浅色/深色主题和字体加载 |
| `rs/devui/src/logic.rs`、`exec.rs` | 不依赖 LVGL 的判断逻辑（可单独测试）；`exec.rs` 在换主题或语言时重新 exec 界面 |
| `rs/devui/src/data/core.rs`、`json.rs` | 从核心（`luyaz-core`，`127.0.0.1:9460` 的 `/v2/state` + `/v2/events`）读数据；写操作走它的 `/control`，只有核心连不上时才跑应急脚本 `/data/luyaz/scripts/luyaz-fallback.sh` |
| `rs/devui/src/data/screen_feed.rs`、`net_view.rs`、`op_view.rs` | 首页卡片/状态栏的结论和写操作事务行由核心的 `GET /v2/screen` 算好（规则在 manager `crates/luyaz-core`），这里只解析 |
| `rs/devui/src/data/core_api.rs` | 核心本机接口 `127.0.0.1:9460/v2/api/...` 的请求（不要 token、不登录）；各功能模块都走它 |
| `rs/devui/src/data/{esim,chill,tailscale,speedtest,netinfo,scenario,alerts}.rs` | 各功能的后端：eSIM / APN 等写操作走核心；CHILL 读 mihomo 控制接口 |
| `rs/plat/` | 硬件接口（DRM、触摸、按键、背光），见 [HARDWARE.zh-CN.md](HARDWARE.zh-CN.md) |
| `rs/lvgl-sys/`、`rs/devui-model/` | LVGL/FreeType 绑定和编译；页面要画的数据模型 |
| `rs/devui/src/conf.rs` | `devui.conf`（`/data/luyaz/ui/devui.conf`） |
| `patches/` | 给 LVGL v9.5.0 的小补丁（FreeType 位图模式的合成粗体），由 `rs/setup-src.sh` 打 |
| `scripts/` | 设备端脚本：`*.init`（procd）、`luyaz-ship.sh`（装成 `luyaz-ship.sh`）、`luyaz-recover.sh`（装成 `luyaz-recover.sh`）、`luyaz-fallback.sh`、`alert-lib.sh`、`doctor.sh`（`<core> doctor` 的薄壳）、`config-backup.sh`、`chaos.sh` 等 |

屏幕的主人不在这里：是 manager 的 `crates/luyaz-sentinel`（10-15 之前是 `u60-uid`，`src/uid.c`）。

界面的设计规则（层级、颜色、点击反馈、确认方式）在 manager 仓库 `docs/DESIGN.zh-CN.md` §4。

## 构建

见 [README](../README.md#构建)。要点：

- 先跑一次 `rs/setup-src.sh`：把打过补丁的 LVGL v9.5.0 和 FreeType 源码放进 docker 卷。
- `rs/build.sh device [FILE]` 编静态 aarch64-musl 的 `luyaz-ui`（共用构建镜像里 cargo zigbuild；这些 crate 是 manager Cargo workspace 的成员）。
- 上机：`luyaz ship touch`（只编 `rs/`）。注：Mac 端的上机命令（`luyaz ship <组件>`、`luyaz rollback`、`luyaz status`）是维护者自己工作区的工具，不在本仓库里；它驱动的设备端是 `touch-ui/scripts/luyaz-ship.sh`（装到 `/data/luyaz/scripts/luyaz-ship.sh`），格式见 [SHIP.md](SHIP.md)。没有这个工具就用装机包（`onboard/`）。

## 字体

加载顺序在 `rs/devui/src/theme.rs`：

| 用途 | 首选 | 找不到时 |
|---|---|---|
| 中文和正文 | 设备自带 `/usr/ui/fonts/ZTEZhengYuan.ttf`（运行时加载，仓库不带） | `/data/luyaz/ui/fonts/u60-cjk-fallback.ttf`（新目录没有时用旧的 `/data/plugins/u60pro-devui/fonts/`）→ LVGL 自带 Montserrat（没有中文） |
| 数字 | `fonts/Nunito-600/700/800.ttf`（OFL） | 设备自带 Roboto → 中文字体 |

粗体都是合成的。启动日志里 `ui: fonts cjk=… numerals=…` 说明实际用了哪套。
中文兜底字体由 `scripts/fonts/build-cjk-fallback.sh <输出目录>` 从 Resource Han Rounded（OFL）裁出来。
字体文件都不进仓库，由装机包（`DEVUI_FONTS_DIR`）带到设备。

## 测试

```sh
scripts/test/docker.sh                 # 设备端 shell 脚本，busybox 容器里跑，命令全部打桩
rs/build.sh test                       # Rust 单元测试（逻辑、数据解析、渲染检查）
rs/smoke.sh                            # 设备程序离屏端到端跑一遍，对假核心
rs/parity.sh MODEL_DIR REF_DIR OUT_DIR # 和 C 版渲染测试逐像素对拍（C 版从 tag legacy-c-touch-ui 检出）
```

设备字体要从设备 `/usr/ui/fonts/` 自己拷，不进仓库。

## 设备上的布局

| 路径 | 内容 |
|---|---|
| `/data/luyaz/ui/luyaz-ui` | 界面程序（正式位置；进程名 `luyaz-ui`） |
| `/data/luyaz/ui/fonts/`、`/data/luyaz/ui/touchsim` | Nunito 和中文兜底字体；模拟触摸工具 |
| `/data/plugins/u60pro-devui/` | 已退役（Luya-Z 改名 F3）：`start.sh` 删了，开机记录和原厂界面备份是 `/data/luyaz/ui/boot-trace.log`、`/data/luyaz/ui/vendor-devui.orig-backup` |
| `/data/luyaz/luyaz-sentinel`、`/etc/init.d/luyaz-sentinel` | 屏幕主人兼看门狗；屏幕状态在 `/data/luyaz/screen/` |
| `/data/luyaz/luyaz-core`、`/etc/init.d/luyaz-core` | 核心（数据服务；状态在 `/data/luyaz/core/`） |
| `/data/luyaz/scripts/` | 设备脚本（`doctor.sh`、`luyaz-ship.sh`、`core-trial.sh`、`luyaz-fallback.sh` 等） |

开机自启只走 `/etc/rc.local` 里的 `/etc/init.d/<名字> start`，不用 `enable`。

屏幕的控制：`echo vendor > /tmp/luyaz-screen.ctl` 把屏幕交给原厂界面，`echo devui > /tmp/luyaz-screen.ctl` 换回来。
哨兵连续两次拉不起界面会放弃并交还原厂界面，
计数在 `/data/luyaz/screen/attempts`、`/data/luyaz/screen/gave-up`。

## 在真机上试新版本

两条硬约束：**一启动就崩的界面会被固件升级成整机重启循环**；**屏幕上几分钟没有任何界面，固件也会整机重启**。
所以新版本先用别的文件名试跑，确认稳定后再换正式位置；两个界面也不能同时抢 `/dev/dri/card0`。

用 `luyaz ship touch`（见 [SHIP.md](SHIP.md)）：它做的就是这些（哨兵 `hold`、旁路 `luyaz-ui.test`、检查、原子替换、`release`、退回）。普通用户用装机包 `./install.sh devui`。

注意：

- 用 `pidof`（按进程名）找测试进程，别用 `pgrep -f`：后者会匹配到这条 SSH 命令自己的 shell，把它杀掉，后面的步骤就不跑了，屏幕会一直空着。
- 哨兵不在跑的时候别往 `/tmp/luyaz-screen.ctl` 写东西，会卡住。
- 设备上没有 scp/sftp，传文件用 `ssh … 'cat > 路径' < 本地文件`。
