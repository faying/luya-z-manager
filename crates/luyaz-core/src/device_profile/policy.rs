//! Fixed owner policies, independent of firmware suffixes or reported support.
use luyaz_api::firmware::{Availability, AvailabilityState};

pub(crate) fn availability(id: &str, radio: Option<&str>) -> Option<Availability> {
    let (state, reason) = match id {
        "modem.recover_from_lpm" => (AvailabilityState::Blocked, "lpm_recovery_forbidden"),
        // An expired/absent observation is not evidence that the radio is offline.
        id if id.starts_with("at.") && radio.is_none() => {
            (AvailabilityState::Unknown, "radio_not_observed")
        }
        id if id.starts_with("at.") && radio != Some("ONLINE") => {
            (AvailabilityState::Blocked, "radio_not_online")
        }
        _ => return None,
    };
    Some(Availability {
        state,
        reason: reason.into(),
    })
}

/// A CFUN setter (including a chained command); queries/tests remain reads.
pub(crate) fn radio_set(cmd: &str) -> bool {
    let s: String = cmd
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    s.match_indices("CFUN=")
        .any(|(i, _)| !s[i + 5..].starts_with('?'))
}

/// Names of reviewed decoders, selected only after this exact capability has live proof.
/// Parsing both log shapes remains content-driven; a firmware suffix never picks a dialect.
pub(crate) fn verified_dialect(id: &str) -> Option<&'static str> {
    match id {
        "qos.eps.log.read" => Some("qos.eps.log.v1"),
        "qos.nr_pdu.log.read" => Some("qos.nr_pdu.log.v1"),
        "at.transport.read" => Some("at.standard.v1"),
        "at.pdp_contexts.read" => Some("at.cgcontrdp.v1"),
        "at.eps_qos.read" => Some("at.cgeqosrdp.v1"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovery_policy_is_fixed_and_at_reads_require_online() {
        for mode in [None, Some("ONLINE"), Some("OFFLINE"), Some("LOW_POWER")] {
            assert_eq!(
                availability("modem.recover_from_lpm", mode).unwrap().state,
                AvailabilityState::Blocked
            );
            assert_eq!(
                availability("at.transport.read", mode).is_none(),
                mode == Some("ONLINE")
            );
        }
        for s in ["AT+CFUN=1", "at + cfun = 0", "ATE0;+CFUN=1,1"] {
            assert!(radio_set(s));
        }
        for s in ["AT+CFUN?", "AT+CFUN=?", "AT+CSQ"] {
            assert!(!radio_set(s));
        }
    }
    #[tokio::test]
    async fn all_at_recovery_paths_stop_before_a_port_or_lock_is_opened() {
        let t = crate::at::Target {
            lock: Some("/does-not-exist/lpm-lock".into()),
            port: Some("/does-not-exist/lpm-port".into()),
        };
        let raw = crate::at::raw(t, "AT+CFUN=1", std::time::Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(raw.contains("lpm_recovery_forbidden"), "{raw}");
        let fixed = crate::at::send_with(
            crate::at::Cmd::CfunOnline,
            None,
            Some("/does-not-exist/lpm-port".into()),
        )
        .await
        .unwrap_err();
        assert!(fixed.contains("lpm_recovery_forbidden"), "{fixed}");
    }
}
