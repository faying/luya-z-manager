//! T3 测试：blob 编解码（手写字节对照 + 往返）、客户端对 mock ubusd（迟到回复、换 ID、ubusd 重启、
//! 超时重连与本轮跳过）、后端选择与 CLI 后端。

use super::backend::{AutoBackend, BackendKind, CliBackend, SocketBackend, UbusBackend};
use super::blob::{self, Attr, MsgHdr, attr, blobmsg_type, msg_type, status};
use super::client::{RoundSkips, UbusClient, UbusError};
use super::mock_ubusd::{Action, Inject, MockUbusd};
use serde_json::{Value, json};
use std::time::{Duration, Instant};

const T: Duration = Duration::from_millis(250);

// ------------------------------------------------------------ 编解码

#[test]
fn blob_hello_frame_bytes() {
    let f = blob::encode_frame(MsgHdr::new(msg_type::HELLO, 0, 0x1234_5678), &[]);
    assert_eq!(f, [0, 0, 0, 0, 0x12, 0x34, 0x56, 0x78, 0, 0, 0, 4]);
    let d = blob::decode_frame(&f).unwrap();
    assert_eq!(d.hdr, MsgHdr::new(msg_type::HELLO, 0, 0x1234_5678));
    assert!(d.body.is_empty());
}

#[test]
fn blob_lookup_request_bytes() {
    let mut body = Vec::new();
    blob::put_string(&mut body, attr::OBJPATH, "system");
    let f = blob::encode_frame(MsgHdr::new(msg_type::LOOKUP, 1, 0), &body);
    let want: &[u8] = &[
        0, 4, 0, 1, 0, 0, 0, 0, // ubus_msghdr: version 0, LOOKUP, seq 1, peer 0
        0, 0, 0, 16, // 顶层 blob：id 0，长度 16
        2, 0, 0, 11, b's', b'y', b's', b't', b'e', b'm', 0,
        0, // OBJPATH，长度 11 + 1 字节填充
    ];
    assert_eq!(f, want);
    let d = blob::decode_frame(&f).unwrap();
    let a = blob::msg_attrs(&d.body).unwrap();
    assert_eq!(
        blob::get_string(a[attr::OBJPATH as usize].as_ref().unwrap()),
        "system"
    );
}

/// {"s":"x","n":5,"b":true}（按这个顺序写入）的 blobmsg 字节，按 blobmsg.h 手算。
const TABLE_FIXTURE: &[u8] = &[
    0x83, 0, 0, 10, 0, 1, b's', 0, b'x', 0, 0,
    0, // STRING "s"="x"：头4 + 名字头4 + "x\0"，填 2
    0x85, 0, 0, 12, 0, 1, b'n', 0, 0, 0, 0, 5, // INT32 "n"=5
    0x87, 0, 0, 9, 0, 1, b'b', 0, 1, 0, 0, 0, // INT8(BOOL) "b"=true，填 3
];

#[test]
fn blobmsg_table_fixture_bytes() {
    let mut out = Vec::new();
    blob::blobmsg_put_json(&mut out, "s", &json!("x"));
    blob::blobmsg_put_json(&mut out, "n", &json!(5));
    blob::blobmsg_put_json(&mut out, "b", &json!(true));
    assert_eq!(out, TABLE_FIXTURE);
    let m = blob::blobmsg_object(TABLE_FIXTURE).unwrap();
    assert_eq!(Value::Object(m), json!({"s":"x","n":5,"b":true}));
}

#[test]
fn blobmsg_array_fixture_bytes() {
    // "l": ["x"]；数组元素 namelen=0，名字头 2+0+1 对齐到 4。
    let want: &[u8] = &[
        0x81, 0, 0, 20, 0, 1, b'l', 0, // ARRAY "l"，长度 20
        0x83, 0, 0, 10, 0, 0, 0, 0, b'x', 0, 0, 0, // STRING 元素
    ];
    let mut out = Vec::new();
    blob::blobmsg_put_json(&mut out, "l", &json!(["x"]));
    assert_eq!(out, want);
    assert_eq!(
        Value::Object(blob::blobmsg_object(want).unwrap()),
        json!({"l":["x"]})
    );
}

