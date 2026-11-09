//! Doing what the user's own permissions don't allow, one command at a time through
//! polkit (`pkexec`, which asks for the password in the desktop's own dialog). The window
//! never runs as root; only the single coreutils command that was refused does.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;

use crate::drives::Tools;

#[derive(Clone, Debug, PartialEq)]
pub enum Task {
    Copy { srcs: Vec<PathBuf>, dest: PathBuf },
    Move { srcs: Vec<PathBuf>, dest: PathBuf },
    Delete { paths: Vec<PathBuf> },
    NewFolder(PathBuf),
    NewFile(PathBuf),
    Rename { from: PathBuf, to: PathBuf },
    /// A new owner and group as chown takes them ("alice:staff", ":staff"), then
    /// permissions as chmod does ("u+rwX,g-w"); all the way down when `deep`.
    Perms { paths: Vec<PathBuf>, owner: Option<String>, mode: Option<String>, deep: bool },
}

impl Task {
    /// The command run as root (programs by absolute path, as pkexec wants them).
    pub fn argv(&self) -> Vec<OsString> {
        let mut v: Vec<OsString> = vec![];
        let mut push = |s: &str| v.push(OsString::from(program(s)));
        match self {
            Task::Copy { .. } => push("cp"),
            Task::Move { .. } | Task::Rename { .. } => push("mv"),
            Task::Delete { .. } => push("rm"),
            Task::NewFolder(_) => push("mkdir"),
            Task::NewFile(_) => push("touch"),
            Task::Perms { owner, .. } => push(if owner.is_some() { "chown" } else { "chmod" }),
        }
        let flags: &[&str] = match self {
            Task::Copy { .. } => &["-a", "--"],
            Task::Delete { .. } => &["-rf", "--"],
            Task::Rename { .. } => &["-T", "--"],
            Task::Perms { deep: true, .. } => &["-R", "--"],
            _ => &["--"],
        };
        v.extend(flags.iter().map(OsString::from));
        match self {
            Task::Copy { srcs, dest } | Task::Move { srcs, dest } => {
                v.extend(srcs.iter().map(|s| s.clone().into_os_string()));
                v.push(dest.clone().into_os_string());
            }
            Task::Delete { paths } => v.extend(paths.iter().map(|s| s.clone().into_os_string())),
            Task::NewFolder(p) | Task::NewFile(p) => v.push(p.clone().into_os_string()),
            Task::Rename { from, to } => {
                v.push(from.clone().into_os_string());
                v.push(to.clone().into_os_string());
            }
            Task::Perms { paths, owner, mode, .. } => {
                v.extend(owner.as_ref().or(mode.as_ref()).map(OsString::from));
                v.extend(paths.iter().map(|s| s.clone().into_os_string()));
            }
        }
        v
    }

    pub fn label(&self) -> String {
        let n = |k: usize| format!("{k} item{}", if k == 1 { "" } else { "s" });
        match self {
            Task::Copy { srcs, .. } => format!("Copying {} as administrator", n(srcs.len())),
            Task::Move { srcs, .. } => format!("Moving {} as administrator", n(srcs.len())),
            Task::Delete { paths } => format!("Deleting {} as administrator", n(paths.len())),
            Task::NewFolder(_) => "Creating a folder as administrator".into(),
            Task::NewFile(_) => "Creating a file as administrator".into(),
            Task::Rename { .. } => "Renaming as administrator".into(),
            Task::Perms { paths, .. } => format!("Changing permissions of {} as administrator", n(paths.len())),
        }
    }
}

/// `/usr/bin/cp` and the like: the first match in the system program directories, never
/// the user's PATH — pkexec runs the program as root, and a `cp` sitting in a folder
/// named on PATH would be run with it.
pub fn program(name: &str) -> PathBuf {
    for d in ["/usr/bin", "/bin", "/usr/sbin", "/sbin"] {
        let p = std::path::Path::new(d).join(name);
        if p.is_file() {
            return p;
        }
    }
    PathBuf::from(name)
}

/// A refusal that administrator rights would get past (not a read-only disk).
pub fn denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied || e.raw_os_error() == Some(libc::EPERM)
}

/// Run the task through pkexec. Waits for the password dialog.
pub fn run(t: &Tools, task: &Task) -> Result<(), String> {
    run_argv(t, &task.argv())?;
    // A new owner first, then the permissions (chown may clear set-ID bits).
    if let Task::Perms { paths, owner: Some(_), mode: Some(m), deep } = task {
        run_argv(t, &Task::Perms { paths: paths.clone(), owner: None, mode: Some(m.clone()), deep: *deep }.argv())?;
    }
    Ok(())
}

/// One command as root through pkexec, its program by absolute path. Waits for the
/// password dialog; the error is the command's last line, or that access was refused.
pub fn run_argv(t: &Tools, argv: &[OsString]) -> Result<(), String> {
    let mut c = t.cmd("pkexec");
    c.args(argv).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    let out = c.output().map_err(|e| format!("pkexec: {e}"))?;
    match out.status.code() {
        Some(0) => Ok(()),
        // pkexec: 126 when the password dialog was dismissed or the request refused.
        Some(126) | Some(127) => Err("Administrator access was not given".into()),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr);
            Err(err.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("Failed").to_string())
        }
    }
}
