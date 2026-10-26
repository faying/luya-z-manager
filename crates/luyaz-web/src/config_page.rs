//! /config (配置工具): look into a ZXHN/ZXWL router config file.
//! The old page did it all in the browser; here the file goes to this server in a plain
//! multipart form POST, is read in memory and dropped with the response. It is never written to
//! disk, never logged, never forwarded (no device or agent call); the cap is [`MAX_FILE`].
//!
//! `POST /config` shows the result page (header, first 256 bytes in hex); `POST /config/hex`
//! answers with the whole hex dump as a download. Both read the same form (two submit buttons),
//! because the server keeps nothing between requests.

use askama::Template;
use axum::Router;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};

use crate::i18n::Lang;
use crate::pages::Shared;

const PATH: &str = "/config";
/// The most one config file may be (the router's own configs are well under 1 MiB).
pub const MAX_FILE: usize = 4 * 1024 * 1024;
/// Room for the form's own framing above the file.
const SLACK: usize = 64 * 1024;
const PREVIEW: usize = 256;


pub fn routes() -> Router<Shared> {
    Router::new()
        .route(PATH, get(page).post(inspect))
        .route("/config/hex", post(hex))
        .layer(DefaultBodyLimit::max(MAX_FILE + SLACK))
}

// ---- parsing (the old parseHeader / toHex) ----

#[derive(Debug, PartialEq)]
pub struct Header {
    pub magic: String,
    pub payload_type: String,
    pub signature: String,
    pub payload_offset: u32,
    pub file_size: usize,
}

/// `None` = not a ZXHN / ZXWL file (shorter than 16 bytes, or the magic is something else).
pub fn parse_header(d: &[u8]) -> Option<Header> {
    if d.len() < 16 || !(d.starts_with(b"ZXHN") || d.starts_with(b"ZXWL")) {
        return None;
    }
    let payload_type = match d[4] {
        0 => "plain".to_owned(),
        1 => "ECB".to_owned(),
        2 => "CBC".to_owned(),
        3 => "CBC-new".to_owned(),
        n => format!("unknown({n})"),
    };
    // signature: bytes 8..24, up to the first NUL
    let sig = &d[8..d.len().min(24)];
    let sig = &sig[..sig.iter().position(|b| *b == 0).unwrap_or(sig.len())];
    let signature = sig
        .iter()
        .map(|b| *b as char)
        .collect::<String>()
        .trim()
        .to_owned();
    // payload offset: u32 LE at byte 28 (missing bytes read as 0), else the usual 64
    let at = |i: usize| u32::from(d.get(i).copied().unwrap_or(0));
    let off = at(28) | at(29) << 8 | at(30) << 16 | at(31) << 24;
    Some(Header {
        magic: String::from_utf8_lossy(&d[..4]).into_owned(),
        payload_type,
        signature,
        payload_offset: if off == 0 { 64 } else { off },
        file_size: d.len(),
    })
}

/// `000000  5a 58 …  ZXHN…`: offset, 16 bytes in hex (padded to 48), printable ASCII.
pub fn to_hex(d: &[u8], max: usize) -> String {
    let s = &d[..d.len().min(max)];
    let mut out = String::with_capacity(s.len() / 16 * 76 + 80);
    for (i, chunk) in s.chunks(16).enumerate() {
        let hex = chunk
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(" ");
        let asc: String = chunk
            .iter()
            .map(|b| {
                if (32..127).contains(b) {
                    *b as char
                } else {
                    '.'
                }
            })
            .collect();
        out.push_str(&format!("{:06x}  {hex:<48}  {asc}\n", i * 16));
    }
    if d.len() > max {
        out.push_str(&format!("... ({} more bytes)\n", d.len() - max));
    }
    out.pop();
    out
}

// ---- reading the form ----

pub enum Read {
    Ok {
        name: String,
        data: Vec<u8>,
    },
    /// No file chosen / empty.
    Nothing,
    TooBig,
    Broken,
}

