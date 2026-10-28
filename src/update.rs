//! Updates from the project's releases on GitHub, only when asked. "Check for updates"
//! looks at the latest release; "Update" downloads its build, checks it against the
//! release's checksum and puts it in place of the installed one, which the next start
//! runs. The program alone and an AppImage are replaced by renaming over them; a .deb,
//! .rpm or pacman install gets the release's package, handed to dpkg, rpm or pacman
//! through pkexec, so the package manager keeps knowing what is installed. Nothing is
//! sent but the requests themselves; curl does them, never with the user's own settings
//! or credentials.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Where the releases are.
pub const REPO: &str = "yappologistic/Maia";
/// A release's files for this machine's processor: the program, and its SHA-256 as
/// `sha256sum` writes it ("maia-linux-x86_64", "maia-linux-aarch64").
#[cfg(target_arch = "aarch64")]
pub const ASSET: &str = "maia-linux-aarch64";
#[cfg(not(target_arch = "aarch64"))]
pub const ASSET: &str = "maia-linux-x86_64";
#[cfg(target_arch = "aarch64")]
pub const SUMS: &str = "maia-linux-aarch64.sha256";
#[cfg(not(target_arch = "aarch64"))]
pub const SUMS: &str = "maia-linux-x86_64.sha256";
/// This machine's processor as the packages name it.
#[cfg(target_arch = "aarch64")]
const ARCH: &str = "aarch64";
#[cfg(not(target_arch = "aarch64"))]
const ARCH: &str = "x86_64";
#[cfg(target_arch = "aarch64")]
const DEB_ARCH: &str = "arm64";
#[cfg(not(target_arch = "aarch64"))]
const DEB_ARCH: &str = "amd64";
/// Every package's checksum, one `sha256sum` line each.
const ALL_SUMS: &str = "SHA256SUMS";
/// A build is a few megabytes; anything near this is not one.
const MAX_BIN: u64 = 64 << 20;

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    /// What's new, as the release says it (first lines only are shown).
    pub notes: String,
    pub bin: String,
    pub sum: String,
    /// Every download of the project's in the release, by name.
    pub assets: Vec<(String, String)>,
}

impl Release {
    fn url(&self, name: &str) -> Option<&str> {
        self.assets.iter().find(|(n, _)| n == name).map(|(_, u)| u.as_str())
    }
}

/// How this copy of Maia was installed, which says how to replace it.
#[derive(Clone, Debug, PartialEq)]
pub enum Installed {
    /// The program alone, in a folder this user can write.
    Own,
    /// An AppImage, in a folder this user can write: the file the system runs, not the
    /// program inside it, which is on a read-only mount.
    AppImage(PathBuf),
    /// By dpkg, rpm or pacman, from the release's package.
    Deb,
    Rpm,
    Pacman,
    /// Somewhere this user can't write that no package Maia makes put it.
    Elsewhere(PathBuf),
}

impl Installed {
    /// The release's file for this kind of install ("maia_0.4.0_amd64.deb").
    pub fn package(&self, version: &str) -> Option<String> {
        let v = version.trim_start_matches(['v', 'V']);
        match self {
            Installed::AppImage(_) => Some(format!("Maia-{v}-{ARCH}.AppImage")),
            Installed::Deb => Some(format!("maia_{v}_{DEB_ARCH}.deb")),
            Installed::Rpm => Some(format!("maia-{v}-1.{ARCH}.rpm")),
            Installed::Pacman => Some(format!("maia-{v}-1-{ARCH}.pkg.tar.zst")),
            Installed::Own | Installed::Elsewhere(_) => None,
        }
    }
}

/// Whether a file can be made in `dir` (tried, since permissions alone don't say: a
/// read-only mount, say).
fn writable(dir: &Path) -> bool {
    let probe = dir.join(format!(".maia-write-check-{}", std::process::id()));
    let ok = std::fs::write(&probe, b"").is_ok();
    let _ = std::fs::remove_file(&probe);
    ok
}

/// The AppImage this process runs from: the runtime says which file in `APPIMAGE` and
/// where it is mounted in `APPDIR`, and the program has to be inside that mount (a
/// program started from an AppImage inherits both).
fn running_appimage(exe: &Path) -> Option<PathBuf> {
    let dir = std::env::var_os("APPDIR")?;
    let file = PathBuf::from(std::env::var_os("APPIMAGE")?);
    (exe.starts_with(&dir) && file.is_absolute()).then_some(file)
}

