//! 确认用的 DNS 探测（write-op-layer.md D25、D33）：向运营商 DNS 发一个 A 查询（约 30 字节，回答约
//! 100 字节），socket 绑定到蜂窝数据接口（`SO_BINDTODEVICE`），不经 mwan3 的其他出口。
//! 连接只有 IPv6 时（D41）改向 IPv6 DNS 发 AAAA 查询，同样绑定接口。
//! 回答要和 Finding 同一套校验（`luyaz_netwatch::finding::path::dns_answer`，R18-P0-02）：问题回显一致、
//! 有这个名字（可经 CNAME）的同族地址才算通；NXDOMAIN/SERVFAIL 等错误码、NODATA、不相关的记录
//! 都不算通（只有 id 不能证明解析器真在工作）。本机建 socket、绑定接口失败时什么也没发出去，回
//! [`ProbeErr::local`]（unknown：不算失败也不算确认）。不是 ubus，不经执行者。

use super::txn::{ProbeErr, ProbeTarget};
use luyaz_api::finding::ResultState;
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
pub async fn dns(target: &ProbeTarget) -> Result<(), ProbeErr> {
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
    // 有一个真问过、没通，就是失败；全都没能发出去（本机失败）才是 unknown
    let mut last: Option<ProbeErr> = None;
    for addr in servers {
        match query(&target.iface, addr, QUERY_TIMEOUT).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                let local = e.local && last.as_ref().is_none_or(|l| l.local);
                last = Some(ProbeErr {
                    local,
                    msg: format!("{addr}: {}", e.msg),
                });
            }
        }
    }
    Err(last.unwrap_or_else(|| "no DNS server".into()))
}

/// 一次绑定接口的查询：IPv4 的 DNS 问 A，IPv6 的 DNS 问 AAAA。
pub async fn query(iface: &str, addr: SocketAddr, timeout: Duration) -> Result<(), ProbeErr> {
    let (local, qtype) = if addr.is_ipv6() {
        ("[::]:0", QTYPE_AAAA)
    } else {
        ("0.0.0.0:0", QTYPE_A)
    };
    let local_err = |e: std::io::Error| ProbeErr::local(e.to_string());
    let sock = std::net::UdpSocket::bind(local).map_err(local_err)?;
    bind_device(&sock, iface).map_err(ProbeErr::local)?;
    sock.set_nonblocking(true).map_err(local_err)?;
    let sock = tokio::net::UdpSocket::from_std(sock).map_err(local_err)?;
    let id = rand::random::<u16>();
    let q = packet(id, NAME, qtype);
    sock.send_to(&q, addr).await.map_err(|e| e.to_string())?;
    // 从这里起是真发出去了：没回答、错误码、没有地址都算失败
    let mut buf = [0u8; 512];
    tokio::time::timeout(timeout, async {
        loop {
            let (n, from) = sock.recv_from(&mut buf).await.map_err(|e| e.to_string())?;
            if from != addr {
                continue;
            }
            match luyaz_netwatch::finding::path::dns_answer(&buf[..n], &q) {
                Some(ResultState::Passed) => return Ok(()),
                Some(ResultState::Failed) => return Err("DNS error rcode".into()),
                Some(ResultState::Unknown) => return Err("answer without an address".into()),
                None => {}
            }
        }
    })
    .await
    .map_err(|_| ProbeErr::from("no answer"))?
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
            chill: false,
            tailnet: false,
        };
        let t6 = |iface: &str, dns: &[&str]| ProbeTarget {
            v6: true,
            chill: false,
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

    /// 把收到的查询变成一个带地址的回答（Finding 的校验要问题回显 + 同族地址）。
    pub(super) fn answer(q: &[u8], rcode: u8) -> Vec<u8> {
        let mut r = q.to_vec();
        r[2] |= 0x80;
        r[3] = 0x80 | rcode;
        if rcode != 0 {
            return r;
        }
        r[7] = 1;
        let qtype = [q[q.len() - 4], q[q.len() - 3]];
        r.extend_from_slice(&[0xc0, 12]);
        r.extend_from_slice(&qtype);
        r.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
        if qtype == [0, 28] {
            r.extend_from_slice(&[0, 16]);
            r.extend_from_slice(&[0x20, 0x01, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        } else {
            r.extend_from_slice(&[0, 4, 192, 0, 2, 1]);
        }
        r
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
                let _ = s.send_to(&answer(&buf[..n], 0), from).await;
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
            Err(e) if e.msg.contains("Operation not permitted") => {
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
            chill: false,
            tailnet: false,
        };
        // 端口固定 53：没有人在 ::1:53 上听，只确认它试的是 IPv6 的那个
        let err = dns(&target).await.unwrap_err();
        assert!(err.msg.starts_with("[::1]:53"), "{err}");
        assert!(!err.local, "a sent query that got nothing is a failure");
    }

    /// 一个按 `rcode` 回答的假 DNS（0 = 带地址；`echo_only` = 只回 id、没有记录）。
    async fn stub_with(rcode: u8, echo_only: bool) -> SocketAddr {
        let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((n, from)) = s.recv_from(&mut buf).await {
                let r = if echo_only {
                    let mut r = buf[..n].to_vec();
                    r[2] |= 0x80;
                    r
                } else {
                    answer(&buf[..n], rcode)
                };
                let _ = s.send_to(&r, from).await;
            }
        });
        addr
    }

    /// R18-P0-02：NXDOMAIN、SERVFAIL、只回 id 的空回答都不算通（旧语义把它们当成通）。
    #[tokio::test]
    async fn nxdomain_servfail_and_empty_replies_are_not_alive() {
        let ok = stub_with(0, false).await;
        match query("lo", ok, Duration::from_secs(2)).await {
            Ok(()) => {}
            Err(e) if e.msg.contains("Operation not permitted") => {
                eprintln!("skipped: cannot bind to a device here ({e})");
                return;
            }
            Err(e) => panic!("answer with an address: {e}"),
        }
        for (rcode, echo, want) in [
            (3, false, "DNS error rcode"),
            (2, false, "DNS error rcode"),
            (0, true, "answer without an address"),
        ] {
            let a = stub_with(rcode, echo).await;
            assert_eq!(
                query("lo", a, Duration::from_secs(2)).await,
                Err(want.into()),
                "rcode {rcode} echo {echo}"
            );
        }
    }

    /// 一个只回 id 的假 DNS。
    async fn stub() -> SocketAddr {
        let s = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = s.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 512];
            while let Ok((n, from)) = s.recv_from(&mut buf).await {
                let _ = s.send_to(&answer(&buf[..n], 0), from).await;
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
            Err(e) if e.msg.contains("Operation not permitted") => {
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
                .is_err_and(|e| e.local),
            "no such interface: nothing sent, unknown"
        );
    }

    /// R18-P0-02 ②：本机 socket/绑定失败是 unknown（`local`），一个都没发出去时 `dns` 也是 unknown。
    #[tokio::test]
    async fn local_bind_failure_is_unknown_not_a_failure() {
        let bad = query(
            "rmnet_data0;",
            "127.0.0.1:53".parse().unwrap(),
            QUERY_TIMEOUT,
        )
        .await
        .unwrap_err();
        assert!(bad.local, "{bad}");
        let target = ProbeTarget {
            iface: "bad;name".into(),
            dns: vec!["192.0.2.53".into(), "192.0.2.54".into()],
            v6: false,
            chill: false,
            tailnet: false,
        };
        assert!(dns(&target).await.unwrap_err().local);
    }
}
