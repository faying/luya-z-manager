//! The last USB boundary: every reviewed USB set reaches this before raw INVOKE.
use super::*;
use crate::device_profile::{
    listing::{self, ListingInput},
    usb::{self, Denial},
};
use luyaz_api::{
    clock::BootTime,
    firmware::{Proof, UsbWritePolicy},
};
use serde_json::json;

impl Shared {
    pub(super) async fn usb_readback(
        &self,
        token: luyaz_api::firmware::Scope,
        object: &str,
        args: &Value,
        before: &Value,
        ack: bool,
    ) {
        let value = if ack && usb::effect(object, args, before).is_none() {
            let method = if object == "zwrt_bsp.powerbank" {
                "get"
            } else {
                "list"
            };
            let query = if method == "get" {
                json!({"property":"state"})
            } else {
                json!({})
            };
            let read = tokio::time::timeout(
                Duration::from_secs(2),
                self.raw_call(false, false, object, method, &query),
            )
            .await;
            self.live.call_done();
            read.ok()
                .and_then(Result::ok)
                .and_then(|v| usb::effect(object, args, &v))
        } else {
            None
        };
        self.hub
            .firmware()
            .usb_write(token, BootTime::read(), object, value);
    }

    pub(super) async fn usb_preflight(&self, id: &str, object: &str) -> Result<Value, UbusError> {
        let human = USB_HUMAN.try_with(|h| *h).unwrap_or(false);
        let policy = self
            .hub
            .firmware()
            .report(BootTime::read().as_ref())
            .usb_write_policy(id)
            .expect("fixed USB binding");
        usb::check(&policy, human).map_err(UbusError::CapabilityDenied)?;
        let token = self.hub.firmware().token();
        // One bounded preflight, no retry and no write in the timed future. Cancellation
        // leaves no write to retry. The next socket read uses its own sequence number.
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            let start = Instant::now();
            self.live.call_started();
            let bytes = self.backend.lock().await.usb_methods(object).await;
            self.live.call_done();
            let text = bytes
                .as_ref()
                .ok()
                .and_then(|b| std::str::from_utf8(b).ok());
            let input = || ListingInput {
                text: text.unwrap_or(""),
                exit_code: bytes.is_ok().then_some(0),
                complete: text.is_some(),
                truncated: false,
                duration_ms: start.elapsed().as_millis().min(u64::MAX as u128) as u64,
            };
            let surface = listing::method_result(input(), object, "set");
            if object == "zwrt_bsp.usb" {
                self.hub
                    .firmware()
                    .usb_methods(token.clone(), BootTime::read(), input());
            } else {
                self.hub
                    .firmware()
                    .methods(token.clone(), BootTime::read(), object, input());
            }
            if surface.proof != Some(Proof::MethodPresent) {
                let absent = surface.proof == Some(Proof::MethodAbsent);
                let mut denial = Denial::new(
                    policy.clone(),
                    if absent {
                        "usb_method_absent"
                    } else {
                        "usb_preflight_unavailable"
                    },
                    if absent {
                        "The current USB object has no set method"
                    } else {
                        "Could not validate the complete USB method listing"
                    },
                );
                denial.preflight = Some(surface);
                return Err(UbusError::CapabilityDenied(Box::new(denial)));
            }
            let method = if object == "zwrt_bsp.powerbank" {
                "get"
            } else {
                "list"
            };
            let args = if method == "get" {
                json!({"property":"state"})
            } else {
                json!({})
            };
            let read = self.raw_call(false, false, object, method, &args).await;
            if object == "zwrt_bsp.usb" {
                self.hub.firmware().ubus(
                    token.clone(),
                    BootTime::read(),
                    object,
                    method,
                    read.as_ref().ok(),
                );
            }
            if !read.as_ref().is_ok_and(|v| usb::valid_read(object, v)) {
                return Err(refuse(
                    &policy,
                    "usb_preflight_unavailable",
                    "Current USB state is unavailable or malformed",
                ));
            }
            if object == "zwrt_bsp.powerbank" {
                let typec = self
                    .raw_call(false, false, "zwrt_bsp.typec", "list", &json!({}))
                    .await;
                if !typec.as_ref().is_ok_and(usb::attached) {
                    return Err(refuse(
                        &policy,
                        "usb_not_attached",
                        "Plug a device into the USB-C port first",
                    ));
                }
            }
            Ok(read.expect("validated USB read"))
        })
        .await;
        // Also finish the liveness span if the preflight future was cancelled.
        self.live.call_done();
        let before = match result {
            Ok(r) => r?,
            Err(_) => {
                return Err(refuse(
                    &policy,
                    "usb_preflight_unavailable",
                    "USB preflight exceeded its 10 second budget",
                ));
            }
        };
        if self.hub.firmware().token() != token {
            return Err(refuse(
                &policy,
                "capability_context_changed",
                "Device context changed during USB preflight",
            ));
        }
        let current = self
            .hub
            .firmware()
            .report(BootTime::read().as_ref())
            .usb_write_policy(id)
            .expect("fixed USB binding");
        usb::check(&current, human).map_err(UbusError::CapabilityDenied)?;
        Ok(before)
    }
}

fn refuse(policy: &UsbWritePolicy, code: &'static str, message: &str) -> UbusError {
    UbusError::CapabilityDenied(Box::new(Denial::new(policy.clone(), code, message)))
}
