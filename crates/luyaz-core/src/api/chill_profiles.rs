//! CHILL configuration profiles.
//!
//! Profile sources and runtime copies live below `/data/chill/profiles`.  The
//! HTTP representation deliberately contains only source metadata.  URLs and
//! the Mihomo secret stay in device files and never leave this module through
//! an API response.

use axum::{body::Bytes, extract::State, response::Response};
use luyaz_api::chill as ch;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serde_yaml::{Mapping, Value as Yaml};
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    time::Duration,
};

use super::{Compat, chill_jobs as jobs, reply};

pub const PROFILES_ROOT: &str = "/data/chill/profiles";
pub const ACTIVE_PROFILE: &str = "/data/chill/active-profile";
/// JSON can escape each source byte as a six-byte `\uXXXX` sequence.  The
/// handler enforces the actual source limit separately after decoding.
pub const IMPORT_BODY_LIMIT: usize = 26 * 1024 * 1024;
const MAX_YAML_BYTES: usize = 4 * 1024 * 1024;
const ID_MAX: usize = 32;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ProfileMeta {
    pub version: u32,
    pub id: String,
    pub name: String,
    pub kind: ProfileKind,
    pub url: Option<String>,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProfileKind {
    FullUrl,
    NodeUrl,
    LocalYaml,
}

impl ProfileKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::FullUrl => "full_url",
            Self::NodeUrl => "node_url",
            Self::LocalYaml => "local_yaml",
        }
    }
}

#[derive(Debug)]
struct ProfileError {
    code: u16,
    message: String,
}

impl ProfileError {
    fn bad(message: impl Into<String>) -> Self {
        Self { code: 400, message: message.into() }
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self { code: 409, message: message.into() }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self { code: 500, message: message.into() }
    }

    fn response(self) -> Response {
        reply(self.code, json!({"ok": false, "error": self.message}))
    }
}

#[derive(Clone, Debug)]
struct ProfilePaths {
    dir: PathBuf,
    meta: PathBuf,
    source: PathBuf,
    source_next: PathBuf,
    runtime: PathBuf,
    runtime_next: PathBuf,
    providers: PathBuf,
    ruleset: PathBuf,
}

fn paths(c: &Compat, id: &str) -> ProfilePaths {
    paths_at(c.path(PROFILES_ROOT).join(id))
}

fn paths_at(dir: PathBuf) -> ProfilePaths {
    ProfilePaths {
        meta: dir.join("profile.json"),
        source: dir.join("source.yaml"),
        source_next: dir.join("source.next.yaml"),
        runtime: dir.join("run/config.yaml"),
        runtime_next: dir.join("run/config.next.yaml"),
        providers: dir.join("providers"),
        ruleset: dir.join("ruleset"),
        dir,
    }
}

pub(crate) fn valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > ID_MAX {
        return false;
    }
    let bytes = id.as_bytes();
    (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes.iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_' || *b == b'-')
}

fn validate_id(id: &str) -> Result<(), ProfileError> {
    valid_id(id).then_some(()).ok_or_else(|| ProfileError::bad(
        "id must match [a-z0-9][a-z0-9_-]{0,31}",
    ))
}

fn now() -> u64 {
    super::home::now_unix()
}

fn write_atomic(path: &Path, data: &[u8]) -> Result<(), ProfileError> {
    let tmp = path.with_extension(format!("tmp-{:016x}", rand::random::<u64>()));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write = || -> io::Result<()> {
        let mut file = options.open(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&tmp, path)?;
        sync_parent(path)
    };
    write().map_err(|_| {
        let _ = fs::remove_file(&tmp);
        ProfileError::internal("cannot save CHILL profile state")
    })
}

fn sync_parent(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn private_dir(path: &Path) -> io::Result<()> {
    let mut builder = DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => sync_parent(path)?,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists && fs::symlink_metadata(path)?.is_dir() => {}
        Err(err) => return Err(err),
    }
    Ok(())
}

fn ensure_dirs(p: &ProfilePaths) -> Result<(), ProfileError> {
    let profiles = p.dir.parent().expect("profile store parent");
    let create = || -> io::Result<()> {
        fs::create_dir_all(profiles.parent().expect("CHILL data directory"))?;
        private_dir(profiles)?;
        private_dir(&p.dir)?;
        private_dir(&p.providers)?;
        private_dir(&p.ruleset)?;
        private_dir(p.runtime.parent().expect("profile runtime directory"))
    };
    create().map_err(|_| ProfileError::internal("cannot create profile directories"))
}

