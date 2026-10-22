# 开发说明

[English](DEVELOPMENT.md) · **中文**

触屏界面用 Rust 写，基于 LVGL 9.5 和 FreeType，全部在 `rs/` 里，编出一个静态程序 `luyaz-ui`。

## 东西在哪

**crate**

| 路径 | 是什么 |
|---|---|
| `rs/ui/` | 界面程序本身 |
| `rs/plat/` | 硬件：DRM 显示、触摸、电源键、背光（[HARDWARE.zh-CN.md](HARDWARE.zh-CN.md)） |
| `rs/lvgl-sys/` | 从源码编 LVGL 和 FreeType，加上生成的绑定；配置在 `../lv_conf.h` |
| `rs/ui-model/` | 页面要画的纯数据类型 |

**`rs/ui/src/` 里面**

| 路径 | 是什么 |
|---|---|
| `main.rs`、`lib.rs` | 启动和主循环 |
| `ui/` | 一页一个文件：五个标签页（首页、蜂窝、Wi-Fi、出口、系统）和它们的二级页 |
| `kit.rs`、`theme.rs` | 共用控件；浅色和深色主题；加载字体 |
| `logic.rs` | 不碰 LVGL 的判断逻辑，可以单独做单元测试 |
| `exec.rs` | 换主题或语言后原地重启程序 |
| `conf.rs` | 读写 `/data/luyaz/ui/ui.conf` |
| `data/core_link.rs` | 从 core 拿实时状态：`127.0.0.1:9460` 上的 `/v2/state` 和 `/v2/events` 推送 |
| `data/screen_feed.rs`、`net_view.rs`、`op_view.rs` | 解析 `GET /v2/screen`：首页卡片、状态栏、写入进度行的结论由 core 算好交过来 |
| `data/core_api.rs` | 请求 core 的本机接口（`/v2/api/...`，不带 token），各功能模块都经过它 |
| `data/esim.rs`、`chill.rs`、`tailscale.rs`、`speedtest.rs`、`netinfo.rs`、`scenario.rs`、`alerts.rs` 等 | 功能模块。写操作都经 core，只有 CHILL 的选组、选节点直接找 mihomo（[CHILL.md](CHILL.md)） |

写操作找不到 core 时，界面退而调用设备脚本 `/data/luyaz/scripts/luyaz-fallback.sh`。

**`touch-ui/` 的其他部分**

