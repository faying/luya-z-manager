//! Read-only standby-slot signal over QMI NAS (全球版 dual SIM, u60g 10-21).
//!
//! The firmware's own QMI client is bound to the data slot, so ubus only has that slot's
//! radio. We open our own client on AF_QIPCRTR, bind it (and only it) to one subscription
//! with Bind Subscription 0x0045 (TLV 0x01, one byte: 0 = slot 1, 1 = slot 2; a 4-byte
//! value gives qmi_error 1) and ask Get Signal Info 0x004F. Nothing here writes modem
//! state. A fresh socket per read, a short receive timeout, every failure is `None`.
//!
//! The sampler thread only starts once `sim_dual` has seen a dual-capable device in dual
//! mode, so the single-slot u60 never opens a QMI client.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Mutex, Once};
use std::time::Duration;

pub const SVC_NAS: u32 = 0x03;
pub const MSG_BIND_SUBSCRIPTION: u16 = 0x0045;
pub const MSG_GET_SIGNAL_INFO: u16 = 0x004F;
pub const MSG_GET_SERVING_SYSTEM: u16 = 0x0024;
/// How often the sampler reads the standby slot.
pub const EVERY: Duration = Duration::from_secs(30);
/// A reading older than this is dropped from the snapshot (three missed cycles).
pub const STALE_SECS: u64 = 90;

/// One slot's radio reading. `rat` is "nr" when the NR TLV has a usable RSRP, else "lte"
/// when the LTE TLV does; no usable TLV means no reading at all.
#[derive(Clone, Debug, PartialEq)]
pub struct Reading {
    pub rat: &'static str,
    pub rsrp: i64,
    pub rsrq: Option<i64>,
    pub snr: Option<f64>,
    /// (MCC, MNC, network name) the slot is registered on, from Get Serving System; None
    /// when not registered or not read. The name may be empty.
    pub serving: Option<(i64, i64, String)>,
}

/// A QMI request: control flags 0x00, txn, msg id, TLV length, TLVs.
pub fn encode_request(txn: u16, msg: u16, tlvs: &[u8]) -> Vec<u8> {
    let mut p = Vec::with_capacity(7 + tlvs.len());
    p.push(0x00);
    p.extend_from_slice(&txn.to_le_bytes());
    p.extend_from_slice(&msg.to_le_bytes());
    p.extend_from_slice(&(tlvs.len() as u16).to_le_bytes());
    p.extend_from_slice(tlvs);
    p
}

/// Bind Subscription TLV: type 0x01, length 1, the subscription byte.
pub fn bind_tlv(sub: u8) -> [u8; 4] {
    [0x01, 1, 0, sub]
}

/// The TLV body of a response to `(txn, msg)`, or `None` for anything else
/// (an indication, another request's answer, a short packet).
pub fn response_body(pkt: &[u8], txn: u16, msg: u16) -> Option<&[u8]> {
    if pkt.len() < 7 || pkt[0] != 0x02 {
        return None;
    }
    let rtxn = u16::from_le_bytes([pkt[1], pkt[2]]);
    let rmsg = u16::from_le_bytes([pkt[3], pkt[4]]);
    (rtxn == txn && rmsg == msg).then(|| &pkt[7..])
}

pub fn tlv_find(body: &[u8], ty: u8) -> Option<&[u8]> {
    let mut i = 0;
    while i + 3 <= body.len() {
        let l = u16::from_le_bytes([body[i + 1], body[i + 2]]) as usize;
        let v = body.get(i + 3..i + 3 + l)?;
        if body[i] == ty {
            return Some(v);
        }
        i += 3 + l;
    }
    None
}

/// Result TLV 0x02 says SUCCESS.
pub fn result_ok(body: &[u8]) -> bool {
    tlv_find(body, 0x02).is_some_and(|v| v.len() >= 4 && v[0] == 0 && v[1] == 0)
}

fn i16_at(v: &[u8], at: usize) -> Option<i64> {
    Some(i16::from_le_bytes([*v.get(at)?, *v.get(at + 1)?]) as i64)
}

