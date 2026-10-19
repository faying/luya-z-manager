//! UCI 配置文件解析器（T11，R19）：核心自己读 `/etc/config/<包>`，不再每次 fork `uci show`。
//!
//! 按 uci 2023-08-10-5781664d（U60 Pro / MU5250 设备上的版本）的 `file.c` 逐步照做：解析在一行缓冲区里
//! 就地进行（引号、反斜杠、`#`、`;` 都在原缓冲区上改写），所以连 uci 的怪行为也一样，例如
//! `option a 1; option b 2` 只得到 `a`（未加引号的 `1;` 的结束符覆盖了 `;`，这一行剩下的被丢掉）。
//! uci 命令行默认严格模式：任何解析错误整个包都读不出来（`uci -q show` 没有输出、退出码 1），
//! 所以这里遇到错误返回 `Err`，调用方退回 `uci show`，不自己猜。
//!
//! `render_show` 生成和 `uci show <包>` 逐字节相同的文本（匿名 section 写成 `@type[N]`，N 按同类型
//! section 的出现顺序计数，**具名的也计数**；值用单引号，`'` 写成 `'\''`；list 各项分别加引号、用空格连接）。
//! `get` 和 `uci get <包>.<节>.<选项>` 的输出相同（不含末尾换行）。
//! 对照：`tests/uci_cases.txt` + 真 uci 生成的 `tests/uci_cases.expected`（`tests/uci_golden_gen.py`）。
//!
//! 什么时候能用解析结果（`source`）：`<savedir>/<包>` **存在且非空**表示有未保存的改动
//! （`uci set` 之后还没 `commit`），这时只有 `uci show` 知道合并后的值，退回子进程。
//! 设备上 `/tmp/.uci` 里有很多 0 字节文件（commit 之后留下的），0 字节 = 没有未保存改动，照样自己解析。
//! 配置文件不存在也退回 `uci show`（结果由 uci 决定，和原来完全一样）。
// SPDX-License-Identifier: MIT

use std::path::Path;

/// 选项的值。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(Vec<u8>),
    List(Vec<Vec<u8>>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// `None` = 匿名 section。
    pub name: Option<Vec<u8>>,
    pub stype: Vec<u8>,
    pub options: Vec<(Vec<u8>, Value)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Package {
    pub sections: Vec<Section>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    pub line: usize,
    pub reason: &'static str,
}

impl std::fmt::Display for ParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "uci parse error at line {}: {}", self.line, self.reason)
    }
}

type Res<T> = Result<T, ParseError>;

/// C 的 `isspace`（C locale）。
fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

/// `uci_validate_str`。
fn validate_str(s: &[u8], name: bool, package: bool) -> bool {
    !s.is_empty()
        && s.iter().all(|&c| {
            c.is_ascii_alphanumeric()
                || c == b'_'
                || (c == b'-' && package)
                || (!name && (33..=126).contains(&c))
        })
}

/// file.c 的解析上下文：一行缓冲区（多行值会接着往后读），下标超出长度当作 NUL。
struct Lexer<'a> {
    input: &'a [u8],
    ipos: usize,
    buf: Vec<u8>,
    pos: usize,
    filled: usize,
    line: usize,
}

