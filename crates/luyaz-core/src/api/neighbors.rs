//! `/api/netinfo/neighbors/scan`: the LTE neighbours the modem already reports, read passively.
//!
//! The stock active scan drops mobile data and finds nothing, and the diag path was rejected
//! (docs/research/lte-neighbors-diag-probe-20261024.md). The modem keeps its own neighbour list
//! in uci `zte_nwinfo.manual_scan.lteg_nbr_content` (read as the whole section): `pci,earfcn,band,rsrp,rsrq;…`, the first
//! entry the serving cell. One in-process uci read (no fork, no write), plus netinfo to drop the
//! serving cell and the carrier-aggregation cells. Empty (SA, no LTE) = `not_reported`.
//! The same list feeds the `neighbors` block of `/api/netinfo` (refreshed with its pass).

use axum::{extract::State, response::Response};
use luyaz_api::netinfo as ni;
use serde_json::{Value, json};

use super::{Compat, reply};

/// One neighbour; a value outside its range is `null`.
#[derive(Debug, PartialEq)]
pub(crate) struct Cell {
    pub pci: Option<u32>,
    pub earfcn: Option<u32>,
    pub band: String,
    pub rsrp: Option<i32>,
    pub rsrq: Option<i32>,
}

/// The cells of `lteg_nbr_content`, minus those whose (pci, earfcn) is in `skip`.
pub(crate) fn parse(raw: &str, skip: &[(u32, u32)]) -> Vec<Cell> {
    raw.split(';')
        .filter(|e| !e.trim().is_empty())
        .filter_map(|e| {
            let f: Vec<&str> = e.split(',').map(str::trim).collect();
            if f.len() < 5 {
                return None;
            }
            let pci = f[0].parse::<u32>().ok().filter(|p| *p <= 503);
            let earfcn = f[1].parse::<u32>().ok();
            if let (Some(p), Some(a)) = (pci, earfcn)
                && skip.contains(&(p, a))
            {
                return None;
            }
            Some(Cell {
                pci,
                earfcn,
                band: f[2].to_string(),
                rsrp: f[3]
                    .parse::<i32>()
                    .ok()
                    .filter(|r| (-140..=-30).contains(r)),
                rsrq: f[4].parse::<i32>().ok().filter(|r| (-40..=0).contains(r)),
            })
        })
        .collect()
}

/// The serving cell (`lte_pci` on `wan_active_channel`), if netinfo has it.
fn serving(net: &Value) -> Option<(u32, u32)> {
    Some((num(&net["lte_pci"])?, num(&net["wan_active_channel"])?))
}

fn num(v: &Value) -> Option<u32> {
    v.as_u64()
        .or_else(|| v.as_str()?.trim().parse().ok())
        .map(|n| n as u32)
}

/// The serving cell and every `lteca` carrier (`idx,PCI,?,band,arfcn,…`).
pub(crate) fn own_cells(net: &Value) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = serving(net).into_iter().collect();
    for c in net["lteca"].as_str().unwrap_or("").split(';') {
        let f: Vec<&str> = c.split(',').map(str::trim).collect();
        if let (Some(p), Some(a)) = (
            f.get(1).and_then(|s| s.parse().ok()),
            f.get(4).and_then(|s| s.parse().ok()),
        ) {
            out.push((p, a));
        }
    }
    out
}

/// At most this many neighbours are kept (the touch UI shows 8).
pub(crate) const MAX_CELLS: usize = 16;

/// The neighbours of `raw` (`lteg_nbr_content`) given netinfo (`Null` if it could not be read),
/// strongest RSRP first (then RSRQ, unknown last), or why there are none (`Err`). In SA the
/// option can keep an older LTE list, and a list whose first entry is not the current serving
/// cell is from an earlier cell: both count as not reported.
pub(crate) fn report(net: &Value, raw: &str) -> Result<Vec<Cell>, &'static str> {
    if net["network_type"].as_str() == Some("SA") {
        return Err(ni::NBR_SA);
    }
    let mut cells = if net.is_null() {
        // without netinfo the first entry is still the serving cell
        let mut c = parse(raw, &[]);
        if !c.is_empty() {
            c.remove(0);
        }
        c
    } else {
        let first = parse(raw, &[]).into_iter().next();
        if let (Some(s), Some(f)) = (serving(net), &first)
            && (f.pci, f.earfcn) != (Some(s.0), Some(s.1))
        {
            return Err(ni::NBR_NONE);
        }
        parse(raw, &own_cells(net))
    };
    if cells.is_empty() {
        return Err(ni::NBR_NONE);
    }
    let key = |c: &Cell| (c.rsrp.unwrap_or(i32::MIN), c.rsrq.unwrap_or(i32::MIN));
    cells.sort_by_key(|c| std::cmp::Reverse(key(c)));
    cells.truncate(MAX_CELLS);
    Ok(cells)
}

