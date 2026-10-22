//! SMS on the vendor stack: reading the two message stores, the `/v2` summary
//! block and sending (through `zwrt_wms`, or through a neighbouring modem's
//! goform endpoint).
//!
//! The firmware wraps phone numbers and bodies in an AES-256-GCM "envelope"
//! once a web crypto session exists. We register our own session key with
//! `zwrt_web` (RSA-wrapped) and keep it in memory only.

use crate::state;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::{RngCore, rngs::OsRng};
use reqwest::redirect::Policy;
use rsa::{Pkcs1v15Encrypt, RsaPublicKey, pkcs1::DecodeRsaPublicKey, pkcs8::DecodePublicKey};
use serde_json::{Value, json};
use std::{sync::LazyLock, time::Duration};
use tokio::sync::Mutex;
use zeroize::Zeroize;

/// The AES key we registered with `zwrt_web` (None until the first successful registration).
static SESSION_KEY: LazyLock<Mutex<Option<[u8; 32]>>> = LazyLock::new(|| Mutex::new(None));

/// Envelope layout on the wire (before base64): nonce, then GCM tag, then ciphertext.
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const HEADER_LEN: usize = NONCE_LEN + TAG_LEN;

/// Maximum number of messages the legacy `normalize_lists` view returns.
const NORMALIZED_CAP: usize = 32;

/// Shape check for a send request. The values end up in a ubus call or a
/// form body, so anything outside a tight alphabet is refused up front.
fn valid(number: &str, message: &str, sms_time: &str) -> bool {
    let number_ok = (1..=32).contains(&number.len())
        && number.char_indices().all(|(pos, c)| match c {
            '0'..='9' => true,
            '+' => pos == 0,
            _ => false,
        });
    // Body is UCS-2 as hex: whole bytes only.
    let body_ok = (1..=4096).contains(&message.len())
        && message.len().is_multiple_of(2)
        && message.chars().all(|c| c.is_ascii_hexdigit());
    let time_ok = (1..=64).contains(&sms_time.len())
        && sms_time
            .chars()
            .all(|c| c.is_ascii_digit() || c == ';' || c == '+' || c == '-');
    number_ok && body_ok && time_ok
}

/// The firmware hands out either an SPKI or a PKCS#1 PEM; accept both.
fn public_key(pem: &str) -> Result<RsaPublicKey, String> {
    match RsaPublicKey::from_public_key_pem(pem) {
        Ok(key) => Ok(key),
        Err(_) => RsaPublicKey::from_pkcs1_pem(pem)
            .map_err(|_| String::from("invalid vendor RSA public key")),
    }
}

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0x0f)] as char);
    }
    out
}

/// Return the registered session key, registering a fresh one if needed.
/// Registration: fetch the vendor certificate, RSA-wrap the hex form of a
/// random 32-byte key and hand it to `web_http_enstr_set`. The key is only
/// remembered once the firmware accepted it.
async fn ensure_crypto_session() -> Result<[u8; 32], String> {
    let mut slot = SESSION_KEY.lock().await;
    if let Some(existing) = slot.as_ref() {
        return Ok(*existing);
    }
    let cert = state::ubus("zwrt_web", "web_crt_get", json!({})).await?;
    let Some(pem) = cert.get("result").and_then(Value::as_str) else {
        return Err("vendor RSA certificate unavailable".into());
    };
    let rsa_key = public_key(pem)?;

    let mut fresh = [0_u8; 32];
    OsRng.fill_bytes(&mut fresh);
    let mut key_text = lower_hex(&fresh);
    let wrapped = rsa_key.encrypt(&mut OsRng, Pkcs1v15Encrypt, key_text.as_bytes());
    key_text.zeroize();
    let wrapped = wrapped.map_err(|_| String::from("vendor RSA key registration failed"))?;

    let registered = state::ubus(
        "zwrt_web",
        "web_http_enstr_set",
        json!({ "web_enstr": STANDARD.encode(wrapped) }),
    )
    .await;
    match registered {
        Ok(_) => {
            *slot = Some(fresh);
            Ok(fresh)
        }
        Err(error) => {
            fresh.zeroize();
            Err(error)
        }
    }
}

pub async fn prepare() -> bool {
    ensure_crypto_session().await.is_ok()
}

fn cipher_for(key: &[u8; 32]) -> Result<Aes256Gcm, String> {
    Aes256Gcm::new_from_slice(key).map_err(|_| String::from("invalid AES key"))
}