/// How `exe` was installed. Asks dpkg, rpm and pacman, so it runs on a thread: a package's
/// program is one its manager lists as owned by the package "maia".
pub fn installed(exe: &Path, appimage: Option<&Path>, tools: Option<&crate::drives::Tools>) -> Installed {
    if let Some(a) = appimage {
        let dir = a.parent().map(Path::to_path_buf).unwrap_or_default();
        return if a.is_file() && writable(&dir) { Installed::AppImage(a.to_path_buf()) } else { Installed::Elsewhere(dir) };
    }
    let dir = exe.parent().map(Path::to_path_buf).unwrap_or_default();
    if writable(&dir) {
        return Installed::Own;
    }
    if let Some(t) = tools {
        let ask = |name: &str, args: &[&str]| {
            if !t.has(name) {
                return String::new();
            }
            let mut c = crate::media::tool_cmd(t.cmd(name));
            c.args(args).arg(exe);
            crate::media::output(c, Duration::from_secs(10)).map(|o| String::from_utf8_lossy(&o).trim().to_string()).unwrap_or_default()
        };
        // "maia: /usr/bin/maia"
        if ask("dpkg-query", &["-S"]).split(':').next() == Some("maia") {
            return Installed::Deb;
        }
        if ask("rpm", &["-qf", "--queryformat", "%{NAME}"]) == "maia" {
            return Installed::Rpm;
        }
        if ask("pacman", &["-Qoq"]) == "maia" {
            return Installed::Pacman;
        }
    }
    Installed::Elsewhere(dir)
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Idle,
    Checking,
    /// This is the newest.
    Latest,
    Available(Release),
    Downloading(String),
    /// Handed to the package manager, which asks for the password.
    Installing(String),
    /// Installed; the next start runs it.
    Ready(String),
    Failed(String),
    /// The program's folder can't be written, and no package of Maia's put it there.
    CantWrite(PathBuf),
}

/// "v1.2.3", "1.2", "1.2.3-beta" as (major, minor, patch); None for anything else.
pub fn version(v: &str) -> Option<(u64, u64, u64)> {
    let core = v.trim().trim_start_matches(['v', 'V']).split(['-', '+']).next()?;
    let mut n = core.split('.').map(|p| p.parse::<u64>());
    let major = n.next()?.ok()?;
    let minor = match n.next() {
        Some(p) => p.ok()?,
        None => 0,
    };
    let patch = match n.next() {
        Some(p) => p.ok()?,
        None => 0,
    };
    n.next().is_none().then_some((major, minor, patch))
}

/// Whether `candidate` is a later version than `current`.
pub fn newer(current: &str, candidate: &str) -> bool {
    matches!((version(current), version(candidate)), (Some(a), Some(b)) if b > a)
}

/// The release in GitHub's answer for the latest one, if it carries both files and
/// they are this project's downloads.
pub fn parse_release(body: &[u8]) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let tag = v["tag_name"].as_str()?.trim().to_string();
    version(&tag)?;
    let prefix = format!("https://github.com/{REPO}/releases/download/");
    let url = |name: &str| {
        v["assets"].as_array()?.iter().find(|a| a["name"].as_str() == Some(name)).and_then(|a| a["browser_download_url"].as_str()).filter(|u| u.starts_with(&prefix)).map(str::to_string)
    };
    let notes = v["body"].as_str().unwrap_or("").trim().to_string();
    let assets = v["assets"].as_array()?.iter().filter_map(|a| Some((a["name"].as_str()?.to_string(), a["browser_download_url"].as_str().filter(|u| u.starts_with(&prefix))?.to_string()))).collect();
    Some(Release { version: tag, notes, bin: url(ASSET)?, sum: url(SUMS)?, assets })
}

/// The checksum `SHA256SUMS` gives the file `name`, lowercase.
pub fn sum_for(text: &str, name: &str) -> Option<String> {
    text.lines().find(|l| l.split_whitespace().nth(1).map(|n| n.trim_start_matches('*')) == Some(name)).and_then(parse_sum)
}

