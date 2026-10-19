//! Record envelope and segment files (u60-platform.md §3, u60-ledger.md §6).
//!
//! Segment = 8-byte magic, then records. Record = 24-byte header + CBOR payload:
//!
//! ```text
//! len u32 LE (payload bytes) | crc32c u32 LE (over seq..payload) | seq u64 LE
//! | schema u16 LE | kind u16 LE | reserved u32 (0) | payload
//! ```
//!
//! Readers skip kinds they do not know and count them (D5: an older build reading a
//! newer log is not corruption). A bad CRC or a short record ends the segment: the
//! rest of that file is dropped and reported, earlier records stay valid.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 8] = b"U60LOG\x00\x01";
pub const HEADER: usize = 24;
pub const MAX_PAYLOAD: usize = 1 << 20;
/// Segment string table: CBOR `(first index, [names])`, appended before the first
/// record that uses a new name. Every segment starts with an empty table, so a
/// segment can be read on its own (u60-ledger.md §6).
pub const KIND_NAMES: u16 = 4;

/// Names defined by the KIND_NAMES records of one segment, in index order.
pub fn segment_names(records: &[Raw]) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for r in records.iter().filter(|r| r.kind == KIND_NAMES) {
        if let Ok((first, more)) = ciborium::from_reader::<(u32, Vec<String>), _>(&r.payload[..])
            && first as usize == names.len()
        {
            names.extend(more);
        }
    }
    names
}

pub fn encode(seq: u64, schema: u16, kind: u16, payload: &[u8], out: &mut Vec<u8>) {
    let mut body = Vec::with_capacity(16 + payload.len());
    body.extend_from_slice(&seq.to_le_bytes());
    body.extend_from_slice(&schema.to_le_bytes());
    body.extend_from_slice(&kind.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());
    body.extend_from_slice(payload);
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(&crc32c::crc32c(&body).to_le_bytes());
    out.extend_from_slice(&body);
}

#[derive(Debug, Clone, PartialEq)]
pub struct Raw {
    pub seq: u64,
    pub schema: u16,
    pub kind: u16,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Decoded {
    pub records: Vec<Raw>,
    /// Bytes after the last good record (bad CRC, torn write, garbage). 0 = clean.
    pub dropped_tail: usize,
}

/// Decodes the records of one segment body (after the magic).
pub fn decode(mut buf: &[u8]) -> Decoded {
    let mut d = Decoded::default();
    while !buf.is_empty() {
        if buf.len() < HEADER {
            d.dropped_tail = buf.len();
            break;
        }
        let len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
        let crc = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        if len > MAX_PAYLOAD
            || buf.len() < HEADER + len
            || crc32c::crc32c(&buf[8..HEADER + len]) != crc
        {
            d.dropped_tail = buf.len();
            break;
        }
        d.records.push(Raw {
            seq: u64::from_le_bytes(buf[8..16].try_into().unwrap()),
            schema: u16::from_le_bytes(buf[16..18].try_into().unwrap()),
            kind: u16::from_le_bytes(buf[18..20].try_into().unwrap()),
            payload: buf[HEADER..HEADER + len].to_vec(),
        });
        buf = &buf[HEADER + len..];
    }
    d
}

/// Where segment files live. `Dir` is a directory on the device (the ledger);
/// `Mem` keeps the same bytes in memory (the event log until every field it
/// stores is marked for sensitivity, u60-platform.md T7). Segments are named by
/// their first sequence number.
pub trait Media: Send {
    /// First sequence numbers of the segments, oldest first.
    fn list(&self) -> io::Result<Vec<u64>>;
    fn read(&self, first: u64) -> io::Result<Vec<u8>>;
    fn len(&self, first: u64) -> io::Result<u64>;
    /// Creates (or truncates) the segment and writes `head` (the magic).
    fn create(&mut self, first: u64, head: &[u8], sync: bool) -> io::Result<()>;
    fn append(&mut self, first: u64, bytes: &[u8], sync: bool) -> io::Result<()>;
    fn remove(&mut self, first: u64) -> io::Result<()>;
}

/// `seg-<first seq, 16 hex>.u60l` files in one directory.
pub struct Dir(pub PathBuf);

impl Dir {
    pub fn open(dir: &Path) -> io::Result<Dir> {
        fs::create_dir_all(dir)?;
        Ok(Dir(dir.to_path_buf()))
    }
    fn path(&self, first: u64) -> PathBuf {
        self.0.join(format!("seg-{first:016x}.u60l"))
    }
}

impl Media for Dir {
    fn list(&self) -> io::Result<Vec<u64>> {
        let mut v: Vec<u64> = fs::read_dir(&self.0)?
            .filter_map(|e| e.ok().and_then(|e| seg_first_seq(&e.path())))
            .collect();
        v.sort();
        Ok(v)
    }
    fn read(&self, first: u64) -> io::Result<Vec<u8>> {
        let mut v = Vec::new();
        File::open(self.path(first))?.read_to_end(&mut v)?;
        Ok(v)
    }
    fn len(&self, first: u64) -> io::Result<u64> {
        Ok(fs::metadata(self.path(first))?.len())
    }
    fn create(&mut self, first: u64, head: &[u8], sync: bool) -> io::Result<()> {
        let mut f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.path(first))?;
        f.write_all(head)?;
        if sync {
            f.sync_all()?;
        }
        Ok(())
    }
    fn append(&mut self, first: u64, bytes: &[u8], sync: bool) -> io::Result<()> {
        let mut f = OpenOptions::new().append(true).open(self.path(first))?;
        f.write_all(bytes)?;
        if sync {
            f.sync_all()?;
        }
        Ok(())
    }
    fn remove(&mut self, first: u64) -> io::Result<()> {
        fs::remove_file(self.path(first))
    }
}

