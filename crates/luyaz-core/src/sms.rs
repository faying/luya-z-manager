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

static SESSION_KEY: LazyLock<Mutex<Option<[u8; 32]>>> = LazyLock::new(|| Mutex::new(None));

fn valid(number: &str, message: &str, sms_time: &str) -> bool {
    !number.is_empty()
        && number.len() <= 32
        && number
            .bytes()
            .enumerate()
            .all(|(index, byte)| byte.is_ascii_digit() || (index == 0 && byte == b'+'))
        && !message.is_empty()
        && message.len() <= 4096
        && message.len().is_multiple_of(2)
        && message.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !sms_time.is_empty()
        && sms_time.len() <= 64
        && sms_time
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b';' | b'+' | b'-'))
}

fn public_key(pem: &str) -> Result<RsaPublicKey, String> {
    RsaPublicKey::from_public_key_pem(pem)
        .or_else(|_| RsaPublicKey::from_pkcs1_pem(pem))
        .map_err(|_| "invalid vendor RSA public key".into())
}

async fn ensure_crypto_session() -> Result<[u8; 32], String> {
    let mut guard = SESSION_KEY.lock().await;
    if let Some(key) = *guard {
        return Ok(key);
    }
    let reply = state::ubus("zwrt_web", "web_crt_get", json!({})).await?;
    let pem = reply
        .get("result")
        .and_then(Value::as_str)
        .ok_or("vendor RSA certificate unavailable")?;
    let public = public_key(pem)?;
    let mut key = [0_u8; 32];
    OsRng.fill_bytes(&mut key);
    let mut hex = String::with_capacity(64);
    for byte in key {
        use std::fmt::Write as _;
        write!(&mut hex, "{byte:02x}").map_err(|e| e.to_string())?;
    }
    let wrapped = public
        .encrypt(&mut OsRng, Pkcs1v15Encrypt, hex.as_bytes())
        .map_err(|_| "vendor RSA key registration failed")?;
    hex.zeroize();
    if let Err(error) = state::ubus(
        "zwrt_web",
        "web_http_enstr_set",
        json!({"web_enstr":STANDARD.encode(wrapped)}),
    )
    .await
    {
        key.zeroize();
        return Err(error);
    }
    *guard = Some(key);
    Ok(key)
}

pub async fn prepare() -> bool {
    ensure_crypto_session().await.is_ok()
}

fn encrypt(key: &[u8; 32], plaintext: &str) -> Result<String, String> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "invalid AES key")?;
    let mut nonce = [0_u8; 12];
    OsRng.fill_bytes(&mut nonce);
    let mut encrypted = cipher
        .encrypt(Nonce::from_slice(&nonce), plaintext.as_bytes())
        .map_err(|_| "SMS envelope encryption failed")?;
    if encrypted.len() < 16 {
        return Err("SMS envelope encryption failed".into());
    }
    let tag = encrypted.split_off(encrypted.len() - 16);
    let mut envelope = Vec::with_capacity(28 + encrypted.len());
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&tag);
    envelope.extend_from_slice(&encrypted);
    Ok(STANDARD.encode(envelope))
}

async fn decrypt(value: &str) -> Result<Option<String>, String> {
    let key = *SESSION_KEY.lock().await;
    open_envelope(key.as_ref(), value)
}

/// 解厂商 AES-GCM 信封。不是信封（明文 hex、太短、不是 base64）→ `Ok(None)`。
fn open_envelope(key: Option<&[u8; 32]>, value: &str) -> Result<Option<String>, String> {
    if value.len() < 40
        || value.len().is_multiple_of(2) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Ok(None);
    }
    let Ok(raw) = STANDARD.decode(value) else {
        return Ok(None);
    };
    if raw.len() <= 28 {
        return Ok(None);
    }
    let key = key.ok_or("vendor SMS crypto session unavailable")?;
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| "invalid AES key")?;
    let mut input = raw[28..].to_vec();
    input.extend_from_slice(&raw[12..28]);
    cipher
        .decrypt(Nonce::from_slice(&raw[..12]), input.as_ref())
        .map(|plain| Some(String::from_utf8_lossy(&plain).into_owned()))
        .map_err(|_| "SMS envelope authentication failed".into())
}

fn utf16be_hex(value: &str) -> String {
    let digits: String = value.chars().filter(|ch| ch.is_ascii_hexdigit()).collect();
    let units: Vec<u16> = digits
        .as_bytes()
        .as_chunks::<4>()
        .0
        .iter()
        .filter_map(|chunk| std::str::from_utf8(chunk).ok())
        .filter_map(|chunk| u16::from_str_radix(chunk, 16).ok())
        .collect();
    String::from_utf16_lossy(&units)
}

