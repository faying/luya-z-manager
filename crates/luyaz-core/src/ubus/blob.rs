//! blob / blobmsg TLV 编解码和 ubus 帧头。
//!
//! 常量出处（按记忆实现，没有联网核对；标 [Gate 0] 的要在设备只读探测时核实）：
//! - `libubox/blob.h`：`struct blob_attr { uint32_t id_len; char data[]; }`，id_len 网络字节序；
//!   `BLOB_ATTR_ID_MASK 0x7f000000`、`BLOB_ATTR_ID_SHIFT 24`、`BLOB_ATTR_LEN_MASK 0x00ffffff`、
//!   `BLOB_ATTR_ALIGN 4`、`BLOB_ATTR_EXTENDED 0x80000000`。长度字段是**不含填充**的原始长度
//!   （含 4 字节头），相邻属性按 4 字节对齐（`blob_pad_len`）。
//! - `libubox/blobmsg.h`：blobmsg 属性带 EXTENDED 位，id 是 `enum blobmsg_type`
//!   （UNSPEC 0、ARRAY 1、TABLE 2、STRING 3、INT64 4、INT32 5、INT16 6、INT8 7、DOUBLE 8，
//!   BOOL = INT8）；数据前是 `struct blobmsg_hdr { uint16_t namelen; uint8_t name[]; }`，
//!   namelen 网络字节序，name 以 NUL 结尾，整个名字头按 `blobmsg_hdrlen = ALIGN(2 + namelen + 1)` 对齐。
//!   整数网络字节序；double 按位当 u64 网络字节序（`blobmsg_add_double`）；字符串带结尾 NUL。
//! - `ubus/ubusmsg.h`：`struct ubus_msghdr { uint8_t version; uint8_t type; uint16_t seq; uint32_t peer; }`
//!   （packed，8 字节，seq/peer 网络字节序，libubus `ubus_send_msg` 里 `cpu_to_be16/32`）[Gate 0]，
//!   后面紧跟一个 id=0 的 blob_attr（`blob_buf_init(&b, 0)`），里面是 `enum ubus_msg_attr` 属性。
//!   `UBUS_MAX_MSGLEN 1048576` [Gate 0]。
//! - ubus-rs（jbit/ubus-rs）的 `message.rs` / `blob.rs` 是同一套布局。

use serde_json::{Map, Number, Value};
use std::fmt;
use tokio::io::{AsyncRead, AsyncReadExt};

pub const BLOB_ATTR_ID_MASK: u32 = 0x7f00_0000;
pub const BLOB_ATTR_ID_SHIFT: u32 = 24;
pub const BLOB_ATTR_LEN_MASK: u32 = 0x00ff_ffff;
pub const BLOB_ATTR_EXTENDED: u32 = 0x8000_0000;
pub const BLOB_ATTR_ALIGN: usize = 4;
pub const BLOB_HDR_LEN: usize = 4;

/// `enum blobmsg_type`（libubox/blobmsg.h）。
pub mod blobmsg_type {
    pub const UNSPEC: u8 = 0;
    pub const ARRAY: u8 = 1;
    pub const TABLE: u8 = 2;
    pub const STRING: u8 = 3;
    pub const INT64: u8 = 4;
    pub const INT32: u8 = 5;
    pub const INT16: u8 = 6;
    pub const INT8: u8 = 7;
    pub const DOUBLE: u8 = 8;
    pub const BOOL: u8 = INT8;
}

/// `enum ubus_msg_type`（ubus/ubusmsg.h）[Gate 0]；只列用到的，取值照原表。
pub mod msg_type {
    pub const HELLO: u8 = 0;
    pub const STATUS: u8 = 1;
    pub const DATA: u8 = 2;
    pub const LOOKUP: u8 = 4;
    pub const INVOKE: u8 = 5;
    /// mock 发「无关帧」用。
    #[cfg(test)]
    pub const NOTIFY: u8 = 10;
}

/// `enum ubus_msg_attr`（ubus/ubusmsg.h）[Gate 0]；只列用到的，取值照原表（0–13）。
pub mod attr {
    pub const STATUS: u8 = 1;
    pub const OBJPATH: u8 = 2;
    pub const OBJID: u8 = 3;
    pub const METHOD: u8 = 4;
    /// mock 的 LOOKUP 回复用。
    #[cfg(test)]
    pub const OBJTYPE: u8 = 5;
    pub const DATA: u8 = 7;
    /// `UBUS_ATTR_MAX`
    pub const MAX: usize = 14;
}

