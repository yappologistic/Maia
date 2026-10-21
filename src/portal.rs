//! Maia as the desktop's Open and Save dialog: a backend for xdg-desktop-portal's
//! FileChooser. Apps ask the portal (org.freedesktop.portal.Desktop) for a file; the
//! portal hands the request to the backend its portals.conf names for
//! `org.freedesktop.impl.portal.FileChooser`, and replies to the app with what the
//! backend answered.
//!
//! `maia --portal` is that backend: started by the bus when the portal first needs it,
//! it holds one connection and answers each OpenFile, SaveFile and SaveFiles call by
//! starting `maia --pick`, a window of its own that reads the request on its standard
//! input and writes the answer on its standard output. One window per request keeps the
//! backend small (no display connection, nothing drawn) and lets a picker that crashes or
//! is closed by the app take nothing else with it. The wire format is fm1.rs's, with the
//! nested values (`a{sv}`, filters) this interface needs.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::fm1::{Bus, Msg, build, read};

pub const NAME: &str = "org.freedesktop.impl.portal.desktop.maia";
const PATH: &str = "/org/freedesktop/portal/desktop";
pub const IFACE: &str = "org.freedesktop.impl.portal.FileChooser";
const REQUEST: &str = "org.freedesktop.impl.portal.Request";

const INTROSPECT: &str = r#"<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
 <interface name="org.freedesktop.impl.portal.FileChooser">
  <method name="OpenFile"><arg type="o" name="handle" direction="in"/><arg type="s" name="app_id" direction="in"/><arg type="s" name="parent_window" direction="in"/><arg type="s" name="title" direction="in"/><arg type="a{sv}" name="options" direction="in"/><arg type="u" name="response" direction="out"/><arg type="a{sv}" name="results" direction="out"/></method>
  <method name="SaveFile"><arg type="o" name="handle" direction="in"/><arg type="s" name="app_id" direction="in"/><arg type="s" name="parent_window" direction="in"/><arg type="s" name="title" direction="in"/><arg type="a{sv}" name="options" direction="in"/><arg type="u" name="response" direction="out"/><arg type="a{sv}" name="results" direction="out"/></method>
  <method name="SaveFiles"><arg type="o" name="handle" direction="in"/><arg type="s" name="app_id" direction="in"/><arg type="s" name="parent_window" direction="in"/><arg type="s" name="title" direction="in"/><arg type="a{sv}" name="options" direction="in"/><arg type="u" name="response" direction="out"/><arg type="a{sv}" name="results" direction="out"/></method>
 </interface>
 <interface name="org.freedesktop.DBus.Introspectable"><method name="Introspect"><arg type="s" name="xml" direction="out"/></method></interface>
 <interface name="org.freedesktop.DBus.Peer"><method name="Ping"/></interface>
</node>"#;

// ------------------------------------------------------------------ values

/// A D-Bus value, as far as this interface needs them. Numbers other than `u`, `i` and
/// `b` are read past (an option nobody asked about may hold one) and kept as `Other`.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Bool(bool),
    U32(u32),
    Str(String),
    Bytes(Vec<u8>),
    List(Vec<Val>),
    /// A struct or a dict entry.
    Tuple(Vec<Val>),
    /// A variant: its signature and its value.
    Var(String, Box<Val>),
    Other,
}

/// The length of the first complete type in `sig`.
fn one_type(sig: &[u8]) -> Option<usize> {
    match *sig.first()? {
        b'a' => Some(1 + one_type(&sig[1..])?),
        open @ (b'(' | b'{') => {
            let close = if open == b'(' { b')' } else { b'}' };
            let mut at = 1;
            while *sig.get(at)? != close {
                at += one_type(&sig[at..])?;
            }
            Some(at + 1)
        }
        _ => Some(1),
    }
}

fn align_of(t: u8) -> usize {
    match t {
        b'y' | b'g' | b'v' => 1,
        b'n' | b'q' => 2,
        b'x' | b't' | b'd' | b'(' | b'{' => 8,
        _ => 4,
    }
}

struct Rd<'a> {
    b: &'a [u8],
    at: usize,
    depth: u32,
}