/// The checksum in a `sha256sum` line ("<64 hex>  name"), lowercase.
pub fn parse_sum(text: &str) -> Option<String> {
    let h = text.split_whitespace().next()?.to_ascii_lowercase();
    (h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit())).then_some(h)
}

/// A GET: the address, and where to save the body (None: return it).
pub type Fetch = Arc<dyn Fn(&str, Option<&Path>) -> Result<Vec<u8>, String> + Send + Sync>;

/// The command for one GET. `-q` first, so no ~/.curlrc is read.
pub fn curl_args(url: &str, to: Option<&Path>) -> Vec<String> {
    let mut a: Vec<String> = ["-q", "--silent", "--show-error", "--fail", "--location", "--max-redirs", "5", "--proto", "=https", "--proto-redir", "=https", "--connect-timeout", "10", "--max-time", "180", "--user-agent", "Maia"].map(String::from).to_vec();
    a.extend(["--max-filesize".into(), MAX_BIN.to_string()]);
    if url.starts_with("https://api.github.com/") {
        a.extend(["--header".into(), "Accept: application/vnd.github+json".into()]);
    }
    if let Some(p) = to {
        a.extend(["--output".into(), p.display().to_string()]);
    }
    a.push(url.to_string());
    a
}

#[cfg(not(test))]
fn curl(url: &str, to: Option<&Path>) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err("Not a secure address".into());
    }
    if !crate::media::have("curl") {
        return Err("Checking for updates needs curl".into());
    }
    let mut c = crate::media::tool("curl");
    c.args(curl_args(url, to));
    crate::media::output_capped(c, Duration::from_secs(200), MAX_BIN + 1024, &|| false).ok_or_else(|| "Couldn't reach GitHub".to_string())
}

/// Test builds never reach the network: a test hands in its own fetch.
#[cfg(test)]
fn curl(_: &str, _: Option<&Path>) -> Result<Vec<u8>, String> {
    Err("No network in tests".into())
}

/// Whether the build at `path` runs here and says it is `version` (one built for a newer
/// system than this one would not start: better found now than after the swap).
#[cfg(not(test))]
fn runs(path: &Path, version: &str) -> bool {
    let mut c = std::process::Command::new(path);
    // An AppImage unpacked rather than mounted, so a system without FUSE can try it too.
    c.arg("--version").env("APPIMAGE_EXTRACT_AND_RUN", "1");
    crate::media::output(c, Duration::from_secs(10)).is_some_and(|out| String::from_utf8_lossy(&out).trim() == format!("maia {}", version.trim_start_matches(['v', 'V'])))
}

/// Test builds' stand-in programs aren't real ones.
#[cfg(test)]
fn runs(_: &Path, _: &str) -> bool {
    true
}

/// SHA-256 of a file, by coreutils' sha256sum.
fn sha256(path: &Path) -> Option<String> {
    let mut c = crate::media::tool("sha256sum");
    c.arg(path);
    parse_sum(&String::from_utf8_lossy(&crate::media::output(c, Duration::from_secs(60))?))
}

pub struct Updater {
    state: Arc<Mutex<State>>,
    fetch: Fetch,
    /// The program to replace.
    exe: PathBuf,
    /// The AppImage it runs from, if it does.
    appimage: Option<PathBuf>,
    /// Where a package waits for its manager.
    work: PathBuf,
}

/// Where downloaded packages wait (tests run outside the sandbox too: never the real
/// cache).
fn work_dir() -> PathBuf {
    let base = if cfg!(test) { None } else { dirs::cache_dir() };
    base.unwrap_or_else(std::env::temp_dir).join("maia/updates")
}

impl Default for Updater {
    fn default() -> Self {
        let exe = std::env::current_exe().unwrap_or_default();
        let appimage = running_appimage(&exe);
        Updater { state: Arc::new(Mutex::new(State::Idle)), fetch: Arc::new(curl), exe, appimage, work: work_dir() }
    }
}

impl Updater {
    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn busy(&self) -> bool {
        matches!(self.state(), State::Checking | State::Downloading(_) | State::Installing(_))
    }