/// `enum ubus_msg_status`（ubus/ubusmsg.h）[Gate 0]。
pub mod status {
    pub const OK: i32 = 0;
    pub const INVALID_COMMAND: i32 = 1;
    pub const INVALID_ARGUMENT: i32 = 2;
    pub const METHOD_NOT_FOUND: i32 = 3;
    pub const NOT_FOUND: i32 = 4;
    pub const NO_DATA: i32 = 5;
    pub const PERMISSION_DENIED: i32 = 6;
    pub const TIMEOUT: i32 = 7;
    pub const NOT_SUPPORTED: i32 = 8;
    pub const UNKNOWN_ERROR: i32 = 9;
    pub const CONNECTION_FAILED: i32 = 10;
    pub const NO_MEMORY: i32 = 11;
    pub const PARSE_ERROR: i32 = 12;
    pub const SYSTEM_ERROR: i32 = 13;

    /// 和 libubus `ubus_strerror` 的文字一致（便于日志对照）。
    pub fn name(code: i32) -> &'static str {
        match code {
            OK => "Success",
            INVALID_COMMAND => "Invalid command",
            INVALID_ARGUMENT => "Invalid argument",
            METHOD_NOT_FOUND => "Method not found",
            NOT_FOUND => "Not found",
            NO_DATA => "No response",
            PERMISSION_DENIED => "Permission denied",
            TIMEOUT => "Request timed out",
            NOT_SUPPORTED => "Operation not supported",
            UNKNOWN_ERROR => "Unknown error",
            CONNECTION_FAILED => "Connection failed",
            NO_MEMORY => "Out of memory",
            PARSE_ERROR => "Parsing message data failed",
            SYSTEM_ERROR => "System error",
            _ => "Unknown error",
        }
    }
}

/// `sizeof(struct ubus_msghdr)`
pub const UBUS_MSGHDR_LEN: usize = 8;
/// `UBUS_MAX_MSGLEN`（整个 blob 的原始长度上限）[Gate 0]。
pub const UBUS_MAX_MSGLEN: usize = 1_048_576;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobError(pub String);

impl fmt::Display for BlobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BlobError {}

fn err<T>(msg: impl Into<String>) -> Result<T, BlobError> {
    Err(BlobError(msg.into()))
}

const fn align(n: usize) -> usize {
    (n + BLOB_ATTR_ALIGN - 1) & !(BLOB_ATTR_ALIGN - 1)
}

/// 解出来的一个 blob 属性；`data` 是负载（不含 4 字节头、不含填充）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Attr<'a> {
    pub id: u8,
    pub extended: bool,
    pub data: &'a [u8],
}

/// 依次解出 `buf` 里的相邻属性（`blob_for_each_attr`）。最后一个属性缺尾部填充也接受。
pub fn parse_attrs(buf: &[u8]) -> Result<Vec<Attr<'_>>, BlobError> {
    let mut out = Vec::new();
    let mut rest = buf;
    while !rest.is_empty() {
        if rest.len() < BLOB_HDR_LEN {
            return err("truncated blob attribute header");
        }
        let id_len = u32::from_be_bytes([rest[0], rest[1], rest[2], rest[3]]);
        let raw_len = (id_len & BLOB_ATTR_LEN_MASK) as usize;
        if raw_len < BLOB_HDR_LEN {
            return err("blob attribute shorter than its header");
        }
        if raw_len > rest.len() {
            return err("blob attribute longer than its container");
        }
        out.push(Attr {
            id: ((id_len & BLOB_ATTR_ID_MASK) >> BLOB_ATTR_ID_SHIFT) as u8,
            extended: id_len & BLOB_ATTR_EXTENDED != 0,
            data: &rest[BLOB_HDR_LEN..raw_len],
        });
        rest = &rest[align(raw_len).min(rest.len())..];
    }
    Ok(out)
}