fn read_meta(c: &Compat, id: &str) -> Result<ProfileMeta, ProfileError> {
    validate_id(id)?;
    let p = paths(c, id);
    if !fs::symlink_metadata(&p.dir).is_ok_and(|meta| meta.is_dir())
        || !fs::symlink_metadata(&p.meta).is_ok_and(|meta| meta.is_file())
    {
        return Err(ProfileError::bad("profile not found"));
    }
    let bytes = std::fs::read(&p.meta)
        .map_err(|_| ProfileError::internal("cannot read profile metadata"))?;
    let meta: ProfileMeta = serde_json::from_slice(&bytes)
        .map_err(|_| ProfileError::internal("invalid profile metadata"))?;
    if meta.version != 1 || meta.id != id {
        return Err(ProfileError::internal("invalid profile metadata"));
    }
    Ok(meta)
}

fn active_id(c: &Compat) -> Option<String> {
    std::fs::read_to_string(c.path(ACTIVE_PROFILE))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|id| valid_id(id))
}

fn source_host(url: Option<&str>) -> Option<String> {
    reqwest::Url::parse(url?).ok()?.host_str().map(str::to_owned)
}

fn profile_view(c: &Compat, meta: &ProfileMeta, active: Option<&str>) -> Value {
    let p = paths(c, &meta.id);
    let (provider_count, group_count) = runtime_counts(&p.runtime);
    let last_error = std::fs::read_to_string(p.dir.join("last-error"))
        .ok()
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty());
    json!({
        "id": meta.id,
        "name": meta.name,
        "kind": meta.kind.as_str(),
        "active": active == Some(meta.id.as_str()),
        "updated_at": meta.updated_at,
        "source_host": source_host(meta.url.as_deref()),
        "has_url": meta.url.is_some(),
        "provider_count": provider_count,
        "group_count": group_count,
        "last_error": last_error,
    })
}

pub(crate) fn summary(c: &Compat) -> Value {
    let active = active_id(c);
    let mut profiles = Vec::new();
    if let Ok(entries) = std::fs::read_dir(c.path(PROFILES_ROOT)) {
        for entry in entries.flatten() {
            let id = entry.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) || !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if let Ok(meta) = read_meta(c, &id) {
                profiles.push(profile_view(c, &meta, active.as_deref()));
            }
        }
    }
    profiles.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    json!({"profiles": profiles, "active_id": active})
}

fn runtime_counts(path: &Path) -> (usize, usize) {
    let Ok(text) = std::fs::read_to_string(path) else { return (0, 0) };
    let Ok(root) = serde_yaml::from_str::<Yaml>(&text) else { return (0, 0) };
    let count = |key: &str| {
        root.get(key)
            .and_then(Yaml::as_mapping)
            .map_or(0, Mapping::len)
    };
    (count("proxy-providers") + count("rule-providers"),
     root.get("proxy-groups").and_then(Yaml::as_sequence).map_or(0, Vec::len))
}

fn yaml_key(key: &str) -> Yaml {
    Yaml::String(key.to_owned())
}

fn string_value(v: &Yaml) -> Option<&str> {
    match v {
        Yaml::String(s) => Some(s),
        _ => None,
    }
}

fn map_mut<'a>(root: &'a mut Yaml, key: &str) -> &'a mut Mapping {
    if !matches!(root, Yaml::Mapping(_)) {
        *root = Yaml::Mapping(Mapping::new());
    }
    let map = root.as_mapping_mut().expect("mapping created above");
    let entry = map
        .entry(yaml_key(key))
        .or_insert_with(|| Yaml::Mapping(Mapping::new()));
    if !matches!(entry, Yaml::Mapping(_)) {
        *entry = Yaml::Mapping(Mapping::new());
    }
    entry.as_mapping_mut().expect("mapping created above")
}

