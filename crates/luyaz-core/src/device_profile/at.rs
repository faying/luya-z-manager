//! Reviewed AT read dialects. Exact command/response grammar, no firmware suffix selection,
//! fallback writes, command retries or generic ERROR => unsupported inference.
use serde_json::{Value, json};

pub(crate) struct Binding {
    pub capability: &'static str,
    pub dialect: &'static str,
    pub target: &'static str,
}

pub(crate) fn qos_command(text: &str) -> Option<crate::at::Cmd> {
    if text == "AT+CGCONTRDP" {
        return Some(crate::at::Cmd::Cgcontrdp);
    }
    let cid = text.strip_prefix("AT+CGEQOSRDP=")?.parse::<u8>().ok()?;
    ((1..=24).contains(&cid) && text == format!("AT+CGEQOSRDP={cid}"))
        .then_some(crate::at::Cmd::Cgeqosrdp(cid))
}

pub(crate) fn binding(text: &str) -> Option<Binding> {
    let (capability, dialect, target) = match text {
        "AT" => ("at.transport.read", "at.read.explicit.v1", "AT"),
        "AT+CGCONTRDP" => ("at.pdp_contexts.read", "at.cgcontrdp.v1", "AT+CGCONTRDP"),
        _ if matches!(qos_command(text), Some(crate::at::Cmd::Cgeqosrdp(_))) => {
            ("at.eps_qos.read", "at.cgeqosrdp.v1", "AT+CGEQOSRDP")
        }
        _ => return None,
    };
    Some(Binding {
        capability,
        dialect,
        target,
    })
}

/// A bounded CSV response record. Quoted commas are allowed, unmatched quotes/control
/// bytes and data after a closing quote are not. The raw fields never enter the report.
fn csv(s: &str) -> Option<Vec<String>> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut quoted = false;
    let mut closed = false;
    for c in s.chars() {
        if c.is_control() {
            return None;
        }
        match c {
            '"' if !closed && (quoted || field.trim().is_empty()) => {
                if quoted {
                    closed = true;
                }
                quoted = !quoted;
            }
            ',' if !quoted => {
                fields.push(field.trim().to_owned());
                field.clear();
                closed = false;
            }
            _ if closed && !c.is_whitespace() => return None,
            _ => field.push(c),
        }
    }
    if quoted || fields.len() >= 32 {
        return None;
    }
    fields.push(field.trim().to_owned());
    Some(fields)
}

pub(crate) fn validate(command: &str, response: &str) -> Option<Value> {
    binding(command)?;
    if response.len() > 65536 {
        return None;
    }
    let lines: Vec<_> = response
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.last() != Some(&"OK") || lines.iter().filter(|l| **l == "OK").count() != 1 {
        return None;
    }
    let mut records = Vec::new();
    for line in &lines[..lines.len() - 1] {
        if *line == command {
            continue;
        }
        if command == "AT" {
            return None;
        }
        let prefix = if command == "AT+CGCONTRDP" {
            "+CGCONTRDP:"
        } else {
            "+CGEQOSRDP:"
        };
        let f = csv(line.strip_prefix(prefix)?.trim())?;
        let cid = f.first()?.parse::<u8>().ok()?;
        if !(1..=24).contains(&cid) {
            return None;
        }
        if command == "AT+CGCONTRDP" {
            if f.len() < 3 || f[1].parse::<u8>().is_err() || f[2].is_empty() {
                return None;
            }
            records.push(json!({"cid":cid}));
        } else {
            let expected = command.strip_prefix("AT+CGEQOSRDP=")?.parse::<u8>().ok()?;
            if cid != expected || !matches!(f.len(), 2 | 6) || !records.is_empty() {
                return None;
            }
            let nums: Vec<u32> = f.iter().map(|s| s.parse()).collect::<Result<_, _>>().ok()?;
            if !(1..=255).contains(&nums[1]) {
                return None;
            }
            records.push(json!({"cid":cid,"qci":nums[1],"rates_kbps":&nums[2..]}));
        }
    }
    if command != "AT" && records.is_empty() {
        return None;
    }
    Some(json!({"record_count":records.len(),"records":records}))
}

