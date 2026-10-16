-- 假 zte-agent：只实现 DevUI eSIM 页用到的 4 个接口，用来在真屏上走切换流程而不碰 SIM。
-- 切换 job 跑 6 秒出结果；切往第 4 张（8944…001）模拟失败。第 4 张还带 3KB 图标和
-- 转义字符，用来测解析。用法见 docs/ESIM.zh-CN.md「不碰真卡的验证」。
--
--   lua esim-mock-agent.lua [端口=9091] [token=mocktoken]     密码固定是 mock
--
-- 设备自带的 uhttpd 被厂商改过，POST 到非白名单路径一律 405，所以用 lua + nixio 自己监听。
-- 设备上没有 nixio.util（writeall 在那里），发送要自己循环。
local nixio = require "nixio"
local srv = assert(nixio.bind("127.0.0.1", tonumber(arg[1]) or 9091))
srv:listen(8)
local TOKEN = arg[2] or "mocktoken"   -- 换个 token 重启 = 模拟 agent 重启、旧 token 失效

local enabled = "8900000000000000003"
local job = { id = 0, status = "idle", start = 0, target = "", kind = "", msg = "" }
local logf = io.open("/tmp/esmock/log", "a")
local function log(s) logf:write(os.date("%H:%M:%S ") .. s .. "\n"); logf:flush() end

local function advance()
  if job.status == "running" and os.time() - job.start >= 6 then
    if job.target == "8944000000000000001" then
      job.status, job.msg = "error", "lpac: profile enable failed (code -1)"
    else
      enabled, job.status, job.msg = job.target, "done", "switched - no reboot needed"
    end
    log("job " .. job.id .. " -> " .. job.status)
  end
end

local function st(i) return enabled == i and "enabled" or "disabled" end

local function profiles_json()
  local icon = string.rep("QUJD", 800)
  return '{"data":{"busy":false,"installed":true,"profiles":[' ..
    '{"iccid":"8900000000000000003","icon":null,"iconType":null,"profileClass":"operational","profileName":"CTM_1.0.5","profileNickname":null,"profileState":"' .. st("8900000000000000003") .. '","serviceProviderName":"CTM"},' ..
    '{"iccid":"89000000000000000002","icon":null,"iconType":null,"profileClass":"operational","profileName":"CMI_GDS","profileNickname":null,"profileState":"' .. st("89000000000000000002") .. '","serviceProviderName":"CMLINK"},' ..
    '{"iccid":"89000000000000000001","icon":null,"iconType":null,"profileClass":"operational","profileName":"Plus","profileNickname":"DJBUS18","profileState":"' .. st("89000000000000000001") .. '","serviceProviderName":"DJB"},' ..
    '{"iccid":"8944000000000000001","icon":"' .. icon .. '","iconType":"png","profileClass":"operational","profileName":"Test <&> \\"Q\\"","profileNickname":"\\u9999\\u6e2f 5G {\\u5361}","profileState":"' .. st("8944000000000000001") .. '","serviceProviderName":"\\u6d4b\\u8bd5\\u8fd0\\u8425\\u5546"}' ..
    ']},"ok":true}'
end

local reasons = { [200] = "OK", [401] = "Unauthorized", [404] = "Not Found", [409] = "Conflict" }
local function sendall(c, s)   -- 设备上没有 nixio.util，自己循环 send
  local i = 1
  while i <= #s do
    local n = c:send(s:sub(i, i + 4095))
    if not n or n <= 0 then return end
    i = i + n
  end
end

local function respond(c, code, body)
  sendall(c, string.format("HTTP/1.0 %d %s\r\nContent-Type: application/json\r\nContent-Length: %d\r\n\r\n%s",
    code, reasons[code], #body, body))
end

local function handle(c)
  local buf = ""
  repeat
    local chunk = c:recv(4096)
    if not chunk or #chunk == 0 then return end
    buf = buf .. chunk
  until buf:find("\r\n\r\n", 1, true)
  local e = buf:find("\r\n\r\n", 1, true)
  local head, body = buf:sub(1, e), buf:sub(e + 4)
  local len = tonumber(head:match("[Cc]ontent%-[Ll]ength:%s*(%d+)")) or 0
  while #body < len do
    local chunk = c:recv(4096)
    if not chunk or #chunk == 0 then break end
    body = body .. chunk
  end
  local method, path = head:match("^(%u+) (%S+)")
  local authed = head:find("Authorization: Bearer " .. TOKEN, 1, true) ~= nil
  advance()
  log((method or "?") .. " " .. (path or "?") .. " auth=" .. tostring(authed) .. (body ~= "" and (" body=" .. body) or ""))

  if path == "/api/auth/login" then
    if body:find('"password":"mock"', 1, true) then respond(c, 200, '{"data":{"token":"' .. TOKEN .. '"},"ok":true}')
    else respond(c, 401, '{"error":"invalid password","ok":false}') end
  elseif not authed then
    respond(c, 401, '{"error":"unauthorized","ok":false}')
  elseif path == "/api/esim/job" then
    respond(c, 200, string.format('{"data":{"finished_unix":0,"iccid":"%s","id":%d,"kind":"%s","message":"%s","rebooting":false,"started_unix":%d,"status":"%s"},"ok":true}',
      job.target, job.id, job.kind, job.msg, job.start, job.status))
  elseif path == "/api/esim/profiles" then
    if job.status == "running" then respond(c, 200, '{"data":{"busy":true,"installed":true,"profiles":null},"ok":true}')
    else respond(c, 200, profiles_json()) end
  elseif path == "/api/esim/switch" and method == "POST" then
    if job.status == "running" then
      respond(c, 409, '{"error":"operation in progress","ok":false}')
    else
      job = { id = job.id + 1, status = "running", start = os.time(), target = body:match('"iccid":"(%d+)"') or "", kind = "switch", msg = "" }
      respond(c, 200, string.format('{"data":{"job_id":%d,"status":"running"},"ok":true}', job.id))
    end
  else
    respond(c, 404, '{"error":"not found","ok":false}')
  end
end

log("mock agent up")
while true do
  local c = srv:accept()
  if c then
    c:setopt("socket", "rcvtimeo", 2)
    local ok, err = pcall(handle, c)
    if not ok then log("error: " .. tostring(err)) end
    c:close()
  end
end
