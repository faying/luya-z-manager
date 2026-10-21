//! The two follow-ups of lan-client-id.md §2, on every projection of the client list (main core,
//! component loaded):
//!
//! - **A new MAC takes over an alias.** Phones use one random MAC per network, but reset it now
//!   and then. A private MAC with no alias of its own whose facts match exactly one *offline*
//!   MAC that has an alias gets that alias (copied into aliases.json, so it survives the old
//!   record's eviction) and the old MAC in `Record::from`, shown as "new address, alias kept".
//!   Match = the same readable mDNS instance name, or the same specific DHCP hostname *and* the
//!   same option 55 list. A hostname alone (`iPhone`, `android`) never matches; a candidate that
//!   is online is another device; two candidates is a guess, so none. Clearing the alias undoes
//!   it.
//! - **A stranger joins.** `known` (in clients.json) holds every MAC ever seen in the list. A MAC
//!   not in it waits [`SETTLE`] (its lease shows up before its DHCP facts reach the component, and
//!   a rotated MAC must get the chance to match first), then becomes known and raises one alert
//!   ([`KIND`], once per MAC through `add_once`; never an SMS, luyaz-alerts `NEVER`). The first
//!   round after this shipped (no `known` yet) takes everything present, recorded or aliased as
//!   known, silently.
//!
//! Both are display and a notice: a MAC can be forged.

use super::*;

/// How long a MAC must be in the list before it counts as a stranger.
pub const SETTLE: Duration = Duration::from_secs(90);
/// The alert queue's kind.
pub const KIND: &str = "lan-new-device";
/// At most this many known MACs (the oldest go first).
pub const MAX_KNOWN: usize = 1024;
/// At most this many stranger alerts per round (a crowd joining at once still gets a few).
const MAX_ALERTS: usize = 4;

/// DHCP hostnames too common to tell devices apart (compared without digits, `-`, `_`, case).
const GENERIC: &[&str] = &[
    "iphone",
    "ipad",
    "ipod",
    "android",
    "macbook",
    "macbookair",
    "macbookpro",
    "imac",
    "mac",
    "galaxy",
    "phone",
    "pc",
    "laptop",
    "desktop",
    "localhost",
    "unknown",
    "windows",
    "linux",
    "esp",
    "espressif",
];

fn specific_host(v: &str) -> bool {
    let core: String = v
        .chars()
        .filter(|c| c.is_alphabetic())
        .flat_map(char::to_lowercase)
        .collect();
    classify::readable_name(v) && core.chars().count() >= 3 && !GENERIC.contains(&core.as_str())
}

/// The same device, by what it said about itself (module doc).
pub fn same_device(a: &Record, b: &Record) -> bool {
    fn f(r: &Record, k: Kind) -> Option<&str> {
        r.facts.get(&k).map(String::as_str)
    }
    if let (Some(x), Some(y)) = (f(a, Kind::MdnsInstance), f(b, Kind::MdnsInstance))
        && classify::readable_name(x)
        && x == y
    {
        return true;
    }
    matches!(
        (f(a, Kind::DhcpHostname), f(b, Kind::DhcpHostname)),
        (Some(x), Some(y)) if specific_host(x) && x == y
    ) && matches!(
        (f(a, Kind::DhcpParams), f(b, Kind::DhcpParams)),
        (Some(x), Some(y)) if !x.is_empty() && x == y
    )
}

/// The alert id for a MAC (luyaz-alerts `valid_event_id`: 8 hex, `-`, ≤12 digits).
pub fn event_id(m: &Mac) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{}",
        m[0],
        m[1],
        m[2],
        m[3],
        u16::from_be_bytes([m[4], m[5]])
    )
}