/// Segments in memory, gone with the process.
#[derive(Default)]
pub struct Mem(pub std::collections::BTreeMap<u64, Vec<u8>>);

impl Media for Mem {
    fn list(&self) -> io::Result<Vec<u64>> {
        Ok(self.0.keys().copied().collect())
    }
    fn read(&self, first: u64) -> io::Result<Vec<u8>> {
        self.0
            .get(&first)
            .cloned()
            .ok_or_else(|| io::ErrorKind::NotFound.into())
    }
    fn len(&self, first: u64) -> io::Result<u64> {
        Ok(self.0.get(&first).map_or(0, |v| v.len() as u64))
    }
    fn create(&mut self, first: u64, head: &[u8], _sync: bool) -> io::Result<()> {
        self.0.insert(first, head.to_vec());
        Ok(())
    }
    fn append(&mut self, first: u64, bytes: &[u8], _sync: bool) -> io::Result<()> {
        match self.0.get_mut(&first) {
            Some(v) => {
                v.extend_from_slice(bytes);
                Ok(())
            }
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }
    fn remove(&mut self, first: u64) -> io::Result<()> {
        self.0.remove(&first);
        Ok(())
    }
}

fn decode_file(v: &[u8]) -> Decoded {
    if v.len() < MAGIC.len() || &v[..MAGIC.len()] != MAGIC {
        return Decoded {
            records: vec![],
            dropped_tail: v.len(),
        };
    }
    decode(&v[MAGIC.len()..])
}

/// Append-only segment store over a `Media`.
/// Records are queued (`push`) and appended in one write (`flush`). Segments rotate
/// only in `begin`, which callers run before interning names for a record, so a
/// record and the names it uses always land in the same segment.
pub struct Store {
    media: Box<dyn Media>,
    seg_max: u64,
    keep_bytes: u64,
    /// current segment: first seq, length
    cur: Option<(u64, u64)>,
    next_seq: u64,
    pending: Vec<u8>,
    /// Bytes appended since start (for the daily budget check).
    pub written: u64,
    names: Vec<String>,
    index: HashMap<String, u32>,
    /// names[unsent..] are not in a KIND_NAMES record yet
    unsent: usize,
    schema: u16,
}

impl Store {
    /// Opens a directory store (see `open_on`).
    pub fn open(dir: &Path, seg_max: u64, keep_bytes: u64) -> io::Result<Store> {
        Store::open_on(Box::new(Dir::open(dir)?), seg_max, keep_bytes)
    }

    /// Finds the next sequence number from the newest segment.
    pub fn open_on(media: Box<dyn Media>, seg_max: u64, keep_bytes: u64) -> io::Result<Store> {
        let mut st = Store {
            media,
            seg_max,
            keep_bytes,
            cur: None,
            next_seq: 1,
            pending: Vec::new(),
            written: 0,
            names: Vec::new(),
            index: HashMap::new(),
            unsent: 0,
            schema: 1,
        };
        if let Some(first) = st.segments()?.pop() {
            let bytes = st.media.read(first)?;
            let d = decode_file(&bytes);
            st.next_seq = d
                .records
                .last()
                .map(|r| r.seq + 1)
                .unwrap_or(first)
                .max(first);
            // never append after a damaged tail: start a fresh segment instead
            if d.dropped_tail == 0 {
                st.cur = Some((first, bytes.len() as u64));
                st.names = segment_names(&d.records);
                st.index = st
                    .names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (n.clone(), i as u32))
                    .collect();
                st.unsent = st.names.len();
            }
        }
        Ok(st)
    }