/// Seal `plaintext` into the vendor envelope (base64 of nonce ‖ tag ‖ ciphertext).
fn encrypt(key: &[u8; 32], plaintext: &str) -> Result<String, String> {
    const FAILED: &str = "SMS envelope encryption failed";
    let cipher = cipher_for(key)?;
    let mut nonce = [0_u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    // aes-gcm appends the tag; the vendor wants it in front of the ciphertext.
    let sealed = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| String::from(FAILED))?;
    let Some(body_len) = sealed.len().checked_sub(TAG_LEN) else {
        return Err(FAILED.into());
    };
    let (body, tag) = sealed.split_at(body_len);
    let envelope: Vec<u8> = [&nonce[..], tag, body].concat();
    Ok(STANDARD.encode(envelope))
}

async fn decrypt(value: &str) -> Result<Option<String>, String> {
    let key = *SESSION_KEY.lock().await;
    open_envelope(key.as_ref(), value)
}

/// 解厂商 AES-GCM 信封。不是信封（明文 hex、太短、不是 base64）→ `Ok(None)`。
fn open_envelope(key: Option<&[u8; 32]>, value: &str) -> Result<Option<String>, String> {
    let looks_plain_hex =
        value.len().is_multiple_of(2) && value.chars().all(|c| c.is_ascii_hexdigit());
    if value.len() < 40 || looks_plain_hex {
        return Ok(None);
    }
    let raw = match STANDARD.decode(value) {
        Ok(raw) if raw.len() > HEADER_LEN => raw,
        _ => return Ok(None),
    };
    let Some(key) = key else {
        return Err("vendor SMS crypto session unavailable".into());
    };
    let (nonce, rest) = raw.split_at(NONCE_LEN);
    let (tag, body) = rest.split_at(TAG_LEN);
    // Put it back in the order aes-gcm expects: ciphertext ‖ tag.
    let sealed: Vec<u8> = [body, tag].concat();
    let plain = cipher_for(key)?
        .decrypt(Nonce::from_slice(nonce), sealed.as_slice())
        .map_err(|_| String::from("SMS envelope authentication failed"))?;
    Ok(Some(String::from_utf8_lossy(&plain).into_owned()))
}

/// Decode UCS-2/UTF-16BE given as hex text. Non-hex characters are ignored and
/// a trailing partial code unit is dropped.
fn utf16be_hex(value: &str) -> String {
    let nibbles: Vec<u16> = value
        .chars()
        .filter_map(|c| c.to_digit(16))
        .map(|d| d as u16)
        .collect();
    let units: Vec<u16> = nibbles
        .as_chunks::<4>()
        .0
        .iter()
        .map(|&[a, b, c, d]| (a << 12) | (b << 8) | (c << 4) | d)
        .collect();
    String::from_utf16_lossy(&units)
}

/// Vendor date `yy,MM,dd,hh,mm,ss,...` → `MM-dd hh:mm`; anything else → empty.
fn sms_date(value: &str) -> String {
    let parts: Vec<&str> = value.split(',').collect();
    if parts.len() < 6 {
        return String::new();
    }
    let mut nums = [0_i64; 4];
    for (dst, src) in nums.iter_mut().zip(&parts[1..5]) {
        match src.parse() {
            Ok(n) => *dst = n,
            Err(_) => return String::new(),
        }
    }
    let [month, day, hour, minute] = nums;
    format!("{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Integer that may arrive as a JSON number or as a decimal string.
fn loose_i64(value: &Value) -> Option<i64> {
    match value {
        Value::String(text) => text.parse().ok(),
        other => other.as_i64(),
    }
}

/// First of two alternative keys that is present, as a string ("" otherwise).
fn str_field<'a>(item: &'a Value, primary: &str, fallback: &str) -> &'a str {
    item.get(primary)
        .or_else(|| item.get(fallback))
        .and_then(Value::as_str)
        .unwrap_or("")
}