impl Lexer<'_> {
    fn ch(&self, i: usize) -> u8 {
        self.buf.get(i).copied().unwrap_or(0)
    }
    fn cur(&self) -> u8 {
        self.ch(self.pos)
    }
    fn set(&mut self, i: usize, c: u8) {
        if i < self.buf.len() {
            self.buf[i] = c;
        }
    }
    fn err(&self, reason: &'static str) -> ParseError {
        ParseError {
            line: self.line,
            reason,
        }
    }
    /// `uci_getln`：从 `offset` 起放下一行（含换行符）。没有更多输入时 `offset` 处为 NUL。
    fn getln(&mut self, offset: usize) {
        self.buf.truncate(offset);
        if self.ipos >= self.input.len() {
            return;
        }
        let rest = &self.input[self.ipos..];
        let n = rest
            .iter()
            .position(|&c| c == b'\n')
            .map_or(rest.len(), |i| i + 1);
        self.buf.extend_from_slice(&rest[..n]);
        self.ipos += n;
        self.filled = self.buf.len();
        if self.buf.last() == Some(&b'\n') {
            self.line += 1;
        }
    }
    fn skip_whitespace(&mut self) {
        while self.cur() != 0 && is_space(self.cur()) {
            self.pos += 1;
        }
    }
    fn addc(&mut self, dest: &mut usize) {
        let c = self.cur();
        self.set(*dest, c);
        *dest += 1;
        self.pos += 1;
    }
    /// `parse_backslash`：返回 true 表示反斜杠后面的字符照原样收下。
    fn backslash(&mut self) -> bool {
        self.pos += 1;
        let c = self.cur();
        if c == 0
            || c == b'\n'
            || (c == b'\r' && self.ch(self.pos + 1) == b'\n' && self.ch(self.pos + 2) == 0)
        {
            self.getln(self.pos);
            return false;
        }
        true
    }
    fn double_quote(&mut self, dest: &mut usize) -> Res<()> {
        self.pos += 1;
        loop {
            match self.cur() {
                b'"' => {
                    self.pos += 1;
                    return Ok(());
                }
                0 => {
                    self.getln(self.pos);
                    if self.cur() == 0 {
                        return Err(self.err("EOF with unterminated \""));
                    }
                }
                b'\\' => {
                    if self.backslash() {
                        self.addc(dest);
                    }
                }
                _ => self.addc(dest),
            }
        }
    }
    fn single_quote(&mut self, dest: &mut usize) -> Res<()> {
        self.pos += 1;
        loop {
            match self.cur() {
                b'\'' => {
                    self.pos += 1;
                    return Ok(());
                }
                0 => {
                    self.getln(self.pos);
                    if self.cur() == 0 {
                        return Err(self.err("EOF with unterminated '"));
                    }
                }
                _ => self.addc(dest),
            }
        }
    }
    /// `parse_str`。
    fn parse_str(&mut self, dest: &mut usize) -> Res<()> {
        let mut next = true;
        loop {
            match self.cur() {
                b'\'' => self.single_quote(dest)?,
                b'"' => self.double_quote(dest)?,
                b'#' => {
                    self.set(self.pos, 0);
                    break;
                }
                0 => break,
                b';' => {
                    next = false;
                    break;
                }
                b'\\' => {
                    if self.backslash() {
                        self.addc(dest);
                    }
                }
                _ => self.addc(dest),
            }
            if self.cur() == 0 || is_space(self.cur()) {
                break;
            }
        }
        if self.cur() != 0 && next {
            self.pos += 1;
        }
        self.set(*dest, 0);
        Ok(())
    }
    /// 从 `at` 起到 NUL 的字节串。
    fn str_at(&self, at: usize) -> Vec<u8> {
        let tail = self.buf.get(at..).unwrap_or_default();
        let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
        tail[..end].to_vec()
    }
    /// `next_arg`。
    fn next_arg(&mut self, required: bool, name: bool, package: bool) -> Res<Vec<u8>> {
        self.skip_whitespace();
        let val = self.pos;
        let mut dest = val;
        if self.cur() == b';' {
            self.set(self.pos, 0);
            self.pos += 1;
        } else {
            self.parse_str(&mut dest)?;
        }
        let s = self.str_at(val);
        if s.is_empty() {
            if required {
                return Err(self.err("insufficient arguments"));
            }
            return Ok(s);
        }
        if name && !validate_str(&s, name, package) {
            return Err(self.err("invalid character in name field"));
        }
        Ok(s)
    }
    fn assert_eol(&mut self) -> Res<()> {
        self.skip_whitespace();
        if !self.next_arg(false, false, false)?.is_empty() {
            return Err(self.err("too many arguments"));
        }
        Ok(())
    }
    /// 关键字之后：`uci_increase_pos(strlen(cur_str) + 1)`。
    fn skip_keyword(&mut self, reason: &'static str) -> Res<()> {
        let len = self.str_at(self.pos).len();
        if self.pos + len + 1 > self.filled {
            return Err(self.err(reason));
        }
        self.pos += len + 1;
        Ok(())
    }
    /// `strtok(cur_str, " \t")`：返回关键字（空 = 没有），并在它后面的分隔符处写 NUL。
    fn strtok(&mut self) -> Vec<u8> {
        let mut i = self.pos;
        while matches!(self.ch(i), b' ' | b'\t') {
            i += 1;
        }
        let start = i;
        while !matches!(self.ch(i), 0 | b' ' | b'\t') {
            i += 1;
        }
        let word = self.buf[start.min(self.buf.len())..i.min(self.buf.len())].to_vec();
        if !word.is_empty() {
            self.set(i, 0);
        }
        word
    }
}

