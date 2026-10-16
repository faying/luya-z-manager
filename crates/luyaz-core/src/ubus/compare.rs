//! `zwrt-datad --ubus-compare 对象:方法 …`：同一个只读调用经 socket 后端和 `ubus call` 各做一遍，比较结果。
//!
//! 给 socket 后端上机前在真设备上核对协议用（帧头字节序、消息/属性/状态编号、blob 编码、HELLO、
//! INVOKE 回复的 peer、LOOKUP 的对象 ID）。每个调用按 socket、CLI、socket 的顺序做三次：
//! 两次 socket 读到的一样的值（稳定值）必须和 CLI 的完全一样；两次 socket 之间变了的值（运行时间、
//! 空闲内存之类）只计数不比较。小数 CLI 只打 6 位（libubox blobmsg_json.c 的 `%lf`），只差在这个精度里的单独计数。
//! 出错的调用比状态码：`ubus call` 的退出码就是 ubus 状态（cli.c；stderr 不是终端时取负，退出码是 256 减状态）。
//! 对象 ID 和 `ubus -v list 对象` 打印的比。
//!
//! 只打印结论、计数和字段路径（不是标识符的路径段写成 `<key>`），不打印任何值：输出可以原样拿回电脑看。
//! 不起服务、不监听端口、不写文件、调用都不带参数（`{}`）。全部一致退出码 0，否则 1，用法错误 2。

use super::backend::{SocketBackend, UbusBackend, cli_bin};
use super::client::UbusError;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::process::Command;

/// CLI 一次最多等多久（设备上没有 `timeout` 命令，靠这里杀）。
const CLI_DEADLINE: Duration = Duration::from_secs(10);
/// 每类路径最多列几个。
const MAX_PATHS: usize = 8;

/// 一次调用的结果。
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    Data(Value),
    /// 状态 OK 但没有数据（CLI 什么都不打印）。
    NoData,
    /// ubus 状态码（非 0）。
    Status(i32),
    /// 超时、连接失败、协议错、CLI 输出不是 JSON 之类。
    Other(String),
}

impl Outcome {
    fn from_socket(r: Result<Value, UbusError>) -> Self {
        match r {
            Ok(v) => Self::Data(v),
            Err(UbusError::Status { code, .. }) => Self::Status(code),
            Err(UbusError::NotFound { .. }) => Self::Status(super::blob::status::NOT_FOUND),
            Err(UbusError::NoData { .. }) => Self::NoData,
            Err(e) => Self::Other(e.to_string()),
        }
    }

    fn from_cli(r: Result<(i32, Vec<u8>), String>) -> Self {
        match r {
            Err(e) => Self::Other(e),
            Ok((0, out)) if out.iter().all(u8::is_ascii_whitespace) => Self::NoData,
            Ok((0, out)) => match serde_json::from_slice(&out) {
                Ok(v) => Self::Data(v),
                Err(e) => Self::Other(format!("ubus call output is not JSON: {e}")),
            },
            Ok((code, _)) => Self::Status(cli_status(code)),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Data(_) => "data".into(),
            Self::NoData => "no data".into(),
            Self::Status(c) => format!("{} ({c})", super::blob::status::name(*c)),
            Self::Other(e) => e.clone(),
        }
    }
}

/// `ubus call` 的退出码 → ubus 状态：stderr 不是终端时 cli.c 返回负的状态，退出码成了 256 减状态。
fn cli_status(exit: i32) -> i32 {
    if exit > 128 { 256 - exit } else { exit }
}

/// 三次结果逐字段比较的计数和路径。
#[derive(Debug, Default, PartialEq)]
struct Diff {
    /// 比过的值（叶子）个数。
    values: usize,
    /// 两次 socket 之间变了的（值、类型、数组长度、字段有无），只计数。
    changed: usize,
    /// 只差在 CLI 的 6 位小数里的。
    precision: usize,
    /// 结构不同（类型、字段、数组长度）的路径。
    shape: Vec<String>,
    /// 稳定值却和 CLI 不同的路径。
    mismatch: Vec<String>,
}

impl Diff {
    fn same(&self) -> bool {
        self.shape.is_empty() && self.mismatch.is_empty()
    }
}

fn diff(s1: &Value, cli: &Value, s2: &Value) -> Diff {
    let mut d = Diff::default();
    walk(&mut d, "", s1, cli, s2);
    d
}

fn kind(v: &Value) -> u8 {
    match v {
        Value::Null => 0,
        Value::Bool(_) => 1,
        Value::Number(_) => 2,
        Value::String(_) => 3,
        Value::Array(_) => 4,
        Value::Object(_) => 5,
    }
}