impl Rd<'_> {
    fn align(&mut self, n: usize) {
        self.at = self.at.div_ceil(n) * n;
    }
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.at..self.at.checked_add(n)?)?;
        self.at += n;
        Some(s)
    }
    fn u32(&mut self) -> Option<u32> {
        self.align(4);
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    /// One value of the single complete type `sig`.
    fn val(&mut self, sig: &[u8]) -> Option<Val> {
        // Nesting is bounded by the spec (32 + 32); a hostile message must not recurse forever.
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let v = self.val_inner(sig);
        self.depth -= 1;
        v
    }
    fn val_inner(&mut self, sig: &[u8]) -> Option<Val> {
        let t = *sig.first()?;
        Some(match t {
            b'b' => Val::Bool(self.u32()? != 0),
            b'u' | b'i' | b'h' => Val::U32(self.u32()?),
            b'y' => {
                self.take(1)?;
                Val::Other
            }
            b'n' | b'q' | b'x' | b't' | b'd' => {
                let n = align_of(t);
                self.align(n);
                self.take(n)?;
                Val::Other
            }
            b's' | b'o' => {
                let n = self.u32()? as usize;
                let s = String::from_utf8_lossy(self.take(n)?).into_owned();
                self.take(1)?;
                Val::Str(s)
            }
            b'g' => {
                let n = *self.take(1)?.first()? as usize;
                let s = String::from_utf8_lossy(self.take(n)?).into_owned();
                self.take(1)?;
                Val::Str(s)
            }
            b'v' => {
                let n = *self.take(1)?.first()? as usize;
                let inner = self.take(n)?.to_vec();
                self.take(1)?;
                if one_type(&inner) != Some(inner.len()) {
                    return None;
                }
                let v = self.val(&inner)?;
                Val::Var(String::from_utf8_lossy(&inner).into_owned(), Box::new(v))
            }
            b'a' => {
                let n = self.u32()? as usize;
                let el = &sig[1..1 + one_type(&sig[1..])?];
                self.align(align_of(*el.first()?));
                let end = self.at.checked_add(n)?;
                if end > self.b.len() {
                    return None;
                }
                if el == b"y" {
                    return Some(Val::Bytes(self.take(n)?.to_vec()));
                }
                let mut v = vec![];
                while self.at < end {
                    v.push(self.val(el)?);
                }
                Val::List(v)
            }
            b'(' | b'{' => {
                self.align(8);
                let mut v = vec![];
                let mut at = 1;
                while at < sig.len() - 1 {
                    let n = one_type(&sig[at..])?;
                    v.push(self.val(&sig[at..at + n])?);
                    at += n;
                }
                Val::Tuple(v)
            }
            _ => return None,
        })
    }
}

/// The values of a message body with signature `sig`.
pub fn parse(sig: &str, body: &[u8]) -> Option<Vec<Val>> {
    let s = sig.as_bytes();
    let mut r = Rd { b: body, at: 0, depth: 0 };
    let mut out = vec![];
    let mut at = 0;
    while at < s.len() {
        let n = one_type(&s[at..])?;
        out.push(r.val(&s[at..at + n])?);
        at += n;
    }
    Some(out)
}

fn pad(v: &mut Vec<u8>, to: usize) {
    while v.len() % to != 0 {
        v.push(0);
    }
}

/// Append `v` as the single complete type `sig`. The body starts 8-aligned in the
/// message, so offsets in `out` are the message's.
pub fn put(out: &mut Vec<u8>, sig: &str, v: &Val) {
    put_bytes(out, sig.as_bytes(), v);
}

fn put_bytes(out: &mut Vec<u8>, sig: &[u8], v: &Val) {
    let Some(&t) = sig.first() else { return };
    match (t, v) {
        (b'b', Val::Bool(b)) => {
            pad(out, 4);
            out.extend_from_slice(&(*b as u32).to_le_bytes());
        }
        (b'u' | b'i', Val::U32(n)) => {
            pad(out, 4);
            out.extend_from_slice(&n.to_le_bytes());
        }
        (b's' | b'o', Val::Str(s)) => crate::fm1::put_str(out, s),
        (b'g', Val::Str(s)) => {
            out.push(s.len() as u8);
            out.extend_from_slice(s.as_bytes());
            out.push(0);
        }
        (b'v', Val::Var(s, inner)) => {
            out.push(s.len() as u8);
            out.extend_from_slice(s.as_bytes());
            out.push(0);
            put_bytes(out, s.as_bytes(), inner);
        }
        (b'a', _) => {
            let Some(n) = one_type(&sig[1..]) else { return };
            let el = &sig[1..1 + n];
            pad(out, 4);
            let len_at = out.len();
            out.extend_from_slice(&[0; 4]);
            pad(out, align_of(el[0]));
            let start = out.len();
            match v {
                Val::Bytes(b) => out.extend_from_slice(b),
                Val::List(items) => items.iter().for_each(|i| put_bytes(out, el, i)),
                _ => {}
            }
            let len = (out.len() - start) as u32;
            out[len_at..len_at + 4].copy_from_slice(&len.to_le_bytes());
        }
        // A number Maia does not read is written back as zero (its size kept).
        (b'y' | b'n' | b'q' | b'x' | b't' | b'd', Val::Other) => {
            let n = align_of(t);
            pad(out, n);
            out.extend(std::iter::repeat_n(0, n));
        }
        (b'(' | b'{', Val::Tuple(items)) => {
            pad(out, 8);
            let mut at = 1;
            for i in items {
                let Some(n) = one_type(&sig[at..]) else { return };
                put_bytes(out, &sig[at..at + n], i);
                at += n;
            }
        }
        _ => {}
    }
}

/// An entry of an `a{sv}`.
fn entry(key: &str, sig: &str, v: Val) -> Val {
    Val::Tuple(vec![Val::Str(key.into()), Val::Var(sig.into(), Box::new(v))])
}

// ------------------------------------------------------------------ requests and answers

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub enum Mode {
    #[default]
    Open,
    Save,
    /// A folder to save several files in, under names the app gives.
    SaveFiles,
}

/// A filter the app offers: its name and its rules, glob patterns (0) or MIME types (1).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Filter {
    pub name: String,
    pub rules: Vec<(u32, String)>,
}

/// One of the app's own questions (an encoding, "open read-only"): its options, or none
/// for a yes or no.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub options: Vec<(String, String)>,
    pub chosen: String,
}