#[test]
fn blobmsg_roundtrip_all_types() {
    let v = json!({
        "str": "中文 ok", "empty": "", "i32": -7, "big": 1_i64 << 40, "neg64": -(1_i64 << 40),
        "t": true, "f": false, "d": 1.5, "null": null,
        "arr": [1, "two", [3], {"four": 4}], "tbl": {"x": {"y": []}}, "e": {}
    });
    let bytes = blob::blobmsg_table(v.as_object().unwrap());
    assert_eq!(Value::Object(blob::blobmsg_object(&bytes).unwrap()), v);
    // 整数按 CLI 规则：i32 范围内 INT32，否则 INT64；小数 DOUBLE。
    let attrs = blob::parse_attrs(&bytes).unwrap();
    let ty = |name: &str| {
        attrs
            .iter()
            .find(|a| blob::blobmsg_decode(a).unwrap().0 == name)
            .unwrap()
            .id
    };
    assert_eq!(ty("i32"), blobmsg_type::INT32);
    assert_eq!(ty("big"), blobmsg_type::INT64);
    assert_eq!(ty("d"), blobmsg_type::DOUBLE);
    assert_eq!(ty("t"), blobmsg_type::BOOL);
    assert_eq!(ty("null"), blobmsg_type::UNSPEC);
}

#[test]
fn blobmsg_nesting_depth_limited() {
    // 顶层对象里套 n 层 table/array（交替）。32 层能解，33 层是协议错误（不无限递归）。
    fn nested(n: usize) -> Value {
        let mut v = json!(1);
        for i in 0..n {
            v = if i % 2 == 0 {
                json!({"t": v})
            } else {
                json!([v])
            };
        }
        v
    }
    for (n, ok) in [(blob::MAX_DEPTH, true), (blob::MAX_DEPTH + 1, false)] {
        let top = json!({"x": nested(n)});
        let bytes = blob::blobmsg_table(top.as_object().unwrap());
        match blob::blobmsg_object(&bytes) {
            Ok(m) if ok => assert_eq!(Value::Object(m), top),
            Err(e) if !ok => assert!(e.0.contains("nested deeper than 32"), "{e:?}"),
            r => panic!("depth {n}: {r:?}"),
        }
    }
    // 顶层就是数组时同样算层数。
    let mut deep = Vec::new();
    blob::blobmsg_put_json(&mut deep, "", &nested(blob::MAX_DEPTH + 1));
    assert!(blob::blobmsg_array(&deep).is_err());
}

#[test]
fn blobmsg_decodes_int16_and_rejects_bad_input() {
    let mut out = Vec::new();
    blob::blobmsg_put_raw(&mut out, blobmsg_type::INT16, "h", &(-2_i16).to_be_bytes());
    assert_eq!(
        Value::Object(blob::blobmsg_object(&out).unwrap()),
        json!({"h":-2})
    );
    // 没有扩展位的属性不是 blobmsg。
    let mut plain = Vec::new();
    blob::put_attr(
        &mut plain,
        blobmsg_type::STRING,
        false,
        &[0, 0, 0, 0, b'x', 0],
    );
    assert!(blob::blobmsg_object(&plain).is_err());
    // INT32 长度不对。
    let mut bad = Vec::new();
    blob::blobmsg_put_raw(&mut bad, blobmsg_type::INT32, "n", &[0, 1]);
    assert!(blob::blobmsg_object(&bad).is_err());
    // 属性长度超出容器。
    assert!(blob::parse_attrs(&[0x83, 0, 0, 40, 0, 0]).is_err());
    let a = Attr {
        id: 0,
        extended: false,
        data: &[1, 2],
    };
    assert!(blob::get_u32(&a).is_err());
}

#[test]
fn frame_truncated_and_oversized_are_errors() {
    let mut body = Vec::new();
    blob::put_string(&mut body, attr::OBJPATH, "system");
    let f = blob::encode_frame(MsgHdr::new(msg_type::LOOKUP, 1, 0), &body);
    for n in 0..f.len() {
        assert!(blob::decode_frame(&f[..n]).is_err(), "truncated at {n}");
    }
    let mut big = [0u8; 12];
    let len = (blob::UBUS_MAX_MSGLEN + 4) as u32;
    big[8..].copy_from_slice(&len.to_be_bytes());
    assert!(blob::decode_head(&big).is_err());
    big[8..].copy_from_slice(&3_u32.to_be_bytes());
    assert!(blob::decode_head(&big).is_err(), "shorter than blob header");
}

#[tokio::test]
async fn read_frame_reports_eof_and_oversize() {
    let (mut a, mut b) = tokio::net::UnixStream::pair().unwrap();
    use tokio::io::AsyncWriteExt;
    let mut head = vec![0, msg_type::DATA, 0, 1, 0, 0, 0, 0];
    head.extend(((blob::UBUS_MAX_MSGLEN + 8) as u32).to_be_bytes());
    a.write_all(&head).await.unwrap();
    assert!(matches!(
        blob::read_frame(&mut b).await,
        Err(blob::ReadError::Protocol(_))
    ));
    drop(a);
    assert!(matches!(
        blob::read_frame(&mut b).await,
        Err(blob::ReadError::Io(_))
    ));
}

// ------------------------------------------------------------ 客户端

fn client(m: &MockUbusd) -> UbusClient {
    UbusClient::with_timeout(m.path(), T)
}

