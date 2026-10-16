//! Field sensitivity (u60-platform.md D14 / T7). Every field of a type that can reach
//! the event log, an export, the web log page or MCP is declared with `contract!`,
//! which takes a level word before each field; a field without one does not compile.
//!
//! - `normal`: as is everywhere.
//! - `personal` (who/where: SIM and device identifiers, phone numbers, client MAC/name/IP,
//!   WAN addresses, cell ids): stored as is on the device and given as is to every
//!   audience here, MCP included: MCP only answers a caller that proved the owner's
//!   secret (owner, 10-07: on a personal device the AI may see these). The level stays
//!   as the mark a view without the owner's credential would cut.
//! - `secret` (message text, keys, passwords): never in plain text outside the live
//!   state. The log stores a keyed reference (HMAC-SHA256 with a device-local key that
//!   never enters the log, first 8 bytes), so a change is visible and a dictionary
//!   cannot recover it; every view shows a mask.
//! - `nested`: a contract type (or a list/map/option of one); its own levels apply.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

/// Who a value is redacted for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum To {
    /// the event log on the device
    Log,
    /// the owner: exports, the web log page
    Owner,
    /// MCP, after the caller proved the owner's secret: personal in full, secret masked
    Mcp,
}

pub struct Ctx<'a> {
    pub to: To,
    /// key for secret references (Log only)
    pub key: &'a [u8],
}

pub const MASK: &str = "•••";

/// A contract type, or a container of them: redact field by field.
pub trait Redact: Sized {
    fn redact(&self, c: &Ctx) -> Self;
}

impl<T: Redact> Redact for Option<T> {
    fn redact(&self, c: &Ctx) -> Self {
        self.as_ref().map(|x| x.redact(c))
    }
}

impl<T: Redact> Redact for Vec<T> {
    fn redact(&self, c: &Ctx) -> Self {
        self.iter().map(|x| x.redact(c)).collect()
    }
}

impl<T: Redact> Redact for BTreeMap<String, T> {
    fn redact(&self, c: &Ctx) -> Self {
        self.iter().map(|(k, v)| (k.clone(), v.redact(c))).collect()
    }
}

/// A leaf value that can be secret.
pub trait Seal: Sized {
    fn seal(&self, c: &Ctx) -> Self;
}

impl Seal for String {
    fn seal(&self, c: &Ctx) -> Self {
        if self.is_empty() {
            return String::new();
        }
        match c.to {
            To::Log => format!("ref:{}", reference(c.key, self.as_bytes())),
            To::Owner | To::Mcp => MASK.into(),
        }
    }
}

impl<T: Seal> Seal for Option<T> {
    fn seal(&self, c: &Ctx) -> Self {
        self.as_ref().map(|x| x.seal(c))
    }
}

impl Seal for Value {
    fn seal(&self, c: &Ctx) -> Self {
        match self {
            Value::Null => Value::Null,
            Value::String(s) => Value::String(s.seal(c)),
            other => Value::String(other.to_string().seal(c)),
        }
    }
}