// Display parsing preserves the previous compatible projection. Live evidence additionally
// passes the strict complete-response/CID validator above; partial display is never proof.
/// `+CGEQOSRDP: <cid>,<QCI>[,<DL_GBR>,<UL_GBR>,<DL_MBR>,<UL_MBR>]` (the first such line).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Qos {
    pub qci: Option<u32>,
    pub dl_gbr: Option<u32>,
    pub ul_gbr: Option<u32>,
    pub dl_mbr: Option<u32>,
    pub ul_mbr: Option<u32>,
}

pub(crate) fn parse_cgeqosrdp(resp: &str) -> Qos {
    let Some(rest) = resp
        .lines()
        .find_map(|l| l.trim().strip_prefix("+CGEQOSRDP:"))
    else {
        return Qos::default();
    };
    let p: Vec<&str> = rest.split(',').map(str::trim).collect();
    let n = |i: usize| p[i].parse().ok();
    let mut q = Qos::default();
    if p.len() >= 2 {
        q.qci = n(1);
    }
    if p.len() >= 6 {
        (q.dl_gbr, q.ul_gbr, q.dl_mbr, q.ul_mbr) = (n(2), n(3), n(4), n(5));
    }
    q
}

/// Every `+CGCONTRDP:` record → (cid, bearer_id, apn), in the modem's order.
pub(crate) fn parse_cgcontrdp(resp: &str) -> Vec<(u32, u32, String)> {
    resp.lines()
        .filter_map(|l| l.trim().strip_prefix("+CGCONTRDP:"))
        .filter_map(|rest| parse_cgcontrdp_head(rest.trim()))
        .collect()
}

/// `<cid>,<bearer_id>,"<apn>",...`: the APN is the first quoted token, else the next field.
fn parse_cgcontrdp_head(s: &str) -> Option<(u32, u32, String)> {
    let mut it = s.splitn(3, ',');
    let cid = it.next()?.trim().parse().ok()?;
    let bearer_id = it.next()?.trim().parse().ok()?;
    let tail = it.next()?.trim_start();
    let apn = match tail.strip_prefix('"') {
        Some(rest) => rest.split('"').next().unwrap_or(""),
        None => tail.split(',').next().unwrap_or("").trim(),
    };
    Some((cid, bearer_id, apn.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn dialects_are_exact_and_redact_payloads() {
        let v = validate(
            "AT+CGCONTRDP",
            "AT+CGCONTRDP\r\n+CGCONTRDP: 1,5,\"private.apn\",\"10.0.0.1\"\r\nOK\r\n",
        )
        .unwrap();
        let out = v.to_string();
        assert!(!out.contains("private") && !out.contains("10.0"));
        for answer in [
            "+CGEQOSRDP: 1,9\r\nOK\r\n",
            "+CGEQOSRDP: 1,9,0,0,100,200\r\nOK\r\n",
        ] {
            assert!(validate("AT+CGEQOSRDP=1", answer).is_some());
        }
        for command in [
            "AT+CGEQOSRDP=0",
            "AT+CGEQOSRDP=25",
            "AT+CGEQOSRDP=01",
            "AT+CGEQOSRDP=1;AT",
            "AT+CGCONTRDP=?",
        ] {
            assert!(binding(command).is_none());
        }
    }
    #[test]
    fn incomplete_wrong_context_and_errors_cannot_prove_support() {
        for r in [
            "OK",
            "ERROR\r\n",
            "+CME ERROR: 4\r\n",
            "+CGEQOSRDP: 2,9\r\nOK\r\n",
            "+CGEQOSRDP: 1,9\r\n",
            "+CGEQOSRDP: 1,9,x\r\nOK\r\n",
            "+CGEQOSRDP: 1,9\r\nERROR\r\nOK\r\n",
            "+CGEQOSRDP: 1,9\r\nOK\r\nnoise",
        ] {
            assert!(validate("AT+CGEQOSRDP=1", r).is_none(), "{r:?}");
        }
        assert!(validate("AT+CGCONTRDP", "+CGCONTRDP: 1,5,\"unclosed\r\nOK\r\n").is_none());
        assert!(validate("AT", "AT\r\nOK\r\n").is_some());
    }
}