/// What an app asked for.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Pick {
    pub mode: Mode,
    pub title: String,
    pub app_id: String,
    /// `x11:<hex XID>`, `wayland:<handle>` or empty.
    pub parent: String,
    pub accept_label: String,
    pub multiple: bool,
    pub directory: bool,
    pub filters: Vec<Filter>,
    /// Index into `filters` of the one to start with.
    pub current_filter: Option<usize>,
    pub choices: Vec<Choice>,
    pub current_name: String,
    pub current_folder: Option<PathBuf>,
    pub current_file: Option<PathBuf>,
    /// SaveFiles: the names to save under.
    pub files: Vec<String>,
}

/// How a picker ended: 0 picked, 1 cancelled, 2 ended some other way.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Answer {
    pub response: u32,
    pub uris: Vec<String>,
    pub choices: Vec<(String, String)>,
    pub filter: Option<usize>,
}

impl Answer {
    pub fn cancelled() -> Answer {
        Answer { response: 1, ..Default::default() }
    }
    pub fn ended() -> Answer {
        Answer { response: 2, ..Default::default() }
    }
}

fn str_of(v: &Val) -> Option<&str> {
    match v {
        Val::Str(s) => Some(s),
        _ => None,
    }
}

fn filter_of(v: &Val) -> Option<Filter> {
    let Val::Tuple(t) = v else { return None };
    let (Some(Val::Str(name)), Some(Val::List(rules))) = (t.first(), t.get(1)) else { return None };
    let rules = rules
        .iter()
        .filter_map(|r| match r {
            Val::Tuple(p) => match (p.first(), p.get(1)) {
                (Some(Val::U32(k)), Some(Val::Str(s))) => Some((*k, s.clone())),
                _ => None,
            },
            _ => None,
        })
        .collect();
    Some(Filter { name: name.clone(), rules })
}

fn filter_val(f: &Filter) -> Val {
    Val::Tuple(vec![Val::Str(f.name.clone()), Val::List(f.rules.iter().map(|(k, s)| Val::Tuple(vec![Val::U32(*k), Val::Str(s.clone())])).collect())])
}

/// A path the portal hands over as a nul-terminated byte string.
fn path_of(v: &Val) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStrExt;
    let Val::Bytes(b) = v else { return None };
    let b = b.strip_suffix(&[0]).unwrap_or(b);
    (!b.is_empty()).then(|| PathBuf::from(std::ffi::OsStr::from_bytes(b)))
}

/// Mnemonics ("_Open") mean nothing here: the label as shown.
pub fn plain_label(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '_' {
            if it.peek() == Some(&'_') {
                out.push('_');
                it.next();
            }
            continue;
        }
        out.push(c);
    }
    out
}

impl Pick {
    /// The request in a call to the backend (`osssa{sv}`): its handle and what it asks.
    pub fn from_call(member: &str, signature: &str, body: &[u8]) -> Option<(String, Pick)> {
        let mode = match member {
            "OpenFile" => Mode::Open,
            "SaveFile" => Mode::Save,
            "SaveFiles" => Mode::SaveFiles,
            _ => return None,
        };
        if signature != "osssa{sv}" {
            return None;
        }
        let v = parse(signature, body)?;
        let mut p = Pick { mode, ..Default::default() };
        let handle = str_of(&v[0])?.to_string();
        p.app_id = str_of(&v[1])?.into();
        p.parent = str_of(&v[2])?.into();
        p.title = str_of(&v[3])?.into();
        let Val::List(opts) = &v[4] else { return None };
        let mut current = None;
        for o in opts {
            let Val::Tuple(kv) = o else { continue };
            let (Some(Val::Str(k)), Some(Val::Var(_, val))) = (kv.first(), kv.get(1)) else { continue };
            let val = val.as_ref();
            match (k.as_str(), val) {
                ("accept_label", Val::Str(s)) => p.accept_label = plain_label(s),
                ("multiple", Val::Bool(b)) => p.multiple = *b,
                ("directory", Val::Bool(b)) => p.directory = *b,
                ("current_name", Val::Str(s)) => p.current_name = s.clone(),
                ("current_folder", _) => p.current_folder = path_of(val),
                ("current_file", _) => p.current_file = path_of(val),
                ("filters", Val::List(fs)) => p.filters = fs.iter().filter_map(filter_of).collect(),
                ("current_filter", _) => current = filter_of(val),
                ("choices", Val::List(cs)) => {
                    for c in cs {
                        let Val::Tuple(t) = c else { continue };
                        let (Some(Val::Str(id)), Some(Val::Str(label)), Some(Val::List(opts)), Some(Val::Str(chosen))) = (t.first(), t.get(1), t.get(2), t.get(3)) else { continue };
                        let options = opts
                            .iter()
                            .filter_map(|o| match o {
                                Val::Tuple(p) => Some((str_of(p.first()?)?.to_string(), str_of(p.get(1)?)?.to_string())),
                                _ => None,
                            })
                            .collect();
                        p.choices.push(Choice { id: id.clone(), label: label.clone(), options, chosen: chosen.clone() });
                    }
                }
                ("files", Val::List(fs)) => p.files = fs.iter().filter_map(path_of).map(|f| f.to_string_lossy().into_owned()).collect(),
                _ => {}
            }
        }
        // The filter to start with: one of the list, or applied by itself when it is not.
        if let Some(c) = current {
            match p.filters.iter().position(|f| *f == c) {
                Some(i) => p.current_filter = Some(i),
                None => {
                    p.filters.push(c);
                    p.current_filter = Some(p.filters.len() - 1);
                }
            }
        }
        Some((handle, p))
    }
}