| 路径 | 是什么 |
|---|---|
| `patches/` | 一个 LVGL 9.5.0 补丁（FreeType 位图模式下的合成粗体），由 `rs/setup-src.sh` 打上 |
| `scripts/` | 设备端脚本：procd 的 `*.init`、`luyaz-ship.sh`（[SHIP.md](SHIP.md)）、`luyaz-recover.sh`、`luyaz-fallback.sh`、`alert-lib.sh`、`doctor.sh`（转给 `luyaz-core doctor`）、`config-backup.sh`、`chaos.sh` 等 |
| `tools/touchsim.c` | 远程测试时注入点按、滑动和按键（[CHILL.md](CHILL.md#不在屏幕前也能验证)） |

屏幕归谁不在这里决定，那是 `crates/luyaz-sentinel` 的事。视觉规则（层级、颜色、点按反馈、确认方式）在 manager 仓库 `docs/DESIGN.md` §4。

## 编译

详见 [README](../README.md#构建)，简单说：

1. `rs/setup-src.sh` 跑一次：把打好补丁的 LVGL 9.5.0 和 FreeType 源码放进一个 Docker 卷。
2. `rs/build.sh device [FILE]`：在共用构建镜像里用 cargo-zigbuild 编出静态的 aarch64-musl `luyaz-ui`（这几个 crate 是 manager Cargo workspace 的成员）。
3. 上设备用 `luyaz ship touch`，它只编 `rs/`。这个命令是维护者自己电脑上的工具，不在本仓库里；它驱动的设备端是 `scripts/luyaz-ship.sh`（装到 `/data/luyaz/scripts/luyaz-ship.sh`）。其他人用 `onboard/` 里的装机包安装。

## 字体

启动时在 `rs/ui/src/theme.rs` 里选：

| 用途 | 先试 | 再退到 |
|---|---|---|
| 中文和正文 | 设备自带的 `/usr/ui/fonts/ZTEZhengYuan.ttf` | `/data/luyaz/ui/fonts/u60-cjk-fallback.ttf`，最后是 LVGL 内置的 Montserrat（没有中文） |
| 数字 | 字体目录里的 `Nunito-600/700/800.ttf`（OFL） | 设备自带的 Roboto，再退到中文字体 |

粗体一律是合成的。日志里 `ui: fonts cjk=… numerals=…` 这一行说明实际用了哪套。兜底中文字体由 `scripts/fonts/build-cjk-fallback.sh <输出目录>` 从 Resource Han Rounded（OFL）裁出来。字体文件不进仓库，由装机包（`UI_FONTS_DIR`）带到设备上。

## 测试

```sh
scripts/test/docker.sh                 # 设备端 shell 脚本，在 busybox 容器里跑，所有命令都打桩
rs/build.sh test                       # Rust 单元测试：逻辑、解析、渲染检查
rs/smoke.sh                            # 离屏运行设备程序，对接一个假的 core
```

要用设备字体的测试，字体需要你自己从设备的 `/usr/ui/fonts/` 拷出来。

## 设备上

| 路径 | 内容 |
|---|---|
| `/data/luyaz/ui/` | `luyaz-ui`（进程名 `luyaz-ui`）、`fonts/`、`touchsim`、`ui.conf`、`chill.conf`、`operator-logos/`、`boot-trace.log`、`vendor-devui.orig-backup`（原厂界面，保留） |
| `/data/luyaz/luyaz-sentinel` | 屏幕主人和看门狗（`/etc/init.d/luyaz-sentinel`；状态在 `/data/luyaz/screen/`） |
| `/data/luyaz/luyaz-core` | core（`/etc/init.d/luyaz-core`；状态在 `/data/luyaz/core/`） |
| `/data/luyaz/scripts/` | `doctor.sh`、`luyaz-ship.sh`、`core-trial.sh`、`luyaz-fallback.sh` 等 |

服务靠 `/etc/rc.local` 里的 `/etc/init.d/<名字> start` 启动，不用 `enable`。

屏幕上显示哪个界面由哨兵决定。`echo vendor > /tmp/luyaz-screen.ctl` 把屏幕交给原厂界面，`echo ui > /tmp/luyaz-screen.ctl` 拿回来。`luyaz-ui` 连续两次起不来，哨兵就放弃、把屏幕还给原厂界面；记录在 `/data/luyaz/screen/attempts` 和 `/data/luyaz/screen/gave-up`。

## 在真机上试新构建

固件有两个行为让这件事有风险：

- 界面一启动就崩溃，固件会把它升级成整机反复重启；
- 屏幕上几分钟没有任何界面，固件也会重启整机。

所以新构建先用另一个文件名跑，确认稳定后才换到正式位置。两个界面绝不能同时占着 `/dev/dri/card0`。

`luyaz ship touch` 把这些都做了（[SHIP.md](SHIP.md)）：哨兵 `hold`、在正式版旁边起 `luyaz-ui.test`、检查、原子替换、`release`、失败就回滚。用装机包的人跑 `./install.sh ui`。

容易踩的坑：

- 找测试进程用 `pidof`（按进程名匹配）。`pgrep -f` 会连跑你这条命令的 SSH shell 一起匹配上并杀掉，后面的步骤都不会执行，屏幕一直空着。
- 哨兵没在运行时，往 `/tmp/luyaz-screen.ctl` 写会卡住。
- 设备上没有 scp 和 sftp，用 `ssh <设备> 'cat > 路径' < 文件` 拷。