pub async fn normalize_lists(replies: &[Value]) -> Vec<crate::contract::SmsItem> {
    let mut seen = std::collections::HashSet::new();
    let mut result = Vec::new();
    let rows = replies.iter().flat_map(|reply| {
        reply
            .get("list")
            .or_else(|| reply.get("messages"))
            .and_then(Value::as_array)
            .map(|rows| rows.as_slice())
            .unwrap_or(&[])
    });
    for row in rows {
        if result.len() == NORMALIZED_CAP {
            break;
        }
        let Some(id) = row.get("id").and_then(loose_i64) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let raw_number = str_field(row, "num", "number");
        let raw_text = str_field(row, "text", "content");
        // An envelope we cannot open (or authenticate) drops the row.
        let (Ok(opened_number), Ok(opened_text)) =
            (decrypt(raw_number).await, decrypt(raw_text).await)
        else {
            continue;
        };
        // Plain numbers are digits already; only decrypted ones are UCS-2 hex.
        let num = match opened_number {
            Some(hex) => utf16be_hex(&hex),
            None => raw_number.to_string(),
        };
        let text = utf16be_hex(opened_text.as_deref().unwrap_or(raw_text));
        let unread = row.get("tag").and_then(loose_i64) == Some(1);
        result.push(crate::contract::SmsItem {
            id,
            num,
            date: sms_date(row.get("date").and_then(Value::as_str).unwrap_or("")),
            unread: i64::from(unread),
            text,
        });
    }
    result
}

/// `sms.list_after` 每页条数（`data_per_page`）。等于 `limit` 上限，也是 zte-agent 在真机上一直用的页大小。
pub const LIST_PAGE: u64 = 50;
/// `sms.list_after` 的 `limit` 上限。
pub const LIST_LIMIT_MAX: u64 = 50;
/// 每个库最多翻多少页（50 × 20 = 1000 条）。翻到上限还没到 `after_id`，就证明不了哪些是最小的，整次失败。
pub const LIST_MAX_PAGES: u64 = 20;

fn item_id(item: &Value) -> Option<u64> {
    item.get("id").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str()?.trim().parse::<u64>().ok())
    })
}

/// Rows of one `zte_libwms_get_sms_data` page (`messages`, older builds `list`).
fn page_items(reply: &Value) -> Vec<Value> {
    match reply.get("messages").or_else(|| reply.get("list")) {
        Some(Value::Array(rows)) => rows.clone(),
        _ => Vec::new(),
    }
}

/// `zte_libwms_get_sms_data` 一页的参数。固件的 `order_by` 只接受 `"order by id desc"`（Gate 0 实测）。
pub fn list_page_args(mem_store: u64, page: u64) -> Value {
    json!({"page":page,"data_per_page":LIST_PAGE,"mem_store":mem_store,"tags":10,"order_by":"order by id desc"})
}

/// `sms.list_after` 的核心（纯逻辑，取页由调用方给）：`fetch(mem_store, page)` 读一页降序列表。
///
/// 两个库（NV = 1，SIM = 0，编号共用一个计数）各自从第 0 页往下翻，读到 id ≤ `after_id`、
/// 短页（不满 `LIST_PAGE`）或者一页里没有新 id 就停；合并去重、升序，取前 `limit` 条。
/// 满页的 id 集合和同库上一页完全相同，说明固件没理 `page`（翻不下去），整次失败，不当成翻完。
/// 因为每个库都翻到了 `after_id`，`has_more`（还有更多 > `after_id` 的）是确定的。
/// 任一库任一页读失败 → 整次失败，不返回半截（和 T6「两库都成功才可信」一致）。
/// 每次调用的 ubus 次数：每个库 ⌈(该库 > after_id 的条数 + 1) / 50⌉，最多 `LIST_MAX_PAGES`。
/// 每条加上 `mem_store`：读到它的那个库（1 = NV，0 = SIM；有重复时是先读到的 NV）。
pub async fn list_after_with<F, Fut>(
    after_id: u64,
    limit: u64,
    mut fetch: F,
) -> Result<(Vec<Value>, bool), String>
where
    F: FnMut(u64, u64) -> Fut,
    Fut: std::future::Future<Output = Result<Value, String>>,
{
    let mut found: std::collections::BTreeMap<u64, Value> = std::collections::BTreeMap::new();
    for store in [1_u64, 0] {
        let mut page = 0;
        let mut prev: Option<std::collections::BTreeSet<u64>> = None;
        loop {
            if page >= LIST_MAX_PAGES {
                return Err(format!(
                    "mem_store {store}: more than {} SMS above id {after_id}",
                    LIST_MAX_PAGES * LIST_PAGE
                ));
            }
            let reply = fetch(store, page)
                .await
                .map_err(|error| format!("mem_store {store}: {error}"))?;
            let items = page_items(&reply);
            let ids: std::collections::BTreeSet<u64> = items.iter().filter_map(item_id).collect();
            if items.len() as u64 >= LIST_PAGE && prev.as_ref() == Some(&ids) {
                return Err(format!(
                    "mem_store {store}: page {page} repeats page {} (firmware ignored page)",
                    page - 1
                ));
            }
            let mut reached = false;
            let mut fresh = false;
            for item in &items {
                let Some(id) = item_id(item) else { continue };
                if id <= after_id {
                    reached = true;
                    continue;
                }
                if let std::collections::btree_map::Entry::Vacant(slot) = found.entry(id) {
                    fresh = true;
                    // 固件的行不带库：记下是从哪个库读到的（网页的「SIM / 设备」）
                    let mut item = item.clone();
                    if let Some(o) = item.as_object_mut() {
                        o.insert("mem_store".into(), json!(store));
                    }
                    slot.insert(item);
                }
            }
            if reached || !fresh || (items.len() as u64) < LIST_PAGE {
                break;
            }
            prev = Some(ids);
            page += 1;
        }
    }
    let has_more = found.len() as u64 > limit;
    Ok((found.into_values().take(limit as usize).collect(), has_more))
}

