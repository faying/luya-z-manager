//! The page-top status block (DESIGN.md §5: one coloured block with the page's conclusion), for
//! pages that build their HTML in Rust. Same markup as the template ones (`dns_leaves.html`,
//! `cpu_leaves.html`): `nd-status--{tone}`, a mark, one sentence, then an optional reason line and
//! an optional meta line.

use crate::agent::esc;

/// `ok` | `warn` | `bad` | `neutral` | `stale`.
pub fn symbol(tone: &str) -> &'static str {
    match tone {
        "bad" => "■",
        "warn" | "stale" => "▲",
        _ => "●",
    }
}

/// The block, wrapped in `<div id="{id}" class="sb-wrap">` (right under the title: a margin below it, none inside a `pg-col` grid that already spaces its children) so it
/// can be a streamed leaf. `reason` and `meta` are
/// HTML the caller has escaped (a link may be in them); `state` is plain text.
pub fn html(id: &str, tone: &str, state: &str, reason: Option<&str>, meta: Option<&str>) -> String {
    html_ided(id, None, tone, state, reason, meta)
}

/// [`html`] with an id on the state's text (tests and e2e read the value there).
pub fn html_ided(
    id: &str,
    state_id: Option<&str>,
    tone: &str,
    state: &str,
    reason: Option<&str>,
    meta: Option<&str>,
) -> String {
    format!(
        "<div id=\"{id}\" class=\"sb-wrap\"><section class=\"nd-status nd-status--{tone}{}\"><div class=\"nd-status__main\"><div class=\"nd-status__state\" role=\"status\"><span class=\"nd-mark nd-mark--{tone}\"><span class=\"nd-mark__sym\" aria-hidden=\"true\">{}</span><span{} data-data>{}</span></span></div>{}{}</div></section></div>",
        if tone == "stale" { " nd-stale" } else { "" },
        symbol(tone),
        state_id.map_or(String::new(), |i| format!(" id=\"{i}\"")),
        esc(state),
        reason.filter(|r| !r.is_empty()).map_or(String::new(), |r| format!("<div class=\"nd-status__reason\">{r}</div>")),
        meta.filter(|m| !m.is_empty()).map_or(String::new(), |m| format!("<div class=\"nd-status__meta\">{m}</div>")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_escapes_the_state_and_marks_the_tone() {
        let h = html("x-status", "warn", "a <b>", Some("why"), None);
        assert!(h.starts_with("<div id=\"x-status\" class=\"sb-wrap\">"));
        assert!(h.contains("nd-status--warn") && h.contains("▲") && h.contains("a &lt;b&gt;"));
        assert!(h.contains("nd-status__reason\">why") && !h.contains("nd-status__meta"));
        assert!(html("y", "stale", "s", None, None).contains("nd-status--stale nd-stale"));
    }
}
