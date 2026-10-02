# 界面术语表 / UI glossary

> L2 第 0 阶段 2026-10-01 定稿（用户定 4 条 + 外部评审）。以后新加界面文字先在这里加词。
> 设计稿：`docs/designs/ui-english.md`（不公开）。措辞规则见 `docs/DESIGN.md` §1 第 6 条，首页结论表和长度预算见 §4。

触屏、管理网页、后台（datad、agent、guard）的英文都以本表为准。同一个东西只有一个英文叫法；提示里点名页面时，用的页名必须和本表一模一样。

宽度是用真机字体 Nunito（正文 600、粗体 700）按实际字号量的；触屏 render 测试最后按像素确认。

## 1. 底栏标签（14 号，每个 ≤60px）

| 中文 | English | 宽 |
|---|---|---|
| 首页 | Home | 37 |
| 蜂窝 | Cellular | 50 |
| Wi-Fi | Wi-Fi | 35 |
| 出口 | Route | 38 |
| 系统 | System | 47 |

## 2. 页名（触屏二级页标题，网页菜单）

页名是专名，和网页菜单一样用 Title Case（10-01 定）；其余文字照句首大写。

| 中文 | 触屏 | 网页菜单（现有） |
|---|---|---|
| 短信 | SMS | SMS |
| 短信详情 | Message | — |
| 小区信息 | Cell Info | — |
| 锁频 | Band Lock | Band Lock |
| 测速 | Speed Test | Speed Test |
| SIM 与 eSIM | SIM & eSIM | SIM / PIN、eSIM（两页） |
| 性能测试 | Performance Test | — |
| Tailscale | Tailscale | Tailscale |
| 健康与告警 | Health & Alerts | Health、Alerts（两页） |
| 运营商选择 | Network Operator | Mobile Network |
| 情景 | Scenarios | Scenarios |
| APN | APN | APN |
| 详情 | Details | — |
| 管理网页 | web admin（句中小写） | — |

## 3. 通用名词

| 中文 | English | 说明 |
|---|---|---|
| 管理后台 | Admin backend | 短信等挤的地方写 Admin |
| 数据服务 | Data service | 网页已在用 |
| 触屏界面 | Screen UI | 网页现写 Touch screen UI，R8 合并时改齐 |
| 原厂界面 | stock UI | |
| 屏幕守护（u60-uid） | Screen supervisor | doctor 用 |
| 看门狗 | watchdog | Wi-Fi watchdog |
| 网络模式 | Network mode | 设置值 Auto / 5G only… 见 §7 |
| 移动数据 | Mobile data | |
| 数据漫游 | Data roaming | |
| 载波 / 载波聚合 | carrier / CA | |
| 锚点（5G NSA 的 4G） | anchor | |
| 告警短信 | alert SMS | 复数 alert texts |
| 漫游权限（套餐） | plan | 能不能漫游是套餐/运营商的事，不说 SIM |

## 4. 首页大字（状态词，24 粗，≤150px）

以 datad `screen.rs` `say()` 为准；中文不动，英文只写状态词，原因交给右栏和提示行。

| story.state | 中文 | English | 宽 | 色调 |
|---|---|---|---|---|
| ok | 顺畅 | All good | 101 | ok |
| weak / noise / crowd / narrow / limit | 慢：X | Slow | 58 | warn |
| nosvc | 无服务 | No service | 118 | bad |
| sos | 只能紧急呼叫 | SOS only | 106 | bad |
| nodata | 没连上网 | Offline | 81 | bad |
| only2g / only3g | 只有 2G / 只有 3G | 2G only / 3G only | 90 | warn |
| nosim | 无 SIM | No SIM | 82 | bad |
| airplane | 移动网络已关 | Airplane | 99 | 中性 |
| （读取中） | 读取中… | Loading… | 91 + … | 中性 |
| （datad 掉线，触屏自己写） | 读不到数据 | No update | 119 | 中性 |

## 5. 首页提示行（13 号，≤2 行，按 ≤540px 算）