/// The reply body (`ua{sv}`) for `a`, its filter named again from `p`'s list.
pub fn reply_body(p: &Pick, a: &Answer) -> Vec<u8> {
    let mut results = vec![];
    if a.response == 0 {
        results.push(entry("uris", "as", Val::List(a.uris.iter().map(|u| Val::Str(u.clone())).collect())));
        if !a.choices.is_empty() {
            results.push(entry("choices", "a(ss)", Val::List(a.choices.iter().map(|(k, v)| Val::Tuple(vec![Val::Str(k.clone()), Val::Str(v.clone())])).collect())));
        }
        if let Some(f) = a.filter.and_then(|i| p.filters.get(i)) {
            results.push(entry("current_filter", "(sa(us))", filter_val(f)));
        }
    }
    let mut body = vec![];
    put(&mut body, "u", &Val::U32(a.response));
    put(&mut body, "a{sv}", &Val::List(results));
    body
}

// ------------------------------------------------------------------ between the backend and a picker

/// Records `kind\0field\0…`, each kind with a fixed number of fields. D-Bus strings and
/// paths hold no nul, so any name or path passes.
fn rec(out: &mut Vec<u8>, kind: &str, fields: &[&str]) {
    out.extend_from_slice(kind.as_bytes());
    out.push(0);
    for f in fields {
        out.extend_from_slice(f.as_bytes());
        out.push(0);
    }
}

fn fields(kind: &[u8]) -> usize {
    match kind {
        b"choice" => 4,
        b"option" | b"rule" | b"answer-choice" => 2,
        _ => 1,
    }
}

fn records(b: &[u8]) -> Vec<(String, Vec<String>)> {
    let mut it = b.split(|c| *c == 0);
    let mut out = vec![];
    while let Some(kind) = it.next() {
        if kind.is_empty() {
            continue;
        }
        let n = fields(kind);
        let f: Vec<String> = it.by_ref().take(n).map(|x| String::from_utf8_lossy(x).into_owned()).collect();
        if f.len() < n {
            break;
        }
        out.push((String::from_utf8_lossy(kind).into_owned(), f));
    }
    out
}

impl Pick {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![];
        let mode = match self.mode {
            Mode::Open => "open",
            Mode::Save => "save",
            Mode::SaveFiles => "save-files",
        };
        rec(&mut o, "mode", &[mode]);
        for (k, v) in [("title", &self.title), ("app", &self.app_id), ("parent", &self.parent), ("accept", &self.accept_label), ("name", &self.current_name)] {
            if !v.is_empty() {
                rec(&mut o, k, &[v]);
            }
        }
        if self.multiple {
            rec(&mut o, "multiple", &["1"]);
        }
        if self.directory {
            rec(&mut o, "directory", &["1"]);
        }
        for (k, v) in [("folder", &self.current_folder), ("file", &self.current_file)] {
            if let Some(p) = v {
                rec(&mut o, k, &[&p.to_string_lossy()]);
            }
        }
        for f in &self.filters {
            rec(&mut o, "filter", &[&f.name]);
            for (k, s) in &f.rules {
                rec(&mut o, "rule", &[&k.to_string(), s]);
            }
        }
        if let Some(i) = self.current_filter {
            rec(&mut o, "current", &[&i.to_string()]);
        }
        for c in &self.choices {
            rec(&mut o, "choice", &[&c.id, &c.label, &c.chosen, &c.options.len().to_string()]);
            for (k, l) in &c.options {
                rec(&mut o, "option", &[k, l]);
            }
        }
        for f in &self.files {
            rec(&mut o, "save", &[f]);
        }
        o
    }

    pub fn decode(b: &[u8]) -> Pick {
        let mut p = Pick::default();
        for (k, f) in records(b) {
            let v = f[0].clone();
            match k.as_str() {
                "mode" => p.mode = match v.as_str() {
                    "save" => Mode::Save,
                    "save-files" => Mode::SaveFiles,
                    _ => Mode::Open,
                },
                "title" => p.title = v,
                "app" => p.app_id = v,
                "parent" => p.parent = v,
                "accept" => p.accept_label = v,
                "name" => p.current_name = v,
                "multiple" => p.multiple = v == "1",
                "directory" => p.directory = v == "1",
                "folder" => p.current_folder = Some(PathBuf::from(v)),
                "file" => p.current_file = Some(PathBuf::from(v)),
                "filter" => p.filters.push(Filter { name: v, rules: vec![] }),
                "rule" => {
                    if let (Some(fl), Ok(kind)) = (p.filters.last_mut(), v.parse()) {
                        fl.rules.push((kind, f[1].clone()));
                    }
                }
                "current" => p.current_filter = v.parse().ok().filter(|i| *i < p.filters.len()),
                "choice" => p.choices.push(Choice { id: v, label: f[1].clone(), chosen: f[2].clone(), options: vec![] }),
                "option" => {
                    if let Some(c) = p.choices.last_mut() {
                        c.options.push((v, f[1].clone()));
                    }
                }
                "save" => p.files.push(v),
                _ => {}
            }
        }
        p
    }
}