fn walk(d: &mut Diff, path: &str, s1: &Value, c: &Value, s2: &Value) {
    if kind(s1) != kind(s2) {
        d.changed += 1;
        return;
    }
    if kind(c) != kind(s1) {
        d.shape.push(label(path));
        return;
    }
    match (s1, c, s2) {
        (Value::Object(a), Value::Object(cm), Value::Object(b)) => {
            let keys: BTreeSet<&String> = a.keys().chain(cm.keys()).chain(b.keys()).collect();
            for k in keys {
                let p = join(path, k);
                match (a.get(k), cm.get(k), b.get(k)) {
                    (Some(x), Some(y), Some(z)) => walk(d, &p, x, y, z),
                    // 两次 socket 都有、CLI 没有，或者只有 CLI 有：结构不同。
                    (Some(_), None, Some(_)) | (None, Some(_), None) => d.shape.push(label(&p)),
                    // 两次 socket 之间出现或消失的字段。
                    _ => d.changed += 1,
                }
            }
        }
        (Value::Array(a), Value::Array(ca), Value::Array(b)) => {
            if a.len() != b.len() {
                d.changed += 1;
            } else if ca.len() != a.len() {
                d.shape.push(label(path));
            } else {
                for (i, ((x, y), z)) in a.iter().zip(ca).zip(b).enumerate() {
                    walk(d, &format!("{path}[{i}]"), x, y, z);
                }
            }
        }
        _ => {
            d.values += 1;
            if s1 != s2 {
                d.changed += 1;
            } else if c != s1 {
                if within_cli_decimals(s1, c) {
                    d.precision += 1;
                } else {
                    d.mismatch.push(label(path));
                }
            }
        }
    }
}

/// CLI 按 `%lf` 打小数（6 位），差不超过 1e-6 就算同一个数。至少一边是小数才算（两个整数不同就是真的不同）。
fn within_cli_decimals(a: &Value, b: &Value) -> bool {
    let float = |v: &Value| v.as_number().is_some_and(serde_json::Number::is_f64);
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => (float(a) || float(b)) && (x - y).abs() <= 1e-6,
        _ => false,
    }
}

/// 路径段：标识符原样，其他（MAC、IP、主机名之类当键）写成 `<key>`。
fn join(path: &str, key: &str) -> String {
    let plain = key
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    let seg = if plain { key } else { "<key>" };
    if path.is_empty() {
        seg.to_string()
    } else {
        format!("{path}.{seg}")
    }
}

fn label(path: &str) -> String {
    if path.is_empty() {
        "(top)".into()
    } else {
        path.into()
    }
}

fn list(paths: &[String]) -> String {
    let mut s = paths
        .iter()
        .take(MAX_PATHS)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if paths.len() > MAX_PATHS {
        s.push_str(&format!(" and {} more", paths.len() - MAX_PATHS));
    }
    s
}

/// `ubus -v list` 的第一行 `'对象' @xxxxxxxx` 里的 ID。
fn listed_id(out: &[u8]) -> Option<u32> {
    let line = String::from_utf8_lossy(out);
    let first = line.lines().next()?;
    let hex = first.split_once(" @")?.1.trim();
    u32::from_str_radix(hex, 16).ok()
}

/// 跑一次 ubus 命令行：返回（退出码，stdout）。起不来、超时、被信号杀掉都是 Err。
async fn cli_run(bin: &str, args: &[&str]) -> Result<(i32, Vec<u8>), String> {
    let child = crate::command::die_with_parent(&mut Command::new(bin))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn {bin}: {e}"))?;
    let out = tokio::time::timeout(CLI_DEADLINE, child.wait_with_output())
        .await
        .map_err(|_| format!("{bin} timed out"))?
        .map_err(|e| format!("{bin}: {e}"))?;
    match out.status.code() {
        Some(code) => Ok((code, out.stdout)),
        None => Err(format!("{bin} was killed by a signal")),
    }
}

/// socket 调一次；超时就等 1 秒重试一次（datad 可能正在调同一个对象）。返回结果、用时、是否重试过。
async fn socket_call(
    sock: &mut SocketBackend,
    object: &str,
    method: &str,
) -> (Outcome, u128, bool) {
    let t = Instant::now();
    let mut r = sock.call(object, method, &json!({})).await;
    let mut retried = false;
    if matches!(&r, Err(e) if e.is_timeout()) {
        retried = true;
        tokio::time::sleep(Duration::from_secs(1)).await;
        r = sock.call(object, method, &json!({})).await;
    }
    (Outcome::from_socket(r), t.elapsed().as_millis(), retried)
}

