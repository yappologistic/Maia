//! Local time formatting via libc (strftime/localtime_r), with no timezone database in the binary.

use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn secs(t: SystemTime) -> i64 {
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => d.as_secs() as i64,
        Err(e) => -(e.duration().as_secs() as i64),
    }
}

pub fn local(t: SystemTime) -> libc::tm {
    let s = secs(t) as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&s, &mut tm);
    }
    tm
}

/// strftime with a Rust format string.
pub fn format(t: SystemTime, fmt: &str) -> String {
    let tm = local(t);
    let Ok(cfmt) = std::ffi::CString::new(fmt) else { return String::new() };
    let mut buf = vec![0u8; 256];
    let n = unsafe { libc::strftime(buf.as_mut_ptr() as *mut libc::c_char, buf.len(), cfmt.as_ptr(), &tm) };
    buf.truncate(n);
    String::from_utf8_lossy(&buf).into_owned()
}

/// Days since the epoch in local time (for "today / yesterday" buckets).
pub fn local_day(t: SystemTime) -> i64 {
    let tm = local(t);
    (secs(t) + tm.tm_gmtoff as i64).div_euclid(86_400)
}

/// Parse a local timestamp like "2026-10-13T23:05:00" (as written in .trashinfo files).
pub fn parse_local(s: &str) -> Option<SystemTime> {
    let n: Vec<i32> = s.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
    if n.len() < 6 {
        return None;
    }
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = n[0] - 1900;
    tm.tm_mon = n[1] - 1;
    tm.tm_mday = n[2];
    tm.tm_hour = n[3];
    tm.tm_min = n[4];
    tm.tm_sec = n[5];
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    if t < 0 {
        return None;
    }
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t as u64))
}

/// A date said against today: "Today 17:10", "Yesterday 09:04", the weekday within the
/// week ("Wed 22:57"), "12 Mar" earlier this year, "12 Mar 2025" before. Names of days
/// and months come from strftime, so they follow the locale like the rest.
pub fn relative(t: SystemTime, now: SystemTime) -> String {
    let (tm, nm) = (local(t), local(now));
    let days = local_day(now) - local_day(t);
    let clock = format!("{:02}:{:02}", tm.tm_hour, tm.tm_min);
    match days {
        0 => format!("Today {clock}"),
        1 => format!("Yesterday {clock}"),
        2..=6 => format!("{} {clock}", format(t, "%a")),
        _ if tm.tm_year == nm.tm_year => format!("{} {}", tm.tm_mday, format(t, "%b")),
        _ => format!("{} {} {}", tm.tm_mday, format(t, "%b"), tm.tm_year + 1900),
    }
}

/// A date in the user's chosen pattern, or said against today when none is chosen.
pub fn date(t: SystemTime, pattern: &str) -> String {
    if pattern.is_empty() { relative(t, SystemTime::now()) } else { format(t, pattern) }
}
