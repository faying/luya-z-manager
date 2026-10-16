//! 确认用的 DNS 探测（write-op-layer.md D25、D33）：向运营商 DNS 发一个 A 查询（约 30 字节，回答约
//! 100 字节），socket 绑定到蜂窝数据接口（`SO_BINDTODEVICE`），不经 mwan3 的其他出口。
//! 连接只有 IPv6 时（D41）改向 IPv6 DNS 发 AAAA 查询，同样绑定接口。
//! 有我们 id 的回答就算通，NXDOMAIN 也算（解析器回了话，路是通的）。不是 ubus，不经执行者。

use super::txn::ProbeTarget;
use std::{
    net::{IpAddr, SocketAddr},
    time::Duration,
};

/// 每个 DNS 等多久。最多问两个，一次探测最长 6 秒（确认每拍 2 秒，探测之间至少隔 5 秒）。
const QUERY_TIMEOUT: Duration = Duration::from_secs(3);
/// 和 agent netwatch 的存活探测问同一个名字。
const NAME: &str = "www.qq.com";

/// 绑定到 `target.iface`，依次问前两个 IPv4 DNS（`target.v6` 时是前两个 IPv6 DNS），有一个回答就算通。
/// 没有接口或没有这一族的 DNS 直接算不通，绝不发不绑定接口的查询。
pub async fn dns(target: &ProbeTarget) -> Result<(), String> {
    if target.iface.is_empty() {
        return Err("no cellular interface".into());
    }
    let servers: Vec<SocketAddr> = target
        .dns
        .iter()
        .filter_map(|s| s.trim().parse::<IpAddr>().ok())
        .filter(|ip| ip.is_ipv6() == target.v6)
        .take(2)
        .map(|ip| SocketAddr::new(ip, 53))
        .collect();
    if servers.is_empty() {
        return Err(if target.v6 {
            "no IPv6 DNS server".into()
        } else {
            "no IPv4 DNS server".into()
        });
    }
    let mut last = String::new();
    for addr in servers {
        match query(&target.iface, addr, QUERY_TIMEOUT).await {
            Ok(()) => return Ok(()),
            Err(e) => last = format!("{addr}: {e}"),
        }
    }
    Err(last)
}

/// 一次绑定接口的查询：IPv4 的 DNS 问 A，IPv6 的 DNS 问 AAAA。
pub async fn query(iface: &str, addr: SocketAddr, timeout: Duration) -> Result<(), String> {
    let (local, qtype) = if addr.is_ipv6() {
        ("[::]:0", QTYPE_AAAA)
    } else {
        ("0.0.0.0:0", QTYPE_A)
    };
    let sock = std::net::UdpSocket::bind(local).map_err(|e| e.to_string())?;
    bind_device(&sock, iface)?;
    sock.set_nonblocking(true).map_err(|e| e.to_string())?;
    let sock = tokio::net::UdpSocket::from_std(sock).map_err(|e| e.to_string())?;
    let id = rand::random::<u16>();
    sock.send_to(&packet(id, NAME, qtype), addr)
        .await
        .map_err(|e| e.to_string())?;
    let mut buf = [0u8; 512];
    tokio::time::timeout(timeout, async {
        loop {
            let (n, from) = sock.recv_from(&mut buf).await.map_err(|e| e.to_string())?;
            if from == addr && n >= 12 && buf[0..2] == id.to_be_bytes() && buf[2] & 0x80 != 0 {
                return Ok(());
            }
        }
    })
    .await
    .map_err(|_| "no answer".to_string())?
}