#[tokio::test]
async fn ubus_call_lookup_then_invoke_caches_id() {
    let m = MockUbusd::start_new().await;
    m.add_method("system", 0x100, "board", json!({"board_name":"mu5250"}));
    m.add_method("system", 0x100, "info", json!({"uptime":5}));
    let mut c = client(&m);
    assert_eq!(
        c.call("system", "board", &json!({})).await.unwrap(),
        Some(json!({"board_name":"mu5250"}))
    );
    assert_eq!(
        c.call("system", "info", &json!({})).await.unwrap(),
        Some(json!({"uptime":5}))
    );
    assert_eq!(c.cached_id("system"), Some(0x100));
    let s = c.stats();
    assert_eq!(
        (s.connects, s.lookups, s.invokes, s.dropped_frames),
        (1, 1, 2, 0)
    );
    assert_eq!(
        (m.connections(), m.lookups(), m.invokes("system")),
        (1, 1, 2)
    );
}

#[tokio::test]
async fn ubus_args_are_encoded_like_the_cli() {
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 7, "echo", json!({}));
    m.script("svc", "echo", Action::Echo);
    let mut c = client(&m);
    let args = json!({"a":"b","n":-3,"big":5_000_000_000_i64,"t":true,"l":[1,{"x":null}],"d":0.25});
    assert_eq!(c.call("svc", "echo", &args).await.unwrap(), Some(args));
}

#[tokio::test]
async fn ubus_late_reply_is_dropped() {
    let m = MockUbusd::start_new().await;
    m.add_method("svc.a", 0x200, "get", json!({"who":"A"}));
    m.script("svc.a", "get", Action::Late);
    let mut c = client(&m);

    // 请求 A：mock 扣住回复，客户端超时、关连接。
    let e = c.call("svc.a", "get", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::Timeout { .. }), "{e:?}");
    assert!(!c.is_connected());

    // 请求 B 在途时：先到 A 的迟到回复（seq=A、peer 对），再到一组 seq=B 但 peer 错的帧，最后才是 B 的回复。
    m.add_method("svc.a", 0x200, "get", json!({"who":"B"}));
    m.inject_before_next_invoke(Inject::WrongPeer(json!({"who":"wrong-peer"})));
    let before = c.stats().dropped_frames;
    let v = c.call("svc.a", "get", &json!({})).await.unwrap();
    assert_eq!(
        v,
        Some(json!({"who":"B"})),
        "B must get its own reply, not A's late one"
    );
    assert_eq!(
        c.stats().dropped_frames - before,
        4,
        "late DATA+STATUS and wrong-peer DATA+STATUS"
    );
    assert_eq!(m.connections(), 2);

    // 之后这条连接上没有残留帧：下一次调用照常。
    m.add_method("svc.a", 0x200, "get", json!({"who":"C"}));
    assert_eq!(
        c.call("svc.a", "get", &json!({})).await.unwrap(),
        Some(json!({"who":"C"}))
    );
    assert_eq!(c.stats().dropped_frames - before, 4);
}

#[tokio::test]
async fn ubus_foreign_seq_and_unrelated_frames_are_dropped() {
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 9, "get", json!({"mine":1}));
    let mut c = client(&m);
    m.inject_before_next_invoke(Inject::OtherSeq(json!({"other":1})));
    m.inject_before_next_invoke(Inject::Unrelated);
    m.inject_before_next_invoke(Inject::WrongPeer(json!({"peer":1})));
    assert_eq!(
        c.call("svc", "get", &json!({})).await.unwrap(),
        Some(json!({"mine":1}))
    );
    assert_eq!(c.stats().dropped_frames, 5);
}

