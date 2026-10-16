# eSIM 切换页

[English](ESIM.md) · **中文**

触屏「蜂窝 → SIM 与 eSIM」列出 SIM 槽里那张可插拔 eUICC 卡（5ber、eSTK.me 这类）上的 profile，点两次切换。
**只做切换**；下载、删除、改名在管理网页里做。

## 数据流：调 core，不自己跑 lpac

```
lpac（qmi_qrtr）◀── luyaz-core（crates/luyaz-esim）──HTTP 127.0.0.1:9460/v2/api──▶ rs/devui/src/data/esim.rs ──▶ 界面
```

用到的接口（core 本机端口上，前缀 `/v2`）：`GET /api/esim/profiles`、`POST /api/esim/switch`、`GET /api/esim/job`（查切换进度）。（10-15 之前界面调的是 :9090 的 zte-agent。）

切换本身只是一条 `lpac profile enable`，难的是让中兴的协议栈认到新卡：它只在守护进程启动时读一次 SIM，
所以 core 会给 UIM 重新上电、重启 `zte_topsw_mdm`、等 IMSI 变化，一般 10 秒内完成。这些都在 core 里做，界面只显示进度和结果。

前提：设备上装了 eSIM 组件（`/data/esim` 的 lpac，装机包 `./install.sh esim`），没装时页面会提示。

## 登录

不用登录：core 的本机端口不要 token。（旧的 `esim.conf`、`/data/zte-agent.env` 密码界面已不再读。）

## 注意

- 有些卡（如 eSTK.me）切换时偶尔回 `catBusy`。等和重试都清不掉，重启设备（或拔插卡）才行。core 只试一次，读卡确认没切过去，屏幕在那张配置的行上写「卡忙：重启设备或拔插卡后再切」。
- 切没切成按卡自己的配置列表判断，不看 lpac 的返回。切换成功后，core 删掉同一配置已被更新一条盖过的旧启用/停用通知，等数据通了再把这次切换的通知发出去（没通就留着，在网页「发送通知」里发）。
- 如果你是通过这台 U60 自己的网络远程操作，别切到没有流量的 profile，切过去就连不回来了，只能到设备跟前在屏幕上切回。