    /// First sequence numbers of the segments, oldest first.
    /// Removes an old segment (not the one being written).
    pub fn remove_segment(&mut self, first: u64) -> io::Result<()> {
        if self.cur.map(|c| c.0) == Some(first) {
            return Ok(());
        }
        self.media.remove(first)
    }

    pub fn segments(&self) -> io::Result<Vec<u64>> {
        self.media.list()
    }

    /// The bytes of one stored segment (magic included).
    #[cfg(test)]
    pub fn segment_bytes(&self, first: u64) -> io::Result<Vec<u8>> {
        self.media.read(first)
    }

    #[cfg(test)]
    pub fn segment_len(&self, first: u64) -> io::Result<u64> {
        self.media.len(first)
    }

    /// True when the next record goes into a new segment (first start, after a
    /// rotation, or after a damaged tail): callers that need every segment to
    /// stand on its own (the event log's checkpoint) push that first.
    pub fn at_segment_start(&self) -> bool {
        self.cur.is_none() && self.pending.is_empty()
    }

    /// Call before building a record of about `est` bytes: when it would overflow
    /// the current segment, writes what is queued and starts a new segment with an
    /// empty name table. Returns whether it rotated.
    pub fn begin(&mut self, est: usize) -> io::Result<bool> {
        let over = match &self.cur {
            Some((_, len)) => *len + (self.pending.len() + est) as u64 > self.seg_max,
            // a new segment still only in memory: the same limit
            None => {
                !self.pending.is_empty()
                    && (MAGIC.len() + self.pending.len() + est) as u64 > self.seg_max
            }
        };
        if !over {
            return Ok(false);
        }
        self.flush(false)?;
        self.cur = None;
        self.names.clear();
        self.index.clear();
        self.unsent = 0;
        Ok(true)
    }

    /// Index of `name` in the current segment's table (adds it if new).
    pub fn intern(&mut self, name: &str) -> u32 {
        if let Some(i) = self.index.get(name) {
            return *i;
        }
        let i = self.names.len() as u32;
        self.names.push(name.to_string());
        self.index.insert(name.to_string(), i);
        i
    }

    /// Queues one record in memory (after any names it introduced); returns its
    /// sequence number.
    pub fn push(&mut self, schema: u16, kind: u16, payload: &[u8]) -> u64 {
        self.schema = schema;
        if self.unsent < self.names.len() {
            let mut b = Vec::new();
            let more = &self.names[self.unsent..];
            ciborium::into_writer(&(self.unsent as u32, more), &mut b).expect("cbor");
            self.unsent = self.names.len();
            self.push_raw(KIND_NAMES, &b);
        }
        self.push_raw(kind, payload)
    }

    fn push_raw(&mut self, kind: u16, payload: &[u8]) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        encode(seq, self.schema, kind, payload, &mut self.pending);
        seq
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// Appends everything queued; `sync` = fsync the file (used for diagnostic results).
    pub fn flush(&mut self, sync: bool) -> io::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        if self.cur.is_none() {
            let first = u64::from_le_bytes(self.pending[8..16].try_into().unwrap());
            self.media.create(first, MAGIC, sync)?;
            self.cur = Some((first, MAGIC.len() as u64));
            self.prune()?;
        }
        let (first, len) = self.cur.as_mut().unwrap();
        self.media.append(*first, &self.pending, sync)?;
        *len += self.pending.len() as u64;
        self.written += self.pending.len() as u64;
        self.pending.clear();
        Ok(())
    }

    /// Deletes the oldest segments while the store is over `keep_bytes`
    /// (never the current one).
    fn prune(&mut self) -> io::Result<()> {
        let mut sizes: Vec<(u64, u64)> = self
            .segments()?
            .into_iter()
            .map(|f| (f, self.media.len(f).unwrap_or(0)))
            .collect();
        let mut total: u64 = sizes.iter().map(|x| x.1).sum();
        let cur = self.cur.map(|c| c.0);
        while total > self.keep_bytes && sizes.len() > 1 {
            let (f, n) = sizes.remove(0);
            if Some(f) == cur {
                break;
            }
            let _ = self.media.remove(f);
            total -= n;
        }
        Ok(())
    }

    /// Every record still stored plus the queued ones, oldest first, grouped by
    /// segment (names resolve within a group); and the bytes dropped as damaged.
    pub fn read_all(&self) -> io::Result<(Vec<Vec<Raw>>, usize)> {
        let mut out = Vec::new();
        let mut dropped = 0;
        for f in self.segments()? {
            let d = decode_file(&self.media.read(f)?);
            dropped += d.dropped_tail;
            out.push(d.records);
        }
        let d = decode(&self.pending);
        if !d.records.is_empty() {
            match (&self.cur, out.last_mut()) {
                (Some(_), Some(last)) => last.extend(d.records),
                _ => out.push(d.records),
            }
        }
        Ok((out, dropped))
    }
}