/// 返回给调用方的一条：字段名和 `zte_libwms_get_sms_data` 原样（`id`、`number`、`content`、`date`、`tag`），
/// 号码和正文如果是厂商 AES-GCM 信封就解开成明文（仍是 UCS-2 hex）；解不开保留原值，不丢这条。
fn plain_item(key: Option<&[u8; 32]>, item: Value) -> Value {
    let text = |name: &str| item.get(name).and_then(Value::as_str).map(str::to_owned);
    let mut out = serde_json::Map::new();
    out.insert("id".into(), json!(item_id(&item).unwrap_or_default()));
    for (name, alt) in [("number", "num"), ("content", "text")] {
        let raw = text(name).or_else(|| text(alt)).unwrap_or_default();
        let plain = match open_envelope(key, &raw) {
            Ok(Some(plain)) => plain,
            _ => raw,
        };
        out.insert(name.into(), json!(plain));
    }
    for name in ["date", "tag", "mem_store"] {
        if let Some(value) = item.get(name) {
            out.insert(name.into(), value.clone());
        }
    }
    Value::Object(out)
}

/// `/control` 的 `sms.list_after {after_id, limit}`（R16）：在控制任务里（执行者上）连着调完。
pub async fn list_after(params: &Value) -> Result<Value, (bool, String)> {
    let object = params.as_object().expect("server validates params");
    let number = |name: &str, default: u64| match object.get(name) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| (true, format!("{name} must be a non-negative integer"))),
    };
    let after_id = number("after_id", 0)?;
    let limit = number("limit", LIST_LIMIT_MAX)?;
    if limit == 0 || limit > LIST_LIMIT_MAX {
        return Err((true, format!("limit must be 1..={LIST_LIMIT_MAX}")));
    }
    let (items, has_more) = list_after_with(after_id, limit, |store, page| {
        state::ubus(
            "zwrt_wms",
            "zte_libwms_get_sms_data",
            list_page_args(store, page),
        )
    })
    .await
    .map_err(|error| (false, error))?;
    let key = *SESSION_KEY.lock().await;
    let plain: Vec<Value> = items
        .into_iter()
        .map(|item| plain_item(key.as_ref(), item))
        .collect();
    Ok(json!({"items":plain,"has_more":has_more}))
}

/// `/v2` 短信块的 data（V2-30）：未读数、最大编号、条数。三次读取（容量、NV 第一页、SIM 第一页）
/// 都成功才算读成功。`max_id` 取两库降序第一页的原始回复（含已发送、草稿），
/// `count` = 两库 `sms_{nv,sim}_{rev,send,draftbox}_total` 之和（B27 字段；`sms_nvused_total` 不可信）。
pub fn block_data(capacity: &Value, nv: &Value, sim: &Value) -> Value {
    let num = |name: &str| {
        capacity
            .get(name)
            .and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str()?.trim().parse::<i64>().ok())
            })
            .unwrap_or(0)
    };
    let max_id = [nv, sim]
        .iter()
        .flat_map(|reply| page_items(reply))
        .filter_map(|item| item_id(&item))
        .max()
        .unwrap_or(0);
    let count: i64 = ["nv", "sim"]
        .iter()
        .flat_map(|store| {
            ["rev", "send", "draftbox"]
                .iter()
                .map(move |kind| format!("sms_{store}_{kind}_total"))
        })
        .map(|name| num(&name))
        .sum();
    crate::contract::value(&crate::contract::Sms {
        unread: num("sms_dev_unread_num") + num("sms_sim_unread_num"),
        max_id,
        count,
    })
}

