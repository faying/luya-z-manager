//! Vendor and type from what lanid learned (lan-client-id.md §4a 合并优先级): small tables over
//! the component's validated strings, no packet bytes. Vendor: mDNS `_device-info` model (via
//! [`MODELS`]) > SSDP `SERVER` > OUI. Type: model > mDNS service types > DHCP options 60/55.

use super::{Kind, Mac, Record};
use std::collections::HashMap;
use std::path::Path;

/// mDNS `model=` prefix → (vendor, type). Longest prefixes first where they overlap.
pub const MODELS: &[(&str, &str, &str)] = &[
    ("MacBook", "Apple", "laptop"),
    ("iMac", "Apple", "computer"),
    ("Macmini", "Apple", "computer"),
    ("MacPro", "Apple", "computer"),
    ("MacStudio", "Apple", "computer"),
    ("Mac", "Apple", "computer"),
    ("iPhone", "Apple", "phone"),
    ("iPad", "Apple", "tablet"),
    ("AppleTV", "Apple", "tv"),
    ("AudioAccessory", "Apple", "speaker"),
    ("Watch", "Apple", "watch"),
];

/// mDNS service type → type.
pub const SERVICES: &[(&str, &str)] = &[
    // iOS/iPadOS only: Xcode/Finder pairing and device sync (Macs do not announce these)
    ("_apple-mobdev2._tcp", "phone"),
    ("_remotepairing._tcp", "phone"),
    ("_googlecast._tcp", "tv"),
    ("_androidtvremote2._tcp", "tv"),
    ("_spotify-connect._tcp", "speaker"),
    ("_sonos._tcp", "speaker"),
    ("_raop._tcp", "speaker"),
    ("_ipp._tcp", "printer"),
    ("_ipps._tcp", "printer"),
    ("_printer._tcp", "printer"),
    ("_pdl-datastream._tcp", "printer"),
    ("_hap._tcp", "iot"),
    ("_matter._tcp", "iot"),
    ("_ssh._tcp", "computer"),
    ("_sftp-ssh._tcp", "computer"),
    ("_smb._tcp", "computer"),
    ("_rfb._tcp", "computer"),
    ("_workstation._tcp", "computer"),
];

/// Types a computer can also play (a Mac is an AirPlay receiver, a PC a cast target): a
/// computer service beats them, a speaker never runs ssh/smb/screen sharing.
const RECEIVER_KINDS: &[&str] = &["speaker", "tv"];

/// DHCP option 60 prefix → type.
pub const VENDOR_CLASSES: &[(&str, &str)] = &[("android-dhcp", "phone"), ("MSFT", "computer")];

/// SSDP `SERVER` product names → vendor (first token that is not an OS or UPnP itself).
pub const SSDP_VENDORS: &[(&str, &str)] = &[
    ("microsoft-windows", "Microsoft"),
    ("sonos", "Sonos"),
    ("roku", "Roku"),
    ("samsung", "Samsung"),
];

fn ssdp_vendor(server: &str) -> Option<String> {
    const SKIP: &[&str] = &[
        "linux", "android", "windows", "darwin", "ios", "unix", "posix", "upnp", "dlnadoc",
    ];
    let tok = server.split_whitespace().find_map(|t| {
        let product = t
            .split('/')
            .next()
            .unwrap_or("")
            .trim_matches(|c: char| !c.is_alphanumeric());
        let lower = product.to_ascii_lowercase();
        (!product.is_empty() && !SKIP.iter().any(|s| lower.starts_with(s))).then_some(product)
    })?;
    let lower = tok.to_ascii_lowercase();
    Some(
        SSDP_VENDORS
            .iter()
            .find(|(k, _)| lower.starts_with(k))
            .map_or_else(|| tok.to_owned(), |(_, v)| (*v).to_owned()),
    )
}

/// A name from the mDNS model when nothing better is known: `MacBookPro18,4` → `MacBook Pro`.
pub fn model_name(rec: Option<&Record>) -> Option<String> {
    let m = rec?.facts.get(&Kind::MdnsModel)?;
    const NAMES: &[(&str, &str)] = &[
        ("MacBookPro", "MacBook Pro"),
        ("MacBookAir", "MacBook Air"),
        ("MacBook", "MacBook"),
        ("Macmini", "Mac mini"),
        ("MacStudio", "Mac Studio"),
        ("MacPro", "Mac Pro"),
        ("iMac", "iMac"),
    ];
    NAMES
        .iter()
        .find(|(p, _)| m.starts_with(p))
        .map(|(_, n)| (*n).to_owned())
}

/// `Xiaomi Communications Co Ltd` → `Xiaomi`: IEEE registrant names carry legal suffixes no one
/// reads on a 320 px screen. The first word, unless that leaves too little.
pub fn short_vendor(v: &str) -> String {
    const TAIL: &[&str] = &[
        "communications", "communication", "technologies", "technology", "electronics",
        "mobile", "telecommunications", "co", "co.", "co.,", "corp", "corp.", "corporation",
        "inc", "inc.", "ltd", "ltd.", "limited", "llc", "gmbh", "international", "device",
        "devices", "computer", "&",
    ];
    let words: Vec<&str> = v.split_whitespace().collect();
    let mut keep = words.len();
    while keep > 1
        && TAIL.contains(&words[keep - 1].trim_end_matches(',').to_ascii_lowercase().as_str())
    {
        keep -= 1;
    }
    words[..keep].join(" ").trim_end_matches([',', '.']).to_owned()
}