fn safe_relative(path: &str) -> Result<&Path, ProfileError> {
    let p = Path::new(path);
    if p.is_absolute() || path.contains(['\\', ':', '\0']) || p.file_name().is_none() {
        return Err(ProfileError::bad("provider/ruleset path must be relative"));
    }
    if p.components().any(|c| matches!(c, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
        return Err(ProfileError::bad("provider/ruleset path may not contain '..' or a root"));
    }
    Ok(p)
}

fn strip_egress(v: &mut Yaml) {
    match v {
        Yaml::Mapping(m) => {
            m.remove(&yaml_key("interface-name"));
            m.remove(&yaml_key("routing-mark"));
            for value in m.values_mut() {
                strip_egress(value);
            }
        }
        Yaml::Sequence(s) => {
            for value in s {
                strip_egress(value);
            }
        }
        Yaml::Tagged(tagged) => strip_egress(&mut tagged.value),
        _ => {}
    }
}

fn rewrite_paths(root: &mut Yaml, key: &str, directory: &str) -> Result<(), ProfileError> {
    let Some(entries) = root.get_mut(key) else {
        return Ok(());
    };
    let map = entries.as_mapping_mut().ok_or_else(|| ProfileError::bad("providers must be a mapping"))?;
    let mut used = std::collections::HashSet::with_capacity(map.len());
    for (name, value) in map.iter_mut() {
        if key == "proxy-providers" {
            strip_egress(value);
        }
        let item = value.as_mapping_mut().ok_or_else(|| ProfileError::bad("provider entry must be a mapping"))?;
        let path = match item.get("path") {
            Some(value) => {
                let path = value.as_str().ok_or_else(|| ProfileError::bad("provider path must be a string"))?;
                let relative = safe_relative(path)?;
                // Keep subdirectories to avoid collisions between equal basenames.
                let relative = relative.strip_prefix(directory).unwrap_or(relative);
                if relative.file_name().is_none() {
                    return Err(ProfileError::bad("provider path must name a file"));
                }
                let relative: PathBuf = relative.components().filter_map(|p| match p {
                    Component::Normal(segment) => Some(segment),
                    _ => None,
                }).collect();
                Some(format!("./{directory}/{}", relative.display().to_string().replace('\\', "/")))
            }
            None if item.get("type").and_then(Yaml::as_str) == Some("http") => {
                use sha2::{Digest, Sha256};
                let name = name.as_str().ok_or_else(|| ProfileError::bad("provider name must be a string"))?;
                let hash = Sha256::digest(name.as_bytes());
                Some(format!("./{directory}/{:x}.cache", hash))
            }
            _ => None,
        };
        if let Some(path) = path {
            if !used.insert(path.clone()) {
                return Err(ProfileError::bad("providers cannot share a cache path"));
            }
            item.insert(yaml_key("path"), Yaml::String(path));
        }
        if key == "proxy-providers" {
            item.insert(yaml_key("interface-name"), Yaml::String(String::new()));
            item.insert(yaml_key("routing-mark"), Yaml::Number(0.into()));
            let overrides = item.entry(yaml_key("override"))
                .or_insert_with(|| Yaml::Mapping(Mapping::new()));
            let overrides = overrides.as_mapping_mut()
                .ok_or_else(|| ProfileError::bad("provider override must be a mapping"))?;
            overrides.insert(yaml_key("interface-name"), Yaml::String(String::new()));
            overrides.insert(yaml_key("routing-mark"), Yaml::Number(0.into()));
        }
    }
    Ok(())
}

fn listener(name: &str, port: u16, proxy: Option<&str>) -> Yaml {
    let mut m = Mapping::new();
    m.insert(yaml_key("name"), Yaml::String(name.to_owned()));
    m.insert(yaml_key("type"), Yaml::String("mixed".into()));
    m.insert(yaml_key("port"), Yaml::Number(port.into()));
    m.insert(yaml_key("listen"), Yaml::String("127.0.0.1".into()));
    m.insert(yaml_key("udp"), Yaml::Bool(false));
    if let Some(proxy) = proxy {
        m.insert(yaml_key("proxy"), Yaml::String(proxy.to_owned()));
    }
    Yaml::Mapping(m)
}

fn has_listener(root: &Yaml, port: u16) -> bool {
    let matches = |value: Option<&Yaml>| {
        value.and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok())) == Some(u64::from(port))
    };
    ["port", "mixed-port", "socks-port", "redir-port", "tproxy-port"]
        .iter()
        .any(|&key| matches(root.get(key)))
        || root.get("listeners")
        .and_then(Yaml::as_sequence)
        .into_iter()
        .flatten()
        .any(|v| matches(v.get("port")))
}