/// Default goform endpoints of the two neighbouring modems, with the
/// environment variable that overrides each.
fn external_endpoint(sender: &str) -> (&'static str, &'static str) {
    match sender {
        "v3e1" => (
            "LUYAZ_SMS_V3E1_URL",
            "http://192.168.56.1/goform/goform_set_cmd_process",
        ),
        _ => (
            "LUYAZ_SMS_V3E2_URL",
            "http://192.168.57.1/goform/goform_set_cmd_process",
        ),
    }
}

/// Body limit for the goform reply; anything larger is not a real answer.
const GOFORM_REPLY_MAX: usize = 4096;

/// Send through another modem's stock web form (`goformId=SEND_SMS`).
async fn send_external(
    sender: &str,
    number: &str,
    message: &str,
    sms_time: &str,
) -> Result<Value, String> {
    let failed = || format!("{sender} SMS send failed");
    let (env_name, fallback) = external_endpoint(sender);
    let url = std::env::var(env_name).unwrap_or_else(|_| fallback.to_string());
    // `valid` already limited both values to digits and `;+-`, so only '+' needs escaping.
    let number = number.replace('+', "%2B");
    let sms_time = sms_time.replace('+', "%2B");
    let body = format!(
        "goformId=SEND_SMS&Number={number}&MessageBody={message}&ID=-1&encode_type=UNICODE&sms_time={sms_time}"
    );
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(Policy::none())
        .build()
        .map_err(|e| e.to_string())?;
    let reply = http
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|_| format!("{sender} SMS endpoint unavailable"))?;
    let too_long = reply
        .content_length()
        .is_some_and(|len| len > GOFORM_REPLY_MAX as u64);
    if too_long || !reply.status().is_success() {
        return Err(failed());
    }
    let raw = reply.bytes().await.map_err(|_| failed())?;
    if raw.len() > GOFORM_REPLY_MAX {
        return Err(failed());
    }
    let accepted = serde_json::from_slice::<Value>(&raw)
        .is_ok_and(|reply| reply.get("result").and_then(Value::as_str) == Some("success"));
    if !accepted {
        return Err(failed());
    }
    Ok(json!({"sender":sender,"status":3}))
}

/// Active SIM slot as the MDM reports it (0 if unknown).
async fn current_slot() -> i64 {
    let Ok(info) = state::sim_info().await else {
        return 0;
    };
    ["current_sim_slot", "sim_slot"]
        .iter()
        .find_map(|key| info.get(*key))
        .and_then(Value::as_i64)
        .unwrap_or(0)
}

fn provision_args(slot: i64, active: bool) -> Value {
    json!({"active_slot": slot, "active_flag": i64::from(active)})
}

/// Switch the provisioning session to `slot` and wait (≤15 s) until the MDM
/// reports it as current.
async fn select_slot(slot: i64) -> Result<(), String> {
    const METHOD: &str = "zwrt_mdm_change_provision_session";
    let other = if slot == 2 { 1 } else { 2 };
    // Releasing the other slot is best effort; the activation below is what counts.
    let _ = state::ubus("zwrt_zte_mdm.api", METHOD, provision_args(other, false)).await;
    state::ubus("zwrt_zte_mdm.api", METHOD, provision_args(slot, true)).await?;
    let mut attempts = 30;
    while attempts > 0 {
        if current_slot().await == slot {
            return Ok(());
        }
        attempts -= 1;
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!("SIM slot {slot} did not become active"))
}

/// `zwrt_wms_get_cmd_status` result codes for the send command (`sms_cmd` 4).
const CMD_DONE: i64 = 3;
const CMD_FAILED: i64 = 2;