#[tokio::test]
async fn ubus_timeout_reconnects_and_skips_object() {
    let m = MockUbusd::start_new().await;
    m.add_method("svc.slow", 1, "get", json!({"slow":true}));
    m.add_method("svc.fast", 2, "get", json!({"fast":true}));
    m.script("svc.slow", "get", Action::Hang);
    let mut c = client(&m);
    c.call("svc.fast", "get", &json!({})).await.unwrap(); // 缓存 svc.fast 的 ID
    assert_eq!(m.connections(), 1);

    let mut round = RoundSkips::new();
    let t0 = Instant::now();
    let e = c
        .call_in_round(&mut round, "svc.slow", "get", &json!({}))
        .await
        .unwrap_err();
    let took = t0.elapsed();
    assert!(
        matches!(&e, UbusError::Timeout { object, .. } if object == "svc.slow"),
        "{e:?}"
    );
    assert!(e.is_timeout());
    assert!(
        took >= T - Duration::from_millis(20) && took < T * 4,
        "took {took:?}"
    );
    assert!(!c.is_connected(), "timeout closes the connection");
    assert_eq!(
        c.cached_id("svc.slow"),
        None,
        "timeout invalidates the object's ID"
    );
    assert_eq!(
        c.cached_id("svc.fast"),
        Some(2),
        "other LOOKUP results are kept"
    );
    assert_eq!(c.stats().timeouts, 1);
    assert!(round.is_skipped("svc.slow"));

    // 同一轮再读 svc.slow：直接 Skipped，不发请求。
    let invokes = m.invokes("svc.slow");
    let e = c
        .call_in_round(&mut round, "svc.slow", "get", &json!({}))
        .await
        .unwrap_err();
    assert!(matches!(e, UbusError::Skipped { .. }), "{e:?}");
    assert_eq!(m.invokes("svc.slow"), invokes);

    // 同一轮读别的对象：重新连（新 HELLO），不重新 LOOKUP。
    let lookups = c.stats().lookups;
    let v = c
        .call_in_round(&mut round, "svc.fast", "get", &json!({}))
        .await
        .unwrap();
    assert_eq!(v, Some(json!({"fast":true})));
    assert_eq!(m.connections(), 2);
    assert_eq!(c.stats().connects, 2);
    assert_eq!(c.stats().lookups, lookups);

    // 下一轮：svc.slow 重新 LOOKUP 后照常读。
    round.clear();
    let mock_lookups = m.lookups();
    let v = c
        .call_in_round(&mut round, "svc.slow", "get", &json!({}))
        .await
        .unwrap();
    assert_eq!(v, Some(json!({"slow":true})));
    assert_eq!(c.stats().lookups, lookups + 1);
    assert_eq!(m.lookups(), mock_lookups + 1);
}

#[tokio::test]
async fn ubus_object_id_change_relookups() {
    let m = MockUbusd::start_new().await;
    m.add_method("zwrt_bsp.battery", 10, "list", json!({"cap":50}));
    let mut c = client(&m);
    assert_eq!(
        c.call("zwrt_bsp.battery", "list", &json!({}))
            .await
            .unwrap(),
        Some(json!({"cap":50}))
    );
    // 服务重启，用新 ID 重新注册。
    m.change_id("zwrt_bsp.battery", 11);
    assert_eq!(
        c.call("zwrt_bsp.battery", "list", &json!({}))
            .await
            .unwrap(),
        Some(json!({"cap":50}))
    );
    assert_eq!(c.cached_id("zwrt_bsp.battery"), Some(11));
    let s = c.stats();
    assert_eq!((s.lookups, s.invokes, s.connects), (2, 3, 1));
    assert_eq!(m.invokes("?"), 1, "one invoke hit the stale ID");
}

#[tokio::test]
async fn ubus_ubusd_restart_reconnects() {
    let mut m = MockUbusd::start_new().await;
    m.add_method("system", 1, "info", json!({"n":1}));
    let mut c = client(&m);
    assert!(c.call("system", "info", &json!({})).await.is_ok());

    // ubusd 重启，对象用新 ID 重新注册：旧连接写失败说明请求没送到，客户端当场重连重发。
    m.restart().await;
    m.change_id("system", 2);
    assert_eq!(
        c.call("system", "info", &json!({})).await.unwrap(),
        Some(json!({"n":1}))
    );
    assert_eq!(m.connections(), 2);
    assert_eq!(c.stats().connects, 2);
    assert_eq!(c.cached_id("system"), Some(2));
    assert_eq!(
        m.invokes("?"),
        0,
        "no invoke is sent with an ID from before the restart"
    );

    // ubusd 不在：报 Io，不挂住；回来后下一次调用成功。
    m.stop().await;
    let t0 = Instant::now();
    let e = c.call("system", "info", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::Io(_)), "{e:?}");
    assert!(t0.elapsed() < T);
    m.start();
    assert_eq!(
        c.call("system", "info", &json!({})).await.unwrap(),
        Some(json!({"n":1}))
    );
    assert_eq!(m.connections(), 3);
}

#[tokio::test]
async fn ubus_ubusd_restart_reconnects_while_socket_held_elsewhere() {
    // 整套测试并行时，别的用例起子进程的瞬间子进程也拿着 mock 的 fd（见 MockUbusd::stop）。
    // 这里替它多拿一份：mock 重启后旧连接照样写不进去，客户端照样重连重发。
    let mut m = MockUbusd::start_new().await;
    m.add_method("system", 1, "info", json!({"n":1}));
    let mut c = client(&m);
    assert!(c.call("system", "info", &json!({})).await.is_ok());
    let held = m.hold_server_sockets();
    assert_eq!(held.len(), 1);

    m.restart().await;
    m.change_id("system", 2);
    assert_eq!(
        c.call("system", "info", &json!({})).await.unwrap(),
        Some(json!({"n":1}))
    );
    assert_eq!(c.stats().connects, 2);
    assert_eq!(m.invokes("?"), 0);
    drop(held);
}

