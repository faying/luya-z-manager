# 设备端测速

管理网页 `/tools/speedtest` 和触屏共用核心测速引擎。流量从设备发出，
不是从浏览器发出。结果注明实际来源和节点；不同来源的结果不能直接等同。

## 来源选择

- **自动**：先尝试 Ookla；获取列表、延迟测试或传输失败后，尝试已配置的
  LibreSpeed 节点。每个来源最多尝试三个节点。
- **Ookla / LibreSpeed**：固定来源，失败时不会换到另一个来源。
- **指定节点**：固定来源和节点，节点编号按来源区分。旧的仅含 `server_id`
  的请求仍代表 Ookla 节点。

默认不内置 LibreSpeed 节点。要启用备用来源，请添加设备网络可访问、
且允许进行测速的服务器，例如自建节点或运营商提供的节点。
能上网不代表一定能访问某个测速服务；俄罗斯网络的可用性需要在当地验证。

## 配置 LibreSpeed

管理网页测速页展开「配置节点」，编辑 JSON，保存并确认。网页会读回核对。
保存不会启动测速，触屏也使用同一份节点配置。

```json
{
  "librespeed_servers": [
    {
      "id": 1,
      "name": "My server",
      "server": "https://example.com/speedtest/",
      "dlURL": "garbage.php",
      "ulURL": "empty.php",
      "pingURL": "empty.php"
    }
  ]
}
```

`example.com` 是示例，不能用来测速。路径按实际后端填写；Go 后端通常使用
`garbage` 和 `empty`。最多八个节点，编号必须是唯一正整数，名称最多 64 个
UTF-8 字节。基础地址必须以 `/` 结尾；支持 HTTP/HTTPS，不接受内嵌账号密码、
片段或跨来源的端点路径。将 `librespeed_servers` 设为 `[]` 可清空备用节点。

配置保存在核心状态目录的 `speedtest.json`，通常是
`/data/luyaz/core/speedtest.json`，不依赖远端 LibreSpeed 目录。
文件无法读取或内容无效时，网页会提示，不会自动覆盖原文件。

## API 与错误

以下路径位于核心 `/v2` 下：

- `GET /api/speedtest/config`：返回 `{ok,data:{config,load_error}}`。
- `PUT /api/speedtest/config`：提交上面的配置对象，验证后保存，沿用兼容层写入开关。
- `GET /api/speedtest/servers`：立即读取缓存并返回 `{ok,data:[...],loading}`。
  初次列表为空且 `loading:true` 不是故障，应继续轮询。列表和开始请求共用
  一次后台更新；失败后普通列表读取退避 30 秒，明确开始测速可重新尝试。
  列表的「重试」使用 `?refresh=1` 跳过退避，但不重复正在执行的更新。
  没有可用节点且获取失败时，返回结构化的测速服务错误。
- `POST /api/speedtest/start`：接受 `{provider:"auto"|"ookla"|"librespeed",server_id?}`。
  准入后立即返回 `200` 和 `status:"started"`。找节点及节点不存在等错误由进度返回。
  已有测速或诊断占用时，仍立即返回 409。
- `GET /api/speedtest/progress`：新增 `discovering` 阶段及 `provider`、`error_code`、
  `error`、`error_en`、`fallback_reason`。外部测速失败不代表核心离线。
- `POST /api/speedtest/stop`：也会取消找节点和后续尝试。已经发出的请求按剩余
  超时退出：找节点、下载、延迟最多 3 秒，上传最多 10 秒。

错误码包括 `provider_unreachable`、`provider_timeout`、`no_servers`、
`invalid_server_list`、`provider_not_configured`、`transfer_failed`、`budget_exhausted`。
超时无法证明网络封锁，所以界面只提示可能受限。传输失败不会显示成成功且 0 Mbps。

完整测速的所有尝试共用 60 秒和 512 MB 测试内容上限。字节计数包含失败尝试，
速率属于当前选择的尝试，协议开销另计。诊断加测仍是最多 5 秒 / 30 MB，
只测下载。两个界面共用停止、互斥和预算。

节点格式参考 [LibreSpeed 官方文档](https://github.com/librespeed/speedtest-cli#use-a-custom-backend-server-list)。

## 验证

运行 `luyaz-speedtest`、`luyaz-core`、`luyaz-web` 测试及 `touch-ui/rs/build.sh test`。
回环测试覆盖列表延迟/不可达、备用成功、手动来源不切换、取消、配置持久化、
传输失败和共享预算。测试不访问公共测速节点，也不代表已验证俄罗斯真实网络封锁。