async fn read_file(req: axum::extract::Request, app: &Shared) -> Read {
    let mut mp = match Multipart::from_request(req, app).await {
        Ok(m) => m,
        Err(_) => return Read::Broken,
    };
    loop {
        let mut f = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => return Read::Nothing,
            Err(e) => return too_big_or_broken(e.status()),
        };
        if f.name() != Some("file") {
            continue;
        }
        let name = f.file_name().unwrap_or("").to_owned();
        let mut data = Vec::new();
        loop {
            match f.chunk().await {
                Ok(Some(c)) => {
                    if data.len() + c.len() > MAX_FILE {
                        // read (and drop) the rest, up to the body limit, so the browser is
                        // still sending when the answer goes out and can show it
                        while let Ok(Some(_)) = f.chunk().await {}
                        return Read::TooBig;
                    }
                    data.extend_from_slice(&c);
                }
                Ok(None) => break,
                Err(e) => return too_big_or_broken(e.status()),
            }
        }
        if data.is_empty() {
            return Read::Nothing;
        }
        return Read::Ok { name, data };
    }
}

fn too_big_or_broken(s: StatusCode) -> Read {
    if s == StatusCode::PAYLOAD_TOO_LARGE {
        Read::TooBig
    } else {
        Read::Broken
    }
}

// ---- the page ----

pub struct Done {
    pub name: String,
    pub header: Option<Header>,
    pub hex: String,
}

pub struct View {
    pub err: Option<String>,
    pub done: Option<Done>,
}


#[derive(Template)]
#[template(path = "config.html")]
struct Page {
    shell: crate::shell::Shell,
    v: View,
    max_mib: usize,
}

fn render(
    app: &Shared,
    headers: &HeaderMap,
    ctx: &crate::topbar::Ctx,
    v: View,
    status: StatusCode,
) -> Response {
    let shell = app
        .hub
        .read(|st| crate::shell::Shell::new(headers, PATH).live(st, ctx, None));
    let html = Page {
        shell,
        v,
        max_mib: MAX_FILE >> 20,
    }
    .render()
    .unwrap();
    let mut r = crate::assets::maybe_gzip(
        headers,
        "text/html; charset=utf-8",
        html.into_bytes(),
        "no-store",
    );
    *r.status_mut() = status;
    r
}

async fn page(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    render(
        &app,
        req.headers(),
        &ctx,
        View {
            err: None,
            done: None,
        },
        StatusCode::OK,
    )
}

fn problem(l: Lang, r: &Read) -> String {
    match r {
        Read::Nothing => l.pick("Choose a config file first.", "请先选一个配置文件。"),
        Read::TooBig => l.pick(
            "That file is larger than 4 MiB, which is more than a router config ever is. Nothing was read.",
            "文件超过 4 MiB，路由器的配置不会这么大。没有读取。",
        ),
        _ => l.pick(
            "The file could not be received. Pick it again.",
            "没能收到这个文件，请重新选择。",
        ),
    }
    .to_owned()
}

async fn inspect(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    let l = ctx.lang;
    match read_file(req, &app).await {
        Read::Ok { name, data } => {
            let header = parse_header(&data);
            let err = header.is_none().then(|| {
                l.pick(
                    "No ZXHN header marker found: this page can't read the file",
                    "没找到 ZXHN 文件头标记，这个页面读不了这个文件",
                )
                .to_owned()
            });
            let hex = to_hex(&data, PREVIEW);
            render(
                &app,
                &headers,
                &ctx,
                View {
                    err,
                    done: Some(Done { name, header, hex }),
                },
                StatusCode::OK,
            )
        }
        bad => {
            let status = if matches!(bad, Read::TooBig) {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::OK
            };
            render(
                &app,
                &headers,
                &ctx,
                View {
                    err: Some(problem(l, &bad)),
                    done: None,
                },
                status,
            )
        }
    }
}

