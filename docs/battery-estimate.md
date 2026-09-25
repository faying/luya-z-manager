# 电池预估时间

网页（`web/src/lib/batteryEstimate.ts`）和触屏（touch-ui `src/estimate.c`）各实现一次，按这里的规则算。两边的测试都跑同一份 `battery-estimate/fixtures.json`，并校验它的 sha256 等于下面这行。改规则时三处一起改：本文、fixtures、两边实现；fixtures 复制到 touch-ui `tests/fixtures/battery-estimate.json`。

fixtures sha256: `c4f7c854162e3e88cb0fff6d3b36a4721a5610214f9d3dd67371fe2843f887b0`

## 输入

- 采样序列，按时间升序：`[t 秒, 电池电流 µA, 充电器是否接着]`。电流正数 = 进电池（充电），负数 = 放电（sysfs `battery/current_now` 的符号）。
- `soc`：`battery/capacity`，0–100。
- `charge_full_uah`：`battery/charge_full`；可缺。
- `charge_counter_uah`：`battery/charge_counter`（剩余电量）；可缺。
- `target_pct`：充电保护开着时是它的上限，否则 100。
- `paused_at_limit`：由调用方算好 = 充电保护开着 且 `charging_stopped` 且不是手动停充（`manual_override` 为假）且充电器插着（ubus `zwrt_bsp.charger` 的 `charger_connect`）。手动停充不算到上限。
  停充时固件把 USB 输入切掉（`usb/online`、`usb/current_now` 会变 0，电池放电约 0.35 A），所以这里不看采样里的充电器状态。

内核的 `power_now`、`time_to_full_now` 不用（实测互相矛盾）。

## 平均电流

1. 从最新一条往前取，遇到下面任一情况就停：时间早于「最新 t − 180 秒」；电流符号和最新一条不同（0 算正）；充电器状态和最新一条不同。
2. 时间加权：第 i 条的权重 = 下一条的 t − 它的 t，最后一条权重为 0。总权重为 0（只剩一条）时直接用最新一条的电流。

## 结果（按顺序判断，先中先返回）

| 条件 | kind | minutes |
|---|---|---|
| 没有采样 | `unknown` | null |
| `paused_at_limit` | `paused_at_limit` | null |
| 充电器接着且 `soc ≥ target_pct`，且平均电流 ≥ 0 或 \|平均电流\| < 50 000 µA（充满后电池会有几 mA 的小放电） | `reached_target` | null |
| \|平均电流\| < 50 000 µA | `unknown` | null |
| 平均电流 > 0：缺 `charge_full_uah` 则 `unknown`；否则 需要 = charge_full × (target − soc) / 100，分钟 = 需要 / 平均 × 60 | `charging_eta` | 四舍五入 |
| 平均电流 < 0：剩余 = charge_counter，缺则 charge_full × soc / 100，都缺则 `unknown`；分钟 = 剩余 / \|平均\| × 60 | `discharging_eta` | 四舍五入 |

## 显示

- `charging_eta`：目标是 100 时写「约 X 充满」，否则「约 X 充到 N%」。
- `discharging_eta`：「约可用 X」。
- `reached_target`：目标 100 写「已充满」，否则「已到上限 N%」。
- `paused_at_limit`：「已到上限，暂停充电」。
- `unknown`：「—」。
- X：不到 60 分钟写「N 分钟」，否则「H 小时 M 分」（M 为 0 时只写「H 小时」）。触屏里不能用 `%f`。