/// Send through this device's own `zwrt_wms`, then poll the command status
/// (up to 20 × 500 ms).
async fn send_host(number: &str, message: &str, sms_time: &str) -> Result<Value, String> {
    let (sealed_number, sealed_body) = {
        let mut key = ensure_crypto_session().await?;
        let sealed = encrypt(&key, number).and_then(|n| Ok((n, encrypt(&key, message)?)));
        key.zeroize();
        sealed?
    };
    let request = json!({
        "number": sealed_number,
        "message_body": sealed_body,
        "sms_time": sms_time,
        "encode_type": "UNICODE",
        "id": "-1",
    });
    state::ubus("zwrt_wms", "zte_libwms_send_sms", request).await?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let status = state::ubus("zwrt_wms", "zwrt_wms_get_cmd_status", json!({"sms_cmd":4}))
            .await
            .ok()
            .and_then(|reply| reply.get("sms_cmd_status_result").and_then(Value::as_i64));
        match status {
            Some(CMD_DONE) => return Ok(json!({"sender":"host","status":3})),
            Some(CMD_FAILED) => return Err("zwrt_wms SMS command failed".into()),
            _ => {}
        }
    }
    Err("zwrt_wms SMS command timed out".into())
}

/// Which path a `sender` value selects.
enum Route {
    External,
    Host,
    Slot(i64),
}

fn route(sender: &str) -> Option<Route> {
    Some(match sender {
        "v3e1" | "v3e2" => Route::External,
        "host" | "x75" => Route::Host,
        "sim1" => Route::Slot(1),
        "sim2" => Route::Slot(2),
        _ => return None,
    })
}

