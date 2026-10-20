//! Context invalidation owner. These notifications revoke knowledge; none can mint an
//! authoritative epoch. Vendor listen has no subscription ACK, sequence or loss contract.
use luyaz_api::{clock::BootTime, firmware::Context};

#[derive(Clone, Copy)]
pub(crate) enum Loss {
    Ubus,
    Usb,
}

pub(crate) fn invalidate(context: &mut Context, loss: Loss) {
    match loss {
        Loss::Ubus => *context = Context::default(),
        Loss::Usb => context.usb_epoch = None,
    }
}

/// Read observations may revoke an epoch, never establish one. Preserve a verified epoch
/// only if an independent producer has supplied it and the observed mode did not change.
pub(crate) fn radio(context: &mut Context, value: Option<&str>) -> bool {
    let mode = value
        .map(str::trim)
        .filter(|m| matches!(*m, "ONLINE" | "OFFLINE" | "LOW_POWER"));
    let changed = context.radio_mode.as_deref() != mode;
    if changed || mode.is_none() {
        context.radio_epoch = None;
        context.sim_epoch = None;
        context.connection_epoch = None;
    }
    context.radio_mode = mode.map(str::to_owned);
    changed
}

pub(crate) fn fresh(at: Option<&BootTime>, now: Option<&BootTime>) -> bool {
    within(at, now, 5)
}

pub(crate) fn within(at: Option<&BootTime>, now: Option<&BootTime>, seconds: u64) -> bool {
    at.zip(now).is_some_and(|(at, now)| {
        !at.boot_id.is_empty()
            && at.boot_id == now.boot_id
            && now
                .seconds
                .checked_sub(at.seconds)
                .is_some_and(|age| age <= seconds)
    })
}

/// Existing producer audit. Values remain absent until a reviewed, boot-bound source
/// covers subscription establishment, reconnect, loss and all relevant lifecycle changes.
/// This diagnostic is never an authority ticket and has no configuration/import path.
pub(crate) fn coverage() -> std::collections::BTreeMap<String, luyaz_api::firmware::ContextCoverage>
{
    [
        (
            "ubus_epoch",
            "ubus_cli_and_socket",
            "no_acknowledged_object_lifecycle_stream",
        ),
        (
            "usb_epoch",
            "BSP_TYPEC_EVENT/BSP_POWERBANK_EVENT",
            "no_subscription_ack_or_sequence_loss_contract",
        ),
        // Minted from ubus `zwrt_card_state_event` once a self-sent probe confirms the
        // subscription; ubusd's behaviour for a slow subscriber is unverified.
        (
            "sim_epoch",
            "ubus_card_state_event_probe_confirmed",
            "single_hotswap_observed_ubusd_loss_contract_unverified",
        ),
        // Minted by `netlink` (subscribe, dump, ENOBUFS revokes). Observed (10-20, core
        // 1.6.9): same-IP redial, IPv6-only loss, dual-stack loss with a new IPv4, steady
        // while idle; SSR and a SIM swap unobserved, so not authoritative yet.
        (
            "connection_epoch",
            "rtnetlink_rmnet_link_addr_route",
            "redial_ipv6_dual_observed_ssr_sim_unverified",
        ),
        // No mode-change event exists (B31 strings, §12.8); derived from the rtnetlink and
        // ubus subscriptions plus mode reads. LPM/SSR can't be exercised, so never observed.
        (
            "radio_epoch",
            "derived_rtnetlink_ubus_card_power_mode",
            "lpm_and_ssr_unobserved_no_mode_event",
        ),
    ]
    .into_iter()
    .map(|(key, source, reason)| {
        (
            key.into(),
            luyaz_api::firmware::ContextCoverage {
                source: source.into(),
                authoritative: false,
                reason: reason.into(),
            },
        )
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observations_and_aba_do_not_create_epochs() {
        let mut c = Context::default();
        for mode in [Some("ONLINE"), Some("LOW_POWER"), Some("ONLINE"), None] {
            radio(&mut c, mode);
            assert_eq!(c.radio_epoch, None);
        }
        c.usb_epoch = Some("synthetic".into());
        invalidate(&mut c, Loss::Usb);
        assert_eq!(c.usb_epoch, None);
        c.ubus_epoch = Some("synthetic".into());
        invalidate(&mut c, Loss::Ubus);
        assert_eq!(c, Context::default());
    }
    #[test]
    fn radio_observations_expire_and_include_suspend() {
        let t = |boot: &str, seconds| BootTime {
            boot_id: boot.into(),
            seconds,
        };
        let at = t("test", 10);
        for (now, valid) in [
            (t("test", 15), true),
            (t("test", 16), false),
            (t("test", 9), false),
            (t("other", 11), false),
        ] {
            assert_eq!(fresh(Some(&at), Some(&now)), valid);
        }
        assert!(!fresh(None, Some(&at)));
        assert!(!fresh(Some(&at), None));
    }
}