fn seg_first_seq(p: &Path) -> Option<u64> {
    let n = p.file_name()?.to_str()?;
    let hex = n.strip_prefix("seg-")?.strip_suffix(".u60l")?;
    u64::from_str_radix(hex, 16).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_skip_bad_tail() {
        let mut b = Vec::new();
        encode(1, 1, 1, b"abc", &mut b);
        encode(2, 3, 99, b"future kind", &mut b);
        let good = b.len();
        encode(3, 1, 1, b"torn", &mut b);
        b.truncate(b.len() - 2);
        let d = decode(&b);
        assert_eq!(d.records.len(), 2);
        assert_eq!(d.records[1].kind, 99);
        assert_eq!(d.records[1].payload, b"future kind");
        assert_eq!(d.dropped_tail, b.len() - good);
    }

    #[test]
    fn crc_flip_stops_segment() {
        let mut b = Vec::new();
        encode(1, 1, 1, b"one", &mut b);
        let first = b.len();
        encode(2, 1, 1, b"two", &mut b);
        encode(3, 1, 1, b"three", &mut b);
        b[first + HEADER] ^= 0xff;
        let d = decode(&b);
        assert_eq!(d.records.len(), 1);
        assert_eq!(d.dropped_tail, b.len() - first);
    }

    #[test]
    fn store_resumes_seq_and_rotates() {
        let dir = std::env::temp_dir().join(format!("u60d-store-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        {
            let mut s = Store::open(&dir, 200, 10_000).unwrap();
            for _ in 0..5 {
                s.begin(84).unwrap();
                s.push(1, 1, &[7u8; 60]);
                s.flush(false).unwrap();
            }
        }
        let mut s = Store::open(&dir, 200, 10_000).unwrap();
        assert!(s.segments().unwrap().len() >= 2);
        assert_eq!(s.push(1, 1, b"x"), 6);
        let (all, dropped) = s.read_all().unwrap();
        assert_eq!(dropped, 0);
        assert_eq!(
            all.iter().flatten().map(|r| r.seq).collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5, 6]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_per_segment_survive_reopen_and_rotation() {
        let dir = std::env::temp_dir().join(format!("u60d-names-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let ids = |s: &mut Store, a: &str, b: &str| {
            s.begin(40).unwrap();
            let v = (s.intern(a), s.intern(b));
            s.push(1, 2, &[v.0 as u8, v.1 as u8]);
            v
        };
        {
            let mut s = Store::open(&dir, 400, 1 << 20).unwrap();
            assert_eq!(ids(&mut s, "mihomo", "tailscaled"), (0, 1));
            assert_eq!(ids(&mut s, "tailscaled", "agent"), (1, 2));
            s.flush(false).unwrap();
        }
        // reopen: the table comes back from the segment, no names repeated
        let mut s = Store::open(&dir, 400, 1 << 20).unwrap();
        assert_eq!(ids(&mut s, "agent", "core"), (2, 3));
        // fill until it rotates: the new segment starts over at 0
        let mut rotated = false;
        for i in 0..40 {
            rotated |= s.begin(40).unwrap();
            let a = s.intern(&format!("p{i}"));
            s.push(1, 2, &[a as u8]);
            if rotated {
                break;
            }
        }
        assert!(rotated);
        s.flush(false).unwrap();
        let (groups, dropped) = s.read_all().unwrap();
        assert_eq!(dropped, 0);
        assert!(groups.len() >= 2);
        let first = segment_names(&groups[0]);
        assert_eq!(&first[..4], &["mihomo", "tailscaled", "agent", "core"]);
        let last = groups.last().unwrap();
        let names = segment_names(last);
        let rec = last.iter().rev().find(|r| r.kind == 2).unwrap();
        assert_eq!(rec.payload[0], 0);
        assert!(names[0].starts_with('p'));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_keeps_budget() {
        let dir = std::env::temp_dir().join(format!("u60d-prune-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let mut s = Store::open(&dir, 120, 300).unwrap();
        for _ in 0..20 {
            s.begin(84).unwrap();
            s.push(1, 1, &[1u8; 60]);
            s.flush(false).unwrap();
        }
        let total: u64 = s
            .segments()
            .unwrap()
            .iter()
            .map(|f| s.segment_len(*f).unwrap())
            .sum();
        assert!(total <= 300 + 120, "total {total}");
        let _ = fs::remove_dir_all(&dir);
    }
}