#[tokio::test]
async fn ubus_status_errors_and_no_data() {
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 5, "get", json!({"v":1}));
    m.add_method("svc", 5, "set", json!({}));
    let mut c = client(&m);

    let e = c.call("nope", "get", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::NotFound { .. }), "{e:?}");

    let e = c.call("svc", "missing", &json!({})).await.unwrap_err();
    assert!(
        matches!(e, UbusError::Status { code, .. } if code == status::METHOD_NOT_FOUND),
        "{e:?}"
    );
    assert_eq!(
        c.cached_id("svc"),
        None,
        "METHOD_NOT_FOUND invalidates the cached ID"
    );

    m.script("svc", "get", Action::Status(status::PERMISSION_DENIED));
    let e = c.call("svc", "get", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::Status { code: 6, .. }), "{e:?}");
    assert_eq!(c.cached_id("svc"), Some(5), "other statuses keep the cache");

    m.script("svc", "set", Action::NoData);
    assert_eq!(c.call("svc", "set", &json!({"x":1})).await.unwrap(), None);

    // 在超时之内的慢回复照常收到。
    m.script("svc", "get", Action::Delay(T / 3));
    assert_eq!(
        c.call("svc", "get", &json!({})).await.unwrap(),
        Some(json!({"v":1}))
    );

    // 对象消失：NOT_FOUND 后重新 LOOKUP 也找不到。
    m.remove_object("svc");
    let e = c.call("svc", "get", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::NotFound { .. }), "{e:?}");
    assert_eq!(c.stats().connects, 1);
    assert_eq!(c.stats().timeouts, 0);
}

#[tokio::test]
async fn ubus_service_not_found_same_id_not_resent() {
    // 服务自己用 NOT_FOUND 表示「条目不存在」：重新 LOOKUP 得到同一个 ID，不重发（写操作只执行一次）。
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 5, "del", json!({}));
    let mut c = client(&m);
    assert_eq!(
        c.call("svc", "del", &json!({})).await.unwrap(),
        Some(json!({}))
    );
    assert_eq!(m.invokes("svc"), 1);
    let lookups = m.lookups();
    m.script("svc", "del", Action::Status(status::NOT_FOUND));
    let e = c.call("svc", "del", &json!({"k":1})).await.unwrap_err();
    assert!(
        matches!(e, UbusError::Status { code, .. } if code == status::NOT_FOUND),
        "{e:?}"
    );
    assert_eq!(m.invokes("svc"), 2, "INVOKE 只发了一次");
    assert_eq!(m.lookups(), lookups + 1, "重新 LOOKUP 过一次");
    assert_eq!(c.cached_id("svc"), Some(5));
    // 真换了 ID 的仍然重发一次（同 ubus_object_id_change_relookups）。
    m.change_id("svc", 9);
    assert_eq!(
        c.call("svc", "del", &json!({})).await.unwrap(),
        Some(json!({}))
    );
    assert_eq!(c.cached_id("svc"), Some(9));
}

#[tokio::test]
async fn ubus_invalid_arguments_send_nothing() {
    let m = MockUbusd::start_new().await;
    let mut c = client(&m);
    for (o, me, a) in [
        ("bad name", "x", json!({})),
        ("svc", "a;b", json!({})),
        ("svc", "x", json!([1])),
    ] {
        let e = c.call(o, me, &a).await.unwrap_err();
        assert!(matches!(e, UbusError::InvalidArgument(_)), "{e:?}");
    }
    assert_eq!(
        c.call("", "x", &json!({})).await.unwrap_err().to_string(),
        "invalid ubus name"
    );
    assert_eq!(m.connections(), 0);
}

// ------------------------------------------------------------ 后端

#[test]
fn backend_kind_parse() {
    assert_eq!(BackendKind::parse(None), (BackendKind::Cli, None));
    assert_eq!(BackendKind::parse(Some("")), (BackendKind::Cli, None));
    assert_eq!(BackendKind::parse(Some("cli")), (BackendKind::Cli, None));
    assert_eq!(
        BackendKind::parse(Some("socket")),
        (BackendKind::Socket, None)
    );
    assert_eq!(
        BackendKind::parse(Some(" socket\n")),
        (BackendKind::Socket, None)
    );
    assert_eq!(
        BackendKind::parse(Some(" auto ")),
        (BackendKind::Auto, None)
    );
    let (k, w) = BackendKind::parse(Some("SOCKET"));
    assert_eq!(k, BackendKind::Cli);
    assert!(w.unwrap().contains("SOCKET"));
}

fn script(dir: &std::path::Path, name: &str, body: &str) -> String {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p.to_string_lossy().into_owned()
}