    /// Ask GitHub for the latest release, on a thread.
    pub fn check(&self, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Checking;
        let (state, fetch, ctx) = (self.state.clone(), self.fetch.clone(), ctx.clone());
        std::thread::spawn(move || {
            let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
            let next = match fetch(&url, None) {
                Ok(body) => match parse_release(&body) {
                    Some(r) if newer(env!("CARGO_PKG_VERSION"), &r.version) => State::Available(r),
                    Some(_) => State::Latest,
                    None => State::Failed("The latest release has no build for this system".into()),
                },
                Err(e) => State::Failed(e),
            };
            *state.lock().unwrap_or_else(|e| e.into_inner()) = next;
            ctx.request_repaint();
        });
    }

    /// Download `r`, check it, and put it in place of the installed program, on a thread.
    pub fn install(&self, r: Release, ctx: &egui::Context) {
        if self.busy() {
            return;
        }
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = State::Downloading(r.version.clone());
        // The tools are this thread's (a simulation's fakes are set per thread).
        let tools = crate::drives::Tools::current();
        let (state, fetch, exe, appimage, work, ctx) = (self.state.clone(), self.fetch.clone(), self.exe.clone(), self.appimage.clone(), self.work.clone(), ctx.clone());
        std::thread::spawn(move || {
            let how = installed(&exe, appimage.as_deref(), tools.as_ref());
            let installing = || {
                *state.lock().unwrap_or_else(|e| e.into_inner()) = State::Installing(r.version.clone());
                ctx.request_repaint();
            };
            let next = match &how {
                Installed::Elsewhere(dir) => State::CantWrite(dir.clone()),
                _ => match apply(&r, &how, &exe, &work, &fetch, tools.as_ref(), &installing) {
                    Ok(()) => State::Ready(r.version.clone()),
                    Err(e) => State::Failed(e),
                },
            };
            *state.lock().unwrap_or_else(|e| e.into_inner()) = next;
            ctx.request_repaint();
        });
    }

