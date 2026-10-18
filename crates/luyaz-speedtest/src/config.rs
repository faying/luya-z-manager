//! Locally managed LibreSpeed endpoints. No remote server-list dependency.
use crate::{Provider, TestServer};
use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub librespeed_servers: Vec<LibreServer>,
}

/// The same endpoint names as LibreSpeed's custom CLI server list.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LibreServer {
    pub id: u64,
    pub name: String,
    pub server: String,
    #[serde(rename = "dlURL")]
    pub download: String,
    #[serde(rename = "ulURL")]
    pub upload: String,
    #[serde(rename = "pingURL")]
    pub ping: String,
}

pub fn http_url(s: &str) -> Result<Url, String> {
    let u = Url::parse(s).map_err(|_| "Invalid endpoint URL".to_string())?;
    if !matches!(u.scheme(), "http" | "https")
        || u.host_str().is_none()
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || s.len() > 512
    {
        return Err("Endpoints must be HTTP(S) URLs without credentials or fragments".into());
    }
    Ok(u)
}

impl Config {
    pub(crate) fn servers(&self) -> Result<Vec<TestServer>, String> {
        if self.librespeed_servers.len() > 8 {
            return Err("At most 8 LibreSpeed servers are supported".into());
        }
        let mut ids = std::collections::HashSet::new();
        self.librespeed_servers
            .iter()
            .map(|s| {
                if s.id == 0
                    || s.id > i64::MAX as u64
                    || !ids.insert(s.id)
                    || s.name.trim().is_empty()
                    || s.name.len() > 64
                {
                    return Err("Servers need unique positive IDs and names of 1–64 bytes".into());
                }
                let base = http_url(&s.server)?;
                if !base.path().ends_with('/') || base.query().is_some() {
                    return Err("The server base URL must end in / and have no query".into());
                }
                let endpoint = |v: &str| -> Result<String, String> {
                    let u = base
                        .join(v)
                        .map_err(|_| "Invalid endpoint path".to_string())?;
                    http_url(u.as_str())?;
                    if u.origin() != base.origin() {
                        return Err("Endpoint paths must stay on the server origin".into());
                    }
                    Ok(u.to_string())
                };
                Ok(TestServer {
                    id: s.id,
                    name: s.name.clone(),
                    sponsor: "LibreSpeed".into(),
                    country: String::new(),
                    provider: Provider::Librespeed,
                    host: base.host_str().unwrap_or_default().into(),
                    url: endpoint(&s.upload)?,
                    base_url: base.to_string(),
                    ping_url: endpoint(&s.ping)?,
                    download_url: endpoint(&s.download)?,
                })
            })
            .collect()
    }
}