/// 刚写好的脚本在多线程测试里 exec 偶尔会 ETXTBSY（别的线程 fork 时继承了写句柄），重试几次。
async fn cli_call(b: &mut CliBackend, o: &str, m: &str, a: &Value) -> Result<Value, UbusError> {
    for _ in 0..20 {
        let r = b.call(o, m, a).await;
        match &r {
            Err(UbusError::Io(msg)) if msg.contains("spawn") => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            _ => return r,
        }
    }
    b.call(o, m, a).await
}

#[tokio::test]
async fn cli_backend_matches_state_ubus() {
    let dir = std::env::temp_dir().join(format!("zwrt-ubus-cli-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mock_ubus.sh");
    let mut b = CliBackend::new(fixture, Duration::from_secs(8));
    assert_eq!(b.kind(), BackendKind::Cli);
    let v = cli_call(&mut b, "system", "board", &json!({}))
        .await
        .unwrap();
    assert!(v.is_object(), "{v}");

    let mut empty = CliBackend::new(script(&dir, "empty.sh", "exit 0"), Duration::from_secs(8));
    let e = cli_call(&mut empty, "svc", "set", &json!({}))
        .await
        .unwrap_err();
    assert!(matches!(e, UbusError::NoData { .. }), "{e:?}");
    let cli_empty = e.to_string();
    assert!(cli_empty.starts_with("invalid ubus JSON: "), "{cli_empty}");

    let mut fail = CliBackend::new(script(&dir, "fail.sh", "exit 4"), Duration::from_secs(8));
    let e = cli_call(&mut fail, "svc", "get", &json!({}))
        .await
        .unwrap_err();
    assert!(
        matches!(&e, UbusError::Io(m) if m.contains("exited with")),
        "{e:?}"
    );

    let mut slow = CliBackend::new(
        script(&dir, "slow.sh", "sleep 5"),
        Duration::from_millis(100),
    );
    let e = cli_call(&mut slow, "svc", "get", &json!({}))
        .await
        .unwrap_err();
    assert!(e.is_timeout(), "{e:?}");
    assert!(e.to_string().ends_with("timed out"), "{e}");

    let e = cli_call(&mut b, "svc", "get", &json!("x"))
        .await
        .unwrap_err();
    assert_eq!(e.to_string(), "args must be an object");

    // Socket 后端在「OK 但没数据」时给出和 CLI 后端相同的错误文字。
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 3, "set", json!({}));
    m.script("svc", "set", Action::NoData);
    let mut s = SocketBackend::new(client(&m));
    assert_eq!(s.kind(), BackendKind::Socket);
    let e = s.call("svc", "set", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::NoData { .. }), "{e:?}");
    assert_eq!(e.to_string(), cli_empty);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn socket_control_timeout_longer_than_round() {
    // 采集轮里用构造时的短超时，采集轮之外（控制任务）用 8 秒：慢写操作不会被 2 秒截断。
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 3, "set", json!({"ok":1}));
    let mut s = SocketBackend::new(client(&m));
    s.set_round(true);
    assert_eq!(s.client().timeout(), T);
    m.script("svc", "set", Action::Delay(T * 2));
    let e = s.call("svc", "set", &json!({})).await.unwrap_err();
    assert!(e.is_timeout(), "{e:?}");
    s.set_round(false);
    assert_eq!(s.client().timeout(), super::backend::CONTROL_TIMEOUT);
    m.script("svc", "set", Action::Delay(T * 2));
    assert_eq!(
        s.call("svc", "set", &json!({})).await.unwrap(),
        json!({"ok":1})
    );
    // 默认 2 秒的后端：轮里 2 秒，轮外 8 秒。
    let mut d = SocketBackend::new(UbusClient::new("/nonexistent"));
    d.set_round(true);
    assert_eq!(d.client().timeout(), Duration::from_secs(2));
    d.set_round(false);
    assert_eq!(d.client().timeout(), Duration::from_secs(8));
}

/// 执行者（T4）要把后端放进 `tokio::spawn` 的任务里：future 必须是 Send。
#[test]
fn backend_futures_are_send() {
    fn assert_send<F: std::future::Future + Send>(_: F) {}
    let mut b = super::backend::Backend::Cli(CliBackend::new("/bin/false", Duration::from_secs(1)));
    let args = json!({});
    assert_send(b.call("a", "b", &args));
    let mut c = UbusClient::new("/nonexistent");
    assert_send(c.call("a", "b", &args));
}

// ------------------------------------------------------------ auto：socket 优先，肯定没送到才退回 CLI