    /// Start the new version in a moment and close this one (its window, and so the app).
    pub fn restart(&self, ctx: &egui::Context) {
        // An AppImage's program is on a mount that goes away with this process.
        let exe = self.appimage.clone().unwrap_or_else(|| self.exe.clone());
        let mut c = std::process::Command::new("setsid");
        c.args(["sh", "-c", "sleep 1; exec \"$0\""]).arg(&exe).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
        if cfg!(not(test)) && c.spawn().is_ok() {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    #[cfg(test)]
    pub fn for_test(fetch: Fetch, exe: PathBuf) -> Updater {
        // Beside the program's folder, which a test may make read-only.
        let work = exe.parent().and_then(Path::parent).map(|d| d.join("updates")).unwrap_or_default();
        Updater { state: Arc::new(Mutex::new(State::Idle)), fetch, exe, appimage: None, work }
    }

    /// The same, run from the AppImage `file`.
    #[cfg(test)]
    pub fn with_appimage(mut self, file: PathBuf) -> Updater {
        self.appimage = Some(file);
        self
    }
}

/// `maia --update`: the same steps as in Options, waited for, with a line to say how it
/// went.
pub fn update_now() -> Result<String, String> {
    let fetch: Fetch = Arc::new(curl);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let body = fetch(&format!("https://api.github.com/repos/{REPO}/releases/latest"), None)?;
    let r = parse_release(&body).ok_or("The latest release has no build for this system")?;
    let current = env!("CARGO_PKG_VERSION");
    if !newer(current, &r.version) {
        return Ok(format!("Maia {current} is the latest"));
    }
    let tools = crate::drives::Tools::current();
    let how = installed(&exe, running_appimage(&exe).as_deref(), tools.as_ref());
    if let Installed::Elsewhere(dir) = &how {
        return Err(format!("Can't write to {}: update Maia the way it was installed", dir.display()));
    }
    apply(&r, &how, &exe, &work_dir(), &fetch, tools.as_ref(), &|| {})?;
    Ok(format!("Updated Maia {current} to {}; start it again to use it", r.version.trim_start_matches('v')))
}

/// Put release `r` in place for an install of kind `how`. `installing` is told when the
/// package manager takes over (and asks for the password).
fn apply(r: &Release, how: &Installed, exe: &Path, work: &Path, fetch: &Fetch, tools: Option<&crate::drives::Tools>, installing: &dyn Fn()) -> Result<(), String> {
    match how {
        Installed::Own => put_in_place(r, exe, fetch),
        Installed::AppImage(file) => {
            let name = how.package(&r.version).unwrap_or_default();
            let url = r.url(&name).ok_or("The latest release has no AppImage for this system")?;
            let want = release_sum(r, fetch, &name)?;
            replace_checked(file, url, &want, fetch, &r.version)
        }
        Installed::Deb | Installed::Rpm | Installed::Pacman => {
            let t = tools.ok_or("No package manager to install with")?;
            let name = how.package(&r.version).unwrap_or_default();
            let url = r.url(&name).ok_or("The latest release has no package for this system")?;
            let want = release_sum(r, fetch, &name)?;
            std::fs::create_dir_all(work).map_err(|e| e.to_string())?;
            // The program alone, tried first: a package that can't run here would leave
            // nothing to come back to.
            let trial = work.join(format!("maia-trial-{}", std::process::id()));
            let tried = replace_checked(&trial, &r.bin, &fetch_sum(&r.sum, fetch)?, fetch, &r.version);
            let _ = std::fs::remove_file(&trial);
            tried?;
            let pkg = work.join(&name);
            let done = (|| {
                fetch(url, Some(&pkg))?;
                let head = std::fs::read(&pkg).map_err(|e| e.to_string())?;
                let magic: &[u8] = match how {
                    Installed::Deb => b"!<arch>\n",
                    Installed::Rpm => b"\xed\xab\xee\xdb",
                    _ => b"\x28\xb5\x2f\xfd",
                };
                if !head.starts_with(magic) {
                    return Err("The download isn't a package".to_string());
                }
                if sha256(&pkg).as_deref() != Some(want.as_str()) {
                    return Err("The download doesn't match the release's checksum".to_string());
                }
                installing();
                let argv: Vec<std::ffi::OsString> = match how {
                    Installed::Deb => vec![t.program("dpkg").into(), "-i".into(), pkg.clone().into()],
                    Installed::Rpm => vec![t.program("rpm").into(), "-U".into(), pkg.clone().into()],
                    _ => vec![t.program("pacman").into(), "-U".into(), "--noconfirm".into(), pkg.clone().into()],
                };
                crate::admin::run_argv(t, &argv)
            })();
            let _ = std::fs::remove_file(&pkg);
            done
        }
        Installed::Elsewhere(dir) => Err(format!("Can't write to {}", dir.display())),
    }
}

/// The checksum the release's `SHA256SUMS` gives `name`.
fn release_sum(r: &Release, fetch: &Fetch, name: &str) -> Result<String, String> {
    let url = r.url(ALL_SUMS).ok_or("The release's checksums couldn't be read")?;
    fetch(url, None).ok().and_then(|b| sum_for(&String::from_utf8_lossy(&b), name)).ok_or_else(|| "The release's checksum couldn't be read".to_string())
}

/// The checksum in a `.sha256` file.
fn fetch_sum(url: &str, fetch: &Fetch) -> Result<String, String> {
    fetch(url, None).ok().and_then(|b| parse_sum(&String::from_utf8_lossy(&b))).ok_or_else(|| "The release's checksum couldn't be read".to_string())
}

/// The program alone, checked and renamed over the installed one.
fn put_in_place(r: &Release, exe: &Path, fetch: &Fetch) -> Result<(), String> {
    replace_checked(exe, &r.bin, &fetch_sum(&r.sum, fetch)?, fetch, &r.version)
}

/// `url` downloaded beside `target`, checked, made runnable, tried, and renamed over it (a
/// rename on the same filesystem: the running program keeps its old file until it exits).
fn replace_checked(target: &Path, url: &str, want: &str, fetch: &Fetch, version: &str) -> Result<(), String> {
    let tmp = target.with_file_name(format!(".maia-update-{}", std::process::id()));
    let done = (|| {
        fetch(url, Some(&tmp))?;
        let head = std::fs::read(&tmp).map_err(|e| e.to_string())?;
        if !head.starts_with(b"\x7fELF") {
            return Err("The download isn't a program".to_string());
        }
        if sha256(&tmp).as_deref() != Some(want) {
            return Err("The download doesn't match the release's checksum".to_string());
        }
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        if !runs(&tmp, version) {
            return Err("This release doesn't run on this system; Maia is unchanged".to_string());
        }
        std::fs::rename(&tmp, target).map_err(|e| e.to_string())
    })();
    if done.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    done
}