中文里原有 5 条指错页（「锁频」「功能 → eSIM」），10-01 第 1 阶段已改成「蜂窝」。

| 情况 | 中文（现有） | English | 宽 |
|---|---|---|---|
| nosim | 插卡，或在「蜂窝 → SIM 与 eSIM」启用 | Insert a SIM, or enable an eSIM profile in SIM & eSIM | 311 |
| airplane | 飞行模式开着，去管理网页关掉 | Turn off airplane mode in the web admin | 243 |
| sos，漫游 | 没注册上：卡要开漫游，或换当地卡 | Ask your carrier to enable roaming, or use a local SIM | 320 |
| sos，本地 | 没注册上：欠费、停机，或这里没这家的网 | Check your balance or line status; this carrier may have no coverage here | 438 |
| nosvc，制式被限定 | 正在搜网；制式被限定，去「蜂窝」改回自动 | Searching; set network mode to Auto in Cellular | 290 |
| nosvc | 正在搜网，换个位置试试 | Searching; try another spot | 约 170 |
| nodata，数据关 | 移动数据关着：去「蜂窝」打开 | Turn on mobile data in Cellular | 184 |
| nodata，漫游关 | 数据漫游关着：去「蜂窝」打开，卡也要开通 | Turn on data roaming in Cellular; your plan must allow it too | 358 |
| nodata，刚换网在拨号 | 正在拨号，换网后要半分钟左右；一直不通多半是卡在这家网络没开漫游 | Connecting, ~30 s after a network change; if stuck, your plan may not roam here | 478 |
| nodata，漫游中 | 数据没拨上：看「蜂窝」里数据漫游开没开，卡也要开通 | Check data roaming in Cellular; your plan must allow it too | 约 350 |
| nodata，本地 | 数据没拨上：查流量开关、APN 或欠费 | Check mobile data, APN, or your balance | 245 |
| limit | 运营商限到 {} Mbps，换位置没用 | Carrier caps speed at {} Mbps; moving won't help | 311 |
| weak（有 RSRP） | RSRP {}：离基站远，靠窗通常好些 | Weak signal, RSRP {} dBm; try near a window | ≤349 |
| weak | 离基站远，靠窗通常好些 | Weak signal; try near a window | |
| noise | SINR {}：杂波多，挪个位置或换个朝向 | Noisy signal, SINR {} dB; move or rotate the device | ≤363 |
| crowd | RSRQ {}：人多抢网，换位置帮助不大 | Cell busy, RSRQ {} dB; moving won't help much | ≤350 |
| only2g / only3g，被限定 | 制式被限定只用 2G（3G），去「蜂窝」改回 | Network mode is 2G (3G) only; set it to Auto in Cellular | 304 |
| only2g | 上网会非常慢，附近可能没有 4G/5G | Very slow; probably no 4G/5G nearby | 229 |
| only3g | 能上网但较慢，附近可能没有 4G/5G | Online but slow; probably no 4G/5G nearby | 262 |
| narrow | 这里只给了 1 条 {} MHz | Only one {} MHz carrier here | 176 |
| ok，限定只用 4G | 制式限定只用 4G，去「蜂窝」改回 | Network mode is 4G only; set it to Auto in Cellular | 304 |
| （漫游时附加） | ；漫游中 | ; roaming | |
| datad 掉线（触屏顶行） | 数据服务掉线 · 数字停在 hh:mm | Data service offline · last update 14:32 | 235 |

## 6. 首页右栏、顶行、顶栏