fn has_group(root: &Yaml, name: &str) -> bool {
    root.get("proxy-groups")
        .and_then(Yaml::as_sequence)
        .into_iter()
        .flatten()
        .any(|v| v.get("name").and_then(string_value) == Some(name))
}

fn overlay(mut root: Yaml, secret: &str, lan_controller: bool) -> Result<Yaml, ProfileError> {
    if !matches!(root, Yaml::Mapping(_)) {
        return Err(ProfileError::bad("YAML root must be a mapping"));
    }
    let map = root.as_mapping_mut().expect("mapping checked above");
    map.insert(yaml_key("allow-lan"), Yaml::Bool(false));
    map.insert(
        yaml_key("external-controller"),
        Yaml::String(if lan_controller { "0.0.0.0:9999" } else { "127.0.0.1:9999" }.into()),
    );
    map.insert(yaml_key("external-ui"), Yaml::String("ui".into()));
    map.insert(yaml_key("secret"), Yaml::String(secret.to_owned()));
    for key in [
        "external-controller-tls",
        "external-controller-unix",
        "external-controller-pipe",
        "external-ui-url",
    ] {
        map.remove(&yaml_key(key));
    }

    let ipv6_disabled = root.get("ipv6").and_then(Yaml::as_bool) == Some(false);
    let tun = map_mut(&mut root, "tun");
    for (k, v) in [
        ("enable", Yaml::Bool(true)),
        ("stack", Yaml::String("system".into())),
        ("device", Yaml::String("chill0".into())),
        ("auto-route", Yaml::Bool(true)),
        ("auto-redirect", Yaml::Bool(false)),
        ("auto-detect-interface", Yaml::Bool(true)),
        ("strict-route", Yaml::Bool(false)),
        ("iproute2-table-index", Yaml::Number(2022.into())),
        ("iproute2-rule-index", Yaml::Number(9000.into())),
    ] {
        tun.insert(yaml_key(k), v);
    }
    tun.insert(
        yaml_key("include-interface"),
        Yaml::Sequence(vec![Yaml::String("br-lan".into())]),
    );
    tun.insert(
        yaml_key("dns-hijack"),
        Yaml::Sequence(vec![Yaml::String("any:53".into())]),
    );
    if ipv6_disabled {
        tun.remove(&yaml_key("inet6-address"));
    }

    let dns = map_mut(&mut root, "dns");
    dns.insert(yaml_key("enable"), Yaml::Bool(true));
    dns.insert(yaml_key("listen"), Yaml::String("127.0.0.1:53".into()));

    rewrite_paths(&mut root, "proxy-providers", "providers")?;
    rewrite_paths(&mut root, "rule-providers", "ruleset")?;

    let need_7894 = !has_listener(&root, 7894);
    let main_group = has_group(&root, ch::MAIN_GROUP).then_some(ch::MAIN_GROUP);
    let need_7895 = has_group(&root, ch::AI_GROUP) && !has_listener(&root, 7895);
    if need_7894 || need_7895 {
        let root_map = root.as_mapping_mut().expect("root mapping");
        let listeners = root_map
            .entry(yaml_key("listeners"))
            .or_insert_with(|| Yaml::Sequence(Vec::new()));
        if !matches!(listeners, Yaml::Sequence(_)) {
            *listeners = Yaml::Sequence(Vec::new());
        }
        let listeners = listeners.as_sequence_mut().expect("sequence created above");
        if need_7894 {
            listeners.push(listener("luyaz-netinfo", 7894, main_group));
        }
        if need_7895 {
            listeners.push(listener("luyaz-netinfo-ai", 7895, Some(ch::AI_GROUP)));
        }
    }
    Ok(root)
}

fn normalize_yaml(text: &[u8], secret: &str, lan_controller: bool) -> Result<Vec<u8>, ProfileError> {
    if text.len() > MAX_YAML_BYTES {
        return Err(ProfileError::bad("YAML exceeds 4 MiB"));
    }
    let mut root: Yaml = serde_yaml::from_slice(text)
        .map_err(|_| ProfileError::bad("invalid YAML"))?;
    root.apply_merge().map_err(|_| ProfileError::bad("invalid YAML merge"))?;
    let root = overlay(root, secret, lan_controller)?;
    serde_yaml::to_string(&root)
        .map(|s| s.into_bytes())
        .map_err(|e| ProfileError::internal(format!("serialize runtime YAML: {e}")))
}