/// 往 `out` 追加一个属性（头 + 负载 + 填充到 4 字节）。
pub fn put_attr(out: &mut Vec<u8>, id: u8, extended: bool, payload: &[u8]) {
    let raw_len = BLOB_HDR_LEN + payload.len();
    assert!(
        raw_len as u32 <= BLOB_ATTR_LEN_MASK && id <= 0x7f,
        "blob attribute too large"
    );
    let mut id_len = ((id as u32) << BLOB_ATTR_ID_SHIFT) | raw_len as u32;
    if extended {
        id_len |= BLOB_ATTR_EXTENDED;
    }
    out.extend_from_slice(&id_len.to_be_bytes());
    out.extend_from_slice(payload);
    out.resize(out.len() + align(raw_len) - raw_len, 0);
}

/// `blob_put_int32`
pub fn put_u32(out: &mut Vec<u8>, id: u8, v: u32) {
    put_attr(out, id, false, &v.to_be_bytes());
}

/// `blob_put_string`（带结尾 NUL）
pub fn put_string(out: &mut Vec<u8>, id: u8, s: &str) {
    let mut p = Vec::with_capacity(s.len() + 1);
    p.extend_from_slice(s.as_bytes());
    p.push(0);
    put_attr(out, id, false, &p);
}

/// `blob_get_u32`
pub fn get_u32(a: &Attr<'_>) -> Result<u32, BlobError> {
    match a.data.try_into() {
        Ok(b) => Ok(u32::from_be_bytes(b)),
        Err(_) => err("int32 attribute has wrong length"),
    }
}

/// `blob_get_string`：到第一个 NUL 为止。
pub fn get_string(a: &Attr<'_>) -> String {
    let end = a.data.iter().position(|&b| b == 0).unwrap_or(a.data.len());
    String::from_utf8_lossy(&a.data[..end]).into_owned()
}