pub async fn send(params: &Value) -> Result<Value, (bool, String)> {
    let fields = params.as_object().expect("server validates params");
    let get = |key: &str| fields.get(key).and_then(Value::as_str);
    let sender = get("sender").unwrap_or("host");
    let Some((number, message, sms_time)) = get("number")
        .zip(get("message_hex"))
        .zip(get("sms_time"))
        .map(|((n, m), t)| (n, m, t))
    else {
        return Err((true, "missing SMS parameters".into()));
    };
    if !valid(number, message, sms_time) {
        return Err((true, "invalid SMS parameters".into()));
    }
    let Some(path) = route(sender) else {
        return Err((true, "invalid SMS sender".into()));
    };
    let outcome = match path {
        Route::External => send_external(sender, number, message, sms_time).await,
        Route::Host => send_host(number, message, sms_time).await,
        Route::Slot(slot) => {
            if current_slot().await != slot {
                select_slot(slot).await.map_err(|e| (false, e))?;
            }
            send_host(number, message, sms_time).await
        }
    };
    outcome.map_err(|e| (false, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_external_shape() {
        // Accepted: international number, hex UCS-2 body, vendor time string.
        assert!(valid("+8613800000000", "6D4B8BD5", "26;08;27;04;00;00;+;0"));
        // Separator inside the number is refused.
        assert!(!valid("+1;reboot", "00", "1"));
        // Odd-length body is refused.
        assert!(!valid("10086", "0", "1"));
    }

    #[test]
    fn aes_envelope_layout_roundtrip() {
        let key = [7_u8; 32];
        let sealed = STANDARD.decode(encrypt(&key, "6D4B8BD5").unwrap()).unwrap();
        // nonce + tag in front of an 8-byte body.
        assert_eq!(sealed.len(), HEADER_LEN + 8);
        let (nonce, rest) = sealed.split_at(NONCE_LEN);
        let (tag, body) = rest.split_at(TAG_LEN);
        let reordered = [body, tag].concat();
        let opened = Aes256Gcm::new_from_slice(&key)
            .unwrap()
            .decrypt(Nonce::from_slice(nonce), reordered.as_slice())
            .unwrap();
        assert_eq!(opened, b"6D4B8BD5");
    }

    #[tokio::test]
    async fn normalizes_plain_sms_and_deduplicates() {
        let first = json!({"messages":[{"id":"7","num":"10086","date":"26,08,27,04,00,00,+,0","tag":"1","text":"6D4B8BD5"}]});
        let again = json!({"list":[{"id":7,"number":"duplicate","content":"0041"}]});
        let out = normalize_lists(&[first, again]).await;
        assert_eq!(out.len(), 1);
        let only = &out[0];
        assert_eq!(
            (
                only.num.as_str(),
                only.text.as_str(),
                only.date.as_str(),
                only.unread
            ),
            ("10086", "测试", "08-27 04:00", 1)
        );
    }

    #[tokio::test]
    async fn authenticates_and_normalizes_encrypted_sms() {
        let key = [9_u8; 32];
        *SESSION_KEY.lock().await = Some(key);
        let row = json!({
            "id": 8,
            "num": encrypt(&key, "00310030003000380036").unwrap(),
            "date": "26,08,27,04,00,00,+,0",
            "tag": 0,
            "text": encrypt(&key, "6D4B8BD5").unwrap(),
        });
        let out = normalize_lists(&[json!({ "list": [row] })]).await;
        *SESSION_KEY.lock().await = None;
        assert_eq!(out[0].num, "10086");
        assert_eq!(out[0].text, "测试");
    }

    /// 假的 `zte_libwms_get_sms_data`：照固件的规矩，只接受 `order by id desc`，按 page/data_per_page 切页。
    fn fake_wms(nv: &[u64], sim: &[u64], args: &Value) -> Result<Value, String> {
        if args["order_by"] != "order by id desc" {
            return Err("Invalid argument".into());
        }
        let mut ids: Vec<u64> = if args["mem_store"] == 1 { nv } else { sim }.to_vec();
        ids.sort_unstable_by(|a, b| b.cmp(a));
        let per = args["data_per_page"].as_u64().unwrap() as usize;
        let page = args["page"].as_u64().unwrap() as usize;
        let rows: Vec<Value> = ids
            .iter()
            .skip(page * per)
            .take(per)
            .map(|id| json!({"id":id.to_string(),"number":"10086","content":"0041","tag":"1","date":"26,08,27,04,00,00,+32"}))
            .collect();
        Ok(json!({"messages":rows}))
    }

    async fn run(
        nv: &[u64],
        sim: &[u64],
        after: u64,
        limit: u64,
        fail_store: Option<u64>,
    ) -> (Result<(Vec<u64>, bool), String>, Vec<Value>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let r = list_after_with(after, limit, |store, page| {
            let args = list_page_args(store, page);
            calls.borrow_mut().push(args.clone());
            let reply = if fail_store == Some(store) {
                Err("timeout".into())
            } else {
                fake_wms(nv, sim, &args)
            };
            async move { reply }
        })
        .await
        .map(|(items, more)| (items.iter().filter_map(item_id).collect(), more));
        (r, calls.into_inner())
    }

    #[tokio::test]
    async fn sms_list_after_pages_desc_only() {
        // 升序被固件拒绝：只能降序翻页，每个库一直翻到 ≤ after_id。
        assert!(
            fake_wms(
                &[1],
                &[],
                &json!({"order_by":"order by id asc","mem_store":1,"page":0,"data_per_page":50})
            )
            .is_err()
        );
        let nv: Vec<u64> = (1..=130).collect();
        let (r, calls) = run(&nv, &[], 10, 50, None).await;
        let (ids, more) = r.unwrap();
        assert_eq!(ids, (11..=60).collect::<Vec<_>>());
        assert!(more);
        assert!(calls.iter().all(|a| a["order_by"] == "order by id desc"
            && a["data_per_page"] == 50
            && a["tags"] == 10));
        // NV：第 0 页 130..81，第 1 页 80..31，第 2 页 30..1（碰到 10，停）；SIM：空页一次。
        let nv_pages: Vec<u64> = calls
            .iter()
            .filter(|a| a["mem_store"] == 1)
            .map(|a| a["page"].as_u64().unwrap())
            .collect();
        assert_eq!(nv_pages, vec![0, 1, 2]);
        assert_eq!(calls.len(), 4);
        // 什么都没有新的：每个库一页。
        let (r, calls) = run(&nv, &[], 130, 50, None).await;
        assert_eq!(r.unwrap(), (vec![], false));
        assert_eq!(calls.len(), 2);
    }

    #[tokio::test]
    async fn sms_list_after_merges_stores_dedup() {
        // 编号在两库之间共用：奇数在 NV，偶数在 SIM；7 两边都有（重复只出一次）。
        let nv: Vec<u64> = (1..=40).filter(|i| i % 2 == 1).collect();
        let sim: Vec<u64> = (1..=40).filter(|i| i % 2 == 0).chain([7]).collect();
        let (r, _) = run(&nv, &sim, 4, 50, None).await;
        let (ids, more) = r.unwrap();
        assert_eq!(ids, (5..=40).collect::<Vec<_>>());
        assert!(!more);
        // 每条带上读到它的库；两边都有的 7 算 NV（先读）
        let r = list_after_with(4, 50, |store, page| {
            let reply = fake_wms(&nv, &sim, &list_page_args(store, page));
            async move { reply }
        })
        .await
        .unwrap();
        let store = |id: u64| {
            r.0.iter()
                .find(|i| item_id(i) == Some(id))
                .map(|i| i["mem_store"].clone())
        };
        assert_eq!(store(5), Some(json!(1)));
        assert_eq!(store(6), Some(json!(0)));
        assert_eq!(store(7), Some(json!(1)));
    }

    #[tokio::test]
    async fn sms_list_after_limit_has_more() {
        let nv: Vec<u64> = (1..=600).filter(|i| i % 5 != 0).collect();
        let sim: Vec<u64> = (1..=600).filter(|i| i % 5 == 0).collect();
        let (r, _) = run(&nv, &sim, 0, 3, None).await;
        assert_eq!(r.unwrap(), (vec![1, 2, 3], true));
        let (r, _) = run(&nv, &sim, 597, 3, None).await;
        assert_eq!(r.unwrap(), (vec![598, 599, 600], false));
        let (r, _) = run(&nv, &sim, 596, 3, None).await;
        assert_eq!(r.unwrap(), (vec![597, 598, 599], true));
        // 600 条突发：按 after_id 翻页全部拿到，不重不漏。
        let (mut after, mut got, mut rounds) = (0, Vec::new(), 0);
        loop {
            let (r, _) = run(&nv, &sim, after, 50, None).await;
            let (ids, more) = r.unwrap();
            after = *ids.last().unwrap_or(&after);
            got.extend(ids);
            rounds += 1;
            if !more {
                break;
            }
        }
        assert_eq!(got, (1..=600).collect::<Vec<_>>());
        assert_eq!(rounds, 12);
    }

    #[tokio::test]
    async fn sms_list_after_page_ignored_fails_whole() {
        // 固件不理 page：第 1 页和第 0 页一模一样（满页）。不能当成翻完（会漏更早的新短信、has_more 假），整次失败。
        let calls = std::cell::Cell::new(0);
        let r = list_after_with(0, 50, |store, _| {
            calls.set(calls.get() + 1);
            let base = if store == 1 { 100 } else { 0 };
            let rows: Vec<Value> = (1..=50).rev().map(|i| json!({"id": base + i})).collect();
            async move { Ok(json!({"messages":rows})) }
        })
        .await;
        let e = r.unwrap_err();
        assert!(
            e.contains("mem_store 1") && e.contains("ignored page"),
            "{e}"
        );
        assert_eq!(calls.get(), 2);
        // 短页重复不算（第 0 页就是短页，直接停）。
        let r = list_after_with(0, 50, |_, _| async {
            Ok(json!({"messages":[{"id":3},{"id":2}]}))
        })
        .await
        .unwrap();
        assert_eq!(r.0.len(), 2);
    }

    #[tokio::test]
    async fn sms_list_after_one_store_fails_whole() {
        let nv: Vec<u64> = (1..=10).collect();
        for store in [1, 0] {
            let (r, _) = run(&nv, &[20], 0, 50, Some(store)).await;
            assert!(r.unwrap_err().contains(&format!("mem_store {store}")));
        }
    }

    #[test]
    fn sms_list_after_returns_plain_fields() {
        let key = [5_u8; 32];
        let number = encrypt(&key, "00310030003000380036").unwrap();
        let item = json!({"id":"9","number":number,"content":"0041","tag":"0","date":"d","x":1});
        let want =
            json!({"id":9,"number":"00310030003000380036","content":"0041","tag":"0","date":"d"});
        assert_eq!(plain_item(Some(&key), item.clone()), want);
        // 没有会话密钥：解不开就保留原值，不丢这条。
        assert_eq!(plain_item(None, item)["number"], json!(number));
    }

    #[test]
    fn sms_block_summary_fields() {
        let capacity = json!({"sms_dev_unread_num":"2","sms_sim_unread_num":1,"sms_nv_rev_total":"6","sms_nv_send_total":2,"sms_nv_draftbox_total":0,"sms_sim_rev_total":1,"sms_nvused_total":0});
        let nv = json!({"messages":[{"id":"31","tag":"2"},{"id":30}]});
        let sim = json!({"messages":[{"id":12}]});
        assert_eq!(
            block_data(&capacity, &nv, &sim),
            json!({"unread":3,"max_id":31,"count":9})
        );
        assert_eq!(
            block_data(&json!({}), &json!({}), &json!({"messages":[]})),
            json!({"unread":0,"max_id":0,"count":0})
        );
    }
}