struct Parser {
    pkg: Package,
    current: Option<usize>,
}

impl Parser {
    fn config(&mut self, lx: &mut Lexer) -> Res<()> {
        lx.skip_keyword("config without name")?;
        let stype = lx.next_arg(true, false, false)?;
        if !validate_str(&stype, false, false) {
            return Err(lx.err("invalid character in type field"));
        }
        let name = lx.next_arg(false, true, false)?;
        lx.assert_eol()?;
        if name.is_empty() {
            self.pkg.sections.push(Section {
                name: None,
                stype,
                options: Vec::new(),
            });
            self.current = Some(self.pkg.sections.len() - 1);
            return Ok(());
        }
        let found = self
            .pkg
            .sections
            .iter()
            .position(|s| s.name.as_deref() == Some(&name[..]));
        match found {
            Some(i) if self.pkg.sections[i].stype != stype => {
                Err(lx.err("section of different type overwrites prior section with same name"))
            }
            Some(i) => {
                self.current = Some(i);
                Ok(())
            }
            None => {
                self.pkg.sections.push(Section {
                    name: Some(name),
                    stype,
                    options: Vec::new(),
                });
                self.current = Some(self.pkg.sections.len() - 1);
                Ok(())
            }
        }
    }
    fn option(&mut self, lx: &mut Lexer, list: bool) -> Res<()> {
        let Some(cur) = self.current else {
            return Err(lx.err("option/list command found before the first section"));
        };
        lx.skip_keyword("option without name")?;
        let name = lx.next_arg(true, true, false)?;
        let value = lx.next_arg(false, false, false)?;
        lx.assert_eol()?;
        let options = &mut self.pkg.sections[cur].options;
        let found = options.iter().position(|(n, _)| *n == name);
        if list {
            // uci_add_list：字符串选项变成 [旧值, 新值]，位置不变；空值也是一项。
            match found {
                None => options.push((name, Value::List(vec![value]))),
                Some(i) => match &mut options[i].1 {
                    Value::List(items) => items.push(value),
                    Value::Str(old) => {
                        let old = std::mem::take(old);
                        options[i].1 = Value::List(vec![old, value]);
                    }
                },
            }
        } else if value.is_empty() {
            // uci_set 设空值：解析文件时既不新建，也不删除已有的（真 uci 实测，见 uci_cases 的 lists）。
        } else {
            match found {
                None => options.push((name, Value::Str(value))),
                Some(i) => options[i].1 = Value::Str(value),
            }
        }
        Ok(())
    }
    fn package(lx: &mut Lexer) -> Res<()> {
        // 按包名加载单个文件时 `package` 行只做检查，不切换包。
        lx.skip_keyword("package without name")?;
        lx.next_arg(true, true, true)?;
        lx.assert_eol()
    }
    /// `uci_parse_line`。
    fn line(&mut self, lx: &mut Lexer) -> Res<()> {
        lx.skip_whitespace();
        loop {
            let word = lx.strtok();
            let Some(&first) = word.first() else {
                return Ok(());
            };
            let rest = &word[1..];
            let is = |full: &[u8]| rest.is_empty() || rest == &full[1..];
            match first {
                b'#' => return Ok(()),
                b'p' if is(b"package") => Self::package(lx)?,
                b'c' if is(b"config") => self.config(lx)?,
                b'o' if is(b"option") => self.option(lx, false)?,
                b'l' if is(b"list") => self.option(lx, true)?,
                _ => return Err(lx.err("invalid command")),
            }
        }
    }
}

