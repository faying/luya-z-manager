# CHILL 屏幕控制页（mihomo）

数据和操作模块 `rs/devui/src/data/chill.rs`，界面在「出口」标签和 CHILL 二级页
（`rs/devui/src/ui/exit.rs`）：查看内核状态、出口（经 core 切）、档位、实时速率、代理组和节点、测延迟、按「分流组 → 节点」汇总的流量、家里出口。
旧 litehtml 版的 `{{SC_*}}` 令牌、`act:sc*` 动作、`ui/functions/chill.html`、切模式和「重启内核」按钮都已删除，留在 git tag `legacy-litehtml`。

## 命名：屏幕上叫 CHILL

设备上显示的名字、配置文件 `chill.conf`、源码模块都叫 CHILL。后端是原生 mihomo（设备上没装 ShellCrash）。

## 为什么直接读 clash API，而不是走 core（luyaz-core）

选节点、测延迟这些操作本来就在触屏二进制里，数据也放在同一个模块里，只维护一处。读的仍然是
**本机** HTTP 接口（`127.0.0.1:9999`），与「不直接调 ubus」的初衷一致。出口、档位、家里出口这些
会改配置的操作走 core（`127.0.0.1:9460/v2/api/services/chill/*`），不直接写 mihomo。

## 配置

`/data/luyaz/ui/chill.conf`，**改完不用重编**，重启触屏界面即生效；不放这个文件时用下面的默认值
（设备上就是这样，见项目规则）：

```
port=9999
secret=
group=🚀 节点选择
```

另有 `host=`（默认 `127.0.0.1`）。`group` 只是**默认**选中的组；页面上可以随时切换，切换后以页面选择为准。
若配置里的组名不存在，自动退回第一个组。

## 实现上的几个坑

**组测延迟必须异步。** 内核要等最慢的节点（`timeout=3000` ⇒ 最长 3 秒才回包），而
`sc_http()` 的读超时是 1.5 秒：同步等会在结果回来之前放弃，界面报「测试失败」——
测试设备上一半节点慢时必现（实测一轮 3.2 秒）。又不能让 UI 线程干等 3 秒，所以连上、
发完请求后把 socket 留着（非阻塞），每次 `chill_poll()` 顺手收一点，对端关闭
（`Connection: close`）就解析；8 秒没收完放弃。换组时丢弃进行中的请求（结果属于旧组）。
日志里有 `chill: delay test started/done in N ms` 可查。

**`sc_http()` 返回静态缓冲区。** 下一次调用会覆盖上一次的返回内容。要用同一份响应里的
多个字段，必须在发下一个请求之前全部取完。追出口链路时会嵌套调用，最初写成「边解析边
发请求」，结果节点列表读不到、界面空白——症状出现在**别的功能**上，很难联想。

**延迟结果要跨刷新保留。** 刷新每 2 秒一次，而一轮组测延迟要好几秒。若刷新时无条件清零，
测完的结果会立刻被抹掉，界面上永远看不到。现在按原名把已有结果继承过来。

**emoji 必须净化。** 设备字体没有 emoji 字形，节点名里的 🇯🇵/🚀 会渲染成豆腐块。
国旗由两个「区域指示符」组成，正好对应 A–Z，还原成国家码字母（🇯🇵 → JP）既能显示又不丢
信息；其余 emoji 丢弃。**显示名和 API 用名分开存**——调接口必须用原名，否则服务端认不出。
国家码若与名字重复（`🇯🇵 JP2-…`）会去掉前缀，320px 的列表宽度很紧张。

**URLTest / Fallback 组不可手选。** 这类组由内核自动挑选，`PUT` 会被拒绝。组列表里把这类组标灰
（`groups()` 的 `auto_pick`），避免用户白点。

## 构建

CHILL 页编进 Rust 版触屏（`rs/devui/src/data/chill.rs`），按 README「构建」用 `rs/build.sh device` 编（C 版 10-15 删除，tag `legacy-c-touch-ui`）。
旧 litehtml 版的构建（`scripts/build.sh`、`remote-build.sh`、静态 litehtml v0.10）和它的 `ui/functions/chill.html` 部署方式
已随 litehtml 渲染器删除，留在 git tag `legacy-litehtml`。
上机走 `tools/luyaz ship touch`（`docs/SHIP.md`），不要手工覆盖正式位置。

**构建产物只能从仓库单向生成，永远不从构建机往回拿代码。** 怀疑「设备上跑的不是最新代码」时：
先比 md5，再看源码与产物的时间戳，最后重新编一次看能不能复现出同样的产物。

## 远程验证（不用凑到屏幕前）

* **模拟触摸**：`tools/touchsim.c`（X5 前在 src/）往 evdev 节点写事件，单独编（构建镜像里的 zig）：
  `zig cc -target aarch64-linux-musl -static -Os -o touchsim tools/touchsim.c`，推到 `/tmp/touchsim`（设备上已有一份 `/data/luyaz/ui/touchsim`），
  `/tmp/touchsim auto tap X Y` / `auto swipe X0 Y0 X1 Y1 MS`。**坐标是面板原始坐标**：
  U60 倒装 180°，截图里看到的 `(x,y)` 要换成 `(319-x, 479-y)` 再送进去。
* **抓屏**：`touch /tmp/luyaz/dumpfb` 后每次刷屏会把整屏 RGB565（320×480）写到 `/tmp/fb.dump`，
  用 `ssh u60 'cat /tmp/fb.dump'` 取回。**验完删掉 `/tmp/luyaz/dumpfb`**，否则每次刷屏都写 300KB。
* **先看背光**：`/sys/class/leds/led:lcd/brightness` 为 0 就是熄屏，**熄屏时所有触摸被丢弃、
  只有电源键能唤醒**，模拟触摸看起来像「没反应」。自动熄屏由 `devui.conf` 的 `autooff`
  决定（示例设备是 30 秒），一串操作要在一次 ssh 会话里做完。用 shell 注入电源键短按：

  ```sh
  # struct input_event（aarch64 共 24 字节）：16 字节时间戳 + type(2) + code(2) + value(4)
  # EV_KEY=1, KEY_POWER=116(\164)；按下 → SYN → 150ms → 抬起 → SYN
  { printf '\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\001\0\164\0\001\0\0\0'
    printf '\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0'; } > /dev/input/event0
  sleep 0.15
  { printf '\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\001\0\164\0\0\0\0\0'
    printf '\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0'; } > /dev/input/event0
  ```
  短按是「切换」，先确认背光为 0 再注入，否则会把亮着的屏幕关掉。
* 验证节点选择有没有被误触：对照 clash API
  `curl 127.0.0.1:9999/proxies/<urlencode 组名>` 里的 `now`，改了再 `PUT {"name":...}` 恢复。