| 位置 | 中文 | English | 宽 / 上限 |
|---|---|---|---|
| 信号词（17 粗） | 弱 / 中 / 强 | Weak / Fair / Strong | ≤56 / 120 |
| 干扰 · 负载（12） | 干扰大/中/小 · 负载高/正常 | Noise high/mid/low · Load high/normal | ≤143 / 150 |
| 带宽 | 带宽很宽 / 充足 / 一般 / 偏窄 | Very wide / Wide / Fair / Narrow | |
| 载波 | 4G 锚点 + 5G，N 条载波 | 4G anchor + 5G, N carriers | 144 |
| 载波 | N 条载波聚合 / 单载波 | N-carrier CA / Single carrier（NSA 只有 1 条时写 1 carrier） | |
| 限速 | 无 / 有 / — | none / capped / — | 接在 Cap 后面 |
| 载波 | 这个制式没有载波聚合 | No CA on this network | 127 |
| 载波（蜂窝页） | 激活 2/3 / 无聚合 / 没连上基站 | Active 2/3 / No CA / No cell | |
| 顶行地点 | 本地 | Local | |
| 顶行地点 | 漫游 | Roaming | 52 |
| 顶行地点 | 漫游到 X | Roaming on X | 「Roaming on Chunghwa Telecom」187 |
| 顶行运营商缺省 | 未注册 | Not registered | 82 |
| 顶行制式 | 5G NSA · 4G 锚点 | 5G NSA · 4G anchor | 整行最长例 236 / 292 |
| 顶栏制式词（13 粗） | 无服务 | No svc | 43（上限：不超过 5G UW 46） |
| 顶栏制式词 | SOS、5G、5G-A、5G UC、5G UW、5G+、4G+、LTE | 原样 | 最宽 5G UW 46 |

## 7. 网络模式取值（datad `net_select_word`）

| 中文 | English |
|---|---|
| 自动 | Auto |
| 只用 5G SA | 5G SA only |
| 只用 5G NSA | 5G NSA only |
| 只用 5G | 5G only |
| 只用 4G | 4G only |
| 只用 3G | 3G only |
| 只用 3G 和 2G | 3G & 2G only |
| 只用 2G | 2G only |
| 只用 TD-SCDMA | TD-SCDMA only |
| 限定了制式 | Restricted |

## 8. 运营商英文名

规则（设计评审决定 13）：用简短通用名，去掉 Co., Ltd.；agent 表里没有的，显示网络广播名原样。datad 的 4 家必须和 agent 一字不差（跨仓库测试比对）。

| MCC-MNC | 中文 | English |
|---|---|---|
| 460-00/02/04/07/08 | 中国移动 | China Mobile |
| 460-01/06/09 | 中国联通 | China Unicom |
| 460-03/05/11 | 中国电信 | China Telecom |
| 460-15 | 中国广电 | China Broadnet |
| 454-03/04 | 3 香港 | 3 HK |
| 454-12/13 | 中国移动香港 | China Mobile HK |
| 454-07 | 中国联通香港 | China Unicom HK |
| 455-00 | SmarTone 澳门 | SmarTone Macau |
| 455-02/07 | 中国电信澳门 | China Telecom Macau |
| 455-03/05 | 3 澳门 | 3 Macau |
| 466-01/05 | 远传电信 | FarEasTone |
| 466-11/92 | 中华电信 | Chunghwa Telecom |
| 466-89/93/97 | 台湾大哥大 | Taiwan Mobile |
| 440-11 | 楽天モバイル | Rakuten Mobile |

agent 表里其余的（csl、SmarTone、CTM、NTT docomo、SoftBank、au (KDDI)、SK Telecom、KT、LG U+、Singtel、M1、StarHub、SIMBA、AT&T、T-Mobile、Verizon、Telus、Bell、Rogers、O2 UK、Vodafone UK、Three UK、EE、Vodafone NL、KPN、Odido、Orange、SFR、Free、Bouygues、Telekom、Vodafone DE、O2 DE、AIS、True、dtac、Maxis、CelcomDigi、U Mobile、Telstra、Optus、Vodafone AU）本来就是英文，原样用。

## 9. 告警短信（guard `sms_body()`，英文模式）

格式 `[U60] <正文> (<日 月 时:分>)`，整条纯 ASCII、≤70 字符（评审 R7）；句末不加句号，几层意思用 `;` 连。时间写 `01 Oct 14:32`（`date '+%d %b %H:%M'`，定宽，10-01 定）。下表「长」是整条长度。