fn model(rec: Option<&Record>) -> Option<(&'static str, &'static str)> {
    let m = rec?.facts.get(&Kind::MdnsModel)?;
    MODELS
        .iter()
        .find(|(p, _, _)| m.starts_with(p))
        .map(|(_, v, k)| (*v, *k))
}

/// mDNS service types only Apple devices announce.
const APPLE_SERVICES: &[&str] = &[
    "_apple-mobdev2._tcp",
    "_remotepairing._tcp",
    "_companion-link._tcp",
    "_airplay._tcp",
];

/// Whether a learned name is meant for people: iOS announces some services under a UUID
/// (`12FE7FED-D51B-…`) and many IoT stacks under a bare hex id or MAC, which are not names.
pub fn readable_name(v: &str) -> bool {
    let core: String = v
        .chars()
        .filter(|c| !matches!(c, '-' | ':' | '_' | '.'))
        .collect();
    // ids like `5215BBA9` (Android adb/cast): 8+ hex digits with a digit in them
    let hexish = core.len() >= 8
        && core.chars().all(|c| c.is_ascii_hexdigit())
        && core.chars().any(|c| c.is_ascii_digit());
    !v.trim().is_empty() && !hexish && !has_uuid(v) && !mac_at(v)
}

/// `02:00:5e:00:53:01@fe80::…` (iOS `_apple-mobdev2`): a random MAC and a link-local
/// address, not a name.
fn mac_at(v: &str) -> bool {
    v.split_once('@').is_some_and(|(m, _)| {
        let hex: String = m.chars().filter(|c| *c != ':' && *c != '-').collect();
        hex.len() == 12 && hex.chars().all(|c| c.is_ascii_hexdigit())
    })
}

/// A UUID anywhere in the name (`nearby-presence-nsd-bb9fbae7-9710-…`, Android Quick
/// Share): an id with a prefix, not a name.
fn has_uuid(v: &str) -> bool {
    let parts: Vec<&str> = v.split('-').collect();
    parts.windows(5).any(|w| {
        w.iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(p, n)| p.len() == n && p.chars().all(|c| c.is_ascii_hexdigit()))
    })
}

/// Every vendor candidate with its source, the winner first.
pub fn vendors(rec: Option<&Record>, mac: &Mac, oui: Option<&Oui>) -> Vec<(&'static str, String)> {
    let mut out = Vec::new();
    if let Some((v, _)) = model(rec) {
        out.push(("mdns", v.to_owned()));
    }
    if out.is_empty()
        && rec.is_some_and(|r| {
            r.services
                .iter()
                .any(|s| APPLE_SERVICES.contains(&s.as_str()))
        })
    {
        out.push(("mdns", "Apple".to_owned()));
    }
    if let Some(v) = rec
        .and_then(|r| r.facts.get(&Kind::SsdpServer))
        .and_then(|s| ssdp_vendor(s))
    {
        out.push(("ssdp", v));
    }
    if let Some(v) = oui.and_then(|o| o.get(mac)) {
        out.push(("oui", short_vendor(v)));
    }
    out
}

/// Every type candidate with its source, the winner first.
pub fn kinds(rec: Option<&Record>) -> Vec<(&'static str, &'static str)> {
    let mut out = Vec::new();
    if let Some((_, k)) = model(rec) {
        out.push(("mdns", k));
    }
    if let Some(r) = rec {
        for s in &r.services {
            if let Some((_, k)) = SERVICES.iter().find(|(n, _)| n == s) {
                out.push(("mdns", *k));
            }
        }
        // the model already decided first place; among services, computer before receiver
        let from = usize::from(model(rec).is_some());
        if out.len() > from {
            out[from..].sort_by_key(|(_, k)| RECEIVER_KINDS.contains(k));
        }
        if let Some(vc) = r.facts.get(&Kind::DhcpVendorClass)
            && let Some((_, k)) = VENDOR_CLASSES.iter().find(|(p, _)| vc.starts_with(p))
        {
            out.push(("dhcp", *k));
        }
    }
    out
}

/// The OUI table, `<DIR>/oui.tsv` (`scripts/lanid/make-oui.sh` builds it from an IEEE oui.txt
/// the user supplies; the public build has none and vendors from it stay empty): one
/// `aabbcc<TAB>Vendor` per line, other lines ignored.
pub struct Oui(HashMap<[u8; 3], String>);

impl Oui {
    pub fn load(path: &Path) -> Option<Oui> {
        // lossy: a vendor cut inside a UTF-8 sequence must not lose the whole table
        let bytes = std::fs::read(path).ok()?;
        Some(Oui::parse(&String::from_utf8_lossy(&bytes)))
    }

    pub fn parse(text: &str) -> Oui {
        let mut m = HashMap::new();
        for l in text.lines() {
            let Some((hex, name)) = l.split_once('\t') else {
                continue;
            };
            let name = name.trim();
            let Some(mac) = super::parse_mac(&format!("{}000000", hex.trim())) else {
                continue;
            };
            if hex.trim().len() == 6 && super::valid_value(name) {
                m.insert([mac[0], mac[1], mac[2]], name.to_owned());
            }
        }
        Oui(m)
    }

    pub fn prefixes(&self) -> usize {
        self.0.len()
    }

    pub fn get(&self, mac: &Mac) -> Option<&str> {
        self.0.get(&[mac[0], mac[1], mac[2]]).map(String::as_str)
    }
}
