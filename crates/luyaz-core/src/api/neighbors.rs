//! `/api/netinfo/neighbors/scan`: the LTE neighbours the modem already reports, read passively.
//!
//! The stock active scan drops mobile data and finds nothing, and the diag path was rejected
//! (docs/research/lte-neighbors-diag-probe-20261024.md). The modem keeps its own neighbour list
//! in uci `zte_nwinfo.manual_scan.lteg_nbr_content` (read as the whole section): `pci,earfcn,band,rsrp,rsrq;…`, the first
//! entry the serving cell. One in-process uci read (no fork, no write), plus netinfo to drop the
//! serving cell and the carrier-aggregation cells. Empty (SA, no LTE) = `not_reported`.

use axum::{extract::State, response::Response};
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

/// The serving cell (`lte_pci` on `wan_active_channel`) and every `lteca` carrier
/// (`idx,PCI,?,band,arfcn,…`).
pub(crate) fn own_cells(net: &Value) -> Vec<(u32, u32)> {
    let num = |v: &Value| {
        v.as_u64()
            .or_else(|| v.as_str()?.trim().parse().ok())
            .map(|n| n as u32)
    };
    let mut out = Vec::new();
    if let (Some(p), Some(a)) = (num(&net["lte_pci"]), num(&net["wan_active_channel"])) {
        out.push((p, a));
    }
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

/// GET or POST: the reported neighbours.
pub async fn scan(State(c): State<Compat>) -> Response {
    // the whole section: `uci get` of an option that is not set answers nothing at all (B31)
    let read = (c.src.ubus)(
        "uci",
        "get",
        json!({"config": "zte_nwinfo", "section": "manual_scan"}),
    )
    .await;
    let raw = match read {
        Ok(v) => v["values"]["lteg_nbr_content"]
            .as_str()
            .unwrap_or("")
            .to_string(),
        // no such section: nothing reported
        Err(e) if e.to_lowercase().contains("not found") => String::new(),
        Err(e) => return reply(502, json!({"ok": false, "error": format!("uci get: {e}")})),
    };
    let skip = if raw.trim().is_empty() {
        Vec::new()
    } else {
        match (c.src.ubus)("zte_nwinfo_api", "nwinfo_get_netinfo", json!({})).await {
            Ok(net) => own_cells(&net),
            // without netinfo the first entry is still the serving cell
            Err(_) => Vec::new(),
        }
    };
    let mut cells = parse(&raw, &skip);
    if skip.is_empty() && !cells.is_empty() {
        cells.remove(0);
    }
    let list: Vec<Value> = cells
        .iter()
        .map(|c| json!({"pci": c.pci, "earfcn": c.earfcn, "band": c.band, "rsrp": c.rsrp, "rsrq": c.rsrq}))
        .collect();
    let status = if raw.trim().is_empty() {
        "not_reported"
    } else {
        "ok"
    };
    reply(200, json!({"ok": true, "status": status, "cells": list}))
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
    fn empty_is_nothing() {
        assert!(parse("", &[]).is_empty());
        assert!(own_cells(&json!({})).is_empty());
    }
}
