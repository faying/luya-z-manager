//! Versioned boot-relative lease shared with BusyBox CHILL. No wall-clock fallback.
use crate::clock::BootTime;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Deadline {
    pub issued: BootTime,
    pub until: u64,
}

impl Deadline {
    pub fn new(issued: BootTime, duration: u64) -> Option<Self> {
        let until = issued.seconds.checked_add(duration)?;
        let d = Self { issued, until };
        Self::decode(&d.encode())
    }

    pub fn encode(&self) -> String {
        format!(
            "v2 {} {} {}\n",
            self.issued.boot_id, self.issued.seconds, self.until
        )
    }

    pub fn decode(text: &str) -> Option<Self> {
        let f: Vec<_> = text.split_ascii_whitespace().collect();
        if f.len() != 4
            || f[0] != "v2"
            || f[1].is_empty()
            || f[1].len() > 64
            || !f[1].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return None;
        }
        let number = |s: &str| -> Option<u64> {
            if s.is_empty()
                || s.len() > 12
                || (s.len() > 1 && s.starts_with('0'))
                || !s.bytes().all(|b| b.is_ascii_digit())
            {
                return None;
            }
            s.parse().ok()
        };
        let (issued, until) = (number(f[2])?, number(f[3])?);
        (until >= issued).then(|| Self {
            issued: BootTime {
                boot_id: f[1].into(),
                seconds: issued,
            },
            until,
        })
    }

    pub fn active(&self, now: Option<&BootTime>) -> bool {
        now.is_some_and(|n| n.elapsed_since(&self.issued).is_some() && n.seconds < self.until)
    }
}

pub fn active(text: &str, now: Option<&BootTime>) -> bool {
    Deadline::decode(text).is_some_and(|d| d.active(now))
}

/// Invalid manual intent is held until explicitly cleared; a valid other-boot pause expires.
pub fn manual_paused(dir: &std::path::Path, now: Option<&BootTime>) -> bool {
    if dir.join("pause_until").try_exists().unwrap_or(true) {
        return true;
    }
    match std::fs::read_to_string(dir.join("pause.v2")) {
        Ok(text) => match Deadline::decode(&text) {
            Some(d) => match now {
                Some(n) if n.boot_id != d.issued.boot_id && !n.boot_id.is_empty() => false,
                Some(n) if n.elapsed_since(&d.issued).is_some() => d.active(Some(n)),
                _ => true,
            },
            None => true,
        },
        Err(e) => e.kind() != std::io::ErrorKind::NotFound,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn at(boot: &str, seconds: u64) -> BootTime {
        BootTime {
            boot_id: boot.into(),
            seconds,
        }
    }
    #[test]
    fn zero_restart_expiry_suspend_reboot_and_bad_data() {
        let d = Deadline::new(at("boot-a", 0), 180).unwrap();
        assert_eq!(Deadline::decode(&d.encode()), Some(d.clone()));
        assert!(d.active(Some(&at("boot-a", 0))));
        assert!(d.active(Some(&at("boot-a", 179)))); // same-boot process restart
        assert!(!d.active(Some(&at("boot-a", 180))));
        assert!(!d.active(Some(&at("boot-a", 7200)))); // suspend counts
        assert!(!d.active(Some(&at("boot-b", 1))));
        assert!(!d.active(None));
        let future = Deadline::new(at("boot-a", 100), 180).unwrap();
        assert!(!future.active(Some(&at("boot-a", 99))));
        for bad in [
            "1790000000",
            "v2 a 00 1",
            "v2 a -1 1",
            "v2 a 2 1",
            "v2 a 1 2 extra",
            "v2 a 1 9999999999999",
        ] {
            assert!(Deadline::decode(bad).is_none(), "{bad}");
        }
        assert!(Deadline::new(at("a", u64::MAX), 1).is_none());
    }
    #[test]
    fn manual_pause_preserves_unknown_intent_but_expires_across_real_boots() {
        let dir = std::env::temp_dir().join(format!("chill-deadline-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("pause.v2");
        assert!(!manual_paused(&dir, None));
        std::fs::write(&file, "v2 boot-a 0 180\n").unwrap();
        assert!(manual_paused(&dir, Some(&at("boot-a", 100))));
        assert!(manual_paused(&dir, None));
        assert!(!manual_paused(&dir, Some(&at("boot-b", 1))));
        assert!(!manual_paused(&dir, Some(&at("boot-a", 180))));
        std::fs::write(&file, "v2 boot-a 100 180\n").unwrap();
        assert!(manual_paused(&dir, Some(&at("boot-a", 99))));
        std::fs::write(&file, "bad").unwrap();
        assert!(manual_paused(&dir, Some(&at("boot-a", 100))));
        std::fs::remove_file(&file).unwrap();
        std::fs::write(dir.join("pause_until"), "1\n").unwrap();
        assert!(manual_paused(&dir, Some(&at("boot-a", 100))));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