/// 记录每次被调用的 CLI 替身，回 `{"via":"cli"}`。返回 (程序路径, 记录文件)。
fn counting_cli(name: &str) -> (std::path::PathBuf, String, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("zwrt-ubus-auto-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let log = dir.join("calls");
    let _ = std::fs::remove_file(&log);
    let bin = script(
        &dir,
        "ubus.sh",
        &format!(
            "echo \"$2 $3\" >> '{}'\necho '{{\"via\":\"cli\"}}'",
            log.display()
        ),
    );
    (dir, bin, log)
}

fn cli_calls(log: &std::path::Path) -> usize {
    std::fs::read_to_string(log).map_or(0, |s| s.lines().count())
}

/// ETXTBSY 重试（同 `cli_call`）。
async fn auto_call(b: &mut AutoBackend, o: &str, m: &str) -> Result<Value, UbusError> {
    for _ in 0..20 {
        let r = b.call(o, m, &json!({})).await;
        match &r {
            Err(UbusError::Io(msg)) if msg.contains("spawn") => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            _ => return r,
        }
    }
    b.call(o, m, &json!({})).await
}

#[tokio::test]
async fn client_reports_whether_the_invoke_may_have_been_sent() {
    let mut m = MockUbusd::start_new().await;
    m.add_method("svc", 3, "get", json!({"ok":1}));
    let mut c = client(&m);
    // 成功、服务报错：INVOKE 已送到。
    c.call("svc", "get", &json!({})).await.unwrap();
    assert!(!c.last_call_not_sent());
    m.script("svc", "get", Action::Status(2));
    c.call("svc", "get", &json!({})).await.unwrap_err();
    assert!(!c.last_call_not_sent());
    // INVOKE 写出去后超时：可能已执行。
    m.script("svc", "get", Action::Hang);
    let e = c.call("svc", "get", &json!({})).await.unwrap_err();
    assert!(e.is_timeout(), "{e:?}");
    assert!(!c.last_call_not_sent(), "a timed-out invoke may have run");
    // 找不到对象（LOOKUP 没有副作用，也算没送到；后端只对传输错误退回，NotFound 不退）。
    let e = c.call("nope", "get", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::NotFound { .. }), "{e:?}");
    assert!(c.last_call_not_sent());
    // 参数不合法：什么都没发。
    c.call("svc", "get", &json!("x")).await.unwrap_err();
    assert!(c.last_call_not_sent());
    // ubusd 不在：连不上。
    m.stop().await;
    let e = c.call("svc", "get", &json!({})).await.unwrap_err();
    assert!(matches!(e, UbusError::Io(_)), "{e:?}");
    assert!(c.last_call_not_sent());
}