/// 比一个调用的三次结果，返回（是否一致，说明）。
fn verdict(s1: &Outcome, c: &Outcome, s2: &Outcome) -> (bool, String) {
    match (s1, c, s2) {
        (Outcome::Data(a), Outcome::Data(b), Outcome::Data(z)) => {
            let d = diff(a, b, z);
            let mut s = format!(
                "{} values, {} changed between the two socket reads",
                d.values, d.changed
            );
            if d.precision > 0 {
                s.push_str(&format!(", {} equal within 6 decimals", d.precision));
            }
            if !d.shape.is_empty() {
                s.push_str(&format!("; structure differs at {}", list(&d.shape)));
            }
            if !d.mismatch.is_empty() {
                s.push_str(&format!("; stable values differ at {}", list(&d.mismatch)));
            }
            (d.same(), s)
        }
        (Outcome::NoData, Outcome::NoData, Outcome::NoData) => (true, "no data from either".into()),
        (Outcome::Status(x), Outcome::Status(y), Outcome::Status(z)) if x == y && y == z => {
            (true, format!("same error: {}", s1.describe()))
        }
        _ => (
            false,
            format!(
                "socket: {}; ubus call: {}; socket again: {}",
                s1.describe(),
                c.describe(),
                s2.describe()
            ),
        ),
    }
}

