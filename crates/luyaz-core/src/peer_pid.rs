//! 本机一条 TCP 连接属于哪个进程：按本端端口在 `/proc/net/tcp*` 找 socket inode，再扫
//! `/proc/*/fd`。Wi-Fi 锁被占时说清是谁占的（`server/write.rs`、`ops/wifi_lock.rs`）。
//! （原在 legacy_hits.rs，Phase 10 X3 删旧接口计数时搬来。）

/// `/proc/<pid>/stat` 的第 4 个字段；`comm` 在括号里、可能带空格，从最后一个 `)` 后面数。
pub(crate) fn parse_ppid(stat: &str) -> Option<u32> {
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// 本机这一端用 `port` 的 TCP 连接属于哪个进程。
pub(crate) fn find_pid(port: u16) -> Option<u32> {
    let inode = ["/proc/net/tcp", "/proc/net/tcp6"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|table| socket_inode(&table, port))?;
    let wanted = format!("socket:[{inode}]");
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == std::process::id() {
            continue;
        }
        let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for fd in fds.flatten() {
            if std::fs::read_link(fd.path()).is_ok_and(|l| l.as_os_str() == wanted.as_str()) {
                return Some(pid);
            }
        }
    }
    None
}

/// `/proc/net/tcp*` 里本地端口是 `port` 的那一行的 inode（第 10 列）。
fn socket_inode(table: &str, port: u16) -> Option<u64> {
    let want = format!(":{port:04X}");
    table.lines().skip(1).find_map(|line| {
        let cols: Vec<&str> = line.split_whitespace().collect();
        let local = cols.get(1)?;
        if !local.ends_with(&want) {
            return None;
        }
        cols.get(9)?.parse::<u64>().ok().filter(|inode| *inode != 0)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppid_skips_comm_with_spaces_and_parens() {
        assert_eq!(parse_ppid("123 (a b) c) S 45 1 1"), Some(45));
        assert_eq!(parse_ppid("123 (sh) S 7 7 7"), Some(7));
    }

    #[test]
    fn inode_matches_local_port_only() {
        let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:24F4 00000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 111 1 0 100 0 0 10 0\n\
   1: 0100007F:C350 0100007F:24F4 01 00000000:00000000 00:00000000 00000000     0        0 222 1 0 20 4 30 10 -1\n\
   2: 0100007F:24F4 0100007F:C350 01 00000000:00000000 00:00000000 00000000     0        0 333 1 0 20 4 30 10 -1\n";
        assert_eq!(socket_inode(table, 0xC350), Some(222));
        assert_eq!(socket_inode(table, 0x24F4), Some(111));
        assert_eq!(socket_inode(table, 1), None);
    }
}
