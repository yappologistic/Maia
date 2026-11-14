//! Helper programs Maia uses when they are there: which package holds each on the common
//! families of Linux, and the command that installs it here. A message that says what
//! needs what ("Sending folders needs bsdtar") gets a "How to install" on its toast.

use std::sync::Mutex;

/// The package managers Maia knows how to name.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Family {
    Arch,
    Debian,
    Fedora,
    Suse,
    Other,
}

/// Each tool and its package on Arch, Debian, Fedora and openSUSE; None where that
/// family has no package of that name to point at.
const TOOLS: &[(&str, [Option<&str>; 4])] = &[
    ("7z", [Some("7zip"), Some("7zip"), Some("7zip"), Some("7zip")]),
    ("b3sum", [Some("b3sum"), Some("b3sum"), Some("b3sum"), Some("b3sum")]),
    ("bsdcat", [Some("libarchive"), Some("libarchive-tools"), Some("bsdtar"), Some("bsdtar")]),
    ("bsdtar", [Some("libarchive"), Some("libarchive-tools"), Some("bsdtar"), Some("bsdtar")]),
    ("curl", [Some("curl"), Some("curl"), Some("curl"), Some("curl")]),
    ("exiftool", [Some("perl-image-exiftool"), Some("libimage-exiftool-perl"), Some("perl-Image-ExifTool"), Some("exiftool")]),
    ("ffmpeg", [Some("ffmpeg"), Some("ffmpeg"), Some("ffmpeg-free"), Some("ffmpeg")]),
    ("ffprobe", [Some("ffmpeg"), Some("ffmpeg"), Some("ffmpeg-free"), Some("ffmpeg")]),
    ("gdbus", [Some("glib2"), Some("libglib2.0-bin"), Some("glib2"), Some("glib2-tools")]),
    ("gvfs-mtp", [Some("gvfs-mtp"), Some("gvfs-backends"), Some("gvfs-mtp"), Some("gvfs-backends")]),
    ("heif-dec", [Some("libheif"), Some("libheif-examples"), Some("libheif-tools"), None]),
    ("ImageMagick", [Some("imagemagick"), Some("imagemagick"), Some("ImageMagick"), Some("ImageMagick")]),
    ("md5sum", [Some("coreutils"), Some("coreutils"), Some("coreutils"), Some("coreutils")]),
    ("pdfinfo", [Some("poppler"), Some("poppler-utils"), Some("poppler-utils"), Some("poppler-tools")]),
    ("pdftoppm", [Some("poppler"), Some("poppler-utils"), Some("poppler-utils"), Some("poppler-tools")]),
    ("pkexec", [Some("polkit"), Some("pkexec"), Some("polkit"), Some("polkit")]),
    ("sha1sum", [Some("coreutils"), Some("coreutils"), Some("coreutils"), Some("coreutils")]),
    ("sha256sum", [Some("coreutils"), Some("coreutils"), Some("coreutils"), Some("coreutils")]),
    ("sha512sum", [Some("coreutils"), Some("coreutils"), Some("coreutils"), Some("coreutils")]),
    ("sqlite3", [Some("sqlite"), Some("sqlite3"), Some("sqlite"), Some("sqlite3")]),
    ("udisksctl", [Some("udisks2"), Some("udisks2"), Some("udisks2"), Some("udisks2")]),
    ("xdg-email", [Some("xdg-utils"), Some("xdg-utils"), Some("xdg-utils"), Some("xdg-utils")]),
];

static FAMILY: Mutex<Option<Family>> = Mutex::new(None);

/// Test builds: the os-release text read in place of the real file, never this machine's.
#[cfg(test)]
static FAKE: Mutex<Option<String>> = Mutex::new(None);

/// Test builds: answer from this os-release text from now on.
#[cfg(test)]
pub fn fake(text: &str) {
    if let Ok(mut f) = FAKE.lock() {
        *f = Some(text.to_string());
    }
    if let Ok(mut f) = FAMILY.lock() {
        *f = None;
    }
}

/// Reads /etc/os-release on a thread of its own, once; until it has, `family` says None.
pub fn warm() {
    if family().is_some() {
        return;
    }
    std::thread::spawn(|| {
        #[cfg(test)]
        let text = FAKE.lock().ok().and_then(|f| f.clone()).unwrap_or_default();
        #[cfg(not(test))]
        let text = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
        if let Ok(mut f) = FAMILY.lock() {
            *f = Some(family_of(&text));
        }
    });
}

pub fn family() -> Option<Family> {
    FAMILY.lock().ok().and_then(|f| *f)
}

/// The family from os-release's ID, then its ID_LIKE.
pub fn family_of(os_release: &str) -> Family {
    let field = |key: &str| os_release.lines().find_map(|l| l.strip_prefix(key)?.strip_prefix('=')).map(|v| v.trim().trim_matches('"').to_ascii_lowercase()).unwrap_or_default();
    let (id, like) = (field("ID"), field("ID_LIKE"));
    for word in std::iter::once(id.as_str()).chain(like.split_whitespace()) {
        let f = match word {
            "arch" | "cachyos" | "manjaro" | "endeavouros" | "garuda" | "artix" => Family::Arch,
            "debian" | "ubuntu" | "linuxmint" | "pop" | "elementary" | "zorin" | "kali" | "raspbian" => Family::Debian,
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "nobara" => Family::Fedora,
            w if w == "suse" || w.starts_with("opensuse") || w == "sles" => Family::Suse,
            _ => continue,
        };
        return f;
    }
    Family::Other
}

/// The tool a message says something needs: the word after "needs" (or "need"), when it
/// is one Maia knows the package of.
pub fn tool_in(msg: &str) -> Option<&'static str> {
    let rest = msg.split_once(" needs ").or_else(|| msg.split_once(" need ")).map(|(_, r)| r)?;
    let word = rest.split_whitespace().next()?.trim_end_matches(|c: char| matches!(c, '.' | ',' | ';' | ')'));
    TOOLS.iter().find(|(t, _)| t.eq_ignore_ascii_case(word)).map(|(t, _)| *t)
}

/// The package holding `tool` in this family, if the table knows it.
pub fn package(tool: &str, f: Family) -> Option<&'static str> {
    let row = TOOLS.iter().find(|(t, _)| *t == tool)?.1;
    match f {
        Family::Arch => row[0],
        Family::Debian => row[1],
        Family::Fedora => row[2],
        Family::Suse => row[3],
        Family::Other => None,
    }
}

/// The command that installs `tool` here, or None when the system or its package is not
/// known (the popup then names the tool alone).
pub fn command(tool: &str, f: Family) -> Option<String> {
    let pkg = package(tool, f)?;
    Some(match f {
        Family::Arch => format!("sudo pacman -S {pkg}"),
        Family::Debian => format!("sudo apt install {pkg}"),
        Family::Fedora => format!("sudo dnf install {pkg}"),
        Family::Suse => format!("sudo zypper install {pkg}"),
        Family::Other => return None,
    })
}