impl Answer {
    pub fn encode(&self) -> Vec<u8> {
        let mut o = vec![];
        rec(&mut o, "response", &[&self.response.to_string()]);
        for u in &self.uris {
            rec(&mut o, "uri", &[u]);
        }
        for (k, v) in &self.choices {
            rec(&mut o, "answer-choice", &[k, v]);
        }
        if let Some(i) = self.filter {
            rec(&mut o, "filter", &[&i.to_string()]);
        }
        o
    }

    /// What a picker wrote; a picker that wrote nothing (it crashed, or was killed)
    /// ended some other way.
    pub fn decode(b: &[u8]) -> Answer {
        let mut a = Answer::ended();
        let mut said = false;
        for (k, f) in records(b) {
            match k.as_str() {
                "response" => {
                    a.response = f[0].parse().unwrap_or(2);
                    said = true;
                }
                "uri" => a.uris.push(f[0].clone()),
                "answer-choice" => a.choices.push((f[0].clone(), f[1].clone())),
                "filter" => a.filter = f[0].parse().ok(),
                _ => {}
            }
        }
        if !said {
            return Answer::ended();
        }
        a
    }
}

// ------------------------------------------------------------------ the backend

/// A request in progress, for Close to end.
#[derive(Default)]
pub struct Stop {
    stopped: AtomicBool,
    /// The picker's process, once started (0 before).
    pid: AtomicU32,
}

impl Stop {
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
    }
    /// End the request: its picker goes away and the app is told it ended (response 2).
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let pid = self.pid.load(Ordering::Relaxed);
        if pid != 0 {
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
        }
    }
}

/// What answers a request: a picker window in Maia, a stand-in in tests.
pub type Run = dyn Fn(&Pick, &Stop) -> Answer + Send + Sync;

struct Out {
    s: UnixStream,
    serial: u32,
}

impl Out {
    fn send(&mut self, kind: u8, fields: &[(u8, char, &str)], reply_to: Option<u32>, sig: &str, body: &[u8]) {
        self.serial = self.serial.wrapping_add(1).max(1);
        let m = build(kind, self.serial, fields, reply_to, sig, body);
        let _ = self.s.write_all(&m);
    }
    fn reply(&mut self, to: &Msg, sig: &str, body: &[u8]) {
        self.send(2, &[(6, 's', &to.sender)], Some(to.serial), sig, body);
    }
    fn error(&mut self, to: &Msg, name: &str, text: &str) {
        let mut body = vec![];
        crate::fm1::put_str(&mut body, text);
        self.send(3, &[(4, 's', name), (6, 's', &to.sender)], Some(to.serial), "s", &body);
    }
}

fn lock(o: &Mutex<Out>) -> std::sync::MutexGuard<'_, Out> {
    o.lock().unwrap_or_else(|e| e.into_inner())
}

/// Answer calls on `bus` until it closes. Each request runs on a thread of its own and
/// is answered when its picker ends; Close on a request's handle ends that picker.
pub fn serve(bus: Bus, run: Arc<Run>) {
    let (mut rd, serial, held) = bus.into_parts();
    let mut held = held.into_iter();
    let Ok(w) = rd.try_clone() else { return };
    let out = Arc::new(Mutex::new(Out { s: w, serial: serial.max(1 << 16) }));
    let open: Arc<Mutex<HashMap<String, Arc<Stop>>>> = Default::default();
    while let Some(m) = held.next().or_else(|| read(&mut rd)) {
        if m.kind != 1 {
            continue;
        }
        match (m.interface.as_str(), m.member.as_str()) {
            ("org.freedesktop.DBus.Introspectable", "Introspect") => {
                let mut body = vec![];
                crate::fm1::put_str(&mut body, INTROSPECT);
                lock(&out).reply(&m, "s", &body);
            }
            ("org.freedesktop.DBus.Peer", "Ping") => lock(&out).reply(&m, "", &[]),
            // The portal's proxy reads the interface's properties as it starts: there are none.
            ("org.freedesktop.DBus.Properties", "GetAll") => {
                let mut body = vec![];
                put(&mut body, "a{sv}", &Val::List(vec![]));
                lock(&out).reply(&m, "a{sv}", &body);
            }
            ("org.freedesktop.DBus.Properties", _) => lock(&out).error(&m, "org.freedesktop.DBus.Error.UnknownProperty", "No such property"),
            (REQUEST, "Close") => {
                if let Some(s) = open.lock().unwrap_or_else(|e| e.into_inner()).get(&m.path) {
                    s.stop();
                }
                lock(&out).reply(&m, "", &[]);
            }
            (IFACE | "", member) if m.path == PATH => match Pick::from_call(member, &m.signature, &m.body) {
                Some((handle, pick)) => {
                    let stop = Arc::new(Stop::default());
                    open.lock().unwrap_or_else(|e| e.into_inner()).insert(handle.clone(), stop.clone());
                    let (out, open, run) = (out.clone(), open.clone(), run.clone());
                    let spawned = std::thread::Builder::new().name("picker".into()).spawn(move || {
                        let a = if stop.stopped() { Answer::ended() } else { run(&pick, &stop) };
                        let a = if stop.stopped() { Answer::ended() } else { a };
                        open.lock().unwrap_or_else(|e| e.into_inner()).remove(&handle);
                        lock(&out).reply(&m, "ua{sv}", &reply_body(&pick, &a));
                    });
                    if spawned.is_err() {
                        crate::signals::log("picker: no thread for a request");
                    }
                }
                None => lock(&out).error(&m, "org.freedesktop.DBus.Error.InvalidArgs", "Not a FileChooser request"),
            },
            _ => lock(&out).error(&m, "org.freedesktop.DBus.Error.UnknownMethod", "No such method"),
        }
    }
}