/// The file name for the download: what the browser sent, reduced to safe characters.
fn dump_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let s: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{}.hex.txt", if s.is_empty() { "config" } else { &s })
}

async fn hex(State(app): State<Shared>, req: axum::extract::Request) -> Response {
    let headers = req.headers().clone();
    let ctx = crate::topbar::Ctx::of_request(&req, PATH);
    match read_file(req, &app).await {
        Read::Ok { name, data } => {
            let mut text = to_hex(&data, data.len());
            text.push('\n');
            (
                [
                    (header::CONTENT_TYPE, "text/plain; charset=utf-8".to_owned()),
                    (
                        header::CONTENT_DISPOSITION,
                        format!("attachment; filename=\"{}\"", dump_name(&name)),
                    ),
                    (header::CACHE_CONTROL, "no-store".to_owned()),
                ],
                text,
            )
                .into_response()
        }
        bad => {
            let status = if matches!(bad, Read::TooBig) {
                StatusCode::PAYLOAD_TOO_LARGE
            } else {
                StatusCode::OK
            };
            let v = View {
                err: Some(problem(ctx.lang, &bad)),
                done: None,
            };
            render(&app, &headers, &ctx, v, status)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zx(kind: u8, sig: &[u8], off: u32) -> Vec<u8> {
        let mut d = vec![0u8; 64];
        d[..4].copy_from_slice(b"ZXHN");
        d[4] = kind;
        d[8..8 + sig.len()].copy_from_slice(sig);
        d[28..32].copy_from_slice(&off.to_le_bytes());
        d
    }

    #[test]
    fn header_fields() {
        let h = parse_header(&zx(2, b"  sig1 ", 0x40)).unwrap();
        assert_eq!(
            h,
            Header {
                magic: "ZXHN".into(),
                payload_type: "CBC".into(),
                signature: "sig1".into(),
                payload_offset: 64,
                file_size: 64
            }
        );
        assert_eq!(
            parse_header(&zx(9, b"", 0)).unwrap().payload_type,
            "unknown(9)"
        );
        assert_eq!(parse_header(&zx(0, b"", 0)).unwrap().payload_offset, 64);
        assert_eq!(
            parse_header(&zx(3, b"", 1234)).unwrap().payload_offset,
            1234
        );
    }

    #[test]
    fn not_a_config() {
        assert!(parse_header(b"ZXHN").is_none()); // under 16 bytes
        assert!(parse_header(b"hello world, hello world").is_none());
        let mut d = zx(1, b"", 0);
        d[..4].copy_from_slice(b"ZXWL");
        assert_eq!(parse_header(&d).unwrap().magic, "ZXWL");
        // exactly 16 bytes: offset bytes missing read as 0
        assert_eq!(parse_header(&d[..16]).unwrap().payload_offset, 64);
    }

    #[test]
    fn hex_layout_like_the_old_page() {
        let d: Vec<u8> = (0..20u8).map(|i| 0x41 + i).collect();
        let t = to_hex(&d, 16);
        let mut lines = t.lines();
        assert_eq!(
            lines.next().unwrap(),
            format!(
                "000000  {:<48}  ABCDEFGHIJKLMNOP",
                "41 42 43 44 45 46 47 48 49 4a 4b 4c 4d 4e 4f 50"
            )
        );
        assert_eq!(lines.next().unwrap(), "... (4 more bytes)");
        assert!(lines.next().is_none());
        assert!(to_hex(&[0, 255], 256).ends_with("  .."));
    }

    #[test]
    fn download_name_is_safe() {
        assert_eq!(dump_name("config.cfg"), "config.cfg.hex.txt");
        assert_eq!(dump_name("C:\\x\\a b\"c.bin"), "a_b_c.bin.hex.txt");
        assert_eq!(dump_name(""), "config.hex.txt");
    }
}
