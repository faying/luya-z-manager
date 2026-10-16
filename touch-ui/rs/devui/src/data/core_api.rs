//! Requests to u60d's screen API: the agent-shaped routes (`{"ok","data"}`) on u60d's own
//! loopback listener, `127.0.0.1:9460/v2/api/...` (M6b, u60-touch-rewrite.md §5). The loopback
//! listener needs no token (the same as `/control` and `/v2/screen`), so there is no login.
//! Callers still name the agent's paths (`/api/...`); the `/v2` prefix is added here.
//! (`agent.rs` / `Agent` until Phase 10 §4; it has called the core, not the agent, since M6b.)
// SPDX-License-Identifier: MIT

use super::http;

pub const PORT_DEFAULT: u16 = 9460;
pub const PREFIX: &str = "/v2";
pub const IO_MS: i32 = 1500;

/// A response: the HTTP status (0 = unreachable or no usable reply) and the body (None on 0).
pub type Reply = (i32, Option<Vec<u8>>);

#[derive(Debug)]
pub struct CoreApi {
    port: u16,
}

impl Default for CoreApi {
    fn default() -> Self {
        CoreApi { port: PORT_DEFAULT }
    }
}

impl CoreApi {
    /// The port (tests).
    pub fn config(&mut self, port: u16) {
        if port > 0 {
            self.port = port;
        }
    }

    /// One request with a per-step timeout.
    pub fn api_ms(&mut self, method: &str, path: &str, json: Option<&str>, cap: usize, io_ms: i32) -> Reply {
        let host = format!("127.0.0.1:{}", self.port);
        let path = format!("{PREFIX}{path}");
        // always Content-Type/Content-Length, as the per-module copies did
        let Some(req) = http::build(method, &path, &host, "", Some(json.unwrap_or("")), 1024) else { return (0, None) };
        let Some(fd) = http::connect_tcp("127.0.0.1", self.port, io_ms) else { return (0, None) };
        let r = http::exchange(fd, req.as_bytes(), cap, io_ms);
        if r.status == 0 {
            return (0, None);
        }
        (r.status, r.body)
    }

    /// The same request (the open status read).
    pub fn http(&mut self, method: &str, path: &str, json: Option<&str>, cap: usize, io_ms: i32) -> Reply {
        self.api_ms(method, path, json, cap, io_ms)
    }

    pub fn api(&mut self, method: &str, path: &str, json: Option<&str>, cap: usize) -> Reply {
        self.api_ms(method, path, json, cap, IO_MS)
    }

    /// Logged in, the body dropped: the status.
    pub fn request_status(&mut self, method: &str, path: &str, json: Option<&str>) -> i32 {
        self.api(method, path, json, 16384).0
    }

    pub fn post(&mut self, path: &str) -> i32 {
        self.request_status("POST", path, None)
    }
}