/// 解析一个配置文件的全部内容。含 NUL 字节的文件不解析（C 的字符串处理在那里行为不同）。
pub fn parse(input: &[u8]) -> Result<Package, ParseError> {
    if input.contains(&0) {
        return Err(ParseError {
            line: 0,
            reason: "NUL byte in file",
        });
    }
    let mut lx = Lexer {
        input,
        ipos: 0,
        buf: Vec::new(),
        pos: 0,
        filled: 0,
        line: 0,
    };
    let mut p = Parser {
        pkg: Package::default(),
        current: None,
    };
    while lx.ipos < input.len() {
        lx.pos = 0;
        lx.getln(0);
        if lx.ch(0) != 0 {
            p.line(&mut lx)?;
        }
    }
    Ok(p.pkg)
}

/// `uci_print_value`：单引号，`'` → `'\''`。
fn quote(out: &mut Vec<u8>, v: &[u8]) {
    out.push(b'\'');
    for &c in v {
        if c == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(c);
        }
    }
    out.push(b'\'');
}

/// 各 section 的 `@type[N]`（N 数同类型的全部 section，具名的也算）。
fn type_refs(pkg: &Package) -> Vec<Vec<u8>> {
    let mut counts: Vec<(&[u8], usize)> = Vec::new();
    pkg.sections
        .iter()
        .map(|s| {
            let idx = match counts.iter_mut().find(|(t, _)| *t == &s.stype[..]) {
                Some((_, n)) => {
                    *n += 1;
                    *n - 1
                }
                None => {
                    counts.push((&s.stype, 1));
                    0
                }
            };
            let mut r = b"@".to_vec();
            r.extend_from_slice(&s.stype);
            r.extend_from_slice(format!("[{idx}]").as_bytes());
            r
        })
        .collect()
}

/// 各 section 在 `uci show` 里的引用名：具名的用名字，匿名的 `@type[N]`。
fn section_refs(pkg: &Package) -> Vec<Vec<u8>> {
    pkg.sections
        .iter()
        .zip(type_refs(pkg))
        .map(|(s, r)| s.name.clone().unwrap_or(r))
        .collect()
}

/// 和 `uci show <package>` 的输出逐字节相同。
pub fn render_show(package: &str, pkg: &Package) -> Vec<u8> {
    let mut out = Vec::new();
    for (s, r) in pkg.sections.iter().zip(section_refs(pkg)) {
        let prefix = [package.as_bytes(), b".", &r].concat();
        out.extend_from_slice(&prefix);
        out.push(b'=');
        out.extend_from_slice(&s.stype);
        out.push(b'\n');
        for (name, value) in &s.options {
            out.extend_from_slice(&prefix);
            out.push(b'.');
            out.extend_from_slice(name);
            out.push(b'=');
            match value {
                Value::Str(v) => quote(&mut out, v),
                Value::List(items) => {
                    for (i, item) in items.iter().enumerate() {
                        if i > 0 {
                            out.push(b' ');
                        }
                        quote(&mut out, item);
                    }
                }
            }
            out.push(b'\n');
        }
    }
    out
}

/// 和 `uci get <包>.<section>.<option>` 的输出相同（不含末尾换行）；`section` 是名字或 `@type[N]`（N ≥ 0）。
/// 找不到返回 `None`（`uci get` 报错）。
pub fn get(pkg: &Package, section: &str, option: &str) -> Option<Vec<u8>> {
    let want = section.as_bytes();
    let i = match pkg
        .sections
        .iter()
        .position(|s| s.name.as_deref() == Some(want))
    {
        Some(i) => i,
        None => type_refs(pkg).iter().position(|r| r == want)?,
    };
    let value = &pkg.sections[i]
        .options
        .iter()
        .find(|(n, _)| n == option.as_bytes())?
        .1;
    Some(match value {
        Value::Str(v) => v.clone(),
        Value::List(items) => {
            let mut out = Vec::new();
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(b' ');
                }
                if item
                    .iter()
                    .any(|&c| matches!(c, b' ' | b'\t' | b'\r' | b'\n'))
                {
                    quote(&mut out, item);
                } else {
                    out.extend_from_slice(item);
                }
            }
            out
        }
    })
}