pub async fn run(args: &[String]) -> i32 {
    let mut calls = Vec::new();
    for a in args {
        match a.split_once(':') {
            Some((o, m)) if super::validate_name(o).is_ok() && super::validate_name(m).is_ok() => {
                calls.push((o.to_string(), m.to_string()))
            }
            _ => {
                eprintln!("usage: luyaz-core --ubus-compare OBJECT:METHOD ...  (bad argument {a:?})");
                return 2;
            }
        }
    }
    if calls.is_empty() {
        eprintln!("usage: luyaz-core --ubus-compare OBJECT:METHOD ...");
        return 2;
    }
    let mut sock = SocketBackend::from_env();
    // 采集轮的超时（默认 2 秒）：上机后读取用的就是它。
    sock.set_round(true);
    let bin = cli_bin();
    let path = sock.client().path().display().to_string();
    let timeout_ms = sock.client().timeout().as_millis();
    println!(
        "u60d {} ubus compare: socket {path} (timeout {timeout_ms} ms), cli {bin}",
        env!("CARGO_PKG_VERSION")
    );
    let mut ok = true;
    let mut ids: BTreeMap<String, u32> = BTreeMap::new();
    for (object, method) in &calls {
        let (s1, t1, r1) = socket_call(&mut sock, object, method).await;
        let t = Instant::now();
        let c = Outcome::from_cli(cli_run(&bin, &["call", object, method, "{}"]).await);
        let tc = t.elapsed().as_millis();
        let (s2, t2, r2) = socket_call(&mut sock, object, method).await;
        if let Some(id) = sock.client().cached_id(object) {
            ids.entry(object.clone()).or_insert(id);
        }
        let (same, what) = verdict(&s1, &c, &s2);
        ok &= same;
        let retried = if r1 || r2 {
            ", timed out once and retried"
        } else {
            ""
        };
        println!(
            "{object} {method}: {} ({what}; socket {t1}/{t2} ms, ubus call {tc} ms{retried})",
            if same { "same" } else { "DIFFERENT" }
        );
    }
    for (object, id) in &ids {
        let listed = match cli_run(&bin, &["-v", "list", object]).await {
            Ok((0, out)) => listed_id(&out),
            _ => None,
        };
        let same = listed == Some(*id);
        ok &= same;
        match listed {
            Some(_) if same => println!("{object}: id @{id:08x}, same as ubus -v list"),
            Some(l) => println!("{object}: id @{id:08x}, DIFFERENT from ubus -v list @{l:08x}"),
            None => println!("{object}: id @{id:08x}, ubus -v list gave no id"),
        }
    }
    let st = sock.client().stats();
    // 超时会关连接、下次重连；除此之外不该重连（重连说明读写出错断过）。
    let clean = st.dropped_frames == 0 && st.connects <= 1 + st.timeouts;
    ok &= clean;
    println!(
        "socket client: connects {}, lookups {}, invokes {}, dropped frames {}, timeouts {}",
        st.connects, st.lookups, st.invokes, st.dropped_frames, st.timeouts
    );
    println!("{}", if ok { "PASS" } else { "FAIL" });
    if ok { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_values_must_match_and_changed_ones_are_only_counted() {
        let s1 = json!({"uptime": 100, "memory": {"total": 4_294_967_296_i64, "free": 10}, "load": [1, 2, 3]});
        let s2 = json!({"uptime": 101, "memory": {"total": 4_294_967_296_i64, "free": 12}, "load": [1, 2, 3]});
        let cli: Value = serde_json::from_str(
            r#"{"uptime":100,"memory":{"total":4294967296,"free":11},"load":[1,2,3]}"#,
        )
        .unwrap();
        let d = diff(&s1, &cli, &s2);
        assert!(d.same(), "{d:?}");
        assert_eq!((d.values, d.changed, d.precision), (6, 2, 0));

        let bad: Value =
            serde_json::from_str(r#"{"uptime":100,"memory":{"total":1,"free":11},"load":[1,2,4]}"#)
                .unwrap();
        let d = diff(&s1, &bad, &s2);
        assert_eq!(d.mismatch, vec!["load[2]", "memory.total"]);
    }

    #[test]
    fn negative_and_int64_numbers_compare_equal_to_the_cli_text() {
        let s = json!({"a": -5, "b": i64::MIN, "c": i64::MAX, "d": true, "e": null, "f": "x"});
        let cli: Value = serde_json::from_str(&format!(
            r#"{{"a":-5,"b":{},"c":{},"d":true,"e":null,"f":"x"}}"#,
            i64::MIN,
            i64::MAX
        ))
        .unwrap();
        assert!(diff(&s, &cli, &s).same());
    }

    #[test]
    fn structure_differences_are_reported() {
        let s = json!({"a": 1, "b": {"c": [1, 2]}, "d": "x"});
        let cli = json!({"a": "1", "b": {"c": [1]}, "e": 2});
        let d = diff(&s, &cli, &s);
        assert_eq!(d.shape, vec!["a", "b.c", "d", "e"]);
        assert!(diff(&s, &json!([1]), &s).shape == vec!["(top)"]);
    }

    #[test]
    fn fields_and_lengths_that_change_between_socket_reads_are_not_differences() {
        let s1 = json!({"clients": [1, 2], "new": 1});
        let s2 = json!({"clients": [1, 2, 3]});
        let cli = json!({"clients": [1]});
        let d = diff(&s1, &cli, &s2);
        assert!(d.same(), "{d:?}");
        assert_eq!(d.changed, 2);
    }

    #[test]
    fn doubles_within_the_cli_six_decimals_are_counted_apart() {
        let s = json!({"t": 1.23456789, "n": 3});
        let cli: Value = serde_json::from_str(r#"{"t":1.234568,"n":3}"#).unwrap();
        let d = diff(&s, &cli, &s);
        assert!(d.same(), "{d:?}");
        assert_eq!(d.precision, 1);
        // 两个整数差 1 不能算精度差。
        let d = diff(&json!({"n": 3}), &json!({"n": 4}), &json!({"n": 3}));
        assert_eq!(d.mismatch, vec!["n"]);
        let d = diff(&json!({"t": 1.5}), &json!({"t": 1.6}), &json!({"t": 1.5}));
        assert_eq!(d.mismatch, vec!["t"]);
    }

    #[test]
    fn keys_that_are_not_identifiers_are_redacted() {
        let s = json!({"aa:bb:cc:dd:ee:ff": {"ip": "x"}, "192.168.0.2": 1, "br-lan": 1, "_x": 1});
        let cli = json!({"aa:bb:cc:dd:ee:ff": {"ip": "y"}, "192.168.0.2": 2, "br-lan": 2, "_x": 2});
        let d = diff(&s, &cli, &s);
        assert_eq!(d.mismatch, vec!["<key>", "_x", "<key>.ip", "br-lan"]);
    }

    #[test]
    fn cli_exit_codes_map_back_to_ubus_status() {
        assert_eq!(cli_status(4), 4); // 找不到对象：cli.c 直接返回状态
        assert_eq!(cli_status(253), 3); // 找不到方法：stderr 不是终端时返回 -3
        assert_eq!(Outcome::from_cli(Ok((253, Vec::new()))), Outcome::Status(3));
        assert_eq!(Outcome::from_cli(Ok((0, b" \n".to_vec()))), Outcome::NoData);
        assert!(matches!(
            Outcome::from_cli(Ok((0, b"{".to_vec()))),
            Outcome::Other(_)
        ));
    }

    #[test]
    fn verdict_needs_the_same_outcome_three_times() {
        let d = Outcome::Data(json!({"a": 1}));
        assert!(verdict(&d, &d, &d).0);
        assert!(
            verdict(
                &Outcome::Status(3),
                &Outcome::Status(3),
                &Outcome::Status(3)
            )
            .0
        );
        assert!(
            !verdict(
                &Outcome::Status(3),
                &Outcome::Status(4),
                &Outcome::Status(3)
            )
            .0
        );
        assert!(!verdict(&d, &Outcome::NoData, &d).0);
        assert!(!verdict(&Outcome::Other("timeout".into()), &d, &d).0);
    }

    #[test]
    fn listed_id_reads_the_first_line_of_ubus_verbose_list() {
        assert_eq!(
            listed_id(b"'system' @6d4f2c1b\n\t\"board\":{}\n"),
            Some(0x6d4f_2c1b)
        );
        assert_eq!(listed_id(b""), None);
        assert_eq!(listed_id(b"system\n"), None);
    }
}