fn rsrp_ok(x: i64) -> Option<i64> {
    (-156..=-31).contains(&x).then_some(x)
}
fn rsrq_ok(x: i64) -> Option<i64> {
    (-43..=20).contains(&x).then_some(x)
}
/// SNR arrives in tenths of a dB.
fn snr_ok(tenths: i64) -> Option<f64> {
    (-300..=500).contains(&tenths).then(|| tenths as f64 / 10.0)
}

/// Get Signal Info body → reading. NR wins over LTE (NSA reports both; the NR leg is the
/// one the data slot shows too). TLV 0x17 NR {i16 rsrp, i16 snr×10}, 0x18 NR {i16 rsrq},
/// 0x14 LTE {i8 rssi, i8 rsrq, i16 rsrp, i16 snr×10}. Sentinels and out-of-range values
/// are dropped.
pub fn decode_signal(body: &[u8]) -> Option<Reading> {
    if !result_ok(body) {
        return None;
    }
    if let Some(v) = tlv_find(body, 0x17) {
        if let Some(rsrp) = i16_at(v, 0).and_then(rsrp_ok) {
            return Some(Reading {
                rat: "nr",
                rsrp,
                rsrq: tlv_find(body, 0x18).and_then(|q| i16_at(q, 0)).and_then(rsrq_ok),
                snr: i16_at(v, 2).and_then(snr_ok),
                serving: None,
            });
        }
    }
    let v = tlv_find(body, 0x14)?;
    let rsrp = i16_at(v, 2).and_then(rsrp_ok)?;
    Some(Reading {
        rat: "lte",
        rsrp,
        rsrq: v.get(1).map(|&b| b as i8 as i64).and_then(rsrq_ok),
        snr: i16_at(v, 4).and_then(snr_ok),
        serving: None,
    })
}

/// Get Serving System: TLV 0x01 {u8 registration (1 = registered), …}, TLV 0x12 current
/// PLMN {u16 mcc, u16 mnc, u8 len, desc}. (MCC, MNC, desc) only when registered.
pub fn decode_serving(body: &[u8]) -> Option<(i64, i64, String)> {
    if !result_ok(body) || tlv_find(body, 0x01)?.first() != Some(&1) {
        return None;
    }
    let v = tlv_find(body, 0x12)?;
    let u = |at: usize| v.get(at..at + 2).map(|b| i64::from(u16::from_le_bytes([b[0], b[1]])));
    let (mcc, mnc) = (u(0)?, u(2)?);
    let len = usize::from(*v.get(4).unwrap_or(&0));
    let desc = v.get(5..5 + len).map_or(String::new(), |d| String::from_utf8_lossy(d).trim().to_string());
    (200..1000).contains(&mcc).then_some((mcc, mnc, desc))
}

/// 0..5 bars from RSRP, the same scale as the firmware's `signalbar` (5 at the data slot's
/// -85 dBm on u60g). Steps follow the common LTE/NR RSRP bands.
pub fn bars(rsrp: i64) -> i64 {
    match rsrp {
        r if r >= -90 => 5,
        r if r >= -100 => 4,
        r if r >= -108 => 3,
        r if r >= -115 => 2,
        r if r >= -125 => 1,
        _ => 0,
    }
}

#[cfg(target_os = "linux")]
mod io {
    use super::*;
    use std::mem::{size_of, zeroed};

    const AF_QIPCRTR: i32 = 42;
    const QRTR_PORT_CTRL: u32 = 0xffff_fffe;
    const QRTR_TYPE_NEW_SERVER: u32 = 4;
    const QRTR_TYPE_NEW_LOOKUP: u32 = 10;

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct SockaddrQrtr {
        family: u16,
        node: u32,
        port: u32,
    }

    struct Fd(i32);
    impl Drop for Fd {
        fn drop(&mut self) {
            unsafe { libc::close(self.0) };
        }
    }

