# CHILL：触屏上的代理面板

CHILL 是触屏上管 mihomo 的那一块：「出口」标签页加上它的 CHILL 二级页。
代码分两层：`rs/devui/src/data/chill.rs` 取数据、发操作，`rs/devui/src/ui/exit.rs` 画界面。
设备上跑的是原生 mihomo，没有装 ShellCrash。

能看到、能做的：内核是否在线、当前出口、档位、实时上下行速率、代理组和组里的节点、
整组测延迟、按「分流组 → 节点」累计的流量，以及家里出口。

## 两条通路

| 内容 | 走哪里 | 为什么 |
|---|---|---|
| 组、节点、连接、延迟，选组和选节点 | 直接请求 mihomo 的控制接口（默认 `127.0.0.1:9999`） | 只读或一次性的小操作，界面自己做最快，也只有界面用 |
| 出口、档位、家里出口 | 经 core：`127.0.0.1:9460/v2/api/services/chill/{exit,profile,home}` | 这些要改配置，统一由 core 负责写和记录，界面不直接动 mihomo 的配置 |

两条都只连本机，不出设备。

## 配置文件

`/data/luyaz/ui/chill.conf`，每行 `键=值`，改完重启触屏界面就生效，不用重编。
文件不存在时全部用默认值，设备上就是这样用的：

| 键 | 默认 | 说明 |
|---|---|---|
| `host` | `127.0.0.1` | 控制接口地址 |
| `port` | `9999` | 控制接口端口 |
| `secret` | 空 | 非空时以 `Authorization: Bearer …` 发出 |
| `group` | `🚀 节点选择` | 打开时先选中的组；页面上可以换，换了以页面为准；组名不存在就用第一个组 |

## 实现要点

- **整组测延迟不能卡住界面。** mihomo 要等最慢的节点回来才回包（请求带 `timeout=3000`，
  一轮可能三秒多），而普通请求的读超时是 1.5 秒。所以测延迟单独处理：请求发出去后连接留着、
  设成非阻塞，每轮刷新读一点，对方关连接时解析结果；8 秒还没读完就放弃。
  测的途中换了组，就丢掉这次请求（结果属于旧组）。日志里可以看到
  `chill: delay test started / done in N ms / gave up`。
- **测出来的延迟要留住。** 页面大约每 5 秒刷新一次节点列表，比一轮测延迟还短；
  刷新时按节点原名把上次测到的延迟带过去，否则结果刚出来就被清掉。
- **选组、选节点不等回包。** 点下去就把请求发出去，下一轮刷新再画 mihomo 实际的状态，
  触摸回调里不阻塞。
- **名字里的 emoji。** 设备字体没有 emoji。国旗是两个区域指示符，正好对应 A–Z，就换成两位国家码（🇯🇵 → JP）；
  其他 emoji 去掉；如果换出来的国家码在名字里已经有了（`🇯🇵 JP2-…`），就不再加前缀，省 320 像素宽的列表空间。
  显示用的名字和调接口用的原名分开存，调接口一定用原名。
- **自动选择的组不能手选。** URLTest、Fallback 这类组由内核自己挑，`PUT` 会被拒；
  列表里这类组显示为灰色（`groups()` 的 `auto_pick`），免得白点。

## 编译和上机

CHILL 是 Rust 触屏的一部分，没有单独的构建：`rs/build.sh device` 编出 `luyaz-ui`，
上机用 `luyaz ship touch`（流程见 [SHIP.md](SHIP.md)）。别手工覆盖正式位置的程序。

怀疑设备上跑的不是最新代码时，按这个顺序查：比 md5 → 对比源码和产物的时间 → 重新编一次，看能不能得到同样的产物。
产物只从仓库往外生成，不要从构建环境往回拷代码。

## 不在屏幕前也能验证

- **先确认屏幕亮着。** `/sys/class/leds/led:lcd/brightness` 为 0 就是息屏；息屏时界面会丢掉所有触摸，
  只认电源键。自动息屏的时长看 `devui.conf` 的 `autooff`，所以一连串操作最好放进同一次 ssh 做完。
  点亮就往电源键所在的输入节点（U60 上是 `/dev/input/event0`）发一次短按：`touchsim /dev/input/event0 key 116`。短按是开关，屏幕本来亮着会被关掉，先看背光再按。
- **模拟触摸。** `tools/touchsim.c` 往输入设备写 evdev 事件。设备上已有 `/data/luyaz/ui/touchsim`；
  自己编：`zig cc -target aarch64-linux-musl -static -Os -o touchsim tools/touchsim.c`（构建镜像里有 zig）。
  用法：`touchsim auto tap X Y`、`touchsim auto swipe X0 Y0 X1 Y1 [毫秒]`、`touchsim NODE key CODE [按住毫秒]`。
  坐标是面板原始坐标：U60 的屏是倒装的，截图上的 `(x, y)` 要换成 `(319-x, 479-y)`。
- **抓屏。** `touch /tmp/luyaz/dumpfb` 之后，界面每刷新一次就把整屏 RGB565（320×480）写到 `/tmp/fb.dump`，
  用 `ssh <设备> 'cat /tmp/fb.dump'` 取回。看完删掉 `/tmp/luyaz/dumpfb`，不然每次刷新都要写 300 KB。
- **检查有没有误选节点。** 看控制接口 `GET /proxies/<URL 编码的组名>` 里的 `now`；被改了就用
  `PUT {"name": "<原节点>"}` 改回去。