/// HMAC-SHA256(key, msg), first 8 bytes as hex.
pub fn reference(key: &[u8], msg: &[u8]) -> String {
    const B: usize = 64;
    let mut k = [0u8; B];
    if key.len() > B {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let pad = |x: u8| k.iter().map(|b| b ^ x).collect::<Vec<u8>>();
    let inner = Sha256::new()
        .chain_update(pad(0x36))
        .chain_update(msg)
        .finalize();
    let outer = Sha256::new()
        .chain_update(pad(0x5c))
        .chain_update(inner)
        .finalize();
    outer[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// Test values: every string-like leaf is `S:<level>:<field>`, so a scan can tell
/// which level leaked.
#[cfg(test)]
pub trait Sample: Sized {
    fn sample_tagged(tag: &str) -> Self;
    fn sample() -> Self {
        Self::sample_tagged("")
    }
}

#[cfg(test)]
mod sample_impls {
    use super::Sample;
    use serde_json::Value;
    use std::collections::BTreeMap;
    impl Sample for String {
        fn sample_tagged(tag: &str) -> Self {
            tag.to_string()
        }
    }
    impl Sample for Value {
        fn sample_tagged(tag: &str) -> Self {
            serde_json::json!({ "inner": tag, "n": 1 })
        }
    }
    impl Sample for i64 {
        fn sample_tagged(_: &str) -> Self {
            7
        }
    }
    impl Sample for u64 {
        fn sample_tagged(_: &str) -> Self {
            7
        }
    }
    impl Sample for u32 {
        fn sample_tagged(_: &str) -> Self {
            7
        }
    }
    impl Sample for bool {
        fn sample_tagged(_: &str) -> Self {
            true
        }
    }
    impl<T: Sample> Sample for Option<T> {
        fn sample_tagged(tag: &str) -> Self {
            Some(T::sample_tagged(tag))
        }
    }
    impl<T: Sample> Sample for Vec<T> {
        fn sample_tagged(tag: &str) -> Self {
            vec![T::sample_tagged(tag), T::sample_tagged(tag)]
        }
    }
    impl<T: Sample> Sample for BTreeMap<String, T> {
        fn sample_tagged(tag: &str) -> Self {
            BTreeMap::from([("k".to_string(), T::sample_tagged(tag))])
        }
    }
}

#[doc(hidden)]
pub fn personal<T: Clone>(x: &T, _c: &Ctx) -> T {
    // every audience in `To` is the owner or acts for them (see the module comment)
    x.clone()
}

/// Declares a contract struct: serde + `Redact`, one level word per field.
///
/// ```ignore
/// contract! {
///     pub struct Sim {
///         normal state: String,
///         personal imsi: String,
///         #[serde(default)]
///         secret note: String,
///     }
/// }
/// ```
macro_rules! contract {
    (@one normal, $f:expr, $c:expr) => { ::std::clone::Clone::clone($f) };
    (@one personal, $f:expr, $c:expr) => { $crate::sens::personal($f, $c) };
    (@one secret, $f:expr, $c:expr) => { $crate::sens::Seal::seal($f, $c) };
    (@one nested, $f:expr, $c:expr) => { $crate::sens::Redact::redact($f, $c) };
    (
        $(#[$sm:meta])*
        pub struct $name:ident {
            $( $(#[$fm:meta])* $level:ident $field:ident : $ty:ty ),* $(,)?
        }
    ) => {
        $(#[$sm])*
        #[derive(::serde::Serialize, ::serde::Deserialize, Clone, Debug, PartialEq, Default)]
        #[cfg_attr(test, derive(::schemars::JsonSchema))]
        pub struct $name {
            $(
                $(#[$fm])*
                #[cfg_attr(test, schemars(extend("x-u60-level" = stringify!($level))))]
                pub $field: $ty,
            )*
        }
        #[cfg(test)]
        impl $crate::sens::Sample for $name {
            fn sample_tagged(_: &str) -> Self {
                $name { $( $field: $crate::sens::Sample::sample_tagged(
                    concat!("S:", stringify!($level), ":", stringify!($field))) ),* }
            }
        }
        impl $crate::sens::Redact for $name {
            fn redact(&self, c: &$crate::sens::Ctx) -> Self {
                let _ = c; // all-normal types do not look at it
                let $name { $( $field ),* } = self;
                $name { $( $field: contract!(@one $level, $field, c) ),* }
            }
        }
    };
}
pub(crate) use contract;

#[cfg(test)]
mod tests {
    use super::*;

    contract! {
        pub struct Item {
            normal id: i64,
            personal num: String,
            secret text: String,
        }
    }

    contract! {
        pub struct Holder {
            normal n: u32,
            nested list: Vec<Item>,
        }
    }

    #[test]
    fn levels() {
        let h = Holder {
            n: 1,
            list: vec![Item {
                id: 7,
                num: "10086".into(),
                text: "code 123456".into(),
            }],
        };
        let key = b"k";
        let log = h.redact(&Ctx { to: To::Log, key });
        assert_eq!(log.list[0].num, "10086");
        assert!(log.list[0].text.starts_with("ref:"));
        assert_eq!(log.list[0].text.len(), 4 + 16);
        // same text, same key: same reference (a change is visible, the text is not)
        assert_eq!(log, h.redact(&Ctx { to: To::Log, key }));
        let other = h.redact(&Ctx {
            to: To::Log,
            key: b"other",
        });
        assert_ne!(log.list[0].text, other.list[0].text);
        let mcp = h.redact(&Ctx { to: To::Mcp, key });
        assert_eq!(mcp.list[0].num, "10086");
        assert_eq!(mcp.list[0].text, MASK);
        assert_eq!(mcp.list[0].id, 7);
        let owner = h.redact(&Ctx { to: To::Owner, key });
        assert_eq!(owner.list[0].num, "10086");
        assert_eq!(owner.list[0].text, MASK);
    }

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        // key "Jefe", data "what do ya want for nothing?"
        assert_eq!(
            reference(b"Jefe", b"what do ya want for nothing?"),
            "5bdcc146bf60754e"
        );
    }
}
