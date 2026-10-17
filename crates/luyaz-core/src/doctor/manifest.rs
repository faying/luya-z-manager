//! doctor.sh's device manifest (touch-ui docs/SHIP.md): the first text row / last tsv row,
//! `--manifest` and `--tree-fp`, with the same md5 cache file (`$MD5_CACHE`, same keys).

use super::{Cfg, cat, digits, sh_out};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::{Command, Stdio};

const OBSERVE: i64 = 600; // = u60-ship.sh

// ── md5 (RFC 1321): the same hex as busybox md5sum ─────────────────────────

pub fn md5_hex(data: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32)
        .collect();
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `md5sum FILE | cut …`: None when it cannot be read
pub fn md5_file(path: &str) -> Option<String> {
    std::fs::read(path).ok().map(|b| md5_hex(&b))
}

// ── tree_fp ─────────────────────────────────────────────────────────────────

/// Every entry under `dir` as find lists it (no link followed), relative, without the root.
/// An unreadable subdirectory is listed but not entered (find says so and goes on).
fn walk(dir: &Path, rel: &str, out: &mut Vec<(String, std::fs::FileType)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        let Ok(ft) = e.file_type() else { continue };
        out.push((r.clone(), ft));
        if ft.is_dir() {
            walk(&e.path(), &r, out);
        }
    }
}

/// doctor.sh `tree_fp`: md5 of "<md5> <path>\n" per regular file in byte order of the paths;
/// None for a missing directory or a link to one, anything but files and directories inside,
/// names with a newline or "|", a file that cannot be read.
pub fn tree_fp(dir: &str) -> Option<String> {
    let p = Path::new(dir);
    if !p.is_dir() || p.is_symlink() {
        return None;
    }
    std::fs::read_dir(p).ok()?; // cd "$1"
    let mut ents = Vec::new();
    walk(p, "", &mut ents);
    let mut files = Vec::new();
    for (r, ft) in &ents {
        let base = r.rsplit('/').next().unwrap_or(r);
        if !(ft.is_file() || ft.is_dir()) || base.contains('|') || base.contains('\n') {
            return None;
        }
        if ft.is_file() {
            files.push(r.clone());
        }
    }
    files.sort();
    let mut text = String::new();
    for f in &files {
        text.push_str(&md5_file(&format!("{dir}/{f}"))?);
        text.push(' ');
        text.push_str(f);
        text.push('\n');
    }
    Some(md5_hex(text.as_bytes()))
}

// ── the md5 cache ──────────────────────────────────────────────────────────