fn secret(c: &Compat) -> String {
    let env = std::fs::read_to_string(c.path(ch::CHILL_ENV)).unwrap_or_default();
    ch::env_var(&env, "CHILL_SECRET")
}

fn lan_controller(c: &Compat) -> bool {
    std::fs::read_to_string(c.path(ch::CHILL_ENV))
        .ok()
        .is_some_and(|s| ch::env_var(&s, "CHILL_API_LAN") == "1")
}

fn write_profile(c: &Compat, meta: &ProfileMeta, files: Option<(&[u8], &[u8])>) -> Result<(), ProfileError> {
    let dest = c.path(PROFILES_ROOT).join(&meta.id);
    let p = paths_at(dest.with_file_name(format!(".{}.import", meta.id)));
    let meta_bytes = serde_json::to_vec_pretty(meta)
        .map_err(|_| ProfileError::internal("cannot encode profile metadata"))?;
    let write = || {
        ensure_dirs(&p)?;
        if let Some((source, runtime)) = files {
            write_atomic(&p.source, source)?;
            write_atomic(&p.runtime, runtime)?;
        }
        write_atomic(&p.meta, &meta_bytes)?;
        fs::rename(&p.dir, &dest).and_then(|()| sync_parent(&dest))
            .map_err(|_| ProfileError::internal("cannot publish profile"))
    };
    let result = write();
    if result.is_err() {
        let _ = fs::remove_dir_all(&p.dir);
    }
    result
}

fn new_id(c: &Compat) -> String {
    loop {
        let id = format!("p-{:016x}", rand::random::<u64>());
        if !c.path(PROFILES_ROOT).join(&id).exists()
            && !c.path(PROFILES_ROOT).join(format!(".{id}.import")).exists()
        {
            return id;
        }
    }
}

fn parsed(body: &[u8]) -> Result<Value, ProfileError> {
    serde_json::from_slice(body).map_err(|_| ProfileError::bad("invalid JSON"))
}

fn name(value: &Value) -> Result<String, ProfileError> {
    let name = value.get("name").and_then(Value::as_str).unwrap_or("").trim();
    if name.is_empty() || name.len() > 128 || name.chars().any(|c| c.is_control()) {
        return Err(ProfileError::bad("name must be 1–128 printable characters"));
    }
    Ok(name.to_owned())
}

fn kind(value: &Value) -> Result<ProfileKind, ProfileError> {
    match value.get("kind").and_then(Value::as_str).unwrap_or("") {
        "full_url" => Ok(ProfileKind::FullUrl),
        "node_url" => Ok(ProfileKind::NodeUrl),
        "local_yaml" => Ok(ProfileKind::LocalYaml),
        _ => Err(ProfileError::bad("kind must be full_url, node_url or local_yaml")),
    }
}

fn url(value: &Value) -> Result<String, ProfileError> {
    let url = value.get("url").and_then(Value::as_str).map(str::trim).unwrap_or("");
    if !ch::valid_url(url) || source_host(Some(url)).is_none() {
        return Err(ProfileError::bad("url must use http:// or https://"));
    }
    Ok(url.to_owned())
}

