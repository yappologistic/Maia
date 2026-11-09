//! What a word in the terminal points at: a web address, or a path as `ls`, `grep -n`
//! and compilers print it (`src/main.rs:12:5`). Looked for only in the line under the
//! pointer, and a path only counts once a worker has found it on disk.

use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    Url(String),
    /// A path as written (relative ones are to the shell's folder), and the line and
    /// column it was followed by.
    Path { text: String, line: Option<u32>, col: Option<u32> },
}

/// Characters that end a path or address written in running text.
fn stop(c: char) -> bool {
    c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | '|' | '\0')
}

/// The link in `line` at character `i`: the characters it spans (`a..b`) and its target.
pub fn at(line: &str, i: usize) -> Option<(usize, usize, Target)> {
    let ch: Vec<char> = line.chars().collect();
    if i >= ch.len() || stop(ch[i]) {
        return None;
    }
    let (mut a, mut b) = (i, i + 1);
    while a > 0 && !stop(ch[a - 1]) {
        a -= 1;
    }
    while b < ch.len() && !stop(ch[b]) {
        b += 1;
    }
    let word: String = ch[a..b].iter().collect();
    // An address inside the word: from its scheme on.
    for scheme in ["https://", "http://", "file://"] {
        if let Some(k) = word.find(scheme) {
            let s = a + word[..k].chars().count();
            let e = trim_end(&ch, s + scheme.len(), b);
            if (s..e).contains(&i) && e > s + scheme.len() {
                let url: String = ch[s..e].iter().collect();
                if scheme == "file://" {
                    return Some((s, e, Target::Path { text: file_uri(&url)?, line: None, col: None }));
                }
                return Some((s, e, Target::Url(url)));
            }
            return None;
        }
    }
    // A path: brackets and the punctuation that closes a sentence aren't part of it.
    while a < b && matches!(ch[a], '(' | '[' | '{' | ',' | ';' | '=') {
        a += 1;
    }
    // `--out=dir/file`: the part after the last `=`.
    if let Some(k) = ch[a..b].iter().rposition(|&c| c == '=') {
        a += k + 1;
    }
    let b = numbered(&ch, a, trim_end(&ch, a, b));
    if a >= b || !(a..b).contains(&i) {
        return None;
    }
    let word: String = ch[a..b].iter().collect();
    let (text, line, col) = split_line(&word);
    if text.is_empty() || text.chars().all(|c| c == '.' || c == ':') {
        return None;
    }
    Some((a, b, Target::Path { text: text.to_string(), line, col }))
}

/// The path of a `file://host/path` address (as OSC 7 and `ls --hyperlink` write them).
/// Any host is taken: a remote shell's working directory is meaningful in its own
/// terminal; the path is resolved on disk before anything opens it.
pub fn file_uri(u: &str) -> Option<String> {
    let rest = u.strip_prefix("file://")?;
    Some(crate::vt::percent_decode(&rest[rest.find('/')?..]))
}

/// The end of a link at `b`, less what ends the sentence around it: a full stop, comma
/// or colon after it, and a closing bracket it didn't open.
fn trim_end(ch: &[char], a: usize, mut b: usize) -> usize {
    loop {
        match ch.get(b.wrapping_sub(1)).copied() {
            Some('.' | ',' | ';' | ':' | '!' | '?') if b > a => b -= 1,
            Some(c @ (')' | ']' | '}')) if b > a => {
                let open = match c {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                let (o, cl) = ch[a..b].iter().fold((0, 0), |(o, cl), &x| (o + (x == open) as i32, cl + (x == c) as i32));
                if cl > o {
                    b -= 1;
                } else {
                    return b;
                }
            }
            _ => return b,
        }
    }
}

/// The end of a path at `b`, or the end of its line and column numbers when it has them
/// and text runs on after another colon, as `grep -n` writes `notes.txt:7:the match`.
fn numbered(ch: &[char], a: usize, b: usize) -> usize {
    let digits = |from: usize| ch[from.min(b)..b].iter().take_while(|c| c.is_ascii_digit()).count();
    let ends = |e: usize| e == b || ch[e] == ':';
    for k in a + 1..b {
        let d = digits(k + 1);
        if ch[k] != ':' || d == 0 || !ends(k + 1 + d) {
            continue;
        }
        let e = k + 1 + d;
        let d2 = if e < b { digits(e + 1) } else { 0 };
        return if d2 > 0 && ends(e + 1 + d2) { e + 1 + d2 } else { e };
    }
    b
}

/// `path:line:col`, `path:line` or `path` (a compiler's message, grep -n's output).
pub fn split_line(w: &str) -> (&str, Option<u32>, Option<u32>) {
    let num = |s: &str| (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse::<u32>().ok()).flatten();
    let mut parts = w.rsplitn(3, ':');
    let (last, mid, rest) = (parts.next(), parts.next(), parts.next());
    match (rest, mid.and_then(num), last.and_then(num)) {
        (Some(p), Some(l), Some(c)) if !p.is_empty() => (p, Some(l), Some(c)),
        _ => match w.rsplit_once(':') {
            Some((p, l)) if !p.is_empty() && num(l).is_some() => (p, num(l), None),
            _ => (w, None, None),
        },
    }
}

/// The path `text` names with `~` for the home folder and relative ones in `base`, written
/// without `.` and `..`, and whether it is a folder; None when nothing is there. Touches
/// the disk: a worker's job.
pub fn resolve(text: &str, base: Option<&Path>) -> Option<(PathBuf, bool)> {
    let p = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => crate::fsx::home().join(rest.trim_start_matches('/')),
        _ => PathBuf::from(text),
    };
    let p = if p.is_absolute() { p } else { base?.join(p) };
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            c => out.push(c),
        }
    }
    let m = std::fs::metadata(&out).ok()?;
    Some((out, m.is_dir()))
}