    fn send(fd: &Fd, to: SockaddrQrtr, buf: &[u8]) -> bool {
        let n = unsafe {
            libc::sendto(
                fd.0,
                buf.as_ptr().cast(),
                buf.len(),
                0,
                (&to as *const SockaddrQrtr).cast(),
                size_of::<SockaddrQrtr>() as u32,
            )
        };
        n == buf.len() as isize
    }

    fn recv(fd: &Fd, buf: &mut [u8]) -> Option<(usize, SockaddrQrtr)> {
        let mut from: SockaddrQrtr = unsafe { zeroed() };
        let mut fl = size_of::<SockaddrQrtr>() as u32;
        let n = unsafe {
            libc::recvfrom(fd.0, buf.as_mut_ptr().cast(), buf.len(), 0, (&mut from as *mut SockaddrQrtr).cast(), &mut fl)
        };
        (n >= 0).then_some((n as usize, from))
    }

    fn open() -> Option<Fd> {
        let s = unsafe { libc::socket(AF_QIPCRTR, libc::SOCK_DGRAM, 0) };
        if s < 0 {
            return None;
        }
        let fd = Fd(s);
        let tv = libc::timeval { tv_sec: 2, tv_usec: 0 };
        unsafe {
            libc::setsockopt(fd.0, libc::SOL_SOCKET, libc::SO_RCVTIMEO, (&tv as *const libc::timeval).cast(), size_of::<libc::timeval>() as u32)
        };
        Some(fd)
    }

    /// The NAS server's address from the QRTR name service.
    fn lookup(fd: &Fd) -> Option<SockaddrQrtr> {
        let mut local: SockaddrQrtr = unsafe { zeroed() };
        let mut sl = size_of::<SockaddrQrtr>() as u32;
        let mut pkt = [0u8; 20];
        pkt[0..4].copy_from_slice(&QRTR_TYPE_NEW_LOOKUP.to_le_bytes());
        pkt[4..8].copy_from_slice(&SVC_NAS.to_le_bytes());
        let mut ctrl = SockaddrQrtr { family: AF_QIPCRTR as u16, node: 0, port: QRTR_PORT_CTRL };
        if !send(fd, ctrl, &pkt) {
            unsafe { libc::getsockname(fd.0, (&mut local as *mut SockaddrQrtr).cast(), &mut sl) };
            ctrl.node = local.node;
            if !send(fd, ctrl, &pkt) {
                return None;
            }
        }
        let mut found = None;
        let mut buf = [0u8; 64];
        // the name service answers with every server and then an all-zero terminator
        for _ in 0..64 {
            let (n, from) = recv(fd, &mut buf)?;
            if from.port != QRTR_PORT_CTRL || n < 20 {
                continue;
            }
            let w = |i: usize| u32::from_le_bytes([buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]);
            if w(0) != QRTR_TYPE_NEW_SERVER {
                continue;
            }
            let (svc, node, port) = (w(4), w(12), w(16));
            if svc == 0 && node == 0 && port == 0 {
                break;
            }
            if svc == SVC_NAS && found.is_none() {
                found = Some(SockaddrQrtr { family: AF_QIPCRTR as u16, node, port });
            }
        }
        found
    }

    fn request(fd: &Fd, to: SockaddrQrtr, txn: u16, msg: u16, tlvs: &[u8]) -> Option<Vec<u8>> {
        if !send(fd, to, &encode_request(txn, msg, tlvs)) {
            return None;
        }
        let mut buf = vec![0u8; 4096];
        for _ in 0..16 {
            let (n, from) = recv(fd, &mut buf)?;
            if from.node != to.node || from.port != to.port {
                continue;
            }
            if let Some(b) = response_body(&buf[..n], txn, msg) {
                return Some(b.to_vec());
            }
        }
        None
    }

    /// One subscription's reading on a fresh client (closed on return).
    pub fn read_sub(sub: u8) -> Option<Reading> {
        let fd = open()?;
        let nas = lookup(&fd)?;
        let b = request(&fd, nas, 1, MSG_BIND_SUBSCRIPTION, &bind_tlv(sub))?;
        if !result_ok(&b) {
            return None;
        }
        let mut r = decode_signal(&request(&fd, nas, 2, MSG_GET_SIGNAL_INFO, &[])?)?;
        r.serving = request(&fd, nas, 3, MSG_GET_SERVING_SYSTEM, &[]).as_deref().and_then(decode_serving);
        Some(r)
    }
}

