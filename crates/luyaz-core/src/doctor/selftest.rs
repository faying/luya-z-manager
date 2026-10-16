//! `doctor --ledger-selftest` (docs/LEDGER.md §13): can the ledger work on this device, item by
//! item. The device tools are tried the way doctor.sh tries them (the same commands and args),
//! since the point is to catch a busybox build that lacks one.

use super::{Cfg, cat, sh_raw};
use std::io::Write;
use std::process::{Command, Stdio};

fn run_in(cmd: &[&str], stdin: Option<&[u8]>) -> String {
    let Some((p, a)) = cmd.split_first() else {
        return String::new();
    };
    let mut c = Command::new(p);
    c.args(a).stderr(Stdio::null()).stdout(Stdio::piped());
    c.stdin(if stdin.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    let Ok(mut ch) = c.spawn() else {
        return String::new();
    };
    if let (Some(input), Some(mut si)) = (stdin, ch.stdin.take()) {
        let _ = si.write_all(input);
    }
    ch.wait_with_output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn words(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

pub fn run(c: &Cfg) -> (String, i32) {
    let mut out = String::new();
    let mut nf = 0;
    let mut st = |ok: bool, item: &str, why: &str| {
        if !ok {
            nf += 1;
        }
        let w = if why.is_empty() {
            String::new()
        } else {
            format!("：{why}")
        };
        out.push_str(&format!("{} {item}{w}\n", if ok { "PASS" } else { "FAIL" }));
    };
    let sx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ std::process::id())
        .unwrap_or(0)
        % 1_000_000_000;
    // /data/ledger takes a 512-byte line, fsynced, and gives it back
    let ld = &c.ledger_dir;
    let f = format!("{ld}/.selftest");
    let sl = "x".repeat(512);
    let wrote = std::fs::create_dir_all(ld).is_ok()
        && std::fs::write(&f, format!("{sl}\n")).is_ok()
        && std::fs::OpenOptions::new()
            .append(true)
            .open(&f)
            .and_then(|h| h.sync_all())
            .is_ok()
        && cat(&f).as_deref() == Some(sl.as_str());
    let item = format!("账本目录可写（{ld}）");
    st(
        wrote,
        &item,
        if wrote {
            ""
        } else {
            "写不进、fsync 失败或读回不一致"
        },
    );
    let _ = std::fs::remove_file(&f);
    // key.log.0 too: right after a rotation this boot's code is only in the older file
    let mut kl = std::fs::read(format!("{}.0", c.keylog)).unwrap_or_default();
    kl.extend(std::fs::read(&c.keylog).unwrap_or_default());
    let kl = String::from_utf8_lossy(&kl);
    let code = kl
        .split('\n')
        .rfind(|l| {
            l.match_indices("reboot_reason_code=")
                .any(|(i, k)| l[i + k.len()..].starts_with(|ch: char| ch.is_ascii_digit()))
        })
        .and_then(|l| {
            l.rmatch_indices("reboot_reason_code=").find_map(|(i, k)| {
                let d: String = l[i + k.len()..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                (!d.is_empty()).then_some(d)
            })
        });
    match code {
        Some(sc) => st(true, &format!("key.log 的原因码（这次开机 {sc}）"), ""),
        None => st(
            false,
            "key.log 的原因码",
            &format!(
                "{} 和 {}.0 里都找不到 reboot_reason_code=",
                c.keylog, c.keylog
            ),
        ),
    }
    let sr = cat(&c.mss_recovery)
        .map(|s| {
            s.split('\n')
                .next()
                .unwrap_or("")
                .trim_matches([' ', '\t'])
                .to_owned()
        })
        .unwrap_or_default();
    if sr.is_empty() {
        st(
            false,
            "基带崩溃自恢复开关",
            &format!("读不到 {}", c.mss_recovery),
        );
    } else {
        st(true, &format!("基带崩溃自恢复开关（{sr}）"), "");
    }
    // device tools, called the way the ledger's users call them
    let tmp = std::env::temp_dir().join(format!("doctor-selftest.{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let tf = tmp.join("f");
    let _ = std::fs::write(&tf, "abcdef\n");
    let tfs = tf.to_string_lossy().into_owned();
    let dd = super::strip_nl(run_in(
        &[
            "dd",
            &format!("if={tfs}"),
            "iflag=skip_bytes",
            "skip=2",
            "bs=65536",
        ],
        None,
    ));
    st(
        dd == "cdef",
        "dd iflag=skip_bytes",
        if dd == "cdef" {
            ""
        } else {
            "观察循环读不了落盘文件"
        },
    );
    let ls = run_in(&["ls", "-ln", &tfs], None);
    let l5 = ls.split_whitespace().nth(4).unwrap_or("").to_owned();
    if l5 == "7" {
        st(true, "ls -ln 第 5 列是大小", "");
    } else {
        st(
            false,
            "ls -ln 第 5 列是大小",
            &format!("读到 {}", if l5.is_empty() { "空" } else { &l5 }),
        );
    }
    let mk = super::strip_nl(run_in(
        &[
            "awk",
            "BEGIN { print mktime(\"2026 01 01 00 00 00\"), strftime(\"%Y\", 1790500000) }",
        ],
        None,
    ));
    let mk_ok = mk.starts_with(|ch: char| ch.is_ascii_digit()) && mk.ends_with(" 2026");
    st(
        mk_ok,
        "awk 的 mktime / strftime",
        if mk_ok {
            ""
        } else {
            "成绩单没法换算时间"
        },
    );
    let tern = super::strip_nl(run_in(
        &["awk", "{ v = (($1 == 1) ? \"yes\" : \"no\"); print v }"],
        Some(b"1\n"),
    ));
    st(tern == "yes", "awk 的 ?:（括起来的写法）", "");
    let lock = tmp.join("lock").to_string_lossy().into_owned();
    let fl = Command::new("sh")
        .args(["-c", "exec 7>>\"$1\" && flock -n 7", "sh", &lock])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    st(
        fl,
        "flock -n",
        if fl {
            ""
        } else {
            "账本写锁和观察循环的锁都用它"
        },
    );
    let mut jf = words(&c.jsonfilter);
    jf.extend(["-e", "X=@.a.b", "-e", "Y=@.a.c", "-e", "Z=@.a.b"]);
    let j = run_in(&jf, Some(b"{\"a\":{\"b\":1}}\n"));
    let j_ok = j.find("X=").is_some_and(|i| j[i + 2..].contains("Z="));
    st(
        j_ok,
        "jsonfilter 多个 -e（中间一个路径不存在）",
        if j_ok { "" } else { "网络缓存读不全" },
    );
    let w = sh_raw(&c.wget, &["-q", "-T", "2", "-O", "-", &c.datad_url], true);
    let w_ok = w.starts_with('{');
    st(
        w_ok,
        "wget -T 2 读 datad /v2/state",
        if w_ok {
            ""
        } else {
            "网络缓存没有来源（datad 没在跑也会这样）"
        },
    );
    let _ = std::fs::remove_dir_all(&tmp);
    // what the crash watcher calls the link up (S2b)
    let wan = &c.wan_if;
    let a = sh_raw(&c.ip, &["-4", "-o", "addr", "show", "dev", wan], true);
    let item = format!("ip -4 -o addr 看得到 {wan} 的地址");
    st(
        a.contains("inet "),
        &item,
        if a.contains("inet ") {
            ""
        } else {
            "崩溃跟随判不出恢复（此刻没联网也会这样）"
        },
    );
    let route = std::fs::read(&c.route)
        .ok()
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    let def = route.as_deref().is_some_and(|r| {
        r.split('\n').any(|l| {
            let f: Vec<&str> = l.split([' ', '\t']).filter(|x| !x.is_empty()).collect();
            f.first() == Some(&wan.as_str()) && f.get(1) == Some(&"00000000")
        })
    });
    let item = format!("默认路由走 {wan}（{}）", c.route);
    st(
        def,
        &item,
        if def {
            ""
        } else {
            "崩溃跟随只能看地址（此刻没联网也会这样）"
        },
    );
    // a test line through /dev/kmsg shows up in this boot's capture file within the wait
    let sb: String = cat(&c.boot_id_file)
        .map(|s| {
            s.split('\n')
                .map(|l| l.chars().take(8).collect::<String>())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    let scf = format!(
        "{}/kmsg-{}.log",
        c.crashcap_dir,
        if sb.is_empty() { "boot" } else { &sb }
    );
    let marker = format!("u60-ledger-selftest {sx}");
    let has = || std::fs::read(&scf).is_ok_and(|b| String::from_utf8_lossy(&b).contains(&marker));
    if !std::path::Path::new(&scf).is_file() {
        st(false, "kmsg 落盘", &format!("没有这次开机的落盘文件 {scf}"));
    } else if std::fs::OpenOptions::new()
        .append(true)
        .open(&c.kmsg)
        .and_then(|mut h| h.write_all(format!("{marker}\n").as_bytes()))
        .is_err()
    {
        st(false, "kmsg 落盘", &format!("写不进 {}", c.kmsg));
    } else {
        let wait = c.selftest_wait;
        let mut sw = 0;
        while sw <= wait && !has() {
            std::thread::sleep(std::time::Duration::from_secs(1));
            sw += 1;
        }
        if !has() {
            st(
                false,
                "kmsg 落盘",
                &format!("测试行 {wait} 秒内没出现在 {scf}"),
            );
        } else {
            st(true, &format!("kmsg 落盘（{sw} 秒内）"), "");
            let pos = || {
                cat(&format!("{}/w.pos", c.ledger_tmp))
                    .map(|s| {
                        s.split('\n')
                            .next()
                            .unwrap_or("")
                            .split_whitespace()
                            .next()
                            .unwrap_or("")
                            .to_owned()
                    })
                    .unwrap_or_default()
            };
            let mut swp = String::new();
            if std::path::Path::new(&format!("{}/watcher.pid", c.ledger_tmp)).is_file() {
                swp = pos();
            }
            if !swp.is_empty() {
                // the byte offset of the line with the marker
                let text = std::fs::read(&scf)
                    .map(|b| String::from_utf8_lossy(&b).into_owned())
                    .unwrap_or_default();
                let mut b = 0i64;
                let mut sat = 0i64;
                for l in text.split('\n') {
                    if l.contains(&marker) {
                        sat = b;
                        break;
                    }
                    b += l.len() as i64 + 1;
                }
                let n = |s: &str| s.parse::<i64>().unwrap_or(0);
                let mut sw = 0;
                while sw <= wait && n(&swp) <= sat {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    sw += 1;
                    swp = pos();
                }
                if n(&swp) > sat {
                    st(true, "观察循环读过了测试行", "");
                } else {
                    st(
                        false,
                        "观察循环读过了测试行",
                        &format!("它的位置 {swp} 没过 {sat}"),
                    );
                }
            }
        }
    }
    out.push('\n');
    out.push_str(&if nf == 0 {
        "全部通过\n".to_owned()
    } else {
        format!("{nf} 项没通过\n")
    });
    (out, if nf == 0 { 0 } else { 1 })
}