fn stat_c(dir: Option<&str>, fmt: &str, names: &[String]) -> String {
    let mut c = Command::new("stat");
    c.arg("-c")
        .arg(fmt)
        .args(names)
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    if let Some(d) = dir {
        c.current_dir(d);
    }
    c.output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// `awk -v p=… -v k=… '$1 == p && $2 == k { print $3; exit }' "$MD5_CACHE"`
fn cache_get(c: &Cfg, p: &str, k: &str) -> String {
    let Some(s) = cat(&c.md5_cache) else {
        return String::new();
    };
    for l in s.split('\n') {
        let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
        if f.first().copied().unwrap_or("") == p && f.get(1).copied().unwrap_or("") == k {
            return f.get(2).copied().unwrap_or("").to_owned();
        }
    }
    String::new()
}

/// `{ grep -v "^$p " cache; echo "$p $k $v"; } >cache.$$ && mv -f cache.$$ cache`
fn cache_put(c: &Cfg, p: &str, k: &str, v: &str) {
    let Some(parent) = Path::new(&c.md5_cache).parent() else {
        return;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return;
    }
    let mut out = String::new();
    if let Ok(b) = std::fs::read(&c.md5_cache) {
        let s = String::from_utf8_lossy(&b);
        let pre = format!("{p} ");
        let mut lines: Vec<&str> = s.split('\n').collect();
        if lines.last() == Some(&"") {
            lines.pop();
        }
        for l in lines {
            if !l.starts_with(&pre) {
                out.push_str(l);
                out.push('\n');
            }
        }
    }
    out.push_str(&format!("{p} {k} {v}\n"));
    let tmp = format!("{}.{}", c.md5_cache, std::process::id());
    if std::fs::write(&tmp, out).is_err() || std::fs::rename(&tmp, &c.md5_cache).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// doctor.sh `md5c`: the file's md5, "-" when it is not a file; cached
pub fn md5c(c: &Cfg, path: &str) -> String {
    if !Path::new(path).is_file() {
        return "-".into();
    }
    let mk = super::strip_nl(stat_c(None, "%s:%i:%y:%z", &[path.to_owned()])).replace(' ', "_");
    let mh = cache_get(c, path, &mk);
    if !mh.is_empty() {
        return mh;
    }
    let mh = match &c.md5sum {
        // DOC_MD5SUM (tests count the calls): that command, `| cut -d' ' -f1`
        Some(cmd) => sh_out(cmd, &[path], true)
            .split(' ')
            .next()
            .unwrap_or("")
            .to_owned(),
        None => md5_file(path).unwrap_or_default(),
    };
    if !mh.is_empty() && !mk.is_empty() {
        cache_put(c, path, &mk, &mh);
    }
    if mh.is_empty() { "-".into() } else { mh }
}

/// the tree's cache key: `(cd d && find . -print0 | xargs -0 stat -c '%n|%s|%i|%y|%z') | LC_ALL=C sort | md5sum`
fn tree_key(dir: &str) -> String {
    let mut names = Vec::new();
    if std::fs::read_dir(dir).is_ok() {
        names.push(".".to_owned());
        let mut ents = Vec::new();
        walk(Path::new(dir), "", &mut ents);
        names.extend(ents.into_iter().map(|(r, _)| format!("./{r}")));
    }
    let out = if names.is_empty() {
        String::new()
    } else {
        stat_c(Some(dir), "%n|%s|%i|%y|%z", &names)
    };
    let mut lines: Vec<&str> = out.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    lines.sort();
    let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
    md5_hex(text.as_bytes())
}

/// doctor.sh `treec`: the fingerprint, "-" when missing, "unreadable" when it cannot be taken
pub fn treec(c: &Cfg, dir: &str, fresh: bool) -> String {
    let p = Path::new(dir);
    if !p.exists() && !p.is_symlink() {
        return "-".into();
    }
    let key = format!("tree:{dir}");
    let mut tk = String::new();
    if !fresh {
        tk = tree_key(dir);
        let th = cache_get(c, &key, &tk);
        if !th.is_empty() {
            return th;
        }
    }
    let th = tree_fp(dir);
    if let Some(th) = &th
        && !fresh
    {
        cache_put(c, &key, &tk, th);
    }
    th.unwrap_or_else(|| "unreadable".into())
}

// ── the manifest ───────────────────────────────────────────────────────────

/// awk `fld(k)`: the first `"k":"…"`
fn fld(l: &str, k: &str) -> String {
    let pat = format!("\"{k}\":\"");
    let Some(i) = l.find(&pat) else {
        return String::new();
    };
    let r = &l[i + pat.len()..];
    r.find('"').map(|e| r[..e].to_owned()).unwrap_or_default()
}

/// the next `"path":"[^"]*","(md5|tree)":"[^"]*"` in `s`: (P, M, is_tree, rest)
fn next_file(s: &str) -> Option<(String, String, bool, &str)> {
    const P: &str = "\"path\":\"";
    let mut from = 0;
    while let Some(off) = s[from..].find(P) {
        let i = from + off;
        let r = &s[i + P.len()..];
        if let Some(pe) = r.find('"') {
            let after = &r[pe..];
            for (key, tree) in [("\",\"md5\":\"", false), ("\",\"tree\":\"", true)] {
                if let Some(m) = after.strip_prefix(key)
                    && let Some(me) = m.find('"')
                {
                    let rest = &m[me + 1..];
                    return Some((r[..pe].to_owned(), m[..me].to_owned(), tree, rest));
                }
            }
        }
        from = i + 1;
    }
    None
}

/// doctor.sh `manifest_entries`, sorted: "<comp|record> <name> <path> <md5> [tree]"
pub fn entries(manifest: &str) -> Vec<String> {
    let Ok(b) = std::fs::read(manifest) else {
        return Vec::new();
    };
    let s = String::from_utf8_lossy(&b);
    let mut files: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut owner: BTreeMap<String, String> = BTreeMap::new();
    let mut rec: BTreeMap<String, String> = BTreeMap::new();
    let mut lines: Vec<&str> = s.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    for l in lines {
        if !l.starts_with("{\"v\":1,") {
            continue;
        }
        let k = fld(l, "kind");
        if k == "ship" || k == "kit" {
            let c = fld(l, "comp");
            if c.is_empty() {
                continue;
            }
            let mut f = Vec::new();
            let mut rest = l;
            while let Some((p, m, tree, r)) = next_file(rest) {
                rest = r;
                let e = format!("{p},{m}").replacen(',', " ", 1);
                f.push(format!("comp {c} {e}{}", if tree { " tree" } else { "" }));
                owner.insert(e.split(' ').next().unwrap_or("").to_owned(), c.clone());
            }
            files.insert(c, f);
        } else if k == "retire" {
            for x in fld(l, "comps").split_whitespace() {
                files.remove(x);
            }
            for x in fld(l, "records").split_whitespace() {
                rec.remove(canon_record(x));
            }
        } else if k == "record" {
            let n = canon_record(&fld(l, "name")).to_owned();
            if n.is_empty() {
                continue;
            }
            let v = if l.contains("\"tree\":\"") {
                format!("record {n} {} {} tree", fld(l, "path"), fld(l, "tree"))
            } else {
                format!("record {n} {} {}", fld(l, "path"), fld(l, "md5"))
            };
            rec.insert(n, v);
        }
    }
    let mut out = Vec::new();
    for (c, ls) in &files {
        for l in ls {
            let f3 = l.split_whitespace().nth(2).unwrap_or("");
            if owner.get(f3) == Some(c) {
                out.push(l.clone());
            }
        }
    }
    out.extend(rec.into_values());
    out.sort();
    out
}

fn phase_word(p: &str) -> &str {
    match p {
        "staged" => "已暂存",
        "trial" => "试跑中",
        "promote" => "转正中",
        "check" => "检查中",
        "manifest" => "写清单中",
        "rollback" => "退回中",
        x => x,
    }
}
fn phase_word_en(p: &str) -> &str {
    match p {
        "trial" => "on trial",
        "promote" => "promoting",
        "check" => "checking",
        "manifest" => "writing the manifest",
        "rollback" => "rolling back",
        x => x,
    }
}
fn en_why(r: &str) -> String {
    if !r.is_empty() && r.bytes().all(|b| (b' '..=b'~').contains(&b)) {
        format!(" ({r})")
    } else {
        String::new()
    }
}

/// `m_uptime`
fn m_uptime(c: &Cfg) -> i64 {
    super::uptime_s(c)
}

pub struct State {
    pub level: &'static str,
    pub text: String,
    pub text_en: String,
    /// "<same|differs> <kind> <name> <path> <recorded> <actual>"
    pub rows: Vec<String>,
}

fn cut8(s: &str) -> String {
    s.chars().take(8).collect()
}

/// doctor.sh `manifest_state`
pub fn state(c: &Cfg, fresh: bool) -> State {
    let st = |level, text: String, text_en: String| State {
        level,
        text,
        text_en,
        rows: Vec::new(),
    };
    let (mut ph, mut cp, mut rs, mut tp, mut bt) = (
        String::new(),
        String::new(),
        String::new(),
        String::new(),
        String::new(),
    );
    if let Ok(b) = std::fs::read(&c.ship_txn) {
        let s = String::from_utf8_lossy(&b);
        let mut ls: Vec<&str> = s.split('\n').collect();
        ls.pop(); // read stops at a last line without a newline
        for l in ls {
            if let Some(v) = l.strip_prefix("phase=") {
                ph = v.into();
            } else if let Some(v) = l.strip_prefix("comp=") {
                cp = v.into();
            } else if let Some(v) = l.strip_prefix("reason=") {
                rs = v.into();
            } else if let Some(v) = l.strip_prefix("t_phase=") {
                tp = v.into();
            } else if let Some(v) = l.strip_prefix("boot_id=") {
                bt = v.into();
            }
        }
    }
    match ph.as_str() {
        "staged" | "trial" | "promote" | "check" | "manifest" | "rollback" => {
            let (mut hu, mut hp) = (None, None);
            if let Ok(b) = std::fs::read(&c.ship_hb) {
                let s = String::from_utf8_lossy(&b);
                let f: Vec<&str> = s
                    .split('\n')
                    .next()
                    .unwrap_or("")
                    .split([' ', '\t'])
                    .filter(|x| !x.is_empty())
                    .collect();
                hu = f.first().and_then(|x| digits(x));
                hp = f.get(1).and_then(|x| digits(x));
            }
            let u = m_uptime(c);
            let live = match (hu, hp) {
                (Some(hu), Some(hp)) if u - hu <= 30 => {
                    std::fs::read(format!("{}/{hp}/cmdline", c.proc_dir)).is_ok_and(|b| {
                        crate::paths::is_ship_cmdline(&String::from_utf8_lossy(&b))
                    })
                }
                _ => false,
            };
            return if live {
                st(
                    "ok",
                    format!("正在上机：{cp}（{}）", phase_word(&ph)),
                    format!("Deploying {cp} ({})", phase_word_en(&ph)),
                )
            } else {
                st(
                    "warn",
                    format!(
                        "上次上机停在半路：{cp}（{}），guard 或 u60 status 会收尾",
                        phase_word(&ph)
                    ),
                    format!(
                        "Last deploy stopped midway: {cp} ({}); guard or u60 status will finish it",
                        phase_word_en(&ph)
                    ),
                )
            };
        }
        "failed" => {
            return st(
                "warn",
                format!("上次上机停在半路，要人处理：{cp}（{rs}）"),
                format!(
                    "Last deploy stopped midway, needs attention: {cp}{}",
                    en_why(&rs)
                ),
            );
        }
        "manifest_pending" => {
            return st(
                "warn",
                format!(
                    "清单待补：{cp} 已转正并通过检查，清单还没写上（下次 ship 或 u60 status 补）"
                ),
                format!(
                    "Manifest pending: {cp} is live and checked, not yet in the manifest (next ship or u60 status adds it)"
                ),
            );
        }
        _ => {}
    }
    if !Path::new(&c.manifest).is_file() {
        return st("ok", "还没有清单".into(), "No manifest yet".into());
    }
    let ents = entries(&c.manifest);
    if ents.is_empty() {
        let m = &c.manifest;
        return st(
            "warn",
            format!("清单读不懂（{m}）"),
            format!("Manifest unreadable ({m})"),
        );
    }
    let (mut n, mut bad) = (0, 0);
    let (mut first, mut first_en) = (String::new(), String::new());
    let mut rows = Vec::new();
    for e in &ents {
        let f: Vec<&str> = e.split_whitespace().collect();
        let g = |i: usize| f.get(i).copied().unwrap_or("");
        let act = if g(4) == "tree" {
            treec(c, g(2), fresh)
        } else {
            md5c(c, g(2))
        };
        n += 1;
        if act == g(3) {
            rows.push(format!("same {} {} {} {} {act}", g(0), g(1), g(2), g(3)));
        } else {
            rows.push(format!("differs {} {} {} {} {act}", g(0), g(1), g(2), g(3)));
            bad += 1;
            let base = g(2).rsplit('/').next().unwrap_or("");
            if first.is_empty() {
                first = format!("{base}（清单 {}，实际 {}）", cut8(g(3)), cut8(&act));
            }
            if first_en.is_empty() {
                first_en = format!("{base} (manifest {}, actual {})", cut8(g(3)), cut8(&act));
            }
        }
    }
    if bad > 0 {
        let (mut t, mut te) = (
            format!("不一致的是 {first}"),
            format!("Mismatch: {first_en}"),
        );
        if bad > 1 {
            t = format!("{t} 等 {bad} 项");
            te = format!("{te} and {} more", bad - 1);
        }
        return State {
            level: "warn",
            text: t,
            text_en: te,
            rows,
        };
    }
    let (mut t, mut te) = (format!("一致（{n} 项）"), format!("All match ({n})"));
    let now_boot: String = cat(&c.boot_id_file)
        .map(|s| {
            s.chars()
                .filter(|ch| ch.is_ascii_digit() || ('a'..='f').contains(ch) || *ch == '-')
                .collect()
        })
        .unwrap_or_default();
    // cat strips trailing newlines, tr -dc drops them anyway
    if ph == "done"
        && let Some(tp) = digits(&tp)
        && bt == now_boot
    {
        let left = OBSERVE - (m_uptime(c) - tp);
        if left > 0 {
            let m = (left + 59) / 60;
            t = format!("观察中（还剩 {m} 分钟）：{cp} 刚上机；{t}");
            te = format!("Watching ({m} min left): {cp} just deployed; {te}");
        }
    }
    match ph.as_str() {
        "rolledback" => {
            t = format!("{t}；上次上机已退回：{cp}（{rs}）");
            te = format!("{te}; last deploy rolled back: {cp}{}", en_why(&rs));
        }
        "aborted" => {
            t = format!("{t}；上次上机已中止：{cp}（{rs}）");
            te = format!("{te}; last deploy aborted: {cp}{}", en_why(&rs));
        }
        _ => {}
    }
    State {
        level: "ok",
        text: t,
        text_en: te,
        rows,
    }
}

/// Record names (new, old) of the Luya-Z rename D4: a manifest has either.
const RECORD_RENAMED: [(&str, &str); 4] = [
    ("ui-fonts", "devui-fonts"),
    ("ui-logos", "devui-logos"),
    ("core-plugins", "u60d-plugins"),
    ("luyaz-recover.sh", "u60-recover.sh"),
];

/// A record name as written before D4c → its new name (as doctor.sh's `canon`).
fn canon_record(n: &str) -> &str {
    RECORD_RENAMED.iter().find(|(_, o)| *o == n).map_or(n, |(n, _)| n)
}

fn same_record(recorded: &str, listed: &str) -> bool {
    recorded == listed
        || RECORD_RENAMED
            .iter()
            .any(|(n, o)| (*n == listed && *o == recorded) || (*o == listed && *n == recorded))
}

/// doctor.sh `record_list`: (name, path, tree)
fn record_list(c: &Cfg) -> Vec<(String, String, bool)> {
    let mut v: Vec<(String, String, bool)> = Vec::new();
    v.push((
        "chill.sh".into(),
        format!("{}/chill.sh", c.chill.dir),
        false,
    ));
    v.push(("template.yaml".into(), c.chill.template.clone(), false));
    let ts = &c.ts_dir;
    v.push(("tailscale-start.sh".into(), format!("{ts}/start.sh"), false));
    v.push(("tuning.env".into(), c.ts_tuning.clone(), false));
    v.push(("tailscaled".into(), format!("{ts}/tailscaled"), false));
    v.push((
        "tailscaled-nofight".into(),
        format!("{ts}/nofight/tailscaled"),
        false,
    ));
    for s in [c.core_name(), "tailscale"] {
        v.push((format!("init.d/{s}"), format!("{}/{s}", c.initd), false));
    }
    v.push(("rc.local".into(), c.rc.clone(), false));
    let txn_dir = c
        .ship_txn
        .rsplit_once('/')
        .map(|(d, _)| d)
        .unwrap_or(&c.ship_txn);
    // the names as doctor.sh lists them (D4c names; older manifest lines are read through
    // `canon_record`), next to the transaction file (as in doctor.sh).
    v.push(("luyaz-recover.sh".into(), format!("{txn_dir}/luyaz-recover.sh"), false));
    v.push(("ui-fonts".into(), c.ui_fonts.clone(), true));
    v.push(("ui-logos".into(), c.ui_logos.clone(), true));
    v.push(("core-plugins".into(), c.u60d_plugins.clone(), true));
    v
}

/// `doctor --manifest`
pub fn report(c: &Cfg) -> String {
    let m = state(c, true);
    let mut out = String::new();
    for r in &m.rows {
        let f: Vec<&str> = r.split_whitespace().collect();
        if f.len() == 6 {
            out.push_str(&format!(
                "{}\t{}:{}\t{}\t{}\t{}\n",
                f[0], f[1], f[2], f[3], f[4], f[5]
            ));
        }
    }
    for (n, p, tree) in record_list(c) {
        let seen = m.rows.iter().any(|r| {
            let f: Vec<&str> = r.split_whitespace().collect();
            f.get(1) == Some(&"record") && f.get(2).is_some_and(|r| same_record(r, &n))
        });
        if seen {
            continue;
        }
        let a = if tree {
            treec(c, &p, true)
        } else {
            md5c(c, &p)
        };
        out.push_str(&format!("unrecorded\trecord:{n}\t{p}\t-\t{a}\n"));
    }
    out.push_str(&format!("state\t{}\t{}\n", m.level, m.text));
    out
}
