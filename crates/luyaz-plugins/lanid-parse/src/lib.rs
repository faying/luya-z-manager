//! The lanid-parse component (manager docs/designs/lan-client-id.md §4): the core queues whole
//! frames; every byte of them is parsed here (`parse.rs`), so a bad packet can at worst trap this
//! instance, never the core. Only type conversion in this file.

pub mod parse;

#[cfg(test)]
#[path = "fixtures.rs"]
pub mod fixtures;

#[cfg(target_arch = "wasm32")]
mod component {
    wit_bindgen::generate!({ path: "../../luyaz-policy/wit/lanid-parse.wit", world: "lanid-parse" });

    use crate::parse::{self as p, Kind};

    struct Parser;

    impl Guest for Parser {
        fn parse(frames: Vec<Vec<u8>>) -> Vec<Vec<Fact>> {
            frames
                .iter()
                .map(|f| {
                    p::parse(f)
                        .into_iter()
                        .map(|x| Fact {
                            kind: match x.kind {
                                Kind::DhcpHostname => FactKind::DhcpHostname,
                                Kind::DhcpVendorClass => FactKind::DhcpVendorClass,
                                Kind::DhcpParams => FactKind::DhcpParams,
                                Kind::MdnsInstance => FactKind::MdnsInstance,
                                Kind::MdnsModel => FactKind::MdnsModel,
                                Kind::MdnsService => FactKind::MdnsService,
                                Kind::SsdpServer => FactKind::SsdpServer,
                                Kind::NetbiosName => FactKind::NetbiosName,
                            },
                            value: x.value,
                        })
                        .collect()
                })
                .collect()
        }
    }

    export!(Parser);
}