impl Shared {
    /// One round over the MACs in the client list: takeovers, then strangers (returned, at most
    /// [`MAX_ALERTS`]; the caller raises them outside the lock).
    pub fn watch(&mut self, present: &[String], now: Instant, wall: u64) -> Vec<String> {
        if !(self.started && self.persist && self.loaded) {
            return Vec::new();
        }
        if self.known.is_none() {
            let seed = present
                .iter()
                .chain(self.records.keys())
                .chain(self.aliases.keys())
                .map(|m| (m.clone(), wall))
                .collect();
            self.known = Some(seed);
            self.dirty = true;
            self.maybe_flush(now, true);
            return Vec::new();
        }
        for m in present {
            self.take_over(m, present, wall);
        }
        self.pending.retain(|m, _| present.contains(m));
        let mut strangers = Vec::new();
        for m in present {
            let known = self.known.get_or_insert_default();
            if known.contains_key(m) {
                self.pending.remove(m);
                continue;
            }
            let since = *self.pending.entry(m.clone()).or_insert(now);
            if now.duration_since(since) < SETTLE {
                continue;
            }
            self.pending.remove(m);
            known.insert(m.clone(), wall);
            self.dirty = true;
            if strangers.len() < MAX_ALERTS {
                strangers.push(m.clone());
            }
        }
        if let Some(known) = self.known.as_mut() {
            while known.len() > MAX_KNOWN {
                let Some(k) = known
                    .iter()
                    .min_by_key(|(_, t)| **t)
                    .map(|(k, _)| k.clone())
                else {
                    break;
                };
                known.remove(&k);
            }
        }
        if !strangers.is_empty() {
            self.maybe_flush(now, true);
        }
        strangers
    }

    /// `m` takes over the alias of the one offline MAC that is the same device, if there is one.
    fn take_over(&mut self, m: &str, present: &[String], wall: u64) {
        if self.aliases.contains_key(m) || !parse_mac(m).is_some_and(|x| is_private(&x)) {
            return;
        }
        let Some(rec) = self.records.get(m) else {
            return;
        };
        let mut hits = self.records.iter().filter(|(k, r)| {
            k.as_str() != m
                && !present.contains(k)
                && self.aliases.contains_key(k.as_str())
                && same_device(rec, r)
        });
        let (Some((old, _)), None) = (hits.next(), hits.next()) else {
            return;
        };
        let (old, alias) = (old.clone(), self.aliases[old.as_str()].clone());
        let mut next = self.aliases.clone();
        next.insert(m.to_owned(), alias);
        if let Err(e) = self.write_aliases(next) {
            eprintln!("lanid: alias takeover {old} -> {m}: {e}");
            return;
        }
        eprintln!("lanid: {m} took over the alias of {old}");
        if let Some(r) = self.records.get_mut(m) {
            r.from = old;
        }
        if let Some(k) = self.known.as_mut() {
            k.entry(m.to_owned()).or_insert(wall);
        }
        self.pending.remove(m);
        self.dirty = true;
        self.maybe_flush(Instant::now(), true);
    }
}

/// The alert text: the name (as the alert queue keeps it: printable ASCII) and the MAC.
pub fn alert_text(mac: &str, name: &str) -> String {
    let name: String = name
        .chars()
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(40)
        .collect();
    let name = name.trim();
    if name.is_empty() || name.eq_ignore_ascii_case(mac) {
        mac.to_owned()
    } else {
        format!("{name} {mac}")
    }
}

/// Raises one [`KIND`] alert per stranger (named as the projection just named it).
pub fn alert(strangers: &[String], list: &[c::Client]) {
    let dir = crate::state::host_path(luyaz_api::alerts::DIR);
    let a = luyaz_api::alerts::Alerts::at(Path::new(&dir));
    let up = std::fs::read_to_string(crate::state::host_path("/proc/uptime"))
        .ok()
        .and_then(|s| s.split('.').next()?.parse::<u64>().ok())
        .unwrap_or(0);
    let wall = luyaz_api::alerts::now();
    for m in strangers {
        let Some(mac) = parse_mac(m) else { continue };
        let name = list
            .iter()
            .find(|cl| parse_mac(&cl.mac) == Some(mac))
            .map(|cl| cl.name.as_str())
            .unwrap_or_default();
        let text = alert_text(m, name);
        eprintln!("lanid: {KIND}: {text}");
        if let Err(e) = a.add_once(&event_id(&mac), KIND, &text, wall, up) {
            eprintln!("lanid: alert: {e}");
        }
    }
}