/// 把 ubus 消息属性按 id 放进数组（`blob_parse` 按 ubus_policy），重复的取最后一个。
pub fn msg_attrs(children: &[u8]) -> Result<[Option<Attr<'_>>; attr::MAX], BlobError> {
    let mut out = [None; attr::MAX];
    for a in parse_attrs(children)? {
        if let Some(slot) = out.get_mut(a.id as usize) {
            *slot = Some(a);
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- blobmsg

fn blobmsg_payload(name: &str, data: &[u8]) -> Vec<u8> {
    let name = name.as_bytes();
    let hdrlen = align(2 + name.len() + 1);
    let mut p = Vec::with_capacity(hdrlen + data.len());
    p.extend_from_slice(&(name.len() as u16).to_be_bytes());
    p.extend_from_slice(name);
    p.resize(hdrlen, 0);
    p.extend_from_slice(data);
    p
}

/// 追加一个 blobmsg 字段（`blobmsg_add_field`）。
pub fn blobmsg_put_raw(out: &mut Vec<u8>, ty: u8, name: &str, data: &[u8]) {
    put_attr(out, ty, true, &blobmsg_payload(name, data));
}

/// 按 ubus CLI（`blobmsg_add_json_element`，libubox blobmsg_json.c）的规则把 JSON 值写成 blobmsg：
/// null → UNSPEC，bool → INT8，整数在 i32 范围内 → INT32、否则 INT64，小数 → DOUBLE，
/// 字符串 → STRING，数组 → ARRAY（元素名为空），对象 → TABLE。
pub fn blobmsg_put_json(out: &mut Vec<u8>, name: &str, v: &Value) {
    use blobmsg_type as t;
    match v {
        Value::Null => blobmsg_put_raw(out, t::UNSPEC, name, &[]),
        Value::Bool(b) => blobmsg_put_raw(out, t::BOOL, name, &[u8::from(*b)]),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                match i32::try_from(i) {
                    Ok(i) => blobmsg_put_raw(out, t::INT32, name, &i.to_be_bytes()),
                    Err(_) => blobmsg_put_raw(out, t::INT64, name, &i.to_be_bytes()),
                }
            } else {
                let f = n.as_f64().unwrap_or(0.0);
                blobmsg_put_raw(out, t::DOUBLE, name, &f.to_bits().to_be_bytes());
            }
        }
        Value::String(s) => {
            let mut d = Vec::with_capacity(s.len() + 1);
            d.extend_from_slice(s.as_bytes());
            d.push(0);
            blobmsg_put_raw(out, t::STRING, name, &d);
        }
        Value::Array(items) => {
            let mut inner = Vec::new();
            for item in items {
                blobmsg_put_json(&mut inner, "", item);
            }
            blobmsg_put_raw(out, t::ARRAY, name, &inner);
        }
        Value::Object(map) => {
            let inner = blobmsg_table(map);
            blobmsg_put_raw(out, t::TABLE, name, &inner);
        }
    }
}

/// 对象的各字段依次写成 blobmsg（不带外层 TABLE 头），用作 INVOKE 的 `UBUS_ATTR_DATA` 负载。
pub fn blobmsg_table(map: &Map<String, Value>) -> Vec<u8> {
    let mut out = Vec::new();
    for (k, v) in map {
        blobmsg_put_json(&mut out, k, v);
    }
    out
}

/// 解一个 blobmsg 字段，返回（名字，值）。值的 JSON 形式和 ubus CLI（`blobmsg_format_json`）一致：
/// INT8 → true/false，INT16/32/64 → 有符号整数，DOUBLE → 数（NaN/Inf → null），UNSPEC → null。
#[cfg(test)]
pub fn blobmsg_decode(a: &Attr<'_>) -> Result<(String, Value), BlobError> {
    decode_at(a, 0)
}

/// 嵌套 table/array 的最大层数：超过就当协议错误，不无限递归（坏帧或恶意帧不会把栈打爆）。
pub const MAX_DEPTH: usize = 32;

/// `depth` = 这个字段外面已经套了几层 table/array（顶层对象里的字段是 0）。
fn decode_at(a: &Attr<'_>, depth: usize) -> Result<(String, Value), BlobError> {
    use blobmsg_type as t;
    if !a.extended {
        return err("blobmsg attribute without the extended bit");
    }
    let p = a.data;
    if p.len() < 3 {
        return err("truncated blobmsg header");
    }
    let namelen = u16::from_be_bytes([p[0], p[1]]) as usize;
    let hdrlen = align(2 + namelen + 1);
    if hdrlen > p.len() {
        return err("blobmsg name longer than attribute");
    }
    let name = String::from_utf8_lossy(&p[2..2 + namelen]).into_owned();
    let d = &p[hdrlen..];
    let fixed = |n: usize| -> Result<&[u8], BlobError> {
        if d.len() == n {
            Ok(d)
        } else {
            err(format!(
                "blobmsg type {} has wrong length {}",
                a.id,
                d.len()
            ))
        }
    };
    let v = match a.id {
        t::UNSPEC => Value::Null,
        t::ARRAY | t::TABLE if depth >= MAX_DEPTH => {
            return err(format!("blobmsg nested deeper than {MAX_DEPTH}"));
        }
        t::ARRAY => Value::Array(array_at(d, depth + 1)?),
        t::TABLE => Value::Object(object_at(d, depth + 1)?),
        t::STRING => {
            let end = d.iter().position(|&b| b == 0).unwrap_or(d.len());
            Value::String(String::from_utf8_lossy(&d[..end]).into_owned())
        }
        t::INT64 => Value::from(i64::from_be_bytes(fixed(8)?.try_into().unwrap())),
        t::INT32 => Value::from(i32::from_be_bytes(fixed(4)?.try_into().unwrap())),
        t::INT16 => Value::from(i16::from_be_bytes(fixed(2)?.try_into().unwrap())),
        t::INT8 => Value::Bool(fixed(1)?[0] != 0),
        t::DOUBLE => {
            let f = f64::from_bits(u64::from_be_bytes(fixed(8)?.try_into().unwrap()));
            Number::from_f64(f).map_or(Value::Null, Value::Number)
        }
        other => return err(format!("unknown blobmsg type {other}")),
    };
    Ok((name, v))
}

/// 一串 blobmsg 字段 → JSON 对象（重复的名字后者覆盖前者，同 `serde_json` 解析 CLI 输出的结果）。
pub fn blobmsg_object(buf: &[u8]) -> Result<Map<String, Value>, BlobError> {
    object_at(buf, 0)
}

fn object_at(buf: &[u8], depth: usize) -> Result<Map<String, Value>, BlobError> {
    let mut m = Map::new();
    for a in parse_attrs(buf)? {
        let (k, v) = decode_at(&a, depth)?;
        m.insert(k, v);
    }
    Ok(m)
}

/// 一串 blobmsg 字段 → JSON 数组（忽略名字）。测试用。
#[cfg(test)]
pub fn blobmsg_array(buf: &[u8]) -> Result<Vec<Value>, BlobError> {
    array_at(buf, 0)
}

fn array_at(buf: &[u8], depth: usize) -> Result<Vec<Value>, BlobError> {
    parse_attrs(buf)?
        .iter()
        .map(|a| decode_at(a, depth).map(|(_, v)| v))
        .collect()
}

// ---------------------------------------------------------------- 帧

/// `struct ubus_msghdr`
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsgHdr {
    pub version: u8,
    pub msg_type: u8,
    pub seq: u16,
    pub peer: u32,
}

impl MsgHdr {
    pub fn new(msg_type: u8, seq: u16, peer: u32) -> Self {
        Self {
            version: 0,
            msg_type,
            seq,
            peer,
        }
    }
}

/// 一帧：头 + 顶层 blob 里的属性（不含顶层 blob 的 4 字节头）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub hdr: MsgHdr,
    pub body: Vec<u8>,
}

