//! APN 事务（第三批 3-3，用户 10-13 定「Codex 修正版」）：一项 `apn`，值 =
//! `{mode, manual, fp}`——模式（0 自动 / 1 手动）、启用的手动 profile（没有为 ""）、它内容的指纹。
//!
//! - 切模式的目标只有 `{mode}`（自动表挑哪条事先不知道，`txn::matches` 只比目标里有的键）；
//!   启用 = `{mode: 1, manual}`，自动模式下拒绝（原厂语义没确认）；改正在用的那条 = `{mode: 1, manual, fp}`，
//!   改别的 profile、新增不是事务（`resolve` 回 None），删正在用的在 `/control` 拒绝。
//! - 指纹 = HMAC-SHA256（本机随机密钥 `apn.key`）算整条内容（含密码）：比得出改没改，猜不出密码。
//! - 密码不进事务值、op 块、流水账。退回/撤销要的旧内容存在本机 `apn-vault.json`（0600，按指纹，留最近 8 条）；
//!   设备配置本来就存明文，这份不离开设备。撤销前核对现状还是那次改完的样子（`expect`），被别人改过就拒绝。

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const KEY: &str = "apn.key";
const VAULT: &str = "apn-vault.json";
const VAULT_MAX: usize = 8;

/// 原厂 profile 里参与指纹、改的时候要写回去的字段（`modify_manu_apn` 的参数名）。
const FIELDS: &[&str] = &[
    "profilename",
    "wanapn",
    "username",
    "password",
    "pppAuthMode",
    "pdpType",
    "roamingPdpType",
];

/// 一条 profile 的规范内容：只留 [`FIELDS`]，数字统一成数字、字符串统一成字符串。
pub fn content(p: &Value) -> Value {
    let mut m = Map::new();
    for k in FIELDS {
        let v = p.get(*k).cloned().unwrap_or(Value::Null);
        let v = match (*k, v) {
            ("pppAuthMode" | "pdpType" | "roamingPdpType", Value::String(s)) => {
                s.trim().parse::<i64>().map_or(Value::Null, |n| json!(n))
            }
            ("pppAuthMode" | "pdpType" | "roamingPdpType", v @ Value::Number(_)) => v,
            ("pppAuthMode" | "pdpType" | "roamingPdpType", _) => Value::Null,
            (_, Value::String(s)) => Value::String(s),
            (_, Value::Null) => Value::String(String::new()),
            (_, v) => Value::String(v.to_string()),
        };
        m.insert((*k).into(), v);
    }
    Value::Object(m)
}

/// `apn.modify` 的 `/control` 参数（`name`、`apn`…，见 `control.rs` 的 `apn()`）→ 原厂字段名的内容。
/// 没给的可选字段按 `base`（改之前的那条）补，和原厂「整条写回」一致。
pub fn content_from_params(params: &Value, base: &Value) -> Value {
    let pick = |ours: &str, theirs: &str| {
        params
            .get(ours)
            .cloned()
            .unwrap_or_else(|| base.get(theirs).cloned().unwrap_or(Value::Null))
    };
    content(&json!({
        "profilename": pick("name", "profilename"),
        "wanapn": pick("apn", "wanapn"),
        "username": pick("username", "username"),
        "password": pick("password", "password"),
        "pppAuthMode": pick("auth_mode", "pppAuthMode"),
        "pdpType": pick("pdp_type", "pdpType"),
        "roamingPdpType": pick("roaming_pdp_type", "roamingPdpType"),
    }))
}

/// 本机密钥和保管箱。`dir` 为 None（测试、目录写不了）时用进程内随机密钥、不保管（撤销改内容会被拒绝）。
/// Wi-Fi 密码（第四批 4-0b）用同一把密钥、另一个保管箱文件（[`Vault::open_as`]）。
pub struct Vault {
    dir: Option<PathBuf>,
    key: [u8; 32],
    file: &'static str,
    max: usize,
}

impl Vault {
    pub fn open(dir: Option<PathBuf>) -> Self {
        Self::open_as(dir, VAULT, VAULT_MAX)
    }

    /// 同一把本机密钥，旧内容存在 `dir/file`（0600，最多 `max` 条）。
    pub fn open_as(dir: Option<PathBuf>, file: &'static str, max: usize) -> Self {
        let key = dir
            .as_ref()
            .and_then(|d| {
                load_or_make_key(d)
                    .map_err(|e| eprintln!("ops: {KEY}: {e}"))
                    .ok()
            })
            .unwrap_or_else(rand::random);
        Self {
            dir,
            key,
            file,
            max,
        }
    }

    /// 内容的指纹（HMAC-SHA256，十六进制前 32 位）。
    pub fn fp(&self, content: &Value) -> String {
        let msg = serde_json::to_vec(content).expect("content serializes");
        let mut k = [0u8; 64];
        k[..32].copy_from_slice(&self.key);
        let pad = |b: u8| k.iter().map(|x| x ^ b).collect::<Vec<_>>();
        let inner = Sha256::new()
            .chain_update(pad(0x36))
            .chain_update(&msg)
            .finalize();
        let outer = Sha256::new()
            .chain_update(pad(0x5c))
            .chain_update(inner)
            .finalize();
        outer.iter().take(16).map(|b| format!("{b:02x}")).collect()
    }

