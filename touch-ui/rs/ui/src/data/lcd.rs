//! Publish the physical panel state to the stock sleep daemon on takeover and on each edge.

use std::process::{Child, Command};
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct LcdStatus {
    sent: Option<bool>,
    sending: Option<(bool, Child)>,
    failed: Option<(bool, Instant)>,
}

impl LcdStatus {
    pub(super) fn observe(&mut self, panel: Option<bool>) {
        // Keep events ordered across quick key presses. The next model refresh also reaps
        // the child while the panel is dark, without adding a timer to the UI.
        if let Some((lit, child)) = &mut self.sending {
            match child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) if status.success() => {
                    self.sent = Some(*lit);
                    self.failed = None;
                }
                _ => {
                    self.sent = None;
                    self.failed = Some((*lit, Instant::now()));
                }
            }
            self.sending = None;
        }
        let Some(lit) = panel else { return };
        if self.sent == Some(lit) || self.failed.is_some_and(|(state, at)| {
            state == lit && at.elapsed() < Duration::from_secs(30)
        }) {
            return;
        }
        let payload = if lit { r#"{"lcd_status":"on"}"# } else { r#"{"lcd_status":"off"}"# };
        match Command::new("/bin/ubus").args(["-t", "1", "send", "zwrt_deviceui_event.lcdstatus", payload]).spawn() {
            Ok(child) => self.sending = Some((lit, child)),
            Err(_) => self.failed = Some((lit, Instant::now())),
        }
    }
}
