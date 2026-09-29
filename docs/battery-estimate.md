# 电池预估时间

只有一份实现：zte-agent `zte-agent/src/battery_eta.rs`。它自己每 5 秒采一次样（不管有没有人在看），
结果放在 `GET /api/battery` 的 `estimate` 和 `GET /api/screen` 的 `battery` 里；管理网页和触屏都只负责把它写成文字。
（2026-09-26 之前网页和触屏各算一份，两边已经出现差异，见 `docs/screen-logic-move.md` #21。）

`battery-estimate/fixtures.json` 是这些规则的例子，由 `battery_eta.rs` 的测试跑；改规则时本文、fixtures、`battery_eta.rs` 一起改。

## 结果里的 state

| state | 意思 | 界面 |
|---|---|---|
| `ok` | 正常 | 按 kind 显示 |
| `estimating` | agent 刚启动，还没采到样 | 「—」 |
| `stale` | 采样线程停了超过 20 秒（数值是最后一次的） | 「—」 |
| `unavailable` | 读不到电池 | 「—」 |

## 输入

- 采样序列，按时间升序：`[t 秒, 电池电流 µA, 充电器是否接着]`。电流正数 = 进电池（充电），负数 = 放电（sysfs `battery/current_now` 的符号）。充电器状态取 datad 的 `charger_connect`，不知道时沿用上一条。
- `soc`：`battery/capacity`，0–100。
- `charge_full_uah`：`battery/charge_full`；可缺。
- `charge_counter_uah`：`battery/charge_counter`（剩余电量）；可缺。
- `target_pct`：充电保护开着且上限在 50–100 之间时是它的上限，否则 100。
- `paused_at_limit` = 充电保护开着 且 `charging_stopped` 且不是手动停充（`manual_override` 为假）且充电器不是确定拔掉（`charger_connect` 不知道时算插着）。手动停充不算到上限。
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
- `state` 不是 `ok` 时一律「—」。