/// 编一帧：8 字节头 + id=0 的顶层 blob（包住 `body`）。
pub fn encode_frame(hdr: MsgHdr, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(UBUS_MSGHDR_LEN + BLOB_HDR_LEN + body.len());
    out.push(hdr.version);
    out.push(hdr.msg_type);
    out.extend_from_slice(&hdr.seq.to_be_bytes());
    out.extend_from_slice(&hdr.peer.to_be_bytes());
    put_attr(&mut out, 0, false, body);
    out
}

/// 解帧头的前 12 字节（ubus 头 + 顶层 blob 头），返回头和顶层 blob 的原始长度。
pub fn decode_head(
    head: &[u8; UBUS_MSGHDR_LEN + BLOB_HDR_LEN],
) -> Result<(MsgHdr, usize), BlobError> {
    let hdr = MsgHdr {
        version: head[0],
        msg_type: head[1],
        seq: u16::from_be_bytes([head[2], head[3]]),
        peer: u32::from_be_bytes([head[4], head[5], head[6], head[7]]),
    };
    let id_len = u32::from_be_bytes([head[8], head[9], head[10], head[11]]);
    let raw_len = (id_len & BLOB_ATTR_LEN_MASK) as usize;
    if raw_len < BLOB_HDR_LEN {
        return err("ubus message blob shorter than its header");
    }
    if raw_len > UBUS_MAX_MSGLEN {
        return err(format!("ubus message too large ({raw_len} bytes)"));
    }
    Ok((hdr, raw_len))
}

/// 从内存里解一整帧（mock 和测试用）。
#[cfg(test)]
pub fn decode_frame(buf: &[u8]) -> Result<Frame, BlobError> {
    let Some(head) = buf.get(..UBUS_MSGHDR_LEN + BLOB_HDR_LEN) else {
        return err("truncated ubus message header");
    };
    let (hdr, raw_len) = decode_head(head.try_into().unwrap())?;
    let Some(body) = buf.get(UBUS_MSGHDR_LEN + BLOB_HDR_LEN..UBUS_MSGHDR_LEN + raw_len) else {
        return err("truncated ubus message body");
    };
    Ok(Frame {
        hdr,
        body: body.to_vec(),
    })
}

#[derive(Debug)]
pub enum ReadError {
    /// 对端关闭（读到 EOF）或读失败。
    Io(std::io::Error),
    /// 帧不合规。
    Protocol(BlobError),
}

/// 从流里读一整帧。被取消（超时）时流可能停在帧中间，调用方必须丢掉这条连接。
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Frame, ReadError> {
    let mut head = [0u8; UBUS_MSGHDR_LEN + BLOB_HDR_LEN];
    r.read_exact(&mut head).await.map_err(ReadError::Io)?;
    let (hdr, raw_len) = decode_head(&head).map_err(ReadError::Protocol)?;
    let mut body = vec![0u8; raw_len - BLOB_HDR_LEN];
    r.read_exact(&mut body).await.map_err(ReadError::Io)?;
    Ok(Frame { hdr, body })
}
