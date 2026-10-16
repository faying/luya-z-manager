//! Incremental `text/event-stream` parser for u60d's `/v2/events` (event + one data line per frame,
//! but multi-line data is joined as the spec says) and the patch data for Datastar.

/// One parsed frame: event name (default "message") and its data.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub event: String,
    pub data: String,
}

/// Feeds bytes in any chunking; returns complete frames.
#[derive(Default)]
pub struct Parser {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl Parser {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<Frame> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    let event = if self.event.is_empty() {
                        "message".to_owned()
                    } else {
                        std::mem::take(&mut self.event)
                    };
                    out.push(Frame {
                        event,
                        data: self.data.join("\n"),
                    });
                }
                self.event.clear();
                self.data.clear();
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (&*line, ""),
            };
            match field {
                "event" => self.event = value.to_owned(),
                "data" => self.data.push(value.to_owned()),
                _ => {}
            }
        }
        out
    }
}

/// The data of a Datastar `datastar-patch-elements` event (outer morph by id): every line of
/// the HTML on its own `elements` line. axum's `Event::data` writes each line as `data: …`.
pub fn patch_data(html: &str) -> String {
    html.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| format!("elements {l}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_survive_any_chunking() {
        let raw =
            b"event: snapshot\ndata: {\"a\":1}\n\n: comment\nevent: block\ndata: x\ndata: y\n\n";
        let whole = Parser::default().feed(raw);
        let mut p = Parser::default();
        let mut bytewise = Vec::new();
        for b in raw {
            bytewise.extend(p.feed(&[*b]));
        }
        assert_eq!(whole, bytewise);
        assert_eq!(
            whole[0],
            Frame {
                event: "snapshot".into(),
                data: "{\"a\":1}".into()
            }
        );
        assert_eq!(
            whole[1],
            Frame {
                event: "block".into(),
                data: "x\ny".into()
            }
        );
    }

    #[test]
    fn crlf_and_unterminated_tail() {
        let mut p = Parser::default();
        assert_eq!(
            p.feed(b"event: a\r\ndata: 1\r\n\r\nevent: b\ndata: 2\n")
                .len(),
            1
        );
        assert_eq!(p.feed(b"\n")[0].event, "b");
    }

    #[test]
    fn patch_puts_each_line_on_its_own_elements_line() {
        assert_eq!(
            patch_data("<div id=\"a\">\n\n<b>1</b>\n</div>"),
            "elements <div id=\"a\">\nelements <b>1</b>\nelements </div>"
        );
    }
}
