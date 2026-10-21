# eSIM 切换

[English](ESIM.md) · **中文**

位置：「蜂窝 → SIM 与 eSIM」。页面列出 SIM 槽里那张可插拔 eUICC 卡（5ber、eSTK.me 之类）上的 profile，点同一个 profile 两次就切过去。屏幕只做切换：下载、删除、改名都在管理网页里。

## 谁在干活

屏幕从不自己跑 lpac。它找 core，由 core（`crates/luyaz-esim`）去调 lpac：

```
界面（rs/devui/src/data/esim.rs）──HTTP 127.0.0.1:9460/v2/api──▶ luyaz-core ──▶ lpac（qmi_qrtr）──▶ eUICC 卡
```

| 请求（前缀 `/v2`，不带 token） | 用途 |
|---|---|
| `GET /api/esim/profiles` | 卡上的列表 |
| `POST /api/esim/switch` | 发起切换 |
| `GET /api/esim/job` | 正在进行的切换的进度 |

启用 profile 只是一次 lpac 调用，慢在让基带用上它：中兴的协议栈只在守护进程启动时读一次 SIM。所以启用之后，core 给 UIM 断电再上电、重启 `zte_topsw_mdm`，等到 IMSI 变了为止，通常不到 10 秒。屏幕只显示进度和结果。

页面需要设备上有 eSIM 组件（`/data/esim` 下的 lpac，用装机包 `./install.sh esim` 装）；没有的话页面会说缺这个组件。

## 值得知道的

- **catBusy。** 有些卡（比如 eSTK.me）偶尔会用 `catBusy` 拒绝切换，过一会儿再试也没用；重启设备或把卡拔出再插回才行。core 只试一次，再读一遍卡确认没变，页面在那个 profile 的行上写「卡忙：重启设备或拔插卡后再切」。
- **成功与否看卡。** core 认的是卡自己报的 profile 列表，不是 lpac 的退出码。切换成功后，它还会删掉同一 profile 已被新通知取代的旧启用/停用通知，等数据通了再把新的发出去；剩下没发的留给管理网页的「发送通知」。
- **远程操作。** 如果你连设备走的正是这台 U60 自己的移动数据，别切到没有流量的 profile：一切过去连接就断了，只能到设备跟前在屏幕上切回来。