/// 这个包该怎么读。
#[derive(Debug, PartialEq, Eq)]
pub enum Source {
    /// 没有未保存改动、配置文件在：自己解析（内容已读好）。
    Parse(Vec<u8>),
    /// 退回 `uci show` / `uci get` 子进程。
    Show,
}

/// R19 的选择规则：`<savedir>/<包>` 存在且非空 → `Show`（有未保存改动；0 字节文件 = 没有改动）；
/// 配置文件读不到 → `Show`；否则 `Parse`。文件一次整读（`uci commit` 是改名替换，读不到半个文件）。
pub fn choose(package: &str, confdir: &Path, savedir: &Path) -> Source {
    if std::fs::metadata(savedir.join(package)).is_ok_and(|m| m.len() > 0) {
        return Source::Show;
    }
    match std::fs::read(confdir.join(package)) {
        Ok(data) => Source::Parse(data),
        Err(_) => Source::Show,
    }
}

/// 按 R19 读包：能自己解析就返回解析结果，否则 `None`（调用方退回子进程）。
pub fn load(package: &str, confdir: &Path, savedir: &Path) -> Option<Package> {
    match choose(package, confdir, savedir) {
        Source::Parse(data) => parse(&data).ok(),
        Source::Show => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CASES: &str = include_str!("../tests/uci_cases.txt");
    const EXPECTED: &[u8] = include_bytes!("../tests/uci_cases.expected");

    /// 和 tests/uci_golden_gen.py 的 cases() 相同的切分规则。
    fn split_cases() -> Vec<(String, Vec<u8>)> {
        let mut out = Vec::new();
        let mut cur: Option<(String, Vec<String>, String)> = None;
        let flush = |cur: Option<(String, Vec<String>, String)>,
                     out: &mut Vec<(String, Vec<u8>)>| {
            if let Some((name, flags, mut data)) = cur {
                if flags.iter().any(|f| f == "nonl") && data.ends_with('\n') {
                    data.pop();
                }
                if flags.iter().any(|f| f == "crlf") {
                    data = data.replace('\n', "\r\n");
                }
                out.push((name, data.into_bytes()));
            }
        };
        for line in CASES.split_inclusive('\n') {
            if let Some(head) = line.strip_prefix("=== ") {
                flush(cur.take(), &mut out);
                let mut parts = head.split_whitespace().map(str::to_owned);
                let name = parts.next().unwrap();
                cur = Some((name, parts.collect(), String::new()));
            } else if let Some((_, _, data)) = cur.as_mut() {
                data.push_str(line);
            }
        }
        flush(cur, &mut out);
        out
    }

    struct Expected {
        code: String,
        show: Vec<u8>,
        gets: Vec<(String, String, Vec<u8>)>,
    }

    fn expected() -> Vec<(String, Expected)> {
        let mut fields = EXPECTED.split(|&c| c == 0);
        let mut next = || {
            fields
                .next()
                .expect("truncated uci_cases.expected")
                .to_vec()
        };
        let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
        let mut out: Vec<(String, Expected)> = Vec::new();
        loop {
            match &next()[..] {
                b"end" => return out,
                b"case" => {
                    let name = text(next());
                    let code = text(next());
                    let show = next();
                    out.push((
                        name,
                        Expected {
                            code,
                            show,
                            gets: Vec::new(),
                        },
                    ));
                }
                b"get" => {
                    let path = text(next());
                    let code = text(next());
                    let value = next();
                    out.last_mut().unwrap().1.gets.push((path, code, value));
                }
                other => panic!("bad field {other:?}"),
            }
        }
    }

    #[test]
    fn uci_show_matches_real_uci_line_by_line() {
        let cases = split_cases();
        let expected = expected();
        assert_eq!(cases.len(), expected.len(), "重跑 tests/uci_golden_gen.py");
        assert!(cases.len() >= 20);
        for ((name, data), (ename, exp)) in cases.iter().zip(&expected) {
            assert_eq!(name, ename);
            let got = parse(data);
            if exp.code != "0" {
                assert!(got.is_err(), "{name}: real uci rejects it, parser accepted");
                continue;
            }
            let pkg = got.unwrap_or_else(|e| panic!("{name}: {e}"));
            let show = render_show(name, &pkg);
            let (a, b) = (
                String::from_utf8_lossy(&show).into_owned(),
                String::from_utf8_lossy(&exp.show).into_owned(),
            );
            for (i, (x, y)) in a.lines().zip(b.lines()).enumerate() {
                assert_eq!(x, y, "{name}: line {} differs", i + 1);
            }
            assert_eq!(show, exp.show, "{name}: uci show output differs");
            for (path, code, value) in &exp.gets {
                assert_eq!(code, "0", "{name}: {path}");
                let (_, rest) = path.split_once('.').unwrap();
                let (sec, opt) = rest.rsplit_once('.').unwrap();
                let mut want = value.clone();
                assert_eq!(want.pop(), Some(b'\n'));
                assert_eq!(get(&pkg, sec, opt), Some(want), "{name}: uci get {path}");
            }
        }
    }

    #[test]
    fn edge_cases_follow_uci() {
        let pkg = parse(b"config t 'n'\n\toption a 1; option b 2\n").unwrap();
        assert_eq!(render_show("p", &pkg), b"p.n=t\np.n.a='1'\n");
        assert!(parse(b"config t 'n'\n\toption a 'b';\n").is_err());
        assert!(parse(b"config t 'n'\n\x00").is_err());
        assert_eq!(get(&pkg, "n", "missing"), None);
        assert_eq!(get(&pkg, "@t[0]", "a"), Some(b"1".to_vec()));
    }

    fn dirs(tag: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let base = std::env::temp_dir().join(format!("zwrt-uci-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (conf, save) = (base.join("config"), base.join("save"));
        std::fs::create_dir_all(&conf).unwrap();
        std::fs::create_dir_all(&save).unwrap();
        (conf, save)
    }

    #[test]
    fn delta_nonempty_uses_uci_show_empty_or_missing_parses() {
        let (conf, save) = dirs("delta");
        std::fs::write(
            conf.join("wireless"),
            "config wifi-iface 'main_2g'\n\toption ssid 'A'\n",
        )
        .unwrap();
        // 没有 delta 文件：自己解析
        assert!(matches!(choose("wireless", &conf, &save), Source::Parse(_)));
        // 0 字节 delta（设备上 commit 之后常见）= 没有未保存改动：还是自己解析
        std::fs::write(save.join("wireless"), "").unwrap();
        assert!(matches!(choose("wireless", &conf, &save), Source::Parse(_)));
        // 非空 delta = 有未保存改动：退回 uci show
        std::fs::write(save.join("wireless"), "wireless.main_2g.ssid='B'\n").unwrap();
        assert_eq!(choose("wireless", &conf, &save), Source::Show);
        assert_eq!(load("wireless", &conf, &save), None);
        // 配置文件不存在：退回 uci show
        assert_eq!(choose("network", &conf, &save), Source::Show);
        // 解析错误：退回（load 返回 None）
        std::fs::write(conf.join("bad"), "config x 'y'\n\tbogus\n").unwrap();
        assert_eq!(load("bad", &conf, &save), None);
        let _ = std::fs::remove_dir_all(conf.parent().unwrap());
    }

    #[test]
    fn external_change_is_seen_on_next_read() {
        let (conf, save) = dirs("ext");
        let path = conf.join("zwrt_deviceui");
        std::fs::write(
            &path,
            "config device 'Device'\n\toption fan_switch_status '0'\n",
        )
        .unwrap();
        let read = || {
            get(
                &load("zwrt_deviceui", &conf, &save).unwrap(),
                "Device",
                "fan_switch_status",
            )
        };
        assert_eq!(read(), Some(b"0".to_vec()));
        // 别的程序 commit（改名替换文件）后，下一次读就看到新值
        let tmp = conf.join(".zwrt_deviceui.tmp");
        std::fs::write(
            &tmp,
            "config device 'Device'\n\toption fan_switch_status '1'\n",
        )
        .unwrap();
        std::fs::rename(&tmp, &path).unwrap();
        assert_eq!(read(), Some(b"1".to_vec()));
        let _ = std::fs::remove_dir_all(conf.parent().unwrap());
    }
}
