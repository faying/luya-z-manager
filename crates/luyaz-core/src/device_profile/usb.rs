//! USB operation bindings and denials shared by the executor and HTTP adapters.
use luyaz_api::firmware::{ProbeResult, Support, UsbWriteDisposition, UsbWritePolicy};
use serde::Serialize;
use serde_json::Value;

pub(crate) fn capability(object: &str, method: &str) -> Option<&'static str> {
    if method != "set" {
        return None;
    }
    match object {
        "zwrt_bsp.usb" => Some("usb.mode.set"),
        "zwrt_bsp.typec" => Some("usb.attach.share"),
        "zwrt_bsp.powerbank" => Some("usb.attach.fast_charge"),
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub(crate) struct Denial {
    pub code: &'static str,
    pub message: String,
    pub policy: UsbWritePolicy,
    pub preflight: Option<ProbeResult>,
}

impl std::fmt::Display for Denial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl Denial {
    pub(crate) fn new(policy: UsbWritePolicy, code: &'static str, message: &str) -> Self {
        Self {
            code,
            message: message.into(),
            policy,
            preflight: None,
        }
    }
    pub(crate) fn status(&self) -> u16 {
        if matches!(self.code, "firmware_unsupported" | "usb_method_absent") {
            501
        } else {
            409
        }
    }
}

pub(crate) fn check(policy: &UsbWritePolicy, human: bool) -> Result<(), Box<Denial>> {
    let reason = if policy.assessment.status == Support::Unsupported {
        Some((
            "firmware_unsupported",
            "This firmware does not support this USB operation",
        ))
    } else if policy.disposition == UsbWriteDisposition::Denied {
        Some((
            "capability_blocked",
            "USB operation blocked by the current capability assessment",
        ))
    } else if !human && policy.disposition != UsbWriteDisposition::Ready {
        Some((
            "capability_unverified",
            "USB operation is unverified; an explicit human confirmation and fresh preflight are required",
        ))
    } else {
        None
    };
    match reason {
        Some((code, message)) => Err(Box::new(Denial::new(policy.clone(), code, message))),
        None => Ok(()),
    }
}

fn bit(v: &Value, key: &str) -> Option<i64> {
    v[key].as_i64().or_else(|| v[key].as_str()?.parse().ok())
}

/// Conditions checked from actual reads in the same executor job, never cached UI data.
pub(crate) fn attached(v: &Value) -> bool {
    bit(v, "cc_attch_state") == Some(1)
        && matches!(v["data_role"].as_str(), Some("host" | "device"))
}
pub(crate) fn valid_read(object: &str, v: &Value) -> bool {
    match object {
        "zwrt_bsp.usb" => matches!(v["mode"].as_str(), Some("user" | "debug" | "mtp" | "rndis")),
        "zwrt_bsp.typec" => attached(v),
        "zwrt_bsp.powerbank" => matches!(bit(v, "state"), Some(0 | 1)),
        _ => false,
    }
}

/// Bind verified effects to the reviewed operation, never an arbitrary setter ACK.
pub(crate) fn effect(object: &str, args: &Value, read: &Value) -> Option<Value> {
    let fields = args.as_object()?;
    let key = match object {
        "zwrt_bsp.usb" => "mode",
        "zwrt_bsp.typec" => "DR_Swap",
        "zwrt_bsp.powerbank" => "state",
        _ => return None,
    };
    if fields.keys().any(|k| k != key && k != "source_module") {
        return None;
    }
    match object {
        "zwrt_bsp.usb"
            if valid_read(object, read)
                && args["mode"].as_str().is_some()
                && args["mode"] == read["mode"] =>
        {
            Some(serde_json::json!({"mode":read["mode"]}))
        }
        "zwrt_bsp.typec"
            if attached(read) && args["DR_Swap"] == "device" && read["data_role"] == "device" =>
        {
            Some(serde_json::json!({"data_role":"device","attached":true}))
        }
        "zwrt_bsp.powerbank"
            if matches!(bit(args, "state"), Some(0 | 1))
                && bit(args, "state") == bit(read, "state") =>
        {
            Some(serde_json::json!({"state":bit(read,"state")}))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn readback_cannot_verify_extra_fields_or_another_usb_operation() {
        let read = json!({"mode":"debug","state":1,"cc_attch_state":1,"data_role":"device"});
        assert!(effect("zwrt_bsp.usb", &json!({"mode":"debug"}), &read).is_some());
        for (object, args) in [
            ("zwrt_bsp.usb", json!({"mode":"debug","state":1})),
            (
                "zwrt_bsp.usb",
                json!({"mode":"debug","usb_port_switch":"1"}),
            ),
            (
                "zwrt_bsp.typec",
                json!({"DR_Swap":"device","PR_Swap":"source"}),
            ),
            ("zwrt_bsp.powerbank", json!({"state":1,"DR_Swap":"device"})),
        ] {
            assert!(effect(object, &args, &read).is_none());
        }
    }
}
