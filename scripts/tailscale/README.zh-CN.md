# 已退役的 Tailscale 集成

Luya-Z 不再安装、启动、发布或恢复 Tailscale。本说明供旧安装的设备主人迁移时
参考，不会自动卸载其他人独立管理的 VPN。

更改设备前，先确认本地管理通道可用，并检查进行中的 ship 事务和
`/data/luyaz/ship/watch/`：旧观察记录可能因 Tailscale 停止而触发回滚。等待观察
结束，或按 `touch-ui/docs/SHIP.md` 的临时 `auto-rollback-off` 流程处理。同时检查
是否仍有 `apply.sh` 或旧 watcher 在运行。

检查 `/etc/rc.local` 中的 `/data/tailscale/start.sh` 和旧版
`/data/tailscale-start.sh`，以及 `/etc/init.d/tailscale`、`/data/tailscale`、
daemon 和 socket。只处理能确认属于旧 Luya-Z 集成的条目和文件。旧版 `tsconfig`
可能含认证密钥，旧状态文件和备份可能含节点身份；决定是否在 daemon 仍运行时注销，
以及如何处理保留的备份。

旧版 `start.sh stop` 会停止 watcher 或 procd 服务并尝试清理 daemon 的网络配置。
应检查实际结果，包括接口、路由规则、table 52 和防火墙链；旧自定义脚本可能还做过
其他更改。不要删除无关路由、防火墙规则或原厂服务。修改 `/etc/rc.local` 和重启须
先获设备主人同意；保留其他启动行，检查编辑后文件的 shell 语法。获准重启后复查管理
通道和网络。

配置恢复仍会校验旧备份，但会报告并跳过其中 `/data/tailscale` 的文件。旧系统备份、
装机快照、旧程序及 ship 回滚副本仍可能含历史脚本或启动行；退役后不要直接恢复它们。