/// A picker window for `p`: `maia --pick`, the request on its input, the answer on its
/// output. It goes with the backend (a parent-death signal) and with Close.
pub fn run_window(p: &Pick, stop: &Stop) -> Answer {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    let Some(exe) = std::env::current_exe().ok().and_then(|e| picker_exe(&e)) else { return Answer::ended() };
    let mut cmd = Command::new(exe);
    cmd.arg("--pick").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::inherit());
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let Ok(mut child) = cmd.spawn() else { return Answer::ended() };
    stop.pid.store(child.id(), Ordering::Relaxed);
    if stop.stopped() {
        let _ = child.kill();
    }
    if let Some(mut i) = child.stdin.take() {
        let _ = i.write_all(&p.encode());
    }
    let mut said = vec![];
    if let Some(mut o) = child.stdout.take() {
        let _ = o.read_to_end(&mut said);
    }
    let _ = child.wait();
    stop.pid.store(0, Ordering::Relaxed);
    Answer::decode(&said)
}

/// The program to open a picker with: this one, or the one at its path since an update
/// replaced it (the kernel names a replaced program `<path> (deleted)`). The backend
/// waits for days; an update meanwhile must not leave every app without a dialog.
pub fn picker_exe(exe: &Path) -> Option<PathBuf> {
    if exe.is_file() {
        return Some(exe.to_path_buf());
    }
    let path = PathBuf::from(exe.to_str()?.strip_suffix(" (deleted)")?);
    path.is_file().then_some(path)
}

/// `maia --portal`: take the backend's name on the session bus and answer it until the
/// bus goes away. False when there is no bus or the name is someone else's.
pub fn run_backend() -> bool {
    let Some(mut bus) = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().and_then(|a| Bus::connect(&a)) else { return false };
    if !bus.own(NAME) {
        return false;
    }
    serve(bus, Arc::new(run_window));
    true
}

// ------------------------------------------------------------------ turning it on

/// The files turning it on writes, and what it reads to decide how: the user's own
/// folders, or temporary ones in tests.
pub struct Places {
    /// XDG_DATA_HOME: the portal file and the D-Bus service file.
    pub data: PathBuf,
    /// XDG_CONFIG_HOME: portals.conf.
    pub config: PathBuf,
    /// Where the previous routing is remembered.
    pub state: PathBuf,
    /// XDG_CURRENT_DESKTOP's names, lower case, most specific first.
    pub desktops: Vec<String>,
    /// Folders below the user's holding a portals.conf, highest precedence first.
    pub system: Vec<PathBuf>,
    /// The portal reads every portals.conf it finds, each line falling back to the
    /// next file (1.21.1 on); before, the first file found was the only one read.
    pub merges: bool,
}

impl Places {
    pub fn user() -> Places {
        let home = crate::fsx::home();
        let env_dirs = |var: &str, default: &str| -> Vec<PathBuf> {
            std::env::var(var).ok().filter(|v| !v.is_empty()).unwrap_or_else(|| default.into()).split(':').filter(|d| !d.is_empty()).map(PathBuf::from).collect()
        };
        let mut system: Vec<PathBuf> = env_dirs("XDG_CONFIG_DIRS", "/etc/xdg").into_iter().map(|d| d.join("xdg-desktop-portal")).collect();
        system.push("/etc/xdg-desktop-portal".into());
        system.push(dirs::data_dir().unwrap_or_else(|| home.join(".local/share")).join("xdg-desktop-portal"));
        system.extend(env_dirs("XDG_DATA_DIRS", "/usr/local/share:/usr/share").into_iter().map(|d| d.join("xdg-desktop-portal")));
        Places {
            data: dirs::data_dir().unwrap_or_else(|| home.join(".local/share")),
            config: dirs::config_dir().unwrap_or_else(|| home.join(".config")),
            state: crate::config::data_dir().join("previous-file-chooser.json"),
            desktops: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default().split(':').filter(|d| !d.is_empty()).map(|d| d.to_ascii_lowercase()).collect(),
            system,
            merges: false,
        }
    }

    pub fn portal_file(&self) -> PathBuf {
        self.data.join("xdg-desktop-portal/portals/maia.portal")
    }

    pub fn service_file(&self) -> PathBuf {
        self.data.join(format!("dbus-1/services/{NAME}.service"))
    }

    /// The portals.conf of `dir` that is read for this desktop, if there is one.
    fn conf_in(&self, dir: &Path) -> Option<PathBuf> {
        self.desktops.iter().map(|d| dir.join(format!("{d}-portals.conf"))).chain([dir.join("portals.conf")]).find(|p| p.is_file())
    }