fn sms_date(value: &str) -> String {
    let fields: Vec<_> = value.split(',').collect();
    if fields.len() >= 6 {
        let parsed: Option<Vec<i64>> = fields[1..=4]
            .iter()
            .map(|field| field.parse().ok())
            .collect();
        if let Some(parts) = parsed {
            return format!(
                "{:02}-{:02} {:02}:{:02}",
                parts[0], parts[1], parts[2], parts[3]
            );
        }
    }
    String::new()
}

pub async fn normalize_lists(replies: &[Value]) -> Vec<crate::contract::SmsItem> {
    let mut output = Vec::new();
    let mut ids = std::collections::HashSet::new();
    let items: Vec<Value> = replies
        .iter()
        .flat_map(|reply| {
            reply
                .get("list")
                .or_else(|| reply.get("messages"))
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
        })
        .collect();
    for item in items {
        if output.len() >= 32 {
            break;
        }
        let Some(id) = item.get("id").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.parse::<i64>().ok())
        }) else {
            continue;
        };
        if !ids.insert(id) {
            continue;
        }
        let number_raw = item
            .get("num")
            .or_else(|| item.get("number"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let text_raw = item
            .get("text")
            .or_else(|| item.get("content"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Ok(number) = decrypt(number_raw).await else {
            continue;
        };
        let Ok(text) = decrypt(text_raw).await else {
            continue;
        };
        let number = number.map_or_else(|| number_raw.to_owned(), |value| utf16be_hex(&value));
        let text = utf16be_hex(text.as_deref().unwrap_or(text_raw));
        let unread = item.get("tag").and_then(|value| {
            value
                .as_i64()
                .or_else(|| value.as_str()?.parse::<i64>().ok())
        }) == Some(1);
        output.push(crate::contract::SmsItem {
            id,
            num: number,
            date: sms_date(item.get("date").and_then(Value::as_str).unwrap_or_default()),
            unread: i64::from(unread),
            text,
        });
    }
    output
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

fn page_items(reply: &Value) -> Vec<Value> {
    reply
        .get("messages")
        .or_else(|| reply.get("list"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
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

async fn send_external(
    sender: &str,
    number: &str,
    message: &str,
    sms_time: &str,
) -> Result<Value, String> {
    let default = if sender == "v3e1" {
        "http://192.168.56.1/goform/goform_set_cmd_process"
    } else {
        "http://192.168.57.1/goform/goform_set_cmd_process"
    };
    let variable = if sender == "v3e1" {
        "LUYAZ_SMS_V3E1_URL"
    } else {
        "LUYAZ_SMS_V3E2_URL"
    };
    let url = std::env::var(variable).unwrap_or_else(|_| default.into());
    let encode_plus = |value: &str| value.replace('+', "%2B");
    let form = format!(
        "goformId=SEND_SMS&Number={}&MessageBody={message}&ID=-1&encode_type=UNICODE&sms_time={}",
        encode_plus(number),
        encode_plus(sms_time)
    );
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .post(url)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(form)
        .send()
        .await
        .map_err(|_| format!("{sender} SMS endpoint unavailable"))?;
    if !response.status().is_success() || response.content_length().is_some_and(|v| v > 4096) {
        return Err(format!("{sender} SMS send failed"));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|_| format!("{sender} SMS send failed"))?;
    if bytes.len() > 4096
        || serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("result")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            })
            .as_deref()
            != Some("success")
    {
        return Err(format!("{sender} SMS send failed"));
    }
    Ok(json!({"sender":sender,"status":3}))
}

async fn current_slot() -> i64 {
    state::ubus("zwrt_zte_mdm.api", "get_sim_info", json!({}))
        .await
        .ok()
        .and_then(|value| {
            value
                .get("current_sim_slot")
                .or_else(|| value.get("sim_slot"))
                .and_then(Value::as_i64)
        })
        .unwrap_or_default()
}

async fn select_slot(slot: i64) -> Result<(), String> {
    let old = if slot == 2 { 1 } else { 2 };
    let _ = state::ubus(
        "zwrt_zte_mdm.api",
        "zwrt_mdm_change_provision_session",
        json!({"active_slot":old,"active_flag":0}),
    )
    .await;
    state::ubus(
        "zwrt_zte_mdm.api",
        "zwrt_mdm_change_provision_session",
        json!({"active_slot":slot,"active_flag":1}),
    )
    .await?;
    for _ in 0..30 {
        if current_slot().await == slot {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(format!("SIM slot {slot} did not become active"))
}

async fn send_host(number: &str, message: &str, sms_time: &str) -> Result<Value, String> {
    let mut key = ensure_crypto_session().await?;
    let encrypted_number = encrypt(&key, number)?;
    let encrypted_message = encrypt(&key, message)?;
    key.zeroize();
    state::ubus(
        "zwrt_wms",
        "zte_libwms_send_sms",
        json!({"number":encrypted_number,"message_body":encrypted_message,"sms_time":sms_time,"encode_type":"UNICODE","id":"-1"}),
    )
    .await?;
    for _ in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let Ok(reply) =
            state::ubus("zwrt_wms", "zwrt_wms_get_cmd_status", json!({"sms_cmd":4})).await
        else {
            continue;
        };
        match reply.get("sms_cmd_status_result").and_then(Value::as_i64) {
            Some(3) => return Ok(json!({"sender":"host","status":3})),
            Some(2) => return Err("zwrt_wms SMS command failed".into()),
            _ => {}
        }
    }
    Err("zwrt_wms SMS command timed out".into())
}

pub async fn send(params: &Value) -> Result<Value, (bool, String)> {
    let object = params.as_object().expect("server validates params");
    let text = |name: &str| object.get(name).and_then(Value::as_str);
    let sender = text("sender").unwrap_or("host");
    let (Some(number), Some(message), Some(sms_time)) =
        (text("number"), text("message_hex"), text("sms_time"))
    else {
        return Err((true, "missing SMS parameters".into()));
    };
    if !valid(number, message, sms_time) {
        return Err((true, "invalid SMS parameters".into()));
    }
    let result = match sender {
        "v3e1" | "v3e2" => send_external(sender, number, message, sms_time).await,
        "host" | "x75" => send_host(number, message, sms_time).await,
        "sim1" | "sim2" => {
            let slot = if sender == "sim1" { 1 } else { 2 };
            if current_slot().await != slot {
                select_slot(slot).await.map_err(|error| (false, error))?;
            }
            send_host(number, message, sms_time).await
        }
        _ => return Err((true, "invalid SMS sender".into())),
    };
    result.map_err(|error| (false, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_external_shape() {
        assert!(valid("+8613800000000", "6D4B8BD5", "26;08;27;04;00;00;+;0"));
        assert!(!valid("+1;reboot", "00", "1"));
        assert!(!valid("10086", "0", "1"));
    }

    #[test]
    fn aes_envelope_layout_roundtrip() {
        let key = [7_u8; 32];
        let encoded = encrypt(&key, "6D4B8BD5").unwrap();
        let raw = STANDARD.decode(encoded).unwrap();
        assert_eq!(raw.len(), 28 + 8);
        let mut input = raw[28..].to_vec();
        input.extend_from_slice(&raw[12..28]);
        let plain = Aes256Gcm::new_from_slice(&key)
            .unwrap()
            .decrypt(Nonce::from_slice(&raw[..12]), input.as_ref())
            .unwrap();
        assert_eq!(plain, b"6D4B8BD5");
    }

    #[tokio::test]
    async fn normalizes_plain_sms_and_deduplicates() {
        let replies = [
            json!({"messages":[{"id":"7","num":"10086","date":"26,08,27,04,00,00,+,0","tag":"1","text":"6D4B8BD5"}]}),
            json!({"list":[{"id":7,"number":"duplicate","content":"0041"}]}),
        ];
        let list = normalize_lists(&replies).await;
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].num, "10086");
        assert_eq!(list[0].text, "测试");
        assert_eq!(list[0].date, "08-27 04:00");
        assert_eq!(list[0].unread, 1);
    }

    #[tokio::test]
    async fn authenticates_and_normalizes_encrypted_sms() {
        let key = [9_u8; 32];
        *SESSION_KEY.lock().await = Some(key);
        let number = encrypt(&key, "00310030003000380036").unwrap();
        let text = encrypt(&key, "6D4B8BD5").unwrap();
        let list = normalize_lists(&[json!({"list":[{
            "id":8,"num":number,"date":"26,08,27,04,00,00,+,0","tag":0,"text":text
        }]})])
        .await;
        *SESSION_KEY.lock().await = None;
        assert_eq!(list[0].num, "10086");
        assert_eq!(list[0].text, "测试");
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