fn bind_device(sock: &std::net::UdpSocket, iface: &str) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    if iface.is_empty() || iface.len() >= libc::IFNAMSIZ || iface.contains('\0') {
        return Err(format!("bad interface name {iface:?}"));
    }
    // SAFETY: fd 是活着的 socket；optval 指向 iface 的字节，长度如实给出。
    let rc = unsafe {
        libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            iface.as_ptr().cast(),
            iface.len() as libc::socklen_t,
        )
    };
    if rc != 0 {
        return Err(format!(
            "bind to {iface}: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

const QTYPE_A: u16 = 1;
const QTYPE_AAAA: u16 = 28;

/// A 或 AAAA 查询，带 RD 位，类 IN。
fn packet(id: u16, name: &str, qtype: u16) -> Vec<u8> {
    let mut q = Vec::with_capacity(32);
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&qtype.to_be_bytes());
    q.extend_from_slice(&[0, 1]);
    q
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn packet_shape() {
        let q = packet(0x1234, "www.qq.com", QTYPE_A);
        assert_eq!(&q[0..2], &[0x12, 0x34]);
        assert_eq!(&q[12..17], b"\x03www\x02");
        assert_eq!(q.len(), 12 + 12 + 4);
        assert_eq!(&q[q.len() - 4..], &[0, 1, 0, 1]);
        // D41：AAAA（28）
        let q = packet(0x1234, "www.qq.com", QTYPE_AAAA);
        assert_eq!(&q[q.len() - 4..], &[0, 28, 0, 1]);
    }

    #[tokio::test]
    async fn refuses_to_send_unbound() {
        let t = |iface: &str, dns: &[&str]| ProbeTarget {
            iface: iface.into(),
            dns: dns.iter().map(|s| s.to_string()).collect(),
            v6: false,
        };
        let t6 = |iface: &str, dns: &[&str]| ProbeTarget {
            v6: true,
            ..t(iface, dns)
        };
        assert_eq!(
            dns(&t("", &["192.0.2.53"])).await,
            Err("no cellular interface".into())
        );
        assert_eq!(
            dns(&t("rmnet_data0", &["2001:db8::53", "x"])).await,
            Err("no IPv4 DNS server".into())
        );
        assert_eq!(
            dns(&t6("", &["2001:db8::53"])).await,
            Err("no cellular interface".into())
        );
        assert_eq!(
            dns(&t6("rmnet_data0", &["192.0.2.53", "x"])).await,
            Err("no IPv6 DNS server".into())
        );
        assert!(
            query(
                "rmnet_data0;",
                "127.0.0.1:53".parse().unwrap(),
                QUERY_TIMEOUT
            )
            .await
            .is_err()
        );
    }

    /// 一个只回 id 的假 DNS（记下问的是哪一类）。
    async fn stub_at(bind: &str) -> std::io::Result<(SocketAddr, Arc<Mutex<Vec<u16>>>)> {
        let s = tokio::net::UdpSocket::bind(bind).await?;
        let addr = s.local_addr()?;
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((n, from)) = s.recv_from(&mut buf).await {
                if n >= 4 {
                    log.lock()
                        .unwrap()
                        .push(u16::from_be_bytes([buf[n - 4], buf[n - 3]]));
                }
                let mut r = buf[..n].to_vec();
                r[2] |= 0x80;
                let _ = s.send_to(&r, from).await;
            }
        });
        Ok((addr, seen))
    }

    /// D41：IPv6 的 DNS 问 AAAA，同样绑定接口（lo 上的 ::1）。容器里没有 IPv6 回环或不能绑定接口时跳过。
    #[tokio::test]
    async fn ipv6_dns_gets_an_aaaa_query_on_the_bound_interface() {
        let (addr, seen) = match stub_at("[::1]:0").await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipped: no IPv6 loopback here ({e})");
                return;
            }
        };
        match query("lo", addr, Duration::from_secs(2)).await {
            Ok(()) => {}
            Err(e) if e.contains("Operation not permitted") => {
                eprintln!("skipped: cannot bind to a device here ({e})");
                return;
            }
            Err(e) => panic!("bound to lo over IPv6: {e}"),
        }
        assert_eq!(*seen.lock().unwrap(), [QTYPE_AAAA]);
        let (addr4, seen4) = stub_at("127.0.0.1:0").await.unwrap();
        query("lo", addr4, Duration::from_secs(2)).await.unwrap();
        assert_eq!(*seen4.lock().unwrap(), [QTYPE_A]);
        // dns() 只问 IPv6 那几个
        let target = ProbeTarget {
            iface: "lo".into(),
            dns: vec![addr4.ip().to_string(), "::1".into()],
            v6: true,
        };
        // 端口固定 53：没有人在 ::1:53 上听，只确认它试的是 IPv6 的那个
        let err = dns(&target).await.unwrap_err();
        assert!(err.starts_with("[::1]:53"), "{err}");
    }

    /// 一个只回 id 的假 DNS。
    async fn stub() -> SocketAddr {
        let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((n, from)) = s.recv_from(&mut buf).await {
                let mut r = buf[..n].to_vec();
                r[2] |= 0x80;
                let _ = s.send_to(&r, from).await;
            }
        });
        addr
    }

    /// 「mwan3 其他出口」：同一个目的地址，绑定到能到达它的接口（lo）就通，绑定到别的接口就不通，
    /// 说明查询确实只走绑定的接口。没有权限绑定接口（非 root 的 CI）时跳过。
    #[tokio::test]
    async fn the_query_only_goes_out_the_bound_interface() {
        let addr = stub().await;
        match query("lo", addr, Duration::from_secs(2)).await {
            Ok(()) => {}
            Err(e) if e.contains("Operation not permitted") => {
                eprintln!("skipped: cannot bind to a device here ({e})");
                return;
            }
            Err(e) => panic!("bound to lo: {e}"),
        }
        // 另一块已启用的网卡（Docker 里是 eth0）：它的路由表到不了 127.0.0.1。
        let other = std::fs::read_dir("/sys/class/net")
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|n| n != "lo")
            .find(|n| {
                std::fs::read_to_string(format!("/sys/class/net/{n}/operstate"))
                    .is_ok_and(|s| s.trim() == "up")
            });
        let Some(other) = other else {
            eprintln!("skipped: no interface up other than lo");
            return;
        };
        assert!(
            query(&other, addr, Duration::from_millis(500))
                .await
                .is_err(),
            "a query bound to {other} reached 127.0.0.1"
        );
        assert!(
            query("nosuch0", addr, Duration::from_millis(500))
                .await
                .is_err()
        );
    }
}