    /// The user's portals.conf to put the line in: the one read now, or a new one for
    /// this desktop.
    fn user_conf(&self) -> PathBuf {
        let dir = self.config.join("xdg-desktop-portal");
        self.conf_in(&dir).unwrap_or_else(|| dir.join(self.desktops.first().map_or("portals.conf".to_string(), |d| format!("{d}-portals.conf"))))
    }
}

pub const PORTAL_FILE: &str = "[portal]\nDBusName=org.freedesktop.impl.portal.desktop.maia\nInterfaces=org.freedesktop.impl.portal.FileChooser;\n";

pub fn service_text(exe: &Path) -> String {
    format!("[D-BUS Service]\nName={NAME}\nExec={} --portal\n", exe.display())
}

/// `key` in the `[preferred]` group of a portals.conf text set to `value` (or taken out
/// for None), every other line kept as it was. Returns the new text and the value the
/// key had.
pub fn conf_set(text: &str, key: &str, value: Option<&str>) -> (String, Option<String>) {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    let is_key = |l: &str| l.split_once('=').is_some_and(|(k, _)| k.trim() == key);
    let mut group = false;
    let mut start = None;
    let mut last = None;
    let mut found = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            group = t == "[preferred]";
            if group && start.is_none() {
                start = Some(i);
                last = Some(i);
            }
            continue;
        }
        if group && start.is_some() {
            if !t.is_empty() && !t.starts_with('#') {
                last = Some(i);
            }
            if found.is_none() && is_key(t) {
                found = Some(i);
            }
        }
    }
    let prev = found.and_then(|i| lines[i].split_once('=').map(|(_, v)| v.trim().to_string()));
    match (found, value) {
        (Some(i), Some(v)) => lines[i] = format!("{key}={v}"),
        (Some(i), None) => {
            lines.remove(i);
        }
        (None, Some(v)) => match last {
            Some(i) => lines.insert(i + 1, format!("{key}={v}")),
            None => {
                if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                    lines.push(String::new());
                }
                lines.push("[preferred]".into());
                lines.push(format!("{key}={v}"));
            }
        },
        (None, None) => {}
    }
    let mut out = lines.join("\n");
    out.push('\n');
    (out, prev)
}

/// What turning it on found, to put back on turning it off.
#[derive(serde::Serialize, serde::Deserialize, Default, Debug, PartialEq)]
struct Before {
    /// The portals.conf the line went into.
    file: PathBuf,
    /// Its text when Maia made it, when it did (None: it was there before).
    made: Option<String>,
    /// The FileChooser line's value before (None: there was none).
    value: Option<String>,
}

/// Write the portal file, the service file and the portals.conf line. Only files: the
/// bus and the portal are told by `set`.
pub fn turn_on(pl: &Places, exe: &Path) -> std::io::Result<()> {
    let write = |p: &Path, text: &str| -> std::io::Result<()> {
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d)?;
        }
        if std::fs::read_to_string(p).ok().as_deref() != Some(text) {
            std::fs::write(p, text)?;
        }
        Ok(())
    };
    write(&pl.portal_file(), PORTAL_FILE)?;
    write(&pl.service_file(), &service_text(exe))?;
    let before: Option<Before> = std::fs::read(&pl.state).ok().and_then(|b| serde_json::from_slice(&b).ok());
    let file = before.as_ref().map(|b| b.file.clone()).filter(|f| f.is_file()).unwrap_or_else(|| pl.user_conf());
    let had = std::fs::read_to_string(&file).ok();
    // A new file holds just this line where the portal falls back to the desktop's own
    // file for the rest. Where it reads only the first file found, it starts as a copy
    // of the one read now: the line alone would drop the desktop's choices for screen
    // sharing and screenshots.
    let made = match &had {
        Some(_) => None,
        None if pl.merges => Some(String::new()),
        None => Some(pl.system.iter().find_map(|d| pl.conf_in(d)).and_then(|p| std::fs::read_to_string(p).ok()).unwrap_or_default()),
    };
    let text = had.clone().or(made.clone()).unwrap_or_default();
    let (new, value) = conf_set(&text, IFACE, Some("maia"));
    if before.is_none() || before.as_ref().is_some_and(|b| b.file != file) {
        let b = Before { file: file.clone(), made, value: value.filter(|v| v != "maia") };
        if let Some(d) = pl.state.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::write(&pl.state, serde_json::to_vec(&b).unwrap_or_default())?;
    }
    write(&file, &new)
}

/// Put the routing back as it was and take Maia's files away.
pub fn turn_off(pl: &Places) -> std::io::Result<()> {
    if let Some(b) = std::fs::read(&pl.state).ok().and_then(|b| serde_json::from_slice::<Before>(&b).ok()) {
        if let Ok(text) = std::fs::read_to_string(&b.file) {
            let (back, _) = conf_set(&text, IFACE, b.value.as_deref());
            // A file Maia made and nobody has changed since: gone again.
            let unchanged = b.made.as_ref().is_some_and(|m| conf_set(m, IFACE, Some("maia")).0 == text);
            if unchanged {
                std::fs::remove_file(&b.file)?;
            } else {
                std::fs::write(&b.file, back)?;
            }
        }
        let _ = std::fs::remove_file(&pl.state);
    }
    for p in [pl.portal_file(), pl.service_file()] {
        if std::fs::read_to_string(&p).is_ok_and(|t| t.contains("maia")) {
            std::fs::remove_file(&p)?;
        }
    }
    Ok(())
}