    /// 记下一条内容（按指纹，最近的在前）。
    pub fn keep(&self, id: &str, content: &Value) {
        let Some(dir) = &self.dir else { return };
        let fp = self.fp(content);
        let mut list = self.list();
        list.retain(|e| e.get("fp").and_then(Value::as_str) != Some(fp.as_str()));
        list.insert(0, json!({"fp": fp, "id": id, "content": content}));
        list.truncate(self.max);
        let bytes = serde_json::to_vec(&list).expect("vault serializes");
        if let Err(e) = write_private(dir, self.file, &bytes) {
            eprintln!("ops: cannot save {}: {e}", self.file);
        }
    }

    /// 按指纹取回内容。
    pub fn get(&self, fp: &str) -> Option<Value> {
        self.list()
            .into_iter()
            .find(|e| e.get("fp").and_then(Value::as_str) == Some(fp))
            .and_then(|e| e.get("content").cloned())
    }

    fn list(&self) -> Vec<Value> {
        self.dir
            .as_ref()
            .and_then(|d| fs::read(d.join(self.file)).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }
}

fn load_or_make_key(dir: &Path) -> std::io::Result<[u8; 32]> {
    if let Ok(b) = fs::read(dir.join(KEY))
        && let Ok(k) = <[u8; 32]>::try_from(b.as_slice())
    {
        return Ok(k);
    }
    fs::create_dir_all(dir)?;
    let k: [u8; 32] = rand::random();
    write_private(dir, KEY, &k)?;
    Ok(k)
}

/// 只有自己能读写（0600）的原子写。
fn write_private(dir: &Path, name: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = dir.join(format!(".{name}.tmp"));
    {
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, dir.join(name))
}

/// 设备现状：模式、启用的手动 profile 和它的规范内容（读回和写步骤都从这里来）。
#[derive(Clone, Debug, PartialEq)]
pub struct Now {
    pub mode: i64,
    pub manual: String,
    /// 启用的那条手动 profile 的规范内容（没有为 None）。
    pub content: Option<Value>,
    /// 手动表里每条的 (编号, 规范内容)。
    pub list: Vec<(String, Value)>,
}

impl Now {
    /// `get_apn_mode`、`get_manu_apn_list` 的回复。
    pub fn parse(mode: &Value, manual: &Value) -> Option<Self> {
        let mode = mode.get("apn_mode")?.as_i64()?;
        let rows = manual.get("apnListArray")?.as_array()?;
        let list: Vec<(String, Value)> = rows
            .iter()
            .map(|p| {
                let id = p.get("profileId").and_then(Value::as_str).unwrap_or("");
                (id.to_owned(), content(p))
            })
            .collect();
        let on = rows
            .iter()
            .find(|p| p.get("isEnable").and_then(Value::as_bool) == Some(true));
        let manual = on
            .and_then(|p| p.get("profileId").and_then(Value::as_str))
            .unwrap_or("")
            .to_owned();
        let content = on.map(content);
        Some(Self {
            mode,
            manual,
            content,
            list,
        })
    }

    /// 事务的读回值。自动模式下手动表不生效：`manual`、`fp` 记空（退回到自动时才对得上）。
    pub fn value(&self, v: &Vault) -> Value {
        if self.mode == 0 {
            return json!({"mode": 0, "manual": "", "fp": ""});
        }
        json!({
            "mode": self.mode,
            "manual": self.manual,
            "fp": self.content.as_ref().map_or(String::new(), |c| v.fp(c)),
        })
    }

    pub fn content_of(&self, id: &str) -> Option<&Value> {
        self.list.iter().find(|(i, _)| i == id).map(|(_, c)| c)
    }
}

type Step = (&'static str, &'static str, Value);

/// 把设备从 `now` 写成 `target`（规范值）要调的 ubus，依次：模式 → 启用 → 改内容。
/// `new_content` = 这次新写的内容（`apn.modify` 的原始参数算的）；退回/撤销时为 None，旧内容从保管箱取。
pub fn steps(
    target: &Value,
    now: &Now,
    new_content: Option<&Value>,
    vault: &Vault,
) -> Result<Vec<Step>, String> {
    let o = "zwrt_apn_object";
    let mut out = Vec::new();
    let mode = target.get("mode").and_then(Value::as_i64);
    let set_mode = mode
        .filter(|m| *m != now.mode)
        .map(|m| (o, "set_apn_mode", json!({"apn_mode": m})));
    let manual = target.get("manual").and_then(Value::as_str);
    let enable = manual
        .filter(|id| !id.is_empty() && *id != now.manual)
        .map(|id| (o, "enable_manu_apn_id", json!({"profileId": id})));
    // 从自动切到手动：先启用、再切模式，切模式触发的重拨才用上它（agent 9-25 起的 dial_manual 顺序）
    let to_manual = mode == Some(1) && now.mode != 1;
    if to_manual {
        out.extend(enable);
        out.extend(set_mode);
    } else {
        out.extend(set_mode);
        out.extend(enable);
    }
    if let Some(fp) = target
        .get("fp")
        .and_then(Value::as_str)
        .filter(|f| !f.is_empty())
    {
        let id = manual.unwrap_or(&now.manual);
        let current = now.content_of(id).map(|c| vault.fp(c));
        if current.as_deref() != Some(fp) {
            let c = match new_content {
                Some(c) => c.clone(),
                None => vault
                    .get(fp)
                    .ok_or("the old APN settings were not kept on the device")?,
            };
            let mut args = c.as_object().cloned().unwrap_or_default();
            args.insert("profileId".into(), json!(id));
            out.push((o, "modify_manu_apn", Value::Object(args)));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
