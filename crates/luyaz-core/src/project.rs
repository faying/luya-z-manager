//! 投影层：纯函数，给什么输入出什么结果，不发请求、不读文件、不看时钟（Phase 2a，D12）。
//! 层次划分见 `rust/LAYERS.md`。
pub mod screen;
pub mod snapshot;

#[cfg(test)]
mod tests {
    /// 投影层的边界：源码（不含测试文件）里不能有 IO、时钟、全局状态，也不能调 IO 层。
    /// Phase 2 把这一层原样搬进 u60d，靠这个测试守住。
    #[test]
    fn projection_layer_does_no_io() {
        const FILES: [(&str, &str); 2] = [
            ("snapshot.rs", include_str!("project/snapshot.rs")),
            ("screen.rs", include_str!("project/screen.rs")),
        ];
        const BANNED: [&str; 18] = [
            "async ",
            ".await",
            "std::fs",
            "fs::",
            "std::io",
            "std::net",
            "std::process",
            "env::var",
            "tokio",
            "SystemTime",
            "Instant",
            "Mutex",
            "OnceLock",
            "crate::state",
            "crate::executor",
            "crate::ubus",
            "crate::command",
            "crate::block",
        ];
        for (file, src) in FILES {
            for (n, line) in src.lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                for bad in BANNED {
                    assert!(!code.contains(bad), "{file}:{}: `{bad}` in {line}", n + 1);
                }
                assert!(!code.starts_with("static "), "{file}:{}: static", n + 1);
            }
        }
    }
}