async fn download(url: &str) -> Result<Vec<u8>, ProfileError> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|e| ProfileError::internal(format!("create download client: {e}")))?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| ProfileError::bad("profile download failed"))?;
    if !response.status().is_success() {
        return Err(ProfileError::bad(format!("download failed with HTTP {}", response.status())));
    }
    if response.content_length().is_some_and(|n| n > MAX_YAML_BYTES as u64) {
        return Err(ProfileError::bad("YAML exceeds 4 MiB"));
    }
    let mut bytes = Vec::with_capacity(response.content_length().unwrap_or(0) as usize);
    while let Some(chunk) = response.chunk().await.map_err(|_| ProfileError::bad("profile download failed"))? {
        if chunk.len() > MAX_YAML_BYTES - bytes.len() {
            return Err(ProfileError::bad("YAML exceeds 4 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn delete_check(active: Option<&str>, profile_count: usize, id: &str) -> Result<(), ProfileError> {
    if active == Some(id) {
        return Err(ProfileError::conflict("cannot delete the active profile"));
    }
    if profile_count <= 1 {
        return Err(ProfileError::conflict("cannot delete the last profile"));
    }
    Ok(())
}

fn set_error(c: &Compat, id: &str, error: Option<&str>) {
    let path = paths(c, id).dir.join("last-error");
    match error {
        Some(error) => { let _ = write_atomic(&path, error.as_bytes()); }
        None => { let _ = std::fs::remove_file(path); }
    }
}

/// GET /api/services/chill/configs
pub async fn list(State(c): State<Compat>) -> Response {
    let data = crate::state::blocking(move || summary(&c)).await;
    reply(200, json!({"ok": true, "data": data}))
}

/// POST /api/services/chill/configs/import
pub async fn import(State(c): State<Compat>, body: Bytes) -> Response {
    let mut value = match parsed(&body) { Ok(v) => v, Err(e) => return e.response() };
    drop(body);
    let name = match name(&value) { Ok(v) => v, Err(e) => return e.response() };
    let kind = match kind(&value) { Ok(v) => v, Err(e) => return e.response() };
    let Some(slot) = jobs::reserve(&c) else { return jobs::busy() };
    let secret = secret(&c);
    let lan = lan_controller(&c);
    let (url, files) = match kind {
        ProfileKind::FullUrl => {
            let url = match url(&value) { Ok(v) => v, Err(e) => return e.response() };
            let source = match download(&url).await { Ok(v) => v, Err(e) => return e.response() };
            let runtime = match normalize_yaml(&source, &secret, lan) { Ok(v) => v, Err(e) => return e.response() };
            (Some(url), Some((source, runtime)))
        }
        ProfileKind::NodeUrl => {
            let url = match url(&value) { Ok(v) => v, Err(e) => return e.response() };
            (Some(url), None)
        }
        ProfileKind::LocalYaml => {
            let Value::String(yaml) = value["yaml"].take() else {
                return ProfileError::bad("yaml is required").response();
            };
            let yaml = yaml.into_bytes();
            let runtime = match normalize_yaml(&yaml, &secret, lan) { Ok(v) => v, Err(e) => return e.response() };
            (None, Some((yaml, runtime)))
        }
    };
    let id = new_id(&c);
    let timestamp = now();
    let meta = ProfileMeta { version: 1, id: id.clone(), name, kind, url, created_at: timestamp, updated_at: timestamp };
    let files = files.as_ref().map(|(source, runtime)| (source.as_slice(), runtime.as_slice()));
    if let Err(e) = write_profile(&c, &meta, files) {
        let _ = std::fs::remove_dir_all(paths(&c, &id).dir);
        return e.response();
    }
    let active = active_id(&c);
    let profile = profile_view(&c, &meta, active.as_deref());
    let work = c.clone();
    let job = jobs::spawn(&c, slot, "profile-import", async move {
        let command = if active.is_none() { "profile-switch" } else { "profile-validate" };
        let result = jobs::profile_command(&work, command, &id).await;
        if result.is_err() && active_id(&work).as_deref() != Some(id.as_str()) {
            let _ = std::fs::remove_dir_all(paths(&work, &id).dir);
        }
        result.map(|()| "Profile imported".into())
    });
    reply(200, json!({"ok": true, "data": {"job_id": job, "status": "running", "profile": profile}}))
}

async fn stage_refresh(c: &Compat, meta: &ProfileMeta) -> Result<(), ProfileError> {
    let secret = secret(c);
    let lan = lan_controller(c);
    let p = paths(c, &meta.id);
    let source = match meta.kind {
        ProfileKind::FullUrl => {
            let url = meta.url.as_deref().ok_or_else(|| ProfileError::internal("full URL profile has no URL"))?;
            download(url).await?
        }
        ProfileKind::NodeUrl => return Ok(()),
        ProfileKind::LocalYaml => {
            std::fs::read(&p.source).map_err(|_| ProfileError::internal("cannot read source YAML"))?
        }
    };
    let runtime = normalize_yaml(&source, &secret, lan)?;
    write_atomic(&p.source_next, &source)?;
    write_atomic(&p.runtime_next, &runtime)
}

/// POST /api/services/chill/configs/refresh
pub async fn refresh(State(c): State<Compat>, body: Bytes) -> Response {
    let value = match parsed(&body) { Ok(v) => v, Err(e) => return e.response() };
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
    let Some(slot) = jobs::reserve(&c) else { return jobs::busy() };
    if let Err(e) = validate_id(id) { return e.response(); }
    let meta = match read_meta(&c, id) { Ok(v) => v, Err(e) => return e.response() };
    if let Err(e) = stage_refresh(&c, &meta).await {
        set_error(&c, id, Some(&e.message));
        return e.response();
    }
    let profile = profile_view(&c, &meta, active_id(&c).as_deref());
    let work = c.clone();
    let job = jobs::spawn(&c, slot, "profile-refresh", async move {
        let mut meta = meta;
        let result = jobs::profile_command(&work, "profile-refresh", &meta.id).await;
        set_error(&work, &meta.id, result.as_ref().err().map(String::as_str));
        result?;
        meta.updated_at = now();
        let bytes = serde_json::to_vec_pretty(&meta).map_err(|_| "cannot encode profile metadata")?;
        write_atomic(&paths(&work, &meta.id).meta, &bytes).map_err(|err| err.message)?;
        Ok("Profile refreshed".into())
    });
    reply(200, json!({"ok": true, "data": {"job_id": job, "status": "running", "profile": profile}}))
}

/// POST /api/services/chill/configs/activate
pub async fn activate(State(c): State<Compat>, body: Bytes) -> Response {
    let value = match parsed(&body) { Ok(v) => v, Err(e) => return e.response() };
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
    let Some(slot) = jobs::reserve(&c) else { return jobs::busy() };
    if let Err(e) = validate_id(id) { return e.response(); }
    if let Err(e) = read_meta(&c, id) { return e.response(); }
    let target = id.to_owned();
    let work = c.clone();
    let job = jobs::spawn(&c, slot, "profile-activate", async move {
        let result = jobs::profile_command(&work, "profile-switch", &target).await;
        set_error(&work, &target, result.as_ref().err().map(String::as_str));
        result.map(|()| "Profile activated".into())
    });
    reply(200, json!({"ok": true, "data": {"job_id": job, "status": "running", "active_id": active_id(&c)}}))
}

/// POST /api/services/chill/configs/delete
pub async fn delete(State(c): State<Compat>, body: Bytes) -> Response {
    let value = match parsed(&body) { Ok(v) => v, Err(e) => return e.response() };
    let id = value.get("id").and_then(Value::as_str).unwrap_or("");
    let Some(slot) = jobs::reserve(&c) else { return jobs::busy() };
    if let Err(e) = validate_id(id) { return e.response(); }
    if let Err(e) = read_meta(&c, id) { return e.response(); }
    let count = summary(&c)["profiles"].as_array().map_or(0, Vec::len);
    if let Err(e) = delete_check(active_id(&c).as_deref(), count, id) {
        return e.response();
    }
    let path = paths(&c, id).dir;
    let job = jobs::spawn(&c, slot, "profile-delete", async move {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(|_| "cannot delete profile".to_owned())?;
        Ok("Profile deleted".into())
    });
    reply(200, json!({"ok": true, "data": {"job_id": job, "status": "running", "deleted_id": id}}))
}

/// PUT /api/services/chill/mode `{"mode":"rule"|"global"|"direct"}`
pub async fn mode(State(c): State<Compat>, body: Bytes) -> Response {
    let value = match parsed(&body) { Ok(v) => v, Err(e) => return e.response() };
    let mode = match ch::mode_wanted(&value) {
        Ok(v) => v.to_owned(),
        Err((code, body)) => return reply(code, body),
    };
    let Some(_slot) = jobs::reserve(&c) else { return jobs::busy() };
    let env = std::fs::read_to_string(c.path(ch::CHILL_ENV)).unwrap_or_default();
    let secret = ch::env_var(&env, "CHILL_SECRET");
    let client = match reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build() {
        Ok(v) => v,
        Err(e) => return ProfileError::internal(format!("create Mihomo client: {e}")).response(),
    };
    let file = c.path(ch::CHILL_MODE_FILE);
    if let Err(error) = crate::ops::chill::write_mode(&client, &c.src.mihomo, &secret, &file, &mode).await {
        return reply(502, json!({"ok": false, "error": error}));
    }
    reply(200, json!({"ok": true, "data": {"mode": mode}}))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yaml_overlay_preserves_policy_and_adds_gateway_values() {
        let input = br#"mode: rule
dns:
  nameserver-policy:
    '+.example.com': [1.1.1.1]
proxy-providers:
  source:
    type: file
    path: ./cache/source.yaml
    override: {interface-name: old, routing-mark: 123, udp: true}
proxy-groups:
  - name: custom
    type: select
    proxies: [DIRECT]
"#;
        let out = normalize_yaml(input, "secret", false).unwrap();
        let v: Yaml = serde_yaml::from_slice(&out).unwrap();
        assert_eq!(v["allow-lan"], Yaml::Bool(false));
        assert_eq!(v["external-controller"].as_str(), Some("127.0.0.1:9999"));
        assert_eq!(v["dns"]["nameserver-policy"]["+.example.com"][0].as_str(), Some("1.1.1.1"));
        assert_eq!(v["proxy-providers"]["source"]["path"].as_str(), Some("./providers/cache/source.yaml"));
        let overrides = &v["proxy-providers"]["source"]["override"];
        assert_eq!(overrides["interface-name"].as_str(), Some(""));
        assert_eq!(overrides["routing-mark"].as_u64(), Some(0));
        assert_eq!(overrides["udp"].as_bool(), Some(true));
        assert!(v["listeners"].as_sequence().unwrap().iter().any(|x| x["port"].as_u64() == Some(7894)));
        assert!(!v["listeners"].as_sequence().unwrap().iter().any(|x| x["port"].as_u64() == Some(7895)));
    }

    #[test]
    fn configured_ports_take_precedence_over_gateway_listeners() {
        let input = b"mixed-port: 7894\nlisteners: [{name: custom, type: mixed, port: 7895}]\n";
        let encoded = normalize_yaml(input, "", false).unwrap();
        let value: Yaml = serde_yaml::from_slice(&encoded).unwrap();
        let listeners = value["listeners"].as_sequence().unwrap();
        assert_eq!(listeners.len(), 1);
        assert_eq!(listeners[0]["name"].as_str(), Some("custom"));
    }

    #[test]
    fn provider_paths_cannot_escape_profile() {
        let err = normalize_yaml(b"proxy-providers:\n  x:\n    path: ../outside.yaml\n", "", false).unwrap_err();
        assert!(err.message.contains("may not contain"));
        let err = normalize_yaml(b"rule-providers:\n  x:\n    path: /tmp/outside\n", "", false).unwrap_err();
        assert!(err.message.contains("relative"));
    }

    #[test]
    fn profile_summary_masks_url() {
        let meta = ProfileMeta { version: 1, id: "p-test".into(), name: "Test".into(), kind: ProfileKind::FullUrl, url: Some("https://secret.example/path?token=redacted".into()), created_at: 1, updated_at: 2 };
        let value = profile_view(&Compat::new("/tmp/no-profile", super::super::Sources::none()), &meta, Some("p-test"));
        assert!(value.get("url").is_none());
        assert_eq!(value["source_host"], "secret.example");
        assert_eq!(value["has_url"], true);
    }

    #[test]
    fn active_and_last_profiles_cannot_be_deleted() {
        assert_eq!(delete_check(Some("p-one"), 2, "p-one").unwrap_err().code, 409);
        assert_eq!(delete_check(None, 1, "p-one").unwrap_err().code, 409);
        assert!(delete_check(Some("p-one"), 2, "p-two").is_ok());
    }

    #[tokio::test]
    async fn import_enforces_decoded_source_size_without_raising_other_limits() {
        let c = Compat::new("/unused", super::super::Sources::none()).with_writes(true);
        let addr = super::super::tests::start(c).await;
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let body = json!({"name": "Large", "kind": "local_yaml", "yaml": "x".repeat(MAX_YAML_BYTES + 1)}).to_string();
        let root = format!("http://{addr}/api/services/chill");
        let response = client.post(format!("{root}/configs/import"))
            .body(body.clone()).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 400);
        let error: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        assert_eq!(error["error"], "YAML exceeds 4 MiB");
        let response = client.post(format!("{root}/configs/delete"))
            .body(body).send().await.unwrap();
        assert_eq!(response.status().as_u16(), 413);
    }
}