/// Whether the portal is routed to Maia: the line is in the portals.conf it reads.
pub fn is_on(pl: &Places) -> bool {
    let file = std::fs::read(&pl.state).ok().and_then(|b| serde_json::from_slice::<Before>(&b).ok()).map(|b| b.file).unwrap_or_else(|| pl.user_conf());
    let Ok(text) = std::fs::read_to_string(file) else { return false };
    let (_, v) = conf_set(&text, IFACE, None);
    v.is_some_and(|v| v.split(';').next().map(str::trim) == Some("maia"))
}

/// An xdg-desktop-portal version ("xdg-desktop-portal 1.18.4") as numbers.
pub fn version_of(text: &str) -> Option<(u32, u32, u32)> {
    let v = text.split_whitespace().last()?;
    let mut it = v.split('.').map(|n| n.parse::<u32>().ok());
    Some((it.next()??, it.next().flatten().unwrap_or(0), it.next().flatten().unwrap_or(0)))
}

/// What stands in the way, said in place of the Options row's line: no portal at all,
/// or one older than 1.20.1, which reads backends only from /usr/share.
pub fn missing(version: Option<(u32, u32, u32)>, system_file: bool) -> Option<&'static str> {
    match version {
        None => Some("Needs xdg-desktop-portal, which isn't installed."),
        Some(v) if v < (1, 20, 1) && !system_file => Some("This xdg-desktop-portal only reads /usr/share: install Maia's package."),
        _ => None,
    }
}

/// The installed portal's version, asked of the program itself.
fn portal_version() -> Option<(u32, u32, u32)> {
    let tools = crate::drives::Tools::current()?;
    let exe = if cfg!(test) { tools.path_of("xdg-desktop-portal") } else { ["/usr/libexec/xdg-desktop-portal", "/usr/lib/xdg-desktop-portal"].iter().map(PathBuf::from).find(|p| p.is_file())? };
    let out = std::process::Command::new(exe).arg("--version").stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().ok()?;
    version_of(&String::from_utf8_lossy(&out.stdout))
}

/// Where a package puts the portal file, read by every version.
const SYSTEM_PORTAL: &str = "/usr/share/xdg-desktop-portal/portals/maia.portal";

/// For the Options row: on or off, and what is missing when it can't work here.
pub fn state() -> (bool, Option<&'static str>) {
    (is_on(&Places::user()), missing(portal_version(), Path::new(SYSTEM_PORTAL).is_file()))
}

/// Turn it on or off for this user, then have the bus and the portal read their files
/// again. Returns what to say: done, or what is still in the way.
pub fn set(on: bool) -> Result<String, String> {
    let mut pl = Places::user();
    let version = portal_version();
    if on {
        pl.merges = version.is_some_and(|v| v >= (1, 21, 1));
        turn_on(&pl, &crate::fm1::exe()).map_err(|e| e.to_string())?;
    } else {
        turn_off(&pl).map_err(|e| e.to_string())?;
    }
    let restarted = reload();
    if let Some(m) = missing(version, Path::new(SYSTEM_PORTAL).is_file()).filter(|_| on) {
        return Ok(m.into());
    }
    Ok(match (on, restarted) {
        (true, true) => "Apps that use the portal now open and save files with Maia".into(),
        (false, true) => "Apps open and save files with the desktop's dialog again".into(),
        (true, false) => "Apps open and save files with Maia once xdg-desktop-portal restarts (or after logging in again)".into(),
        (false, false) => "Apps go back to the desktop's dialog once xdg-desktop-portal restarts (or after logging in again)".into(),
    })
}

/// The bus reads its service folders again (dbus-broker only does when asked).
fn reload_bus() -> Option<Bus> {
    let mut b = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok().and_then(|a| Bus::connect(&a))?;
    b.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "ReloadConfig", "", &[]);
    Some(b)
}

/// The bus reads its service folders again, and a running portal starts over to read
/// portals.conf (it reads it once, at start). False when one is running that Maia could
/// not restart (started by the compositor rather than by systemd).
fn reload() -> bool {
    #[cfg(not(test))]
    {
        let Some(mut b) = reload_bus() else { return true };
        let mut name = vec![];
        crate::fm1::put_str(&mut name, "org.freedesktop.portal.Desktop");
        if b.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", "s", &name).is_none() {
            // None running: the next app to ask starts one that reads the new files.
            return true;
        }
        let Some(t) = crate::drives::Tools::current().filter(|t| t.has("systemctl")) else { return false };
        let ctl = |args: &[&str]| t.cmd("systemctl").arg("--user").args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        ctl(&["--quiet", "is-active", "xdg-desktop-portal.service"]) && ctl(&["restart", "xdg-desktop-portal.service"])
    }
    #[cfg(test)]
    true
}

/// On starting: a service file that names another copy of Maia (moved, installed since)
/// names this one again.
pub fn refresh() {
    let pl = Places::user();
    let p = pl.service_file();
    if std::fs::read_to_string(&p).is_ok_and(|t| t.contains("maia")) {
        let want = service_text(&crate::fm1::exe());
        if std::fs::read_to_string(&p).ok().as_deref() != Some(want.as_str()) {
            let _ = std::fs::write(&p, want);
            let _ = reload_bus();
        }
    }
}