#[tokio::test]
async fn auto_falls_back_to_cli_while_ubusd_is_down_then_retries_socket() {
    let mut m = MockUbusd::start_new().await;
    m.add_method("system", 1, "info", json!({"via":"socket"}));
    let (dir, bin, log) = counting_cli("down");
    let retry = Duration::from_millis(400);
    let mut b = AutoBackend::new(
        SocketBackend::new(client(&m)),
        CliBackend::new(bin, Duration::from_secs(8)),
        retry,
    );
    assert_eq!(b.kind(), BackendKind::Auto);
    assert_eq!(
        auto_call(&mut b, "system", "info").await.unwrap(),
        json!({"via":"socket"})
    );
    assert_eq!(cli_calls(&log), 0);

    // ubusd 不在：这一次换 CLI 做，之后 retry 内都走 CLI，不再碰 socket。
    m.stop().await;
    assert_eq!(
        auto_call(&mut b, "system", "info").await.unwrap(),
        json!({"via":"cli"})
    );
    assert_eq!((b.fallbacks, cli_calls(&log)), (1, 1));
    m.start();
    let conns = m.connections();
    assert_eq!(
        auto_call(&mut b, "system", "info").await.unwrap(),
        json!({"via":"cli"})
    );
    assert_eq!(cli_calls(&log), 2);
    assert_eq!(
        m.connections(),
        conns,
        "socket left alone during the window"
    );

    // 过了 retry：再试 socket，成功后一直走 socket。
    tokio::time::sleep(retry + Duration::from_millis(50)).await;
    assert_eq!(
        auto_call(&mut b, "system", "info").await.unwrap(),
        json!({"via":"socket"})
    );
    assert_eq!(
        auto_call(&mut b, "system", "info").await.unwrap(),
        json!({"via":"socket"})
    );
    assert_eq!((b.fallbacks, cli_calls(&log)), (1, 2));
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn auto_never_resends_an_invoke_that_may_have_run() {
    // 写操作超时：原厂可能已经执行，不能换 CLI 再做一次（总共只执行一次）。
    let m = MockUbusd::start_new().await;
    m.add_method("svc", 3, "set", json!({"ok":1}));
    m.script("svc", "set", Action::Hang);
    let (dir, bin, log) = counting_cli("hang");
    let mut b = AutoBackend::new(
        SocketBackend::new(client(&m)),
        CliBackend::new(bin, Duration::from_secs(8)),
        Duration::from_secs(30),
    );
    b.set_round(true);
    let e = auto_call(&mut b, "svc", "set").await.unwrap_err();
    assert!(e.is_timeout(), "{e:?}");
    assert_eq!(m.invokes("svc"), 1);
    assert_eq!((b.fallbacks, cli_calls(&log)), (0, 0));
    // 服务报错、找不到对象、OK 没数据：都是 ubusd 的回答，不退回。
    m.script("svc", "set", Action::Status(2));
    auto_call(&mut b, "svc", "set").await.unwrap_err();
    auto_call(&mut b, "nope", "get").await.unwrap_err();
    m.script("svc", "set", Action::NoData);
    let e = auto_call(&mut b, "svc", "set").await.unwrap_err();
    assert!(matches!(e, UbusError::NoData { .. }), "{e:?}");
    assert_eq!((b.fallbacks, cli_calls(&log)), (0, 0));
    let _ = std::fs::remove_dir_all(&dir);
}

// ------------------------------------------------------------ 基准（手动跑，不进 CI）
//
// `cargo test --release bench_socket_vs_cli -- --ignored --nocapture`
// 同一个只读调用经 socket（mock ubusd）和 CLI（`tests/mock_ubus.sh`，一次一个 sh 进程）各做 N 次，
// 打印 p50/p95/p99 和起进程次数。这是容器/开发机上的相对数字，不代表设备；设备上的数字见
// docs/PERF-AUDIT.md（9-29 设备探测：socket 0～1 ms，`ubus call` 3～5 ms）。

fn pct(sorted: &[Duration], p: f64) -> Duration {
    sorted[((sorted.len() as f64 - 1.0) * p).round() as usize]
}

fn report(name: &str, mut v: Vec<Duration>, forks: usize) {
    v.sort();
    let sum: Duration = v.iter().sum();
    println!(
        "bench {name:<6} n={} p50={:?} p95={:?} p99={:?} max={:?} total={:?} forks={forks}",
        v.len(),
        pct(&v, 0.50),
        pct(&v, 0.95),
        pct(&v, 0.99),
        v[v.len() - 1],
        sum
    );
}

#[tokio::test]
#[ignore]
async fn bench_socket_vs_cli() {
    let n: usize = std::env::var("BENCH_N")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let m = MockUbusd::start_new().await;
    m.add_method(
        "system",
        1,
        "board",
        json!({"board_name":"mu5250","model":"x"}),
    );
    let mut s = SocketBackend::new(client(&m));
    s.set_round(true);
    let mut lat = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        s.call("system", "board", &json!({})).await.unwrap();
        lat.push(t.elapsed());
    }
    report("socket", lat, 0);
    assert_eq!(m.connections(), 1, "one connection for all calls");

    let fixture = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/mock_ubus.sh");
    let mut c = CliBackend::new(fixture, Duration::from_secs(8));
    let mut lat = Vec::with_capacity(n);
    for _ in 0..n {
        let t = Instant::now();
        cli_call(&mut c, "system", "board", &json!({}))
            .await
            .unwrap();
        lat.push(t.elapsed());
    }
    report("cli", lat, n);
}

#[tokio::test]
async fn auto_fallback_stays_within_the_cli_timeout() {
    // ubusd 收下连接却不发 HELLO：socket 用掉自己的超时后退回 CLI，CLI 只拿剩下的时间，
    // 一次调用总共不超过 CLI 的超时（加最少保留时间），不会变成 socket 超时 + 整个 CLI 超时。
    let path = std::env::temp_dir().join(format!("zwrt-ubus-nohello-{}.sock", std::process::id()));
    let _ = std::fs::remove_file(&path);
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let held = tokio::spawn(async move {
        let mut conns = Vec::new();
        while let Ok((s, _)) = listener.accept().await {
            conns.push(s);
        }
    });
    let (dir, _, _) = counting_cli("nohello");
    let slow = script(&dir, "slow.sh", "sleep 5");
    let cli_timeout = Duration::from_millis(1200);
    let mut b = AutoBackend::new(
        SocketBackend::new(UbusClient::with_timeout(&path, Duration::from_millis(700))),
        CliBackend::new(slow, cli_timeout),
        Duration::from_secs(30),
    );
    b.set_round(true);
    let t0 = Instant::now();
    let e = auto_call(&mut b, "svc", "get").await.unwrap_err();
    let took = t0.elapsed();
    assert!(e.is_timeout(), "{e:?}");
    assert_eq!(b.fallbacks, 1);
    assert!(
        took < cli_timeout + Duration::from_millis(300),
        "took {took:?}, socket 700 ms + CLI should be capped at {cli_timeout:?}"
    );
    held.abort();
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_dir_all(&dir);
}