| kind | 中文（现有） | English 整条 | 长 |
|---|---|---|---|
| wifi-takeover | 管理后台没反应了。为了让你能上网，已自动打开U60的Wi-Fi。不用管。 | [U60] Admin down; Wi-Fi turned on; no action needed (01 Oct 14:32) | 66 |
| wifi-restore-failed | 想自动打开U60的Wi-Fi没成功，还在重试。如果手机连不上U60，请重启它。 | [U60] Wi-Fi didn't start; retrying; stuck? Restart U60 (01 Oct 14:32) | 69 |
| agent-silent | 管理后台超过5分钟没反应。上网一般不受影响；Wi-Fi要是关着，会自动打开。 | [U60] Admin silent 5+ min; internet usually fine (01 Oct 14:32) | 63 |
| agent-hung | 管理后台卡住了，已强制重启。不用管。 | [U60] Admin hung; force-restarted; no action needed (01 Oct 14:32) | 66 |
| agent-crash | 管理后台意外退出，已自动重启。不用管。 | [U60] Admin crashed; restarted; no action needed (01 Oct 14:32) | 63 |
| datad-crash | 屏幕的数据服务意外退出，已自动重启。不用管。 | [U60] Data service crashed; restarted; no action needed (01 Oct 14:32) | 70 |
| datad-degraded | 屏幕的数据服务超过5分钟不正常，管理后台已改用备用方式读数据。上网不受影响。 | [U60] Data service down 5+ min; using fallback; net OK (01 Oct 14:32) | 69 |
| devui-crash | 屏幕界面闪退了，已自动重新打开。不用管。 | [U60] Screen UI crashed; reopened; no action needed (01 Oct 14:32) | 66 |
| devui-gave-up | 屏幕界面连续打不开，已换成原厂界面，上网不受影响。长按屏幕右下角3秒可换回。 | [U60] Stock UI on; hold bottom-right 3s to switch back (01 Oct 14:32) | 69 |
| sms-test | 这是测试短信。收到了，说明告警短信能正常发到你手机。 | [U60] Test SMS: alert texts reach your phone (01 Oct 14:32) | 59 |
| 其他 | 有一条新告警（kind），请到管理网页「系统→告警」查看。 | [U60] New alert <kind>; see Alerts on web (01 Oct 14:32) | 最长 kind（devui-theme-paused）68 |

为了压进 70 字，英文比中文少说了的：agent-silent 没写「Wi-Fi 关着会自动打开」（真打开时另有 wifi-takeover 短信）；devui-gave-up 没写「上网不受影响」，「屏幕界面打不开」由 Stock UI on 带出。

## 外部评审（10-01，独立子代理；Codex 卡死没跑成）

15 条采纳 14 条：漫游写成找运营商/套餐而非 SIM；RSRP/SINR/RSRQ 带单位；no action needed 代替 nothing to do；devui-gave-up 说清长按是换回；on backup 易被当成电池，改 using fallback；Airplane 代替 Cell off（和提示一致）；datad 掉线照实写 Data service offline；web admin；干扰统一叫 Noise；提示行一句话用 `;`；带宽 Wide、No cell、Cell busy、Restricted 等小改。
不采纳 1 条：「运营商选择」和网页 Mobile Network 不是同一页（网页那页还管飞行模式），名字不必一样。

## 10-01 用户定的 4 条

1. 页名用 Title Case，和网页菜单一致。
2. 「出口」标签叫 Route。
3. 顶栏无服务写 No svc；长度预算改成「不超过 5G UW（46px）」。
4. 英文短信时间写 `01 Oct 14:32`。

## 顺带发现（10-01 第 1 阶段已改）

- datad 5 条指错页的中文提示已改成「蜂窝」，语料只做这两处替换（179 行），没有重录。
- datad 认无服务改看 `story.state`（nosvc、sos），不再比对中文大字。