/// The modem's neighbour option (empty if not set or not readable).
pub(crate) async fn read_raw(c: &Compat) -> Result<String, String> {
    // the whole section: `uci get` of an option that is not set answers nothing at all (B31)
    match (c.src.ubus)(
        "uci",
        "get",
        json!({"config": "zte_nwinfo", "section": "manual_scan"}),
    )
    .await
    {
        Ok(v) => Ok(v["values"]["lteg_nbr_content"]
            .as_str()
            .unwrap_or("")
            .to_string()),
        // no such section: nothing reported
        Err(e) if e.to_lowercase().contains("not found") => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// The `/api/netinfo` `neighbors` block (`at` = when read).
pub(crate) fn block(r: &Result<Vec<Cell>, &'static str>, at: i64) -> Value {
    match r {
        Ok(cells) => {
            let list: Vec<Value> = cells
                .iter()
                .map(|c| {
                    json!({"rat": "LTE", "pci": c.pci, "arfcn": c.earfcn, "band": c.band,
                        "rsrp": c.rsrp, "rsrq": c.rsrq, "sinr": null})
                })
                .collect();
            ni::neighbors_json("ok", "", at, &list)
        }
        Err(why) => ni::neighbors_json("not_reported", why, at, &[]),
    }
}

/// GET or POST: the reported neighbours, strongest first.
pub async fn scan(State(c): State<Compat>) -> Response {
    let raw = match read_raw(&c).await {
        Ok(r) => r,
        Err(e) => return reply(502, json!({"ok": false, "error": format!("uci get: {e}")})),
    };
    // also when empty: in SA the reason is SA
    let net = (c.src.ubus)("zte_nwinfo_api", "nwinfo_get_netinfo", json!({}))
        .await
        .unwrap_or(Value::Null);
    let (status, cells, why) = match report(&net, &raw) {
        Ok(cells) => ("ok", cells, ""),
        Err(why) => ("not_reported", Vec::new(), why),
    };
    let list: Vec<Value> = cells
        .iter()
        .map(|c| json!({"pci": c.pci, "earfcn": c.earfcn, "band": c.band, "rsrp": c.rsrp, "rsrq": c.rsrq}))
        .collect();
    reply(
        200,
        json!({"ok": true, "status": status, "reason": why, "cells": list}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const B31: &str = "155,1750,B3,-100,-15;182,1750,B3,-99,-16;7,1650,B3,-112,-19;";

    #[test]
    fn drops_serving_and_ca_cells() {
        let net = json!({"lte_pci": "155", "wan_active_channel": "1750", "lteca": "1,7,0,3,1650,20,0,-110,-12,5,-80"});
        let cells = parse(B31, &own_cells(&net));
        assert_eq!(
            cells,
            vec![Cell {
                pci: Some(182),
                earfcn: Some(1750),
                band: "B3".into(),
                rsrp: Some(-99),
                rsrq: Some(-16)
            }]
        );
    }

    #[test]
    fn out_of_range_values_are_null_and_junk_skipped() {
        let cells = parse("600,1750,B3,-20,-15;9,x,B1,-150,5;short,1;", &[]);
        assert_eq!(cells.len(), 2);
        assert_eq!((cells[0].pci, cells[0].rsrp), (None, None));
        assert_eq!(
            (cells[1].earfcn, cells[1].rsrp, cells[1].rsrq),
            (None, None, None)
        );
    }

    #[test]
    fn report_ranks_strongest_first() {
        let net = json!({"network_type": "LTE", "lte_pci": "155", "wan_active_channel": "1750"});
        let raw = "155,1750,B3,-100,-15;7,1650,B3,-112,-19;182,1750,B3,-99,-16;9,100,B1,x,-9;3,100,B1,-99,-10;";
        let pcis: Vec<_> = report(&net, raw).unwrap().iter().map(|c| c.pci).collect();
        assert_eq!(pcis, [Some(3), Some(182), Some(7), Some(9)]);
        let b = block(&report(&net, raw), 5);
        assert_eq!(b["state"], "ok");
        let c0 = &b["cells"][0];
        assert_eq!(
            (
                c0["rat"].as_str(),
                c0["arfcn"].as_u64(),
                c0["band"].as_str()
            ),
            (Some("LTE"), Some(100), Some("B1"))
        );
    }

    #[test]
    fn sa_and_stale_lists_are_not_reported() {
        let sa = json!({"network_type": "SA", "lte_pci": "155", "wan_active_channel": "1750"});
        assert_eq!(report(&sa, B31).unwrap_err(), ni::NBR_SA);
        // the serving cell moved on: the list's first entry is an old one
        let moved = json!({"network_type": "LTE", "lte_pci": "200", "wan_active_channel": "1750"});
        assert_eq!(report(&moved, B31).unwrap_err(), ni::NBR_NONE);
        assert_eq!(report(&moved, "").unwrap_err(), ni::NBR_NONE);
        let b = block(&report(&sa, B31), 5);
        assert_eq!(
            (b["state"].as_str(), b["error"].as_str()),
            (Some("not_reported"), Some(ni::NBR_SA))
        );
        assert_eq!(b["error_en"], ni::NBR_SA_EN);
    }

    #[test]
    fn without_netinfo_the_first_is_dropped() {
        let pcis: Vec<_> = report(&Value::Null, B31)
            .unwrap()
            .iter()
            .map(|c| c.pci)
            .collect();
        assert_eq!(pcis, [Some(182), Some(7)]);
    }

    #[test]
    fn empty_is_nothing() {
        assert!(parse("", &[]).is_empty());
        assert!(own_cells(&json!({})).is_empty());
    }
}
