//! The handover list, `/tmp/luyaz/handover`: the duties the core runs, one name per line.
//! The sentinel writes it (`luyaz-sentinel/src/worker.rs`, `Paths::handover`). (The agent's own list and its
//! `agent-retired` marker went in Phase 10, X1.)

/// The file's text for these parts: one name per line.
pub fn text(parts: &[&str]) -> String {
    parts.iter().map(|p| format!("{p}\n")).collect()
}

/// Whether the file's text names `part`.
pub fn names(text: &str, part: &str) -> bool {
    text.lines().any(|l| l.trim() == part)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let t = text(&["scheduler", "home_exit"]);
        assert_eq!(t, "scheduler\nhome_exit\n");
        assert!(names(&t, "home_exit"));
        assert!(!names(&t, "home"));
        assert!(!names("", "home_exit"));
    }
}