#[cfg(target_os = "linux")]
pub use io::read_sub;
#[cfg(not(target_os = "linux"))]
pub fn read_sub(_sub: u8) -> Option<Reading> {
    None
}

/// The slot (1 or 2) the sampler should read; 0 = none (single mode, or no standby slot).
static WANT: AtomicI64 = AtomicI64::new(0);
/// (slot, reading, device epoch seconds) of the last good read.
static LAST: Mutex<Option<(i64, Reading, u64)>> = Mutex::new(None);
static START: Once = Once::new();

/// Called from each snapshot: which slot is standby right now (0 = none). Starts the
/// sampler the first time a standby slot exists.
pub fn want(slot: i64) {
    WANT.store(slot, Ordering::Relaxed);
    if slot != 0 && cfg!(not(test)) {
        START.call_once(|| {
            let _ = std::thread::Builder::new().name("qmi-nas".into()).spawn(sampler);
        });
    }
}

fn sampler() {
    loop {
        let slot = WANT.load(Ordering::Relaxed);
        if slot == 1 || slot == 2 {
            let got = std::panic::catch_unwind(|| read_sub((slot - 1) as u8)).ok().flatten();
            let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
            match got {
                Some(r) => *last = Some((slot, r, now())),
                None if last.as_ref().is_some_and(|l| l.0 != slot) => *last = None,
                None => {}
            }
        }
        std::thread::sleep(EVERY);
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The sampler's last good reading: (slot, reading, device epoch seconds).
pub fn last() -> Option<(i64, Reading, u64)> {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// `last` is usable for `slot` at `now`: same slot, not older than [`STALE_SECS`].
pub fn usable(last: Option<&(i64, Reading, u64)>, slot: i64, now: u64) -> Option<(Reading, u64)> {
    let (s, r, at) = last?;
    (*s == slot && now.saturating_sub(*at) <= STALE_SECS).then(|| (r.clone(), *at))
}

/// `luyaz-core qmi-signal [sub…]`: one read per subscription (default 0 and 1), printed.
pub fn main(args: &[String]) -> i32 {
    let subs: Vec<u8> = if args.is_empty() { vec![0, 1] } else { args.iter().filter_map(|a| a.parse().ok()).collect() };
    for sub in subs {
        match read_sub(sub) {
            Some(r) => println!(
                "sub{sub} slot{} rat={} rsrp={} rsrq={} snr={} bars={} serving={}",
                sub + 1,
                r.rat,
                r.rsrp,
                r.rsrq.map_or("-".into(), |x| x.to_string()),
                r.snr.map_or("-".into(), |x| format!("{x:.1}")),
                bars(r.rsrp),
                r.serving.map_or("-".into(), |(c, n, d)| format!("{c}-{n:02} {d:?}"))
            ),
            None => println!("sub{sub} slot{} none", sub + 1),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok() -> Vec<u8> {
        vec![0x02, 4, 0, 0, 0, 0, 0]
    }
    fn tlv(t: u8, v: &[u8]) -> Vec<u8> {
        let mut o = vec![t, v.len() as u8, 0];
        o.extend_from_slice(v);
        o
    }
    fn le(x: i16) -> [u8; 2] {
        x.to_le_bytes()
    }

    #[test]
    fn request_and_bind_bytes() {
        assert_eq!(encode_request(0x0102, 0x0045, &bind_tlv(1)), vec![0, 2, 1, 0x45, 0, 4, 0, 1, 1, 0, 1]);
        assert_eq!(encode_request(3, 0x004F, &[]), vec![0, 3, 0, 0x4F, 0, 0, 0]);
    }

    #[test]
    fn response_matches_txn_and_msg_only() {
        let pkt = [0x02, 2, 0, 0x4F, 0, 7, 0, 0x02, 4, 0, 0, 0, 0, 0];
        assert_eq!(response_body(&pkt, 2, 0x4F), Some(&pkt[7..]));
        assert_eq!(response_body(&pkt, 3, 0x4F), None);
        assert_eq!(response_body(&pkt, 2, 0x45), None);
        let mut ind = pkt;
        ind[0] = 0x04;
        assert_eq!(response_body(&ind, 2, 0x4F), None);
        assert_eq!(response_body(&pkt[..5], 2, 0x4F), None);
    }

    #[test]
    fn nr_wins_and_carries_rsrq() {
        // SUB2 on u60g 10-21: NR -107 / 1.5 / -15, LTE leg also present
        let mut b = ok();
        b.extend(tlv(0x14, &[0xb0, 0xf3, le(-110)[0], le(-110)[1], le(30)[0], le(30)[1]]));
        b.extend(tlv(0x17, &[le(-107)[0], le(-107)[1], le(15)[0], le(15)[1]]));
        b.extend(tlv(0x18, &le(-15)));
        assert_eq!(decode_signal(&b), Some(Reading { rat: "nr", rsrp: -107, rsrq: Some(-15), snr: Some(1.5), serving: None }));
    }

    #[test]
    fn lte_when_nr_missing_or_sentinel() {
        let lte = tlv(0x14, &[0xb0, (-12i8) as u8, le(-95)[0], le(-95)[1], le(-15)[0], le(-15)[1]]);
        let want = Some(Reading { rat: "lte", rsrp: -95, rsrq: Some(-12), snr: Some(-1.5), serving: None });
        let mut b = ok();
        b.extend(lte.clone());
        assert_eq!(decode_signal(&b), want);
        let mut b = ok();
        b.extend(tlv(0x17, &[le(i16::MIN)[0], le(i16::MIN)[1], 0, 0]));
        b.extend(lte);
        assert_eq!(decode_signal(&b), want);
    }

    #[test]
    fn bad_values_drop() {
        let mut b = ok();
        b.extend(tlv(0x17, &[le(-90)[0], le(-90)[1], le(i16::MIN)[0], le(i16::MIN)[1]]));
        b.extend(tlv(0x18, &le(-32768)));
        assert_eq!(decode_signal(&b), Some(Reading { rat: "nr", rsrp: -90, rsrq: None, snr: None, serving: None }));
        // failure result, no TLVs, truncated TLV
        assert_eq!(decode_signal(&[0x02, 4, 0, 1, 0, 1, 0]), None);
        assert_eq!(decode_signal(&ok()), None);
        let mut t = ok();
        t.extend([0x17, 4, 0, 1]);
        assert_eq!(decode_signal(&t), None);
    }

    #[test]
    fn usable_needs_same_slot_and_fresh() {
        let r = Reading { rat: "nr", rsrp: -100, rsrq: None, snr: None, serving: None };
        let l = (2, r.clone(), 1000);
        assert_eq!(usable(Some(&l), 2, 1090), Some((r, 1000)));
        assert_eq!(usable(Some(&l), 2, 1091), None);
        assert_eq!(usable(Some(&l), 1, 1000), None);
        assert_eq!(usable(None, 2, 1000), None);
    }

    #[test]
    fn serving_plmn_only_when_registered() {
        let mut plmn = le(460).to_vec();
        plmn.extend(le(0));
        plmn.extend([4]);
        plmn.extend(b"CMCC");
        let b = [ok(), tlv(0x01, &[1, 1, 1, 1, 1, 8]), tlv(0x12, &plmn)].concat();
        assert_eq!(decode_serving(&b), Some((460, 0, "CMCC".into())));
        let b = [ok(), tlv(0x01, &[2, 1, 1, 1, 0]), tlv(0x12, &plmn)].concat();
        assert_eq!(decode_serving(&b), None);
        assert_eq!(decode_serving(&[ok(), tlv(0x01, &[1])].concat()), None);
    }

    #[test]
    fn bar_steps() {
        assert_eq!([-85, -95, -106, -110, -120, -130].map(bars), [5, 4, 3, 2, 1, 0]);
    }
}
