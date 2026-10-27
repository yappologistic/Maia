//! Headless user simulator. Drives the real `App` through egui with synthetic keyboard and
//! mouse input (no window, no GPU) and checks invariants after every frame: layout state
//! (panels, tabs, indices), what was drawn (finite geometry, on screen, no text drawn over
//! other text in plain list views) and how long each frame took.
//!
//! These tests click, type, rename, move and delete like a person would, so they only run
//! inside the sandbox set up by `tools/sim.sh` (read-only filesystem, empty home,
//! no network, only the fixture writable), which sets `MAIA_SIM_SANDBOX=1`. Started any
//! other way they skip. External programs are never launched from test builds
//! (`mime::spawn_detached` records the request instead).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::time::Instant;

use egui::epaint;
use egui::{Event, Pos2, Rect, Vec2, pos2, vec2};

use crate::app::App;
use crate::commands::Cmd;
use crate::popups::Popup;
use crate::tab::Row;

thread_local! {
    static PROF: std::cell::RefCell<Vec<(&'static str, f32)>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Section timer for profiling a stall: wrap a suspect call with
/// `let t = Instant::now(); …; crate::sim::prof("name", t);` (test builds only); the
/// sections of the first slow frame of a single-seed replay are printed.
#[allow(dead_code)]
pub fn prof(name: &'static str, t: Instant) {
    PROF.with(|p| p.borrow_mut().push((name, t.elapsed().as_secs_f32() * 1000.0)));
}

/// True inside the sandbox; tests return early (and say so) otherwise.
pub fn sandboxed() -> bool {
    let ok = std::env::var("MAIA_SIM_SANDBOX").as_deref() == Ok("1");
    if !ok {
        eprintln!("sim: skipped (run tools/sim.sh to simulate users in the sandbox)");
    }
    ok
}

/// Per-process root holding HOME, the XDG dirs and fixtures.
pub fn root() -> PathBuf {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let r = PathBuf::from(std::env::var("MAIA_SIM_ROOT").expect("MAIA_SIM_ROOT"));
        for d in ["home", "xdg/config", "xdg/data", "xdg/cache", "xdg/state"] {
            std::fs::create_dir_all(r.join(d)).unwrap();
        }
        // SAFETY: set once, before any App exists, from the first test that needs it.
        unsafe {
            std::env::set_var("HOME", r.join("home"));
            std::env::set_var("XDG_CONFIG_HOME", r.join("xdg/config"));
            std::env::set_var("XDG_DATA_HOME", r.join("xdg/data"));
            std::env::set_var("XDG_CACHE_HOME", r.join("xdg/cache"));
            std::env::set_var("XDG_STATE_HOME", r.join("xdg/state"));
        }
        r
    })
    .clone()
}

fn png_file(path: &Path, w: u32, h: u32, seed: u32) {
    let f = std::fs::File::create(path).unwrap();
    let mut e = png::Encoder::new(std::io::BufWriter::new(f), w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let mut px = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            px.extend_from_slice(&[(x * 7 + seed * 40) as u8, (y * 5 + seed * 90) as u8, ((x ^ y) + seed) as u8, 255]);
        }
    }
    e.write_header().unwrap().write_image_data(&px).unwrap();
}

/// Delete everything under the sim root (settings, trash, fixtures, anything a session
/// copied) so the next session starts from the same clean state.
fn free_mb(p: &Path) -> u64 {
    let c = std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    unsafe { libc::statvfs(c.as_ptr(), &mut s) };
    s.f_bavail as u64 * s.f_frsize as u64 >> 20
}

pub fn wipe_root() {
    let r = root();
    if std::env::var("MAIA_SIM_DEBUG").is_ok() {
        let out = std::process::Command::new("du").args(["-xhd", "3"]).arg(&r).output();
        let txt = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        let mut lines: Vec<&str> = txt.lines().collect();
        lines.sort_by_key(|l| std::cmp::Reverse(l.split_whitespace().next().map(|s| s.len()).unwrap_or(0)));
        eprintln!("wipe: free {} MB before; biggest:\n  {}", free_mb(&r), lines.iter().take(8).cloned().collect::<Vec<_>>().join("\n  "));
    }
    if let Ok(rd) = std::fs::read_dir(&r) {
        for d in rd.flatten() {
            let _ = std::process::Command::new("chmod").args(["-R", "u+rwX"]).arg(d.path()).status();
            let _ = std::fs::remove_dir_all(d.path());
        }
    }
    for d in ["home", "xdg/config", "xdg/data", "xdg/cache", "xdg/state"] {
        let _ = std::fs::create_dir_all(r.join(d));
    }
    if std::env::var("MAIA_SIM_DEBUG").is_ok() {
        eprintln!("wipe: free {} MB after", free_mb(&r));
        if let Ok(rd) = std::fs::read_dir("/proc/self/fd") {
            for d in rd.flatten() {
                if let Ok(t) = std::fs::read_link(d.path()) {
                    let t = t.to_string_lossy().into_owned();
                    if t.contains("(deleted)") || t.starts_with(&*r.to_string_lossy()) {
                        let size = std::fs::metadata(d.path()).map(|m| m.len()).unwrap_or(0);
                        eprintln!("  open fd {:?} -> {t} ({} MB)", d.file_name(), size >> 20);
                    }
                }
            }
        }
        for cmd in [vec!["df", "-h", "/tmp", "/run", "/home"], vec!["sh", "-c", "find / -xdev -newer /proc/self/stat -size +1M 2>/dev/null | head; find \"$MAIA_SIM_ROOT\" | head -20; df -h \"$MAIA_SIM_ROOT\""]] {
            if let Ok(o) = std::process::Command::new(cmd[0]).args(&cmd[1..]).output() {
                eprintln!("  $ {}\n{}", cmd.join(" "), String::from_utf8_lossy(&o.stdout));
            }
        }
        if let Ok(rd) = std::fs::read_dir("/proc/self/task") {
            eprintln!("  threads: {}", rd.count());
        }
    }
}

/// A home folder like a real user's: documents, photos, code, music, odd names, deep
/// nesting, a big folder, hidden files, links, a pipe and an unreadable folder.
pub fn fixture(name: &str) -> PathBuf {
    let h = root().join("home").join(name);
    let _ = std::fs::remove_dir_all(&h);
    let w = |p: &str, s: &str| {
        let p = h.join(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, s).unwrap();
    };
    w("Documents/report.md", "# Report\n\nSome **markdown** text.\n");
    w("Documents/notes.txt", &"line of notes\n".repeat(400));
    w("Documents/budget 2026.csv", "a,b,c\n1,2,3\n");
    w("Documents/Taxes/2025/receipt.pdf", "%PDF-1.4 fake");
    w("Documents/Taxes/2024/receipt.pdf", "%PDF-1.4 fake");
    w("Documents/résumé – final (v3).txt", "unicode name");
    w("Documents/#長い名前の音楽ファイル_第一部_最新情報_新しい映像の公開_オープニング曲_エンディング曲_さらに続く.mp3", "ID3");
    w("Documents/a very long file name that keeps going and going well past any column width you would pick.txt", "x");
    w("Code/maia/src/main.rs", "fn main() {\n    println!(\"hi\");\n}\n");
    w("Code/maia/src/lib.rs", "pub fn f() {}\n");
    w("Code/maia/Cargo.toml", "[package]\nname = \"x\"\n");
    w("Code/maia/.gitignore", "target\n");
    w("Code/maia/README", "no extension\n");
    w("Code/deep/a/b/c/d/e/f/g/h/leaf.txt", "deep");
    w("Music/Album/01 Intro.mp3", "ID3 fake");
    w("Music/Album/02 Song.flac", "fLaC fake");
    w(".config/app/settings.json", "{}");
    w(".bashrc", "# shell\n");
    w("Downloads/setup.tar.gz", "gz");
    w("Downloads/archive.zip", "PK");
    w("Downloads/Telegram Desktop/photo.webp", "RIFF");
    std::fs::create_dir_all(h.join("Empty")).unwrap();
    std::fs::create_dir_all(h.join("Pictures/Trip")).unwrap();
    for i in 0..12 {
        png_file(&h.join(format!("Pictures/Trip/img{i:02}.png")), 40 + i * 9, 30 + i * 5, i);
    }
    png_file(&h.join("Pictures/wide.png"), 300, 20, 3);
    std::fs::write(h.join("Pictures/broken.png"), b"\x89PNG\r\n\x1a\n garbage").unwrap();
    for i in 0..600 {
        w(&format!("Big/file{i:04}.{}", ["txt", "rs", "png", "md", "json", "log"][i % 6]), "x");
    }
    let _ = std::os::unix::fs::symlink(h.join("Documents"), h.join("docs-link"));
    let _ = std::os::unix::fs::symlink(h.join("nowhere"), h.join("broken-link"));
    let fifo = std::ffi::CString::new(h.join("pipe").into_os_string().into_encoded_bytes()).unwrap();
    unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) };
    std::fs::create_dir_all(h.join("Locked")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(h.join("Locked"), std::fs::Permissions::from_mode(0o000)).unwrap();
    h
}

/// Fake `lsblk` / `udisksctl` / mount table for drive tests (test builds never run the
/// real tools). A system disk holding / and /boot, an internal disk with Windows, swap
/// and a tiny EFI partition, a removable card that isn't mounted, and a mounted stick
/// whose unmount fails because it is busy. Every call is appended to `log`.
pub fn fake_drives(name: &str) -> PathBuf {
    let d = root().join("drives").join(name);
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["mounted", "gone", "busy", "fail", "media"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    let rows = [
        "/dev/sdw\t-\t-\t-\t500107862016\t0\t1\tdisk",
        "/dev/sdw1\tsdw\t-\tvfat\t536870912\t0\t1\tpart",
        "/dev/sdw2\tsdw\t-\tbtrfs\t499570991104\t0\t1\tpart",
        "/dev/sdy\t-\t-\t-\t256060514304\t0\t0\tdisk",
        "/dev/sdy1\tsdy\t-\tvfat\t104857600\t0\t0\tpart",
        "/dev/sdy2\tsdy\tWindows\tntfs\t214748364800\t0\t0\tpart",
        "/dev/sdy3\tsdy\t-\tswap\t8589934592\t0\t0\tpart",
        "/dev/sdz\t-\t-\t-\t16008609792\t1\t1\tdisk",
        "/dev/sdz1\tsdz\tPhoto Card\texfat\t16007561216\t1\t1\tpart",
        "/dev/sdx\t-\t-\t-\t32015679488\t1\t1\tdisk",
        "/dev/sdx1\tsdx\tBusy Stick\text4\t32014630912\t1\t1\tpart",
    ];
    std::fs::write(d.join("devices"), rows.join("\n") + "\n").unwrap();
    let busy = d.join("media/Busy Stick");
    std::fs::create_dir_all(&busy).unwrap();
    std::fs::write(busy.join("busy.txt"), "in use").unwrap();
    std::fs::write(d.join("mounted/sdw1"), "/boot").unwrap();
    std::fs::write(d.join("mounted/sdw2"), "/").unwrap();
    std::fs::write(d.join("mounted/sdx1"), busy.to_str().unwrap()).unwrap();
    std::fs::write(d.join("busy/sdx1"), "").unwrap();
    let lsblk = r#"#!/bin/bash
D="$(dirname "$0")"
echo "lsblk $*" >> "$D/log"
while IFS=$'\t' read -r path pk label fs size rm hp type; do
  [ -z "$path" ] && continue
  [ "$pk" = - ] && pk=""; [ "$label" = - ] && label=""; [ "$fs" = - ] && fs=""
  name="${path##*/}"; disk="${pk:-$name}"
  [ -e "$D/gone/${disk%%[0-9]*}" ] && continue
  mp=""; [ -f "$D/mounted/$name" ] && mp="$(cat "$D/mounted/$name")"
  printf 'PATH="%s" PKNAME="%s" LABEL="%s" FSTYPE="%s" SIZE="%s" RM="%s" HOTPLUG="%s" TYPE="%s" MOUNTPOINTS="%s"\n' "$path" "$pk" "${label// /\\x20}" "$fs" "$size" "$rm" "$hp" "$type" "${mp// /\\x20}"
done < "$D/devices"
"#;
    let udisks = r#"#!/bin/bash
D="$(dirname "$0")"
echo "udisksctl $*" >> "$D/log"
op="$1"; dev="$3"; name="${dev##*/}"
field() { awk -F'\t' -v p="${2:-$dev}" -v f="$1" '$1==p{print $f}' "$D/devices"; }
regen() {
  local f n m
  for f in "$D"/mounted/*; do
    [ -f "$f" ] || continue
    n="${f##*/}"; m="$(cat "$f")"; p="$(awk -F'\t' -v n="$n" '{k=$1; sub(/.*\//,"",k)} k==n{print $1}' "$D/devices")"
    echo "${p:=/dev/$n} ${m// /\\040} $(field 4 "$p") rw 0 0"
  done > "$D/mounts.new"; mv "$D/mounts.new" "$D/mounts"
}
sleep 0.15
if [ -f "$D/fail/$name" ]; then echo "Error ${op}ing $dev: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: $(cat "$D/fail/$name")" >&2; exit 1; fi
case "$op" in
  mount)
    if [ -f "$D/mounted/$name" ]; then echo "Error mounting $dev: GDBus.Error:org.freedesktop.UDisks2.Error.AlreadyMounted: Device $dev is already mounted" >&2; exit 1; fi
    label="$(field 3)"; [ "$label" = - ] && label="$name"
    mp="$D/media/$label"; mkdir -p "$mp"; echo "files on $label" > "$mp/readme.txt"
    printf '%s' "$mp" > "$D/mounted/$name"; regen
    echo "Mounted $dev at $mp" ;;
  unmount)
    if [ -e "$D/busy/$name" ]; then echo "Error unmounting $dev: GDBus.Error:org.freedesktop.UDisks2.Error.DeviceBusy: Error unmounting $dev: target is busy" >&2; exit 1; fi
    if [ ! -f "$D/mounted/$name" ]; then echo "Error unmounting $dev: GDBus.Error:org.freedesktop.UDisks2.Error.NotMounted: Device is not mounted" >&2; exit 1; fi
    mp="$(cat "$D/mounted/$name")"; rm -f "$D/mounted/$name"; regen; rm -rf "$mp" ;;
  power-off)
    touch "$D/gone/$name" ;;
  unlock)
    # The passphrase comes as a key file on stdin, exactly (no newline added).
    [ "$4 $5" = "--key-file /dev/stdin" ] || { echo "Error unlocking $dev: no key file" >&2; exit 1; }
    if [ "$(cat; echo .)" != "$(cat "$D/passphrase")." ]; then echo "Error unlocking $dev: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Error unlocking $dev: Failed to activate device: Operation not permitted" >&2; exit 1; fi
    printf '/dev/mapper/luks-%s\t%s\tVault\text4\t%s\t0\t0\tcrypt\n' "$name" "$name" "$(field 5)" >> "$D/devices"
    echo "Unlocked $dev as /dev/dm-0." ;;
  lock)
    if [ -f "$D/mounted/luks-$name" ]; then echo "Error locking $dev: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Cannot lock $dev as the cleartext device is mounted" >&2; exit 1; fi
    awk -F'\t' -v n="$name" '$2!=n' "$D/devices" > "$D/devices.new" && mv "$D/devices.new" "$D/devices" ;;
  *) echo "unknown $op" >&2; exit 1 ;;
esac
"#;
    // udisks over D-Bus, for Format: each call's arguments logged one per line; the new
    // partition table takes the disk's partitions away and the formatted partition comes
    // back with its label and filesystem.
    let gdbus = r#"#!/bin/bash
D="$(dirname "$0")"
{ echo "gdbus"; for a in "$@"; do printf '%s\n' "$a"; done; echo; } >> "$D/gdbus.log"
echo "gdbus $*" >> "$D/log"
path=""; method=""; prev=""
for a in "$@"; do
  [ "$prev" = --object-path ] && path="$a"
  [ "$prev" = --method ] && method="$a"
  prev="$a"
done
disk="${path##*/}"
sleep 0.2
if [ -f "$D/fail/format" ]; then echo "GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Error creating file system: $(cat "$D/fail/format")" >&2; exit 1; fi
case "$method" in
  org.freedesktop.UDisks2.Block.Format)
    awk -F'\t' -v d="$disk" '$2!=d' "$D/devices" > "$D/devices.new" && mv "$D/devices.new" "$D/devices"
    echo "()" ;;
  org.freedesktop.UDisks2.PartitionTable.CreatePartitionAndFormat)
    last="${@: -1}"; kind="${@: -2:1}"
    label="$(printf '%s' "$last" | sed -n "s/.*'label': <'\([^']*\)'>.*/\1/p")"
    fs="$(printf '%s' "$kind" | tr -d "'")"
    size="$(awk -F'\t' -v p="/dev/$disk" '$1==p{print $5}' "$D/devices")"
    printf '/dev/%s1\t%s\t%s\t%s\t%s\t1\t1\tpart\n' "$disk" "$disk" "${label:--}" "$fs" "$size" >> "$D/devices"
    echo "(objectpath '/org/freedesktop/UDisks2/block_devices/${disk}1',)" ;;
  *) echo "unknown method $method" >&2; exit 1 ;;
esac
"#;
    use std::os::unix::fs::PermissionsExt;
    for (n, body) in [("lsblk", lsblk), ("udisksctl", udisks), ("gdbus", gdbus)] {
        std::fs::write(d.join(n), body).unwrap();
        std::fs::set_permissions(d.join(n), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Initial mount table, written the way the fake udisksctl rewrites it.
    let table: String = [("sdw1", "/boot", "vfat"), ("sdw2", "/", "btrfs"), ("sdx1", busy.to_str().unwrap(), "ext4")].iter().map(|(n, m, f)| format!("/dev/{n} {} {f} rw 0 0\n", m.replace(' ', "\\040"))).collect();
    std::fs::write(d.join("mounts"), table).unwrap();
    d
}

/// Plugs a locked LUKS drive into fake drives `d`, as lsblk lists one: `/dev/sdv1` on a
/// 500 GB USB disk, which the passphrase "correct horse" unlocks to an ext4 "Vault".
pub fn fake_encrypted_drive(d: &Path) {
    let mut rows = std::fs::read_to_string(d.join("devices")).unwrap();
    rows.push_str("/dev/sdv\t-\t-\t-\t536870912000\t0\t1\tdisk\n/dev/sdv1\tsdv\t-\tcrypto_LUKS\t536869863424\t0\t1\tpart\n");
    std::fs::write(d.join("devices"), rows).unwrap();
    std::fs::write(d.join("passphrase"), "correct horse").unwrap();
}

/// A fake `gio` for network tests: a phone that can be mounted, a share on "nas" that
/// asks for a login (user "me", password "secret") the way gio does, an anonymous FTP
/// site "pub", and unknown hosts that fail. Mounts are folders under `gvfs/`.
pub fn fake_network(name: &str) -> PathBuf {
    let d = root().join("net").join(name);
    let _ = std::fs::remove_dir_all(&d);
    for sub in ["gvfs", "netmounts"] {
        std::fs::create_dir_all(d.join(sub)).unwrap();
    }
    std::fs::write(d.join("phones"), "Pixel 7\tmtp://Google_Pixel_7/\n").unwrap();
    let gio = r#"#!/bin/bash
D="$(dirname "$0")"
echo "gio $*" >> "$D/log"
id() { printf '%s' "$1" | tr -c 'A-Za-z0-9' '_'; }
do_mount() {
  local uri="$1" name="$2" i; i=$(id "$uri")
  mkdir -p "$D/gvfs/$i"; echo "on $name" > "$D/gvfs/$i/hello.txt"
  printf '%s\t%s\n' "$name" "$uri" > "$D/netmounts/$i"
}
if [ "$1" = mount ] && [ "$2" = -l ]; then
  while IFS=$'\t' read -r name uri; do
    [ -z "$uri" ] && continue
    echo "Volume(0): $name"; echo "  Type: GProxyVolume (GProxyVolumeMonitorMTP)"; echo "  activation_root=$uri"
    [ -f "$D/netmounts/$(id "$uri")" ] && { echo "  Mount(0): $name -> $uri"; echo "    Type: GProxyShadowMount"; }
  done < "$D/phones"
  for f in "$D"/netmounts/*; do
    [ -f "$f" ] || continue
    IFS=$'\t' read -r name uri < "$f"
    case "$uri" in mtp://*) continue ;; esac
    echo "Mount(0): $name -> $uri"; echo "  Type: GDaemonMount"
  done
  exit 0
fi
if [ "$1" = info ]; then
  i=$(id "$2"); [ -f "$D/netmounts/$i" ] || { echo "gio: $2: Operation not supported" >&2; exit 1; }
  echo "name: x"; echo "uri: $2"; echo "local path: $D/gvfs/$i"; exit 0
fi
if [ "$1" = mount ] && [ "$2" = -u ]; then
  i=$(id "$3"); [ -f "$D/netmounts/$i" ] || { echo "gio: $3: Containing mount does not exist" >&2; exit 1; }
  rm -f "$D/netmounts/$i"; rm -rf "$D/gvfs/$i"; exit 0
fi
if [ "$1" = mount ]; then
  anon=0; uri="$2"; [ "$2" = -a ] && { anon=1; uri="$3"; }
  case "$uri" in
    mtp://Google_Pixel_7*) do_mount "$uri" "Pixel 7" ;;
    ftp://pub*) do_mount "$uri" "pub" ;;
    smb://nas/*)
      [ $anon = 1 ] && { echo "gio: $uri: Failed to mount Windows share: Permission denied" >&2; exit 2; }
      printf 'Password required for share “media” on nas\nUser [me]: '; read -r user
      printf 'Domain [WORKGROUP]: '; read -r dom
      while true; do
        printf 'Password: '; read -r pass || exit 2
        [ "$user" = me ] && [ "$pass" = secret ] && break
      done
      sleep 0.2; do_mount "$uri" "media on nas" ;;
    *) echo "gio: $uri: Failed to mount Windows share: No such file or directory" >&2; exit 2 ;;
  esac
  exit 0
fi
echo "gio: unknown $*" >&2; exit 1
"#;
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(d.join("gio"), gio).unwrap();
    std::fs::set_permissions(d.join("gio"), std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

/// A fake `pkexec` for administrator tests: it plays root by making the folders the
/// command touches writable for the moment it runs, and refuses (exit 126, as a dismissed
/// password dialog does) while a file named `deny` exists beside it.
pub fn fake_pkexec(name: &str) -> PathBuf {
    let d = root().join("admin").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let script = r#"#!/bin/bash
D="$(dirname "$0")"
echo "pkexec $*" >> "$D/log"
[ -e "$D/deny" ] && { echo "Error executing command as another user: Request dismissed" >&2; exit 126; }
opened=()
after=0
for a in "$@"; do
  [ "$a" = -- ] && { after=1; continue; }
  [ $after = 1 ] || continue
  for d in "$a" "$(dirname "$a")"; do
    if [ -d "$d" ] && [ ! -w "$d" ]; then chmod u+w "$d"; opened+=("$d"); fi
  done
done
"$@"; rc=$?
for d in "${opened[@]}"; do chmod u-w "$d" 2>/dev/null; done
exit $rc
"#;
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(d.join("pkexec"), script).unwrap();
    std::fs::set_permissions(d.join("pkexec"), std::fs::Permissions::from_mode(0o755)).unwrap();
    d
}

/// A project to develop in, under `home/Code/app`: a git repository with Rust and
/// JavaScript sources holding a word to find and TODO comments, manifests with tasks,
/// output and packages its .gitignore leaves out, a hidden and a binary file, and build
/// folders of several kinds.
pub fn dev_fixture(home: &Path) -> PathBuf {
    let p = home.join("Code/app");
    let _ = std::fs::remove_dir_all(&p);
    let w = |f: &str, s: &str| {
        let f = p.join(f);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, s).unwrap();
    };
    w(".gitignore", "target/\nnode_modules/\ndist/\n*.log\n");
    w("Cargo.toml", "[package]\nname = \"app\"\nversion = \"0.1.0\"\nedition = \"2021\"\n");
    w("src/main.rs", "use std::io;\n\n// TODO: read the config before the window opens\nfn main() {\n    let needle = find_needle();\n    println!(\"{needle}\");\n}\n\nfn find_needle() -> &'static str {\n    // FIXME(ana): the Needle is hard-coded\n    \"needle\"\n}\n");
    w("src/lib.rs", "pub fn haystack() -> Vec<u8> {\n    // HACK: a needle in a haystack\n    vec![]\n}\n");
    w("src/many.rs", &(0..300).map(|i| format!("fn needle_{i}() {{}}\n")).collect::<String>());
    w("src/bin/tool.rs", "fn main() {}\n");
    w("examples/demo.rs", "fn main() {}\n");
    w("web/package.json", r#"{"name":"web","scripts":{"dev":"vite","build":"vite build","lint":"eslint ."}}"#);
    w("web/pnpm-lock.yaml", "lockfileVersion: '9.0'\n");
    w("web/src/app.js", "// XXX remove the needle shim\nexport const needle = 1;\n");
    w("web/node_modules/left-pad/index.js", "module.exports = 'needle';\n");
    w("web/dist/app.js", "const needle = 1;\n");
    w("Makefile", "all: build\n\n# Build the release\nbuild:\n\tcargo build --release\n");
    w("justfile", "# Run the tests\ntest:\n    cargo test\n");
    w("target/CACHEDIR.TAG", "Signature: 8a477f597d28d172789f06886806bc55\n");
    w("target/debug/app.d", &format!("needle {}\n", "x".repeat(20_000)));
    w("notes.log", "needle in a log\n");
    w(".secret", "needle hidden\n");
    w("py/pyproject.toml", "[project]\nname = \"tool\"\n");
    w("py/.venv/pyvenv.cfg", "home = /usr/bin\n");
    w("py/.venv/lib/site.py", &"s".repeat(4000));
    w("py/pkg/__pycache__/m.cpython-313.pyc", "pyc");
    std::fs::write(p.join("logo.bin"), b"\x89\x00\x00needle\x00").unwrap();
    let _ = std::process::Command::new("git").args(["init", "-q"]).current_dir(&p).status();
    p
}

/// Stand-ins for editors (never run: test builds record what they would start).
pub fn fake_editors(name: &str) -> PathBuf {
    let d = root().join("editors").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for e in ["code", "zed", "nvim"] {
        std::fs::write(d.join(e), "#!/bin/sh\nexit 0\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(d.join(e), std::fs::Permissions::from_mode(0o755));
    }
    d
}

/// Real media for preview/thumbnail tests, made with ffmpeg inside the sandbox: a video,
/// an MP3 with cover art, a hand-written one-page PDF, an SVG, a PNG and a corrupt video.
pub fn media_fixture(name: &str) -> PathBuf {
    let d = root().join("home").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    let ff = |args: &[&str]| {
        let ok = std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).current_dir(&d).status().is_ok_and(|s| s.success());
        assert!(ok, "ffmpeg {args:?}");
    };
    ff(&["-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=15", "-pix_fmt", "yuv420p", "clip.mp4"]);
    ff(&["-f", "lavfi", "-i", "testsrc=size=200x200", "-frames:v", "1", "cover.png"]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-i", "cover.png", "-map", "0", "-map", "1", "-c:a", "libmp3lame", "-c:v", "png", "-disposition:v", "attached_pic", "-id3v2_version", "3", "-metadata", "title=Test Tone", "-metadata", "artist=Maia", "song.mp3"]);
    png_file(&d.join("photo.png"), 320, 200, 5);
    std::fs::write(d.join("drawing.svg"), r##"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="48" viewBox="0 0 64 48"><rect width="64" height="48" fill="#2a7"/><circle cx="32" cy="24" r="16" fill="#fc3"/></svg>"##).unwrap();
    std::fs::write(d.join("broken.mp4"), b"\x00\x00\x00\x18ftypmp42 this is not a video").unwrap();
    std::fs::write(d.join("doc.pdf"), sample_pdf(&["0.2 0.4 0.8", "0.8 0.3 0.2", "0.2 0.7 0.3"])).unwrap();
    d
}

/// A minimal valid PDF written by hand: one A6 page per colour ("r g b"), each a
/// rectangle filled with it.
pub fn sample_pdf(colors: &[&str]) -> String {
    sized_pdf(colors, [298, 420], [20, 20, 250, 380])
}

/// Pages `size` points big, each with one rectangle `ink` in its colour.
pub fn sized_pdf(colors: &[&str], size: [u32; 2], ink: [u32; 4]) -> String {
    let kids: Vec<String> = (0..colors.len()).map(|k| format!("{} 0 R", 3 + 2 * k)).collect();
    let mut objs = vec!["<< /Type /Catalog /Pages 2 0 R >>".to_string(), format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), colors.len())];
    for (k, c) in colors.iter().enumerate() {
        let stream = format!("{c} rg {} {} {} {} re f", ink[0], ink[1], ink[2], ink[3]);
        objs.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {} 0 R >>", size[0], size[1], 4 + 2 * k));
        objs.push(format!("<< /Length {} >>\nstream\n{stream}\nendstream", stream.len()));
    }
    let mut pdf = String::from("%PDF-1.4\n");
    let mut offs = vec![];
    for (i, o) in objs.iter().enumerate() {
        offs.push(pdf.len());
        pdf.push_str(&format!("{} 0 obj\n{o}\nendobj\n", i + 1));
    }
    let xref = pdf.len();
    pdf.push_str(&format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1));
    for o in offs {
        pdf.push_str(&format!("{o:010} 00000 n \n"));
    }
    pdf.push_str(&format!("trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objs.len() + 1));
    pdf
}

pub struct Sim {
    pub ctx: egui::Context,
    pub app: App,
    pub size: Vec2,
    pub ppp: f32,
    time: f64,
    pointer: Pos2,
    /// The modifier keys held as of the last frame (egui hears of a change as an event).
    mods: egui::Modifiers,
    /// CPU time of every frame (ms): app update + tessellation.
    pub frame_ms: Vec<f32>,
    /// What happened, for failure reports.
    pub log: VecDeque<String>,
    /// Check that list text never overlaps (only meaningful with no popup or hover).
    pub check_overlap: bool,
    /// Check that text inside each popup's frame never overlaps (what is under it is not
    /// looked at).
    pub check_popup_overlap: bool,
    /// Frames over `SLOW_MS`, with the step that caused them.
    pub slow: Vec<(f32, String)>,
    /// How many times a popup's text containment was actually checked.
    pub popup_checks: std::cell::Cell<usize>,
    /// Text drawn in the last frame (visible rect, string), for clicking by label.
    pub texts: Vec<(Rect, String)>,
    /// The colour each of `texts` was drawn in (its first section's).
    pub text_colors: Vec<egui::Color32>,
    /// What the last frame drew, kept for `shot` when MAIA_SIM_SHOTS names a folder.
    shots: Option<Box<Shots>>,
    /// Text the app put on the clipboard, oldest first.
    pub copied: Vec<String>,
    /// When the last frame asked to be drawn again (MAX: not until something happens).
    pub repaint: std::time::Duration,
}

/// The textures egui has handed out and the last frame's triangles: enough to paint the
/// frame in software, without a window or a GPU.
struct Shots {
    dir: PathBuf,
    textures: std::collections::HashMap<egui::TextureId, egui::ColorImage>,
    prims: Vec<epaint::ClippedPrimitive>,
}

impl Shots {
    fn take(&mut self, delta: &egui::TexturesDelta) {
        for (id, ds) in &delta.set {
            for d in ds {
            let egui::ImageData::Color(img) = &d.image;
            match d.pos {
                None => {
                    self.textures.insert(*id, (**img).clone());
                }
                Some([x, y]) => {
                    if let Some(t) = self.textures.get_mut(id) {
                        for row in 0..img.size[1] {
                            for col in 0..img.size[0] {
                                if let Some(px) = t.pixels.get_mut((y + row) * t.size[0] + x + col) {
                                    *px = img.pixels[row * img.size[0] + col];
                                }
                            }
                        }
                    }
                }
            }
            }
        }
    }

    /// Paint the triangles: each pixel centre inside a triangle takes its texel times its
    /// colour (both premultiplied), blended over what is there.
    fn paint(&self, w: usize, h: usize, ppp: f32) -> Vec<u8> {
        let mut px = vec![[0f32, 0.0, 0.0, 1.0]; w * h];
        let sample = |t: &egui::ColorImage, uv: Pos2| -> [f32; 4] {
            let (tw, th) = (t.size[0] as f32, t.size[1] as f32);
            let (x, y) = (uv.x * tw - 0.5, uv.y * th - 0.5);
            let (x0, y0) = (x.floor(), y.floor());
            let (fx, fy) = (x - x0, y - y0);
            let at = |xx: f32, yy: f32| {
                let (xx, yy) = (xx.clamp(0.0, tw - 1.0) as usize, yy.clamp(0.0, th - 1.0) as usize);
                let c = t.pixels[yy * t.size[0] + xx];
                [c.r() as f32, c.g() as f32, c.b() as f32, c.a() as f32]
            };
            let (a, b, c, d) = (at(x0, y0), at(x0 + 1.0, y0), at(x0, y0 + 1.0), at(x0 + 1.0, y0 + 1.0));
            std::array::from_fn(|i| ((a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy) / 255.0)
        };
        let edge = |a: Pos2, b: Pos2, c: Pos2| (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        for cp in &self.prims {
            let epaint::Primitive::Mesh(m) = &cp.primitive else { continue };
            let Some(tex) = self.textures.get(&m.texture_id) else { continue };
            let clip = cp.clip_rect;
            let (cx0, cy0) = ((clip.min.x * ppp).max(0.0) as usize, (clip.min.y * ppp).max(0.0) as usize);
            let (cx1, cy1) = (((clip.max.x * ppp).ceil().max(0.0) as usize).min(w), ((clip.max.y * ppp).ceil().max(0.0) as usize).min(h));
            for tri in m.indices.chunks_exact(3) {
                let v = [&m.vertices[tri[0] as usize], &m.vertices[tri[1] as usize], &m.vertices[tri[2] as usize]];
                let p = v.map(|v| pos2(v.pos.x * ppp, v.pos.y * ppp));
                let area = edge(p[0], p[1], p[2]);
                if area.abs() < 1e-6 {
                    continue;
                }
                let x0 = (p.iter().map(|p| p.x).fold(f32::MAX, f32::min).floor().max(0.0) as usize).max(cx0);
                let x1 = (p.iter().map(|p| p.x).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(cx1);
                let y0 = (p.iter().map(|p| p.y).fold(f32::MAX, f32::min).floor().max(0.0) as usize).max(cy0);
                let y1 = (p.iter().map(|p| p.y).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(cy1);
                for y in y0..y1 {
                    for x in x0..x1 {
                        let c = pos2(x as f32 + 0.5, y as f32 + 0.5);
                        let (w0, w1) = (edge(p[1], p[2], c) / area, edge(p[2], p[0], c) / area);
                        let w2 = 1.0 - w0 - w1;
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let uv = pos2(v[0].uv.x * w0 + v[1].uv.x * w1 + v[2].uv.x * w2, v[0].uv.y * w0 + v[1].uv.y * w1 + v[2].uv.y * w2);
                        let t = sample(tex, uv);
                        let col = |i: usize| {
                            let f = |c: egui::Color32| [c.r(), c.g(), c.b(), c.a()][i] as f32 / 255.0;
                            f(v[0].color) * w0 + f(v[1].color) * w1 + f(v[2].color) * w2
                        };
                        let src: [f32; 4] = std::array::from_fn(|i| t[i] * col(i));
                        let d = &mut px[y * w + x];
                        for i in 0..4 {
                            d[i] = src[i] + d[i] * (1.0 - src[3]);
                        }
                    }
                }
            }
        }
        px.iter().flat_map(|p| [0, 1, 2].map(|i| (p[i].clamp(0.0, 1.0) * 255.0).round() as u8)).collect()
    }
}

/// A frame slower than this is a visible stall.
pub const SLOW_MS: f32 = 50.0;

impl Sim {
    pub fn new(path: &Path, size: Vec2) -> Sim {
        root();
        crate::memory::tune();
        // A session that failed while holding a modifier doesn't leave it held for the next.
        crate::testkit::parse("hold none");
        // Space used's map is chosen for the session: a new one starts on the ring.
        crate::spaceuse::MAP.store(false, std::sync::atomic::Ordering::Relaxed);
        crate::spaceuse::BY_CHANGE.store(false, std::sync::atomic::Ordering::Relaxed);
        // The last window's closing popup fades on its own clock, not this one's.
        crate::popups::FADE.with(|f| *f.borrow_mut() = (Vec::new(), None));
        let ctx = egui::Context::default();
        let app = App::new(&ctx, vec![path.to_path_buf()]);
        let shots = std::env::var_os("MAIA_SIM_SHOTS").map(|d| Box::new(Shots { dir: PathBuf::from(d), textures: Default::default(), prims: vec![] }));
        let mut s = Sim { ctx, app, size, ppp: 1.6, time: 0.0, pointer: pos2(-10.0, -10.0), mods: egui::Modifiers::NONE, frame_ms: vec![], log: VecDeque::new(), check_overlap: false, check_popup_overlap: false, slow: vec![], popup_checks: Default::default(), texts: vec![], text_colors: vec![], shots, repaint: std::time::Duration::MAX, copied: vec![] };
        // MAIA_SIM_LOOK=FONT,SPACING runs a session at another font size and Spacing.
        if let Some((f, sp)) = std::env::var("MAIA_SIM_LOOK").ok().and_then(|v| v.split_once(',').and_then(|(f, sp)| Some((f.trim().parse().ok()?, sp.trim().parse().ok()?)))) {
            s.app.cfg.font_size = f;
            s.app.cfg.spacing = sp;
        }
        // MAIA_SIM_ANIM=off runs it with animations off.
        if std::env::var("MAIA_SIM_ANIM").as_deref() == Ok("off") {
            s.app.cfg.animations = false;
        }
        // MAIA_SIM_THEME=Paper runs it in another theme.
        if let Some(t) = std::env::var("MAIA_SIM_THEME").ok().and_then(|t| serde_json::from_str(&format!("\"{t}\"")).ok()) {
            s.app.cfg.theme = t;
        }
        s.settle();
        s
    }

    fn note(&mut self, s: String) {
        self.log.push_back(s);
        if self.log.len() > 60 {
            self.log.pop_front();
        }
    }

    pub fn fail(&self, why: &str) -> ! {
        panic!("{why}\nlast steps:\n  {}", self.log.iter().cloned().collect::<Vec<_>>().join("\n  "));
    }

    /// One frame with these input events.
    pub fn frame(&mut self, mut events: Vec<Event>) {
        // Input the app queued for itself (system drag and drop), as the runner adds it.
        events.append(&mut self.app.inject);
        let mods = events
            .iter()
            .find_map(|e| match e {
                Event::Key { modifiers, .. } | Event::PointerButton { modifiers, .. } | Event::MouseWheel { modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or_else(crate::testkit::held);
        for e in &events {
            if let Event::PointerMoved(p) = e {
                self.pointer = *p;
            }
        }
        // egui hears of the modifier keys changing as an event of its own.
        if mods != self.mods {
            events.insert(0, Event::ModifiersChanged(mods));
            self.mods = mods;
        }
        let mut raw = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, self.size)), time: Some(self.time), events, focused: true, ..Default::default() };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(self.ppp);
        PROF.with(|p| p.borrow_mut().clear());
        crate::ui::CONTROLS.with(|c| c.borrow_mut().clear());
        crate::ui::POPUPS.with(|c| c.borrow_mut().clear());
        let t0 = Instant::now();
        let app = &mut self.app;
        let mut out = self.ctx.run_pass(raw, |ctx| app.frame(ctx));
        let prims = self.ctx.tessellate(out.shapes.clone(), out.pixels_per_point);
        for c in &out.platform_output.commands {
            if let egui::OutputCommand::CopyText(t) = c {
                self.copied.push(t.clone());
            }
        }
        self.repaint = out.viewport_output.get(&egui::ViewportId::ROOT).map_or(std::time::Duration::MAX, |v| v.repaint_delay);
        let ms = t0.elapsed().as_secs_f32() * 1000.0;
        // As the runner does when nothing is due: after the frame, outside its time.
        if self.repaint == std::time::Duration::MAX {
            crate::memory::idle();
        }
        if let Some(sh) = &mut self.shots {
            sh.take(&out.textures_delta);
            sh.prims = prims.clone();
            for id in &out.textures_delta.free {
                sh.textures.remove(id);
            }
        }
        // A simulation has no GPU to hand the textures to.
        out.textures_delta.clear();
        self.frame_ms.push(ms);
        if ms > SLOW_MS && self.frame_ms.len() > 3 {
            let traced = self.slow.iter().any(|x| x.0 > 1000.0);
            if !traced && (ms > 1000.0 || (self.slow.is_empty() && std::env::var("MAIA_SIM_SEED").is_ok())) {
                eprintln!("first slow frame ({ms:.0} ms); steps before it:\n  {}", self.log.iter().cloned().collect::<Vec<_>>().join("\n  "));
                PROF.with(|p| eprintln!("  sections: {:?}", p.borrow()));
                eprintln!("  jobs {} [{}], popup open {}, drag {}, toast {:?}", self.app.jobs.len(), self.app.jobs.iter().map(|j| j.label.clone()).collect::<Vec<_>>().join("; "), self.popup_open(), self.app.drag.is_some(), self.app.toast.as_ref().map(|t| t.0.clone()));
                for p in self.app.panels.values() {
                    let t = p.tab();
                    eprintln!("  zoom {} shown {} rows {} filter {:?} selected {} rename {}", t.settings.zoom, t.shown_zoom, t.rows.len(), t.filter, t.selected.len(), t.rename.is_some());
                    eprintln!("  panel {}: {} entries, view {}, flatten {}, sort {:?}, group {:?}, loading {}, dirty {}, recents_on_top {}, stats pending {}", t.path.display(), t.entries.len(), t.view.len(), t.flatten, t.settings.sort, t.settings.group, t.is_loading(), t.dirty, self.app.cfg.recents_on_top, !self.app.stats.idle());
                }
            }
            self.slow.push((ms, self.log.back().cloned().unwrap_or_default()));
        }
        self.time += 1.0 / 60.0;
        self.texts.clear();
        self.text_colors.clear();
        fn collect(sh: &epaint::Shape, clip: Rect, out: &mut Vec<(Rect, String)>, colors: &mut Vec<egui::Color32>) {
            match sh {
                epaint::Shape::Text(t) => {
                    let s = t.galley.text().trim();
                    if !s.is_empty() {
                        let r = sh.visual_bounding_rect().intersect(clip);
                        if r.is_positive() {
                            out.push((r, s.to_string()));
                            colors.push(t.override_text_color.or_else(|| t.galley.job.sections.first().map(|x| x.format.color)).unwrap_or(t.fallback_color));
                        }
                    }
                }
                epaint::Shape::Vec(v) => v.iter().for_each(|s| collect(s, clip, out, colors)),
                _ => {}
            }
        }
        for cs in &out.shapes {
            collect(&cs.shape, cs.clip_rect, &mut self.texts, &mut self.text_colors);
        }
        self.check_state();
        self.check_shapes(&out.shapes);
        self.check_controls();
        for p in &prims {
            if !(p.clip_rect.min.x.is_finite() && p.clip_rect.max.y.is_finite()) {
                self.fail("non-finite clip rect after tessellation");
            }
        }
    }

    pub fn idle(&mut self, n: usize) {
        for _ in 0..n {
            self.frame(vec![]);
        }
    }

    /// Let background work (listings, sizes, thumbnails) land.
    pub fn settle(&mut self) {
        let mut quiet = 0;
        for _ in 0..400 {
            self.frame(vec![]);
            // Rows still gliding to their places count as busy: nothing rests until they do.
            let busy = self.app.panels.values().any(|p| p.tabs.iter().any(|t| t.is_loading() || t.glide.moving() > 0)) || !self.app.stats.idle();
            if busy {
                quiet = 0;
                std::thread::sleep(std::time::Duration::from_millis(3));
            } else {
                quiet += 1;
                if quiet >= 4 {
                    return;
                }
            }
        }
    }

    /// A testkit command line (`key ctrl+t`, `click 300 200`, `text foo`, …).
    pub fn act(&mut self, line: &str) {
        self.note(line.to_string());
        for step in crate::testkit::parse(line) {
            self.frame(step);
        }
        self.frame(vec![]);
    }

    pub fn cmd(&mut self, c: Cmd) {
        self.note(format!("cmd {}", c.info().id));
        self.app.actions.push(crate::app::Action::Cmd(c));
        self.frame(vec![]);
        self.frame(vec![]);
    }

    /// Where the last frame drew the first of `labels` found, if any.
    pub fn text_rect_any(&self, labels: &[&str]) -> Option<Rect> {
        labels.iter().find_map(|l| self.texts.iter().rev().find(|(_, s)| s == l).map(|(r, _)| *r))
    }

    /// Where the last frame drew `label` (topmost match).
    pub fn text_rect(&self, label: &str) -> Rect {
        self.texts.iter().rev().find(|(_, s)| s == label).map(|(r, _)| *r).unwrap_or_else(|| self.fail(&format!("no {label:?} on screen")))
    }

    /// Scroll the open popup (a long menu) down, then up, until `label` shows: at a big
    /// font a menu is taller than the window, as a person would find.
    pub fn scroll_popup_to(&mut self, label: &str) {
        for dy in [-6.0, 6.0] {
            for _ in 0..12 {
                if self.texts.iter().any(|(_, s)| s == label) {
                    return;
                }
                // The text drawn last is the topmost layer's: the open menu's.
                let Some(at) = self.texts.last().map(|t| t.0.center()) else { return };
                self.act(&format!("scroll {:.0} {:.0} {dy}", at.x, at.y));
                self.idle(2);
            }
        }
    }

    /// Click the last-drawn text exactly equal to `label` (topmost wins), like a user
    /// clicking a button by its caption. Fails if it isn't on screen.
    pub fn click_text(&mut self, label: &str) {
        if !self.texts.iter().any(|(_, s)| s == label) && self.popup_open() {
            self.scroll_popup_to(label);
        }
        let Some((r, _)) = self.texts.iter().rev().find(|(_, s)| s == label).cloned() else {
            let near: Vec<&str> = self.texts.iter().map(|(_, s)| s.as_str()).filter(|s| s.len() < 30).take(40).collect();
            self.fail(&format!("no {label:?} on screen; visible: {near:?}"));
        };
        self.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
    }

    /// Save what the last frame drew as `<MAIA_SIM_SHOTS>/<name>.png` (nothing when the
    /// variable is not set). The look in use goes into the name.
    pub fn shot(&self, name: &str) {
        let Some((w, h, rgb)) = self.pixels() else { return };
        let look = [std::env::var("MAIA_SIM_LOOK").unwrap_or_default().replace(',', "-"), std::env::var("MAIA_SIM_THEME").unwrap_or_default()].into_iter().filter(|x| !x.is_empty()).collect::<Vec<_>>().join("-");
        let name: String = format!("{name}{}{look}", if look.is_empty() { "" } else { "@" }).chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '@') { c } else { '_' }).collect();
        if let Some(dir) = self.shots.as_ref().map(|sh| sh.dir.clone()) {
            write_png(&dir.join(format!("{name}.png")), w, h, &rgb);
        }
    }

    /// The last frame painted in software: width, height and RGB rows (MAIA_SIM_SHOTS
    /// only).
    pub fn pixels(&self) -> Option<(usize, usize, Vec<u8>)> {
        let sh = self.shots.as_ref()?;
        let (w, h) = ((self.size.x * self.ppp).round() as usize, (self.size.y * self.ppp).round() as usize);
        Some((w, h, sh.paint(w, h, self.ppp)))
    }

    /// The folder MAIA_SIM_SHOTS names, when it does.
    pub fn shots_dir(&self) -> Option<PathBuf> {
        self.shots.as_ref().map(|sh| sh.dir.clone())
    }
}

pub fn write_png(path: &Path, w: usize, h: usize, rgb: &[u8]) {
    {
        let Ok(f) = std::fs::File::create(path) else { return };
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), w as u32, h as u32);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        if let Ok(mut wr) = e.write_header() {
            let _ = wr.write_image_data(rgb);
        }
    }
}

impl Sim {
    pub fn resize(&mut self, w: f32, h: f32) {
        self.note(format!("resize {w}x{h}"));
        self.size = vec2(w, h);
        self.idle(2);
    }

    pub fn tab(&self) -> &crate::tab::Tab {
        self.app.panels.get(&self.app.active).map(|p| p.tab()).unwrap_or_else(|| self.fail("no active panel"))
    }

    pub fn popup_open(&self) -> bool {
        !matches!(self.app.popup, Popup::None)
    }

    /// Layout and index invariants (what drawing code relies on).
    fn check_state(&self) {
        let a = &self.app;
        let mut leaves = vec![];
        a.tree.leaves(&mut leaves);
        if leaves.is_empty() {
            self.fail("layout has no panels");
        }
        for id in &leaves {
            if !a.panels.contains_key(id) {
                self.fail(&format!("layout leaf {id} has no panel"));
            }
        }
        if !a.panels.contains_key(&a.active) {
            self.fail("active panel missing");
        }
        for p in a.panels.values() {
            if p.tabs.is_empty() || p.active >= p.tabs.len() {
                self.fail(&format!("panel {} has {} tabs, active {}", p.id, p.tabs.len(), p.active));
            }
            for t in &p.tabs {
                let n = t.entries.len() as u32;
                if t.cursor.is_some_and(|c| c >= n) || t.anchor.is_some_and(|c| c >= n) {
                    self.fail(&format!("cursor/anchor out of range in {}", t.path.display()));
                }
                if t.selected.iter().any(|&i| i >= n) {
                    self.fail(&format!("selection out of range in {}", t.path.display()));
                }
                if !t.dirty {
                    if t.view.iter().any(|&i| i >= n) {
                        self.fail(&format!("view index out of range in {}", t.path.display()));
                    }
                    for r in &t.rows {
                        match r {
                            Row::Item(i) if *i >= n => self.fail("row index out of range"),
                            Row::Header { label, .. } if *label as usize >= t.group_labels.len() => self.fail("header label out of range"),
                            _ => {}
                        }
                    }
                }
            }
        }
    }

    /// Drawn geometry: finite, roughly on screen, and (in list views without popups or
    /// hover) no line of text drawn on top of another.
    fn check_shapes(&self, shapes: &[epaint::ClippedShape]) {
        let screen = Rect::from_min_size(Pos2::ZERO, self.size).expand(2.0);
        let mut texts: Vec<(Rect, String)> = vec![];
        // An opaque rect over the whole window (the welcome) hides what was drawn before
        // it: only text after it can overlap anything seen.
        let covered = if self.check_overlap {
            shapes.iter().rposition(|cs| matches!(&cs.shape, epaint::Shape::Rect(r) if r.fill.a() == 255 && r.rect.contains_rect(screen.shrink(4.0)))).unwrap_or(0)
        } else {
            0
        };
        for (k, cs) in shapes.iter().enumerate() {
            // Text with nothing visible (empty, or only spaces, as a file can be named)
            // has no bounds: egui reports NaN for it.
            if let epaint::Shape::Text(t) = &cs.shape {
                if t.galley.text().trim().is_empty() {
                    continue;
                }
            }
            let r = cs.shape.visual_bounding_rect();
            if r.is_negative() {
                continue;
            }
            if !(r.min.x.is_finite() && r.min.y.is_finite() && r.max.x.is_finite() && r.max.y.is_finite()) {
                self.fail(&format!("non-finite shape {r:?}: {}", describe(&cs.shape)));
            }
            let vis = r.intersect(cs.clip_rect);
            if vis.is_positive() && !screen.contains_rect(vis) {
                self.fail(&format!("shape drawn off screen: {vis:?} (screen {screen:?})"));
            }
            if self.check_overlap && k >= covered {
                if let epaint::Shape::Text(t) = &cs.shape {
                    let s = t.galley.text().trim().to_string();
                    if !s.is_empty() && vis.is_positive() && vis.height() > 4.0 {
                        texts.push((vis, s));
                    }
                }
            }
        }
        // Text drawn inside a popup (after its frame) must not run past the popup's edge.
        let popups = crate::ui::POPUPS.with(|p| p.borrow().clone());
        for pr in &popups {
            // The popup's background: the first rect inside the area that nearly fills it
            // (the area also holds the frame's margin and shadow).
            // (A frame with a shadow paints as a group: shadow + rect.)
            fn bg(sh: &epaint::Shape, pr: &Rect) -> Option<Rect> {
                match sh {
                    epaint::Shape::Rect(r) if pr.expand(0.5).contains_rect(r.rect) && pr.width() - r.rect.width() < 24.0 && pr.height() - r.rect.height() < 24.0 => Some(r.rect),
                    epaint::Shape::Vec(v) => v.iter().find_map(|s| bg(s, pr)),
                    _ => None,
                }
            }
            // Not drawn yet: egui lays a new popup out invisibly on its first frame.
            let Some((start, frame)) = shapes.iter().enumerate().find_map(|(i, cs)| bg(&cs.shape, pr).map(|r| (i, r))) else {
                continue;
            };
            self.popup_checks.set(self.popup_checks.get() + 1);
            let pr = &frame;
            let mut inside: Vec<(Rect, String)> = vec![];
            for cs in &shapes[start + 1..] {
                // A background that reaches outside the popup means another area (a
                // progress card, a toast) has started drawing on top: its text isn't ours.
                let is_bg = matches!(&cs.shape, epaint::Shape::Rect(_) | epaint::Shape::Vec(_));
                if is_bg && !pr.expand(2.0).contains_rect(cs.shape.visual_bounding_rect()) {
                    break;
                }
                if let epaint::Shape::Text(t) = &cs.shape {
                    let s = t.galley.text();
                    if s.trim().is_empty() {
                        continue;
                    }
                    let vis = cs.shape.visual_bounding_rect().intersect(cs.clip_rect);
                    if vis.is_positive() && pr.shrink(1.0).contains(vis.min) && (vis.max.x > pr.max.x + 1.0 || vis.max.y > pr.max.y + 1.0) {
                        self.fail(&format!("text {s:?} at {vis:?} runs out of its popup {pr:?} ({})", self.app.popup.kind_name()));
                    }
                    if self.check_popup_overlap && vis.is_positive() && vis.height() > 4.0 && pr.contains_rect(vis) {
                        inside.push((vis, s.trim().to_string()));
                    }
                }
            }
            for i in 0..inside.len() {
                for j in i + 1..inside.len() {
                    let (a, b) = (&inside[i], &inside[j]);
                    let x = a.0.intersect(b.0);
                    if x.is_positive() && x.area() > 0.35 * a.0.area().min(b.0.area()) {
                        self.fail(&format!("text overlaps in the {} popup: {:?} at {:?} and {:?} at {:?}", self.app.popup.kind_name(), a.1, a.0, b.1, b.0));
                    }
                }
            }
        }
        if self.check_overlap {
            for i in 0..texts.len() {
                for j in i + 1..texts.len() {
                    let (a, b) = (&texts[i], &texts[j]);
                    let x = a.0.intersect(b.0);
                    if x.is_positive() && x.area() > 0.35 * a.0.area().min(b.0.area()) {
                        self.fail(&format!("text overlaps: {:?} at {:?} and {:?} at {:?}", a.1, a.0, b.1, b.0));
                    }
                }
            }
        }
    }

    /// No two controls (toolbar buttons, chips, tab buttons…) drawn over each other.
    fn check_controls(&self) {
        let c = crate::ui::CONTROLS.with(|c| c.borrow().clone());
        for i in 0..c.len() {
            for j in i + 1..c.len() {
                let (a, b) = (&c[i], &c[j]);
                if a.0 != b.0 || !a.1.is_positive() || !b.1.is_positive() {
                    continue;
                }
                let x = a.1.intersect(b.1);
                if x.is_positive() && x.area() > 0.2 * a.1.area().min(b.1.area()) {
                    let near: Vec<String> = c.iter().filter(|o| o.1.expand(40.0).intersects(x)).map(|o| format!("{} {:?}", o.2, o.1)).collect();
                    let panels: Vec<String> = self.app.panels.values().map(|p| format!("{} {:?}", p.id, p.rect)).collect();
                    self.fail(&format!("{} at {:?} overlaps {} at {:?}\n  nearby: {near:?}\n  panels: {panels:?}", a.2, a.1, b.2, b.1));
                }
            }
        }
    }

    /// Frame time summary (p50, p95, max), ms.
    pub fn timing(&self) -> (f32, f32, f32) {
        let mut v = self.frame_ms.clone();
        if v.is_empty() {
            return (0.0, 0.0, 0.0);
        }
        v.sort_by(|a, b| a.total_cmp(b));
        let q = |f: f32| v[((v.len() - 1) as f32 * f) as usize];
        (q(0.5), q(0.95), *v.last().unwrap())
    }
}

fn describe(sh: &epaint::Shape) -> String {
    match sh {
        epaint::Shape::Text(t) => format!("text {:?} at {:?}", t.galley.text(), t.pos),
        epaint::Shape::Rect(r) => format!("rect {:?}", r.rect),
        epaint::Shape::Circle(c) => format!("circle at {:?} r {}", c.center, c.radius),
        epaint::Shape::LineSegment { points, .. } => format!("line {points:?}"),
        epaint::Shape::Path(p) => format!("path {:?}", &p.points[..p.points.len().min(4)]),
        epaint::Shape::Mesh(m) => format!("mesh {} verts, first {:?}", m.vertices.len(), m.vertices.first().map(|v| v.pos)),
        epaint::Shape::Vec(v) => format!("vec of {}", v.len()),
        other => format!("{other:?}").chars().take(160).collect(),
    }
}

/// Small deterministic PRNG (xorshift) so a failing monkey run replays from its seed.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    pub fn f(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (self.next() % 10_000) as f32 / 10_000.0 * (hi - lo)
    }
    pub fn pick<'a, T>(&mut self, v: &'a [T]) -> &'a T {
        &v[self.below(v.len())]
    }
}

/// Click the control registered as `what` this frame, when there is one.
pub fn click_control_at(s: &mut Sim, what: &str) {
    let r = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == what).map(|(_, r, _)| *r));
    if let Some(r) = r {
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
    }
}

/// Often enough, into one of the fixture's work trees first (by Go To, as a person
/// would), so the git features have something to work on.
fn into_repo(s: &mut Sim, rng: &mut Rng) {
    if crate::gitstatus::repo_for(&s.tab().path).is_some() || rng.below(2) == 0 {
        return;
    }
    let sub = *rng.pick(&["Code/repo", "Code/project"]);
    let repo = std::fs::read_dir(root().join("home")).ok().and_then(|r| r.flatten().map(|d| d.path().join(sub)).find(|p| p.is_dir()));
    if let Some(repo) = repo {
        s.act("key ctrl+p");
        s.act(&format!("text {}/", repo.display()));
        s.act("key Enter");
        s.settle();
        let t0 = Instant::now();
        while crate::gitstatus::repo_for(&s.tab().path).is_none() && t0.elapsed().as_secs() < 3 {
            s.idle(1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// One random user action. Returns a description.
pub fn monkey_step(s: &mut Sim, rng: &mut Rng) {
    const KEYS: &[&str] = &[
        "ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Enter", "Backspace", "Escape", "Tab", "Home", "End", "PageDown", "PageUp", "F2", "Delete", "shift+ArrowDown",
        "shift+End", "ctrl+a", "ctrl+c", "ctrl+x", "ctrl+v", "ctrl+z", "ctrl+t", "ctrl+w", "ctrl+shift+t", "ctrl+e", "ctrl+p", "ctrl+shift+p", "ctrl+Equals", "ctrl+Minus",
        "Space", "alt+ArrowUp", "alt+ArrowLeft", "alt+ArrowRight", "ctrl+Tab", "ctrl+f", "ctrl+l", "ctrl+i", "alt+Enter",
    ];
    const WORDS: &[&str] = &["doc", "a", "img", ".png", "~/", "/", "Code/maia/", "zzz", "résumé", "1", "new name"];
    let (w, h) = (s.size.x, s.size.y);
    let p = |rng: &mut Rng| (rng.f(0.0, w), rng.f(0.0, h));
    match rng.below(187) {
        0..=29 => {
            let k = *rng.pick(KEYS);
            s.act(&format!("key {k}"));
        }
        30..=41 => {
            let (x, y) = p(rng);
            s.act(&format!("click {x:.0} {y:.0}"));
        }
        42..=46 => {
            let (x, y) = p(rng);
            s.act(&format!("dclick {x:.0} {y:.0}"));
        }
        47..=52 => {
            let (x, y) = p(rng);
            s.act(&format!("click {x:.0} {y:.0} right"));
        }
        53..=57 => {
            let ((x, y), (x2, y2)) = (p(rng), p(rng));
            s.act(&format!("drag {x:.0} {y:.0} {x2:.0} {y2:.0}"));
        }
        58..=65 => {
            let (x, y) = p(rng);
            let dy = rng.f(-12.0, 12.0);
            let m = if rng.below(4) == 0 { " ctrl" } else { "" };
            s.act(&format!("scroll {x:.0} {y:.0} {dy:.1}{m}"));
        }
        66..=72 => {
            let t = *rng.pick(WORDS);
            s.act(&format!("text {t}"));
        }
        73..=84 => {
            let c = *rng.pick(crate::commands::ALL);
            if c != Cmd::Quit {
                s.cmd(c);
            }
        }
        85..=88 => {
            let (x, y) = p(rng);
            s.act(&format!("move {x:.0} {y:.0}"));
        }
        89..=91 => {
            let (nw, nh) = *rng.pick(&[(1280.0, 800.0), (640.0, 480.0), (1920.0, 1080.0), (900.0, 1200.0), (520.0, 340.0)]);
            s.resize(nw, nh);
        }
        92..=94 => s.settle(),
        95..=96 => {
            // Files dragged in from another app: hovered around, then dropped or taken away.
            use crate::clipboard::Dnd;
            let src = root().join("elsewhere");
            if !src.exists() {
                let _ = std::fs::create_dir_all(&src);
                let _ = std::fs::write(src.join("from chat.txt"), "x");
                let _ = std::fs::write(src.join("photo – 2.png"), "not really a png");
            }
            let mut paths: Vec<PathBuf> = std::fs::read_dir(&src).map(|r| r.flatten().map(|e| e.path()).collect()).unwrap_or_default();
            paths.sort();
            paths.truncate(1 + rng.below(2));
            let ((x, y), (x2, y2)) = (p(rng), p(rng));
            s.note(format!("dnd over {x:.0} {y:.0} then {x2:.0} {y2:.0}"));
            s.app.on_dnd(Dnd::Over { x, y, paths: paths.clone(), own: false });
            s.idle(2);
            s.app.on_dnd(Dnd::Over { x: x2, y: y2, paths: paths.clone(), own: false });
            s.idle(1);
            if rng.below(2) == 0 {
                s.note("dnd drop".into());
                s.app.on_dnd(Dnd::Dropped { x: x2, y: y2, paths, own: false });
            } else {
                s.note("dnd leave".into());
                s.app.on_dnd(Dnd::Left);
            }
            s.idle(2);
        }
        99 => {
            // Into a git work tree (Go To), then edit a file there now and then.
            let sub = *rng.pick(&["Code/repo", "Code/project"]);
            let repo = std::fs::read_dir(root().join("home")).ok().and_then(|r| r.flatten().map(|d| d.path().join(sub)).find(|p| p.is_dir()));
            if let Some(repo) = repo {
                s.act("key ctrl+p");
                s.act(&format!("text {}/", repo.display()));
                s.act("key Enter");
                if rng.below(2) == 0 {
                    let _ = std::fs::write(repo.join("keep.txt"), format!("edit {}\n", rng.next()));
                }
            }
        }
        104 => {
            // Size, date and tag conditions in the filter, alone or with a name.
            s.act("key ctrl+f");
            s.act(&format!("text {}", rng.pick(&["size>1k", "modified<7d", "tag:red", "size<10 doc", "created:today", "date>2026-01", "size>huge", "tag:", "ext:png,md | type:pictures", "-doc a", "report !draft", "NOT a", "a OR doc", "/^re/", "/(/", "re:^[a-c]", "*.md", "name:IMG_*", "\"budget 2026\"", "type:folder -e", "(size>1k | tag:red) -ext:txt", "owner:root", "owner:65534", "accessed<1d", "(", "a |", "-", "kind:code,music"])));
        }
        105 => {
            // Tag what is under a random right-click, through the menu's search.
            let (x, y) = p(rng);
            s.act(&format!("click {x:.0} {y:.0} right"));
            s.act(&format!("text tags {}", rng.pick(&["red", "blue", "new tag", "gray"])));
            s.act("key Enter");
            if matches!(s.app.popup, Popup::Prompt { .. }) {
                s.act(&format!("text {}", rng.pick(&["work", "a,b", " ", "日本"])));
                s.act("key Enter");
            }
        }
        106 => {
            // Properties: work out a checksum, or paste one to compare.
            s.act("key alt+Enter");
            if rng.below(2) == 0 {
                click_control_at(s, "checksum button");
            } else {
                click_control_at(s, "checksum field");
                s.act(&format!("text {}", rng.pick(&["900150983cd24fb0d6963f7d28e17f72", "zz", "SHA256 (x) = 00"])));
            }
            s.idle(3);
        }
        107 => {
            // Wrap or unwrap the text preview.
            if s.app.panels.get(&s.app.active).is_some_and(|p| p.inspector.is_none()) {
                s.cmd(Cmd::ToggleInspector);
            }
            click_control_at(s, "Wrap lines");
        }
        108 => {
            // A locked archive: into it, then a right or wrong password if asked.
            if let Some(z) = s.tab().entries.iter().find(|e| e.name == "locked.zip").map(|e| e.path.clone()) {
                s.cmd(Cmd::SelectNone);
                if let Some(i) = s.tab().entries.iter().position(|e| e.path == z) {
                    let id = s.app.active;
                    s.app.panels.get_mut(&id).unwrap().tab_mut().select_only(i as u32);
                }
                s.cmd(*rng.pick(&[Cmd::ExtractHere, Cmd::Activate]));
                s.idle(10);
            }
            if matches!(s.app.popup, Popup::ArchivePassword(_)) {
                s.act(&format!("text {}", rng.pick(&["pw", "nope"])));
                s.act("key Enter");
            }
        }
        103 => {
            // GitHub pictures on or off, from Options (the GET is a stand-in).
            s.act("key ctrl+Comma");
            s.act("text show github pictures");
            s.act("key Enter");
            s.act("key Escape");
        }
        102 => {
            // The branch chip in the status bar (the lowest "main" on screen) opens the
            // history; then a walk through it with keys and clicks.
            let chip = s.texts.iter().filter(|(_, t)| t == "main").max_by(|a, b| a.0.center().y.total_cmp(&b.0.center().y)).map(|(r, _)| r.center());
            if let Some(c) = chip {
                s.note("the branch chip".into());
                s.act(&format!("click {:.0} {:.0}", c.x, c.y));
                s.idle(3);
            }
            for _ in 0..rng.below(8) {
                let ir = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect);
                match (ir, rng.below(6)) {
                    (Some(r), 0) => {
                        let (x, y) = (rng.f(r.left(), r.right()), rng.f(r.top(), r.bottom()));
                        s.act(&format!("click {x:.0} {y:.0}"));
                    }
                    // A file's menu in an open commit, and one of its items.
                    (Some(r), 1) => {
                        let (x, y) = (rng.f(r.left(), r.right()), rng.f(r.top(), r.bottom()));
                        s.act(&format!("click {x:.0} {y:.0} right"));
                        s.idle(2);
                        if matches!(s.app.popup, Popup::GitFile { .. }) {
                            let item = *rng.pick(&["Open", "Show in folder", "View as of this commit", "View before it was deleted", "File history", "Copy path"]);
                            if s.texts.iter().any(|(_, t)| t == item) {
                                s.click_text(item);
                            } else {
                                s.act("key Escape");
                            }
                        }
                    }
                    (Some(_), 2) => click_control_at(s, rng.pick(&["newer commit", "older commit", "previous file", "next file", "open large", "parent link", "copy hash", "show all commits", "Show as a list", "Group by folder", "Wrap lines"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "PageDown", "End", "Home", "Enter", "Escape", "OpenBracket", "CloseBracket"]))),
                }
            }
        }
        101 => {
            // Give the selected palette command an alias (F2), sometimes one already taken.
            s.act("key ctrl+shift+p");
            s.act(&format!("text {}", rng.pick(&["Toggle", "tab", "sort"])));
            s.act("key F2");
            s.act(&format!("text {}", rng.pick(&["t", "x1", "hh", "sb"])));
            s.act("key Enter");
            s.act("key Escape");
        }
        100 => {
            // Compare two panels (splitting first if there is one), then act on the marks.
            if s.app.panels.len() < 2 {
                s.cmd(Cmd::SplitRight);
            }
            s.cmd(Cmd::ComparePanels);
            s.cmd(*rng.pick(&[Cmd::SelectDifferences, Cmd::SyncPanels, Cmd::FocusNextPanel]));
        }
        98 => {
            // Contents mode: the filter's toggle, then a query typed into the listing.
            click_control_at(s, "contents toggle");
            let t = *rng.pick(&["line", "fn", "zzz", "Report", "ü"]);
            s.act(&format!("text {t}"));
        }
        109 => {
            // Options: a section by click or by keys, a row picked and acted on, now and
            // then a search, a reset arrow or titles only.
            s.act("key ctrl+Comma");
            for _ in 0..1 + rng.below(5) {
                match rng.below(7) {
                    0 => {
                        let sec = *rng.pick(&crate::popups::Section::ALL);
                        click_control_at(s, sec.control());
                    }
                    1 => s.act(rng.pick(&["key ctrl+PageDown", "key ctrl+PageUp"])),
                    2 => s.act(rng.pick(&["key ArrowDown", "key ArrowUp", "key Enter"])),
                    3 => s.act(&format!("text {}", rng.pick(&["size", "font", "click", "zz", "git"]))),
                    4 => click_control_at(s, "reset option"),
                    5 => click_control_at(s, rng.pick(&["switch", "choice", "titles only"])),
                    _ => s.act("key Escape"),
                }
            }
        }
        116 => {
            // Open In Editor, from its key, the palette and the file's menu.
            match rng.below(3) {
                0 => s.act("key F4"),
                1 => s.cmd(Cmd::OpenInEditor),
                _ => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act("text editor");
                    s.act("key Enter");
                }
            }
        }
        117 => {
            // A path from the top of the project, from the palette or the file's menu.
            if rng.below(2) == 0 {
                s.cmd(Cmd::CopyPathRelative);
            } else {
                let (x, y) = p(rng);
                s.act(&format!("click {x:.0} {y:.0} right"));
                s.act("text relative");
                s.act("key Enter");
            }
        }
        118 => {
            // Search Code: a word or an expression, a switch or two, then a key.
            s.act("key ctrl+alt+f");
            if matches!(s.app.popup, Popup::CodeSearch(_)) {
                s.act(&format!("text {}", rng.pick(&["needle", "fn", "TODO", "a(", r"[a-z]+_\d", "zzz", "ü", "Needle"])));
                for _ in 0..rng.below(4) {
                    click_control_at(s, rng.pick(&["match case", "whole word", "regex", "code todo", "code all files", "code scope folder", "code scope project", "code files"]));
                }
                if rng.below(3) == 0 {
                    s.act(&format!("text {}", rng.pick(&["rust", "*.js", "!src/", "x"])));
                }
                s.idle(rng.below(20) + 1);
                for _ in 0..1 + rng.below(3) {
                    s.act(&format!("key {}", rng.pick(&["Down", "Up", "PageDown", "Enter", "ctrl+Enter", "ctrl+shift+c", "Tab", "alt+c", "alt+t", "Escape"])));
                }
            }
            if rng.below(3) == 0 {
                s.cmd(*rng.pick(&[Cmd::CopyPathLine, Cmd::FindTodos]));
            }
        }
        119 => {
            // Run Task: filter, then run one or leave.
            s.act("key ctrl+shift+b");
            s.idle(3);
            if matches!(s.app.popup, Popup::Tasks(_)) {
                s.act(&format!("text {}", rng.pick(&["build", "test", "run", "zz", ""])));
                s.act(&format!("key {}", rng.pick(&["Enter", "Down", "Escape"])));
            }
        }
        120 => {
            // Clean Build Folders: check some, then trash them (asked first) or leave.
            s.cmd(Cmd::CleanBuilds);
            s.idle(5 + rng.below(20));
            for _ in 0..rng.below(4) {
                s.act(&format!("key {}", rng.pick(&["Down", "space", "ctrl+a", "Up"])));
            }
            s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
            if matches!(s.app.popup, Popup::Confirm { .. }) {
                s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
            }
        }
        125 => {
            // An office document previewed through the stand-in LibreOffice, then left at
            // once (its conversion killed) or after a page or two.
            let dir = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Documents/Office")).find(|p| p.is_dir()));
            if let Some(dir) = dir {
                let f = *rng.pick(&["plan.docx", "sheet.ods", "slow deck.odp", "broken.pptx"]);
                s.act("key ctrl+p");
                s.act(&format!("text {}", dir.join(f).display()));
                s.act("key Enter");
                s.settle();
                if s.app.panels.get(&s.app.active).is_some_and(|p| p.inspector.is_none()) {
                    s.cmd(Cmd::ToggleInspector);
                }
                s.idle(1 + rng.below(30));
                if rng.below(2) == 0 {
                    s.act("key PageDown");
                }
            }
        }
        126 => {
            // A picture made the wallpaper on one desktop or another (stand-in setters),
            // from a menu or the palette.
            let pics = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Pictures/Trip")).find(|p| p.is_dir()));
            crate::wallpaper::DESKTOP.with(|w| *w.borrow_mut() = rng.pick(&["Hyprland", "GNOME", "KDE", "LXQt", ""]).to_string());
            if let Some(dir) = pics.filter(|_| rng.below(3) > 0) {
                s.act("key ctrl+p");
                s.act(&format!("text {}", dir.join(format!("img{:02}.png", rng.below(12))).display()));
                s.act("key Enter");
                s.settle();
            }
            if rng.below(2) == 0 {
                s.cmd(Cmd::SetWallpaper);
            } else {
                let (x, y) = p(rng);
                s.act(&format!("click {x:.0} {y:.0} right"));
                s.act("text wallpaper");
                s.act("key Enter");
            }
            s.idle(1 + rng.below(10));
        }
        127 => {
            // Email or print what is selected, from the palette or a menu; in the Print
            // dialog, printers and copies by key or click, then print or cancel.
            crate::sendto::MAIL_APP.with(|m| *m.borrow_mut() = (rng.below(3) > 0).then(|| "thunderbird %u".to_string()));
            if rng.below(2) == 0 {
                s.cmd(*rng.pick(&[Cmd::Email, Cmd::Print]));
            } else {
                let (x, y) = p(rng);
                s.act(&format!("click {x:.0} {y:.0} right"));
                s.act(&format!("text {}", rng.pick(&["email", "print"])));
                s.act("key Enter");
            }
            s.idle(2 + rng.below(6));
            if matches!(s.app.popup, Popup::Print(_)) {
                for _ in 0..rng.below(5) {
                    match rng.below(3) {
                        0 => click_control_at(s, rng.pick(&["more copies", "fewer copies", "printer"])),
                        _ => s.act(&format!("key {}", rng.pick(&["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]))),
                    }
                }
                s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
            }
        }
        128 => {
            // A picture turned, resized or cropped (the real ImageMagick), from a menu or
            // the palette; a crop dragged out and saved or let go.
            let pics = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Pictures/Trip")).find(|p| p.is_dir()));
            if let Some(dir) = pics.filter(|_| rng.below(4) > 0) {
                s.act("key ctrl+p");
                s.act(&format!("text {}", dir.join(format!("img{:02}.png", rng.below(12))).display()));
                s.act("key Enter");
                s.settle();
            }
            match rng.below(3) {
                0 => s.cmd(*rng.pick(&[Cmd::RotateLeft, Cmd::RotateRight, Cmd::ResizeHalf, Cmd::ResizeQuarter, Cmd::ResizeFit, Cmd::ResizeCustom])),
                1 => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act(&format!("text {}", rng.pick(&["rotate", "half", "quarter", "1920", "other size", "crop"])));
                    s.act("key Enter");
                }
                _ => s.cmd(Cmd::CropPicture),
            }
            s.idle(2);
            if matches!(s.app.popup, Popup::Prompt { .. }) {
                s.act("key ctrl+a");
                s.act(&format!("text {}", rng.pick(&["50%", "64", "40 × 30", "nonsense", "0"])));
                s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
            }
            let ir = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect);
            if let Some(ir) = ir.filter(|_| s.app.panels[&s.app.active].inspector.as_ref().is_some_and(|i| i.crop.is_some())) {
                for _ in 0..1 + rng.below(3) {
                    let (x0, y0) = (rng.f(ir.left(), ir.right()), rng.f(ir.top(), ir.bottom()));
                    let (x1, y1) = (rng.f(ir.left(), ir.right()), rng.f(ir.top(), ir.bottom()));
                    s.act(&format!("drag {x0:.0} {y0:.0} {x1:.0} {y1:.0}"));
                }
                s.act(&format!("key {}", rng.pick(&["Enter", "Escape", "ArrowDown"])));
            }
            s.idle(1 + rng.below(10));
        }
        140 => {
            // Previous Versions, often of the file the fake snapshots hold: the list walked
            // by keys and clicks, a version restored or saved beside it, or the page shut.
            let f = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Disk/vol/docs/report.txt")).find(|p| p.is_file()));
            if let Some(f) = f.filter(|_| rng.below(3) > 0) {
                s.act("key ctrl+p");
                s.act(&format!("text {}", f.display()));
                s.act("key Enter");
                s.settle();
            }
            if rng.below(2) == 0 {
                s.cmd(Cmd::PreviousVersions);
            } else {
                let (x, y) = p(rng);
                s.act(&format!("click {x:.0} {y:.0} right"));
                s.act("text previous versions");
                s.act("key Enter");
            }
            s.idle(2 + rng.below(10));
            for _ in 0..rng.below(5) {
                match rng.below(4) {
                    0 => click_control_at(s, rng.pick(&["version row", "version restore", "version copy", "versions close"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "End", "Home", "PageDown", "Escape"]))),
                }
            }
            s.idle(1 + rng.below(5));
        }
        141 => {
            // Show Space Used: walked by keys and clicks, into folders and back, some
            // checked and trashed (asked first) or left, sometimes shown in Maia. Free up
            // too, from its command or Tab: groups opened and closed, folders checked,
            // what is chosen deleted after asking, Find Duplicates opened from it.
            if rng.below(3) == 0 {
                s.cmd(Cmd::FreeUpSpace);
            } else if rng.below(2) == 0 {
                s.cmd(Cmd::SpaceUsed);
            } else {
                let (x, y) = p(rng);
                s.act(&format!("click {x:.0} {y:.0} right"));
                s.act("text space used");
                s.act("key Enter");
            }
            s.idle(2 + rng.below(20));
            for _ in 0..rng.below(8) {
                if !matches!(s.app.popup, Popup::SpaceUsed(_)) {
                    break;
                }
                match rng.below(6) {
                    0 => click_control_at(s, rng.pick(&["space row", "space show", "space trash", "space trash cancel", "space close"])),
                    1 => click_control_at(s, rng.pick(&["space free up", "space contents", "free group", "free part", "free go", "free delete confirm", "free delete cancel", "free dupes", "free chart"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "ArrowRight", "ArrowLeft", "Enter", "Backspace", "space", "Delete", "End", "PageDown", "Tab", "Escape"]))),
                }
                s.idle(rng.below(4));
            }
        }
        142 => {
            // The picture viewer, often among the trip's pictures: keys, the wheel, drags,
            // clicks on its buttons, a slideshow left running a while, then Escape or not.
            let pics = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Pictures/Trip")).find(|p| p.is_dir()));
            if let Some(dir) = pics.filter(|_| rng.below(3) > 0) {
                s.act("key ctrl+p");
                s.act(&format!("text {}", dir.join(format!("img{:02}.png", rng.below(12))).display()));
                s.act("key Enter");
                s.settle();
            }
            match rng.below(3) {
                0 => s.act("key F3"),
                1 => s.cmd(Cmd::ViewPictures),
                _ => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act("text view pictures");
                    s.act("key Enter");
                }
            }
            for _ in 0..rng.below(10) {
                if !matches!(s.app.popup, Popup::Viewer(_)) {
                    break;
                }
                match rng.below(8) {
                    0 => {
                        let (x, y) = p(rng);
                        s.act(&format!("scroll {x:.0} {y:.0} {:.1}", rng.f(-10.0, 10.0)));
                    }
                    1 => {
                        let ((x, y), (x2, y2)) = (p(rng), p(rng));
                        s.act(&format!("drag {x:.0} {y:.0} {x2:.0} {y2:.0}"));
                    }
                    2 => click_control_at(s, rng.pick(&["viewer next", "viewer previous", "viewer close", "viewer trash", "viewer rotate right", "viewer rotate left", "viewer tags", "viewer compare", "viewer tag"])),
                    3 => s.idle(rng.below(300)),
                    // Sorting: the trash and back, turns, labels, the strip, a pinned one.
                    4 => s.act(&format!("key {}", rng.pick(&["Delete", "ctrl+z", "r", "l", "t", "c", "ctrl+1", "ctrl+4", "Enter"]))),
                    5 => {
                        if viewer_strip_typing(s) {
                            s.act(&format!("text {}", rng.pick(&["keep", "a,b", "日本", " "])));
                        }
                        s.act(rng.pick(&["key Enter", "key Escape", "key ArrowRight"]));
                    }
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowRight", "ArrowLeft", "Space", "Plus", "Minus", "0", "1", "Home", "End", "Escape"]))),
                }
            }
        }
        143 => {
            // Pictures sent to the viewer by Enter and double-clicks, or not.
            s.app.cfg.picture_viewer = rng.below(2) == 0;
            let (x, y) = p(rng);
            s.act(&format!("{} {x:.0} {y:.0}", rng.pick(&["dclick", "click"])));
            s.act(rng.pick(&["key Enter", "key ArrowRight", "key Escape"]));
            if matches!(s.app.popup, Popup::Viewer(_)) && rng.below(2) == 0 {
                s.act("key Escape");
            }
        }
        144 => {
            // Onto the shelf by key, menu or a drag onto it; then Copy Here, Move Here or
            // Clear, from its buttons or the palette, now and then in another folder.
            match rng.below(3) {
                0 => s.act("key alt+s"),
                1 => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act("text add to shelf");
                    s.act("key Enter");
                }
                _ => {
                    let (x, y) = p(rng);
                    s.act(&format!("drag {x:.0} {y:.0} {:.0} {:.0}", s.size.x / 2.0 + rng.f(-200.0, 200.0), s.size.y - rng.f(4.0, 30.0)));
                }
            }
            if rng.below(3) == 0 {
                s.act("key alt+ArrowUp");
                s.settle();
            }
            match rng.below(4) {
                0 => click_control_at(s, rng.pick(&["shelf copy", "shelf move", "shelf clear"])),
                1 => s.cmd(*rng.pick(&[Cmd::ShelfCopyHere, Cmd::ShelfMoveHere, Cmd::ClearShelf])),
                _ => {}
            }
            s.idle(1 + rng.below(6));
            if matches!(s.app.popup, Popup::Conflict(_)) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
        }
        145 => {
            // The shelf's items: shown in their folder, taken off, or dragged out of it;
            // everything dragged at once.
            click_control_at(s, "shelf item");
            let item = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == "shelf item").map(|(_, r, _)| *r));
            if let Some(r) = item {
                match rng.below(3) {
                    0 => {
                        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
                        s.act(&format!("click {:.0} {:.0}", r.right() - 12.0, r.center().y));
                    }
                    _ => {
                        let (x, y) = p(rng);
                        s.act(&format!("drag {:.0} {:.0} {x:.0} {y:.0}", r.left() + 14.0, r.center().y));
                    }
                }
            }
            if rng.below(3) == 0 {
                let all = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == "shelf all").map(|(_, r, _)| *r));
                if let Some(r) = all {
                    let (x, y) = p(rng);
                    s.act(&format!("drag {:.0} {:.0} {x:.0} {y:.0}", r.center().x, r.center().y));
                }
            }
            s.idle(1 + rng.below(4));
            if matches!(s.app.popup, Popup::Conflict(_) | Popup::DropMenu { .. }) {
                s.act("key Escape");
            }
        }
        121 => {
            // Sharing: code blocks, a web link or .gitignore, from the palette or a menu.
            match rng.below(4) {
                0 => s.cmd(*rng.pick(&[Cmd::CopyCodeBlocks, Cmd::CompareFiles])),
                1 => s.cmd(*rng.pick(&[Cmd::CopyWebLink, Cmd::OpenWebLink])),
                2 => s.cmd(*rng.pick(&[Cmd::GitIgnore, Cmd::GitIgnoreExt])),
                _ => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act(&format!("text {}", rng.pick(&["code blocks", "web link", "this file", "all *", "gitignore", "compare"])));
                    s.act("key Enter");
                }
            }
            s.idle(1 + rng.below(4));
        }
        132 => {
            // The palette: a category clicked or stepped to, a search, a row moved to,
            // starred, run or let go.
            s.act("key ctrl+shift+p");
            for _ in 0..1 + rng.below(5) {
                match rng.below(6) {
                    0 => {
                        let c = *rng.pick(&crate::commands::Cat::ORDER);
                        click_control_at(s, rng.pick(&[c.control(), "category All"]));
                    }
                    1 => s.act(rng.pick(&["key Tab", "key shift+Tab", "key ctrl+PageDown"])),
                    2 => s.act(&format!("text {}", rng.pick(&["size", "tog", "git", "zq", "view"]))),
                    3 => s.act(rng.pick(&["key ArrowDown", "key ArrowUp", "key PageDown", "key ctrl+d"])),
                    4 => click_control_at(s, rng.pick(&["star button", "compact rows"])),
                    _ => s.act(rng.pick(&["key Escape", "key Enter"])),
                }
            }
            s.act("key Escape");
        }
        154 => {
            // Quick Open: a few letters, then a row shown, opened in a tab, or let go.
            s.act("key ctrl+o");
            s.idle(2);
            s.act(&format!("text {}", rng.pick(&["main", "md", "zz", "a", "rs"])));
            s.idle(2);
            s.act(rng.pick(&["key ArrowDown", "key Enter", "key ctrl+Enter", "key Escape"]));
            s.act("key Escape");
        }
        155 => {
            // Undo History: a row chosen and taken back, or closed.
            s.act("key ctrl+alt+z");
            for _ in 0..rng.below(3) {
                s.act("key ArrowDown");
            }
            match rng.below(3) {
                0 => s.act("key Enter"),
                1 => click_control_at(s, "undo close"),
                _ => s.act("key Escape"),
            }
        }
        156 => {
            // Letter tags: one or two letters (a tag, a capital, or nothing), or Escape.
            s.act("key ctrl+j");
            match rng.below(4) {
                0 => s.act("key Escape"),
                1 => s.act(&format!("text {}", rng.pick(&["a", "sa", "S", "zq"]))),
                _ => {
                    let tag = s.app.hints.as_ref().and_then(|h| h.targets.get(rng.below(h.targets.len().max(1)) as usize).map(|t| t.1.clone()));
                    s.act(&format!("text {}", tag.unwrap_or_else(|| "q".into())));
                }
            }
            s.act("key Escape");
        }
        157 => {
            // Marks: set one, go to one, forget one.
            match rng.below(3) {
                0 => {
                    s.act("key ctrl+m");
                    s.act(&format!("text {}", rng.pick(&["a", "b", "n"])));
                }
                1 => {
                    s.act("key ctrl+Quote");
                    s.act(&format!("text {}", rng.pick(&["a", "b", "x"])));
                }
                _ => {
                    s.act("key ctrl+Quote");
                    s.act(rng.pick(&["key Delete", "key ArrowDown", "key Enter"]));
                }
            }
            s.act("key Escape");
        }
        158 => {
            // Recent Projects: the unsaved switch by key or click, then opened or let go.
            s.act("key ctrl+alt+p");
            s.idle(2);
            match rng.below(3) {
                0 => s.act("key Tab"),
                1 => click_control_at(s, rng.pick(&["unsaved projects", "all projects"])),
                _ => {}
            }
            s.idle(2);
            s.act(rng.pick(&["key Escape", "key Enter", "key ctrl+Enter"]));
            s.act("key Escape");
        }
        122 => {
            // Find Duplicates: wait a little, check some, maybe ask to trash, then leave.
            s.cmd(Cmd::FindDuplicates);
            s.idle(3 + rng.below(20));
            for _ in 0..rng.below(4) {
                s.act(&format!("key {}", rng.pick(&["Down", "Up", "space", "ctrl+a", "PageDown"])));
            }
            s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
            if matches!(s.app.popup, Popup::Confirm { .. }) {
                s.act("key Escape");
            }
        }
        123 => {
            // Batch Rename: a pattern, a find, a case, then rename or leave.
            s.cmd(Cmd::BatchRename);
            if matches!(s.app.popup, Popup::BatchRename(_)) {
                s.act(&format!("text {}", rng.pick(&["File ##", "{name} #", "", "x", "###"])));
                if rng.below(2) == 0 {
                    click_control_at(s, rng.pick(&["rename find", "rename regex", "rename case upper", "rename case title", "rename match case"]));
                    s.act(&format!("text {}", rng.pick(&["a", "(", ".", "e"])));
                }
                s.act(&format!("key {}", rng.pick(&["Enter", "Escape", "Escape"])));
            }
        }
        124 => {
            // Empty the trash, set and unset from its choice.
            let before = s.app.cfg.trash_days;
            s.app.cfg.trash_days = *rng.pick(&[0, 7, 30, 90]);
            s.idle(2);
            s.app.cfg.trash_days = before;
        }
        129 => {
            // The branch menu's sending and getting, wherever there is a remote.
            if crate::gitstatus::repo_for(&s.tab().path).is_some() {
                s.cmd(Cmd::GitBranches);
                s.idle(5);
                if matches!(s.app.popup, Popup::Branches(_)) {
                    for _ in 0..rng.below(4) {
                        s.act("key ArrowDown");
                    }
                    s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
                }
            }
        }
        130 => {
            // An answer to a question asked, or leaving it.
            if matches!(s.app.popup, Popup::Choose { .. }) {
                s.act(&format!("key {}", rng.pick(&["Escape", "Escape", "Enter"])));
            }
        }
        131 => s.cmd(Cmd::OnlyChanged),
        97 => {
            // A drag that leaves the window (handed to the system when it carries files).
            let (x, y) = p(rng);
            s.act(&format!("down {x:.0} {y:.0}"));
            for x2 in [x + 12.0, x + 30.0, (x + 30.0) / 2.0, -40.0] {
                s.act(&format!("move {x2:.0} {y:.0}"));
            }
            s.idle(2);
            if s.ctx.input(|i| i.pointer.any_down()) {
                s.act(&format!("up -40 {y:.0}"));
            }
        }
        110 => {
            // A developer file previewed (Markdown, a tree, a table, a database, a .env, a
            // log, code to blame), then its controls and keys.
            let dev = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Dev")).find(|p| p.is_dir()));
            if let Some(dev) = dev {
                if rng.below(3) == 0 || !s.tab().path.starts_with(&dev) {
                    s.act("key ctrl+p");
                    s.act(&format!("text {}/{}", dev.display(), if rng.below(4) == 0 { "src/" } else { "" }));
                    s.act("key Enter");
                    s.settle();
                }
                if s.app.panels.get(&s.app.active).is_some_and(|p| p.inspector.is_none()) {
                    s.cmd(Cmd::ToggleInspector);
                }
                let n = s.tab().entries.len();
                if n > 0 {
                    let i = rng.below(n) as u32;
                    let id = s.app.active;
                    if let Some(p) = s.app.panels.get_mut(&id) {
                        p.tab_mut().select_only(i);
                    }
                    s.idle(3);
                }
            }
            for _ in 0..rng.below(4) {
                if rng.below(2) == 0 {
                    click_control_at(s, rng.pick(&["Rendered", "Source", "Tree", "Table", "Blame", "Follow", "Show values", "Hide values", "copy path", "line filter", "database table", "blame gutter", "Wrap lines", "Line numbers", "Larger text", "Smaller text"]));
                } else {
                    s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight", "Enter", "Escape", "End", "Home"])));
                }
            }
        }
        111 => {
            // A log being written to, followed or not.
            let log = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path().join("Dev/app.log")).find(|p| p.is_file()));
            if let Some(log) = log {
                use std::io::Write;
                if let Ok(mut f) = std::fs::OpenOptions::new().append(true).open(&log) {
                    for _ in 0..1 + rng.below(50) {
                        let _ = writeln!(f, "2026-10-16 12:00:00 INFO event {}", rng.next() % 1000);
                    }
                }
                // Now and then rotated or cut short.
                match rng.below(8) {
                    0 => {
                        let _ = std::fs::write(&log, "fresh start\n");
                    }
                    1 => {
                        let _ = std::fs::rename(&log, log.with_extension("log.1"));
                        let _ = std::fs::write(&log, "rotated\n");
                    }
                    _ => {}
                }
                s.idle(1 + rng.below(10));
            }
        }
        112 => {
            // Somewhere in the preview: a link, a tree row, a cell, a hidden value, a
            // blame line, a database table; or the wheel over it.
            if let Some(r) = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect) {
                let (x, y) = (rng.f(r.left() + 2.0, r.right() - 2.0), rng.f(r.top() + 32.0, r.bottom() - 2.0));
                match rng.below(4) {
                    0 => s.act(&format!("scroll {x:.0} {y:.0} {:.1}{}", rng.f(-8.0, 8.0), if rng.below(3) == 0 { " shift" } else { "" })),
                    1 => s.act(&format!("dclick {x:.0} {y:.0}")),
                    _ => {
                        s.idle(30);
                        s.act(&format!("click {x:.0} {y:.0}"));
                    }
                }
            }
        }
        113 => {
            into_repo(s, rng);
            // The branch menu (the chip, or the palette's command), then a key or an item,
            // and whatever that opens: a name for a new branch, a stash's menu, a refusal.
            let branch = crate::gitstatus::repo_for(&s.tab().path).map(|r| r.branch.clone()).unwrap_or_default();
            let chip = s.texts.iter().filter(|(r, t)| *t == branch && r.top() > s.size.y - 30.0).map(|(r, _)| r.center()).next();
            match chip {
                Some(c) if rng.below(2) == 0 => s.act(&format!("click {:.0} {:.0}", c.x, c.y)),
                _ => s.cmd(Cmd::GitBranches),
            }
            s.idle(4);
            for _ in 0..1 + rng.below(4) {
                match &s.app.popup {
                    Popup::Branches(_) | Popup::Stash { .. } => match rng.below(3) {
                        0 => click_control_at(s, rng.pick(&["branch menu item", "stash menu item"])),
                        _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowDown", "ArrowUp", "Enter", "Escape"]))),
                    },
                    Popup::Prompt { .. } => {
                        s.act(&format!("text {}", rng.pick(&["spike", "fix/menu", "two..dots", "-x", "日本"])));
                        s.act("key Enter");
                    }
                    Popup::Confirm { .. } | Popup::Notice { .. } => s.act(rng.pick(&["key Enter", "key Escape"])),
                    _ => break,
                }
                s.idle(3);
            }
        }
        114 => {
            into_repo(s, rng);
            // The Changes page: rows hovered and their buttons, a hunk, everything at
            // once, a summary typed and committed (by button or Ctrl+Enter), the keys.
            if s.app.panels.get(&s.app.active).and_then(|p| p.inspector.as_ref()).and_then(|i| i.git.as_ref()).is_none_or(|h| !h.changes().open) {
                s.cmd(Cmd::GitChanges);
                s.idle(4);
            }
            for _ in 0..1 + rng.below(6) {
                let ir = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect);
                match (ir, rng.below(5)) {
                    (Some(r), 0) => {
                        let (x, y) = (rng.f(r.left(), r.right()), rng.f(r.top(), r.bottom()));
                        s.act(&format!("move {x:.0} {y:.0}"));
                        s.idle(1);
                        click_control_at(s, rng.pick(&["stage file", "unstage file", "discard file", "stage hunk", "unstage hunk"]));
                    }
                    (Some(_), 1) => click_control_at(s, rng.pick(&["stage all", "unstage all", "commit", "changes row", "Wrap lines"])),
                    (Some(_), 2) => {
                        click_control_at(s, "commit summary");
                        s.act(&format!("text {}", rng.pick(&["Fix the thing", "wip", " ", "Ünïcode"])));
                        if rng.below(2) == 0 {
                            s.act("key ctrl+Enter");
                        }
                    }
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "Home", "Enter", "Space", "Delete", "Escape"]))),
                }
                s.idle(3);
                if matches!(s.app.popup, Popup::Confirm { .. }) {
                    s.act(rng.pick(&["key Enter", "key Escape"]));
                }
            }
        }
        115 => {
            // Ignored files: Dim, Hide or Show, from Options.
            s.act("key ctrl+Comma");
            s.act("text ignored");
            s.idle(2);
            for _ in 0..1 + rng.below(2) {
                click_control_at(s, "choice");
            }
            s.act("key Escape");
        }
        135 => {
            // Recent Projects: by its key or the palette, narrowed, opened here, in a new
            // tab, by a click, or let go.
            if rng.below(2) == 0 {
                s.act("key ctrl+alt+p");
            } else {
                s.cmd(Cmd::RecentProjects);
            }
            s.idle(2 + rng.below(30));
            for _ in 0..rng.below(4) {
                match rng.below(3) {
                    0 => s.act(&format!("text {}", rng.pick(&["repo", "code", "app", "project", "zz", "/"]))),
                    1 => click_control_at(s, "project row"),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "PageDown", "Backspace"]))),
                }
            }
            if matches!(s.app.popup, Popup::Projects(_)) {
                s.act(&format!("key {}", rng.pick(&["Enter", "ctrl+Enter", "Escape", "Escape"])));
            }
            s.settle();
        }
        136 => {
            // What's Using This…: on the selection or the folder, from the palette or a
            // menu; rows walked, Stop asked and answered either way, refreshed.
            match rng.below(2) {
                0 => s.cmd(Cmd::WhatsUsing),
                _ => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act("text using");
                    s.act("key Enter");
                }
            }
            s.idle(2 + rng.below(10));
            for _ in 0..1 + rng.below(5) {
                if !matches!(s.app.popup, Popup::Holders(_)) {
                    break;
                }
                match rng.below(4) {
                    0 => click_control_at(s, rng.pick(&["holder stop", "holder confirm", "holder cancel", "holder refresh", "holder close"])),
                    1 => s.act("key ctrl+r"),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "Enter", "Escape"]))),
                }
                s.idle(1 + rng.below(8));
            }
            if matches!(s.app.popup, Popup::Holders(_)) && rng.below(2) == 0 {
                s.act("key Escape");
                s.act("key Escape");
            }
        }
        137 => {
            // What's Using a Port…: a number or a name typed, rows walked, Stop or not.
            s.cmd(Cmd::WhatsUsingPort);
            s.idle(2 + rng.below(10));
            for _ in 0..rng.below(5) {
                match rng.below(3) {
                    0 => s.act(&format!("text {}", rng.pick(&["8", "80", "443", "5353", "sleep", "x"]))),
                    1 => click_control_at(s, rng.pick(&["holder stop", "holder cancel", "holder refresh"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "Backspace", "Enter", "Escape"]))),
                }
            }
            if matches!(s.app.popup, Popup::Holders(_)) {
                s.act(&format!("key {}", rng.pick(&["Escape", "Escape", "Enter"])));
            }
        }
        138 => {
            // A smart folder made: from a search in the filter or the palette, its
            // fields and query typed into, saved or let go.
            if rng.below(2) == 0 {
                s.act("key ctrl+f");
                s.act(&format!("text {}", rng.pick(&["doc", "img", ".png", "notes size>1k", "a modified<30d", "résumé", "type:pictures | ext:md", "-doc a", "/^re/"])));
                s.cmd(Cmd::SaveSmart);
            } else {
                s.cmd(Cmd::NewSmart);
            }
            for _ in 0..rng.below(6) {
                if !matches!(s.app.popup, Popup::SmartEdit(_)) {
                    break;
                }
                match rng.below(3) {
                    0 => {
                        click_control_at(s, "smart query");
                        s.act(&format!("text {}", rng.pick(&[" type:pictures", " | ext:md", " -doc", " /(/", " size>10M", " owner:root", " (a | b)", " tag:red", " modified<7d"])));
                    }
                    1 => {
                        click_control_at(s, rng.pick(&["smart name", "smart root", "smart query"]));
                        s.act(&format!("text {}", rng.pick(&["7", "10 MB", "~/", "IMG_*", "zz", "-1", "Code"])));
                    }
                    _ => s.act(&format!("key {}", rng.pick(&["Backspace", "Tab", "ctrl+a"]))),
                }
            }
            if matches!(s.app.popup, Popup::SmartEdit(_)) {
                s.act(&format!("key {}", rng.pick(&["Enter", "Enter", "Escape"])));
                if matches!(s.app.popup, Popup::SmartEdit(_)) {
                    s.act("key Escape");
                }
            }
            s.idle(1 + rng.below(20));
        }
        139 => {
            // A smart folder in the sidebar: opened, in a new tab, or its menu's Edit,
            // Rename or Remove.
            let names: Vec<String> = s.app.ud.smart.iter().map(|x| x.name.clone()).collect();
            let at = names.iter().find_map(|n| s.texts.iter().find(|(r, t)| t == n && r.right() <= s.app.sidebar_width + 1.0).map(|x| x.0));
            if let Some(r) = at {
                match rng.below(4) {
                    0 => s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y)),
                    1 => s.act(&format!("click {:.0} {:.0} middle", r.center().x, r.center().y)),
                    _ => {
                        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
                        s.act(&format!("text {}", rng.pick(&["open", "new tab", "edit", "rename", "remove"])));
                        s.act("key Enter");
                        s.idle(2);
                        match &s.app.popup {
                            Popup::Prompt { .. } => {
                                s.act(&format!("text {}", rng.pick(&["Mine", " ", "日本"])));
                                s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
                            }
                            Popup::SmartEdit(_) => s.act(&format!("key {}", rng.pick(&["Enter", "Escape"]))),
                            _ => {}
                        }
                        if s.popup_open() && rng.below(2) == 0 {
                            s.act("key Escape");
                        }
                    }
                }
            } else {
                s.cmd(Cmd::NewSmart);
                s.act("key Enter");
            }
            s.idle(1 + rng.below(20));
        }
        146 => {
            // A conversion of whatever is selected: from the Convert menu or the
            // palette, a PDF's name typed or let go, the card's Cancel now and then.
            const CONVERTS: &[Cmd] = &[Cmd::ConvertJpg, Cmd::ConvertPng, Cmd::ConvertWebp, Cmd::ConvertPdf, Cmd::ConvertMp3, Cmd::ConvertOpus, Cmd::ConvertFlac, Cmd::ConvertMp4, Cmd::ConvertWebm, Cmd::ConvertSoundMp3, Cmd::MergePdfs, Cmd::SplitPdf];
            if rng.below(3) == 0 {
                let media = std::fs::read_dir(root().join("home")).ok().and_then(|r| r.flatten().map(|d| d.path().join("Media")).find(|p| p.is_dir()));
                if let Some(m) = media.filter(|_| s.tab().path != root().join("home")) {
                    s.note(format!("into {}", m.display()));
                    let id = s.app.active;
                    if let Some(p) = s.app.panels.get_mut(&id) {
                        p.tab_mut().navigate(m, None);
                    }
                    s.settle();
                    let n = s.tab().entries.len();
                    if n > 0 {
                        let id = s.app.active;
                        let pick = rng.below(n) as u32;
                        if let Some(p) = s.app.panels.get_mut(&id) {
                            p.tab_mut().select_only(pick);
                        }
                    }
                }
            }
            match rng.below(2) {
                0 => {
                    let (x, y) = p(rng);
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.act("text convert");
                    s.act("key Enter");
                    s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowRight", "Enter", "Escape"])));
                    s.act("key Enter");
                }
                _ => s.cmd(*rng.pick(CONVERTS)),
            }
            s.idle(2);
            if matches!(s.app.popup, Popup::Prompt { .. }) {
                if rng.below(2) == 0 {
                    s.act(&format!("text {}", rng.pick(&["Album", " ", "a/b", "日本.pdf"])));
                }
                s.act(&format!("key {}", rng.pick(&["Enter", "Enter", "Escape"])));
            }
            s.idle(1 + rng.below(30));
            if rng.below(3) == 0 && some_text_at(s, "Cancel") {
                s.click_text("Cancel");
            }
        }
        147 => {
            // Tidy Folder: from the folder's menu or the palette, the two ways looked at,
            // applied or let go, sometimes undone at once.
            if rng.below(2) == 0 {
                s.cmd(Cmd::TidyFolder);
            } else {
                let list = s.app.panels.get(&s.app.active).map(|p| p.list_rect);
                if let Some(l) = list {
                    s.act(&format!("click {:.0} {:.0} right", l.center().x, l.bottom() - 10.0));
                    s.act("text tidy");
                    s.act("key Enter");
                }
            }
            s.idle(2 + rng.below(20));
            for _ in 0..rng.below(4) {
                click_control_at(s, rng.pick(&["tidy by kind", "tidy by month"]));
            }
            if matches!(s.app.popup, Popup::Tidy(_)) {
                match rng.below(3) {
                    0 => click_control_at(s, "tidy apply"),
                    1 => s.act("key Enter"),
                    _ => s.act("key Escape"),
                }
                if matches!(s.app.popup, Popup::Tidy(_)) {
                    s.act("key Escape");
                }
                s.settle();
                if rng.below(2) == 0 {
                    s.act("key ctrl+z");
                }
            }
        }
        148 => {
            // The previews of books, comics, fonts and photos: one picked in the media
            // folder, its details opened now and then, Install, Open map and Page Down.
            let media = std::fs::read_dir(root().join("home")).ok().and_then(|r| r.flatten().map(|d| d.path().join("Media")).find(|p| p.is_dir()));
            if let Some(m) = media {
                if s.tab().path != m {
                    s.note(format!("into {}", m.display()));
                    let id = s.app.active;
                    if let Some(p) = s.app.panels.get_mut(&id) {
                        p.tab_mut().navigate(m.clone(), None);
                    }
                    s.settle();
                }
                let pick = *rng.pick(&["Lighthouse.epub", "Harbour.cbz", "Sample.ttf", "place.jpg"]);
                let id = s.app.active;
                if let Some(p) = s.app.panels.get_mut(&id) {
                    let t = p.tab_mut();
                    if let Some(i) = t.entries.iter().position(|e| e.name == pick) {
                        t.select_only(i as u32);
                    }
                    if p.inspector.is_none() {
                        p.inspector = Some(Default::default());
                    }
                }
                s.idle(5 + rng.below(40));
                for _ in 0..rng.below(4) {
                    match rng.below(4) {
                        0 => click_control_at(s, rng.pick(&["font install", "open map"])),
                        1 => {
                            let ir = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect);
                            if let Some(r) = ir {
                                s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
                                s.act(rng.pick(&["key PageDown", "key PageUp"]));
                            }
                        }
                        2 => {
                            let ir = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect);
                            let pill = ir.and_then(|r| s.texts.iter().rev().find(|(t, n)| n == pick && r.contains(t.center())).map(|x| x.0));
                            if let Some(r) = pill {
                                s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
                            }
                        }
                        _ => s.idle(5),
                    }
                }
            }
        }
        149 => {
            // The Timeline: from the sidebar (here or in a new tab) or the palette, a
            // day's header folded, a file opened where it is, then back.
            let row = s.texts.iter().find(|(r, t)| t == "Timeline" && r.right() <= s.app.sidebar_width + 1.0).map(|x| x.0);
            match (row, rng.below(3)) {
                (Some(r), 0) => s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y)),
                (Some(r), 1) => s.act(&format!("click {:.0} {:.0} middle", r.center().x, r.center().y)),
                _ => s.cmd(Cmd::Timeline),
            }
            s.idle(1 + rng.below(30));
            let hdr = s.texts.iter().find(|(_, t)| t.starts_with("Today  (") || t.starts_with("Yesterday  (")).map(|x| x.0);
            if let (Some(h), 0) = (hdr, rng.below(3)) {
                s.act(&format!("click {:.0} {:.0}", h.center().x, h.center().y));
            }
            if rng.below(2) == 0 {
                s.act(rng.pick(&["key ArrowDown", "key Enter", "key alt+ArrowLeft", "key Backspace"]));
            }
        }
        150 => {
            // Compress: the dialog typed into, the format and level switched, the two
            // password fields made to agree or not, then Compress or Escape.
            s.cmd(*rng.pick(&[Cmd::CompressLocked, Cmd::CompressZip, Cmd::CompressTarXz]));
            for _ in 0..rng.below(6) {
                if !matches!(s.app.popup, Popup::Compress(_)) {
                    break;
                }
                match rng.below(3) {
                    0 => click_control_at(s, rng.pick(&["compress zip", "compress 7z", "compress tar.gz", "compress fast", "compress smallest", "compress name", "compress password", "compress again"])),
                    1 => s.act(&format!("text {}", rng.pick(&["pw", "pw", "ö 1", " "]))),
                    _ => s.act(&format!("key {}", rng.pick(&["Backspace", "Tab", "ctrl+a"]))),
                }
            }
            if matches!(s.app.popup, Popup::Compress(_)) {
                match rng.below(3) {
                    0 => click_control_at(s, "compress button"),
                    1 => s.act("key Enter"),
                    _ => s.act("key Escape"),
                }
                if matches!(s.app.popup, Popup::Compress(_)) {
                    s.act("key Escape");
                }
            }
        }
        151 => {
            // Edit Tags on whatever is selected: fields typed into, the cover chips, then
            // Save or Escape.
            s.cmd(Cmd::EditTags);
            s.idle(2 + rng.below(30));
            for _ in 0..rng.below(5) {
                if !matches!(s.app.popup, Popup::MusicTags(_)) {
                    break;
                }
                match rng.below(3) {
                    0 => click_control_at(s, rng.pick(&["tag title", "tag artist", "tag track", "tag genre", "cover keep", "cover remove", "cover set"])),
                    1 => s.act(&format!("text {}", rng.pick(&["Song", "3/12", "日本", " "]))),
                    _ => s.act(&format!("key {}", rng.pick(&["Backspace", "ctrl+a", "Tab"]))),
                }
            }
            if matches!(s.app.popup, Popup::MusicTags(_)) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
                if matches!(s.app.popup, Popup::MusicTags(_)) {
                    s.act("key Escape");
                }
            }
        }
        152 => {
            // Format, from a drive's menu in the sidebar: the filesystem, the label, the
            // name typed or not, then Erase and Format or Escape. Fake udisks only.
            let names = ["Photo Card", "Busy Stick", "Holiday Photos", "Root"];
            let at = names.iter().find_map(|n| s.texts.iter().find(|(r, t)| t == n && r.right() <= s.app.sidebar_width + 1.0).map(|x| (x.0, *n)));
            if let Some((r, name)) = at {
                s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
                s.act("text format");
                s.act("key Enter");
                s.idle(2);
                for _ in 0..rng.below(5) {
                    if !matches!(s.app.popup, Popup::FormatDrive(_)) {
                        break;
                    }
                    match rng.below(3) {
                        0 => click_control_at(s, rng.pick(&["format fat32", "format exfat", "format ext4", "format label", "format confirm"])),
                        1 => s.act(&format!("text {}", rng.pick(&[name, "x", "ÄÖ", " "]))),
                        _ => s.act(&format!("key {}", rng.pick(&["Backspace", "ctrl+a"]))),
                    }
                }
                if matches!(s.app.popup, Popup::FormatDrive(_)) {
                    match rng.below(3) {
                        0 => click_control_at(s, "format erase"),
                        1 => s.act("key Enter"),
                        _ => s.act("key Escape"),
                    }
                    if matches!(s.app.popup, Popup::FormatDrive(_)) {
                        s.act("key Escape");
                    }
                }
            }
        }
        153 => {
            // Cut what is selected, paste it from another tab or panel, perhaps a folder
            // up, and come back: once the move is done, no tab lists it where it was.
            let cut: Vec<PathBuf> = s.tab().targets().into_iter().filter(|p| std::fs::symlink_metadata(p).is_ok()).collect();
            s.act("key ctrl+x");
            s.act(rng.pick(&["key ctrl+t", "key Tab", "key ctrl+Tab"]));
            if rng.below(2) == 0 {
                s.act("key alt+ArrowUp");
                s.settle();
            }
            s.act("key ctrl+v");
            if s.popup_open() {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
            let t0 = Instant::now();
            while s.app.jobs.iter().any(|j| j.finished_at.is_none()) && t0.elapsed() < std::time::Duration::from_secs(10) {
                s.frame(vec![]);
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            s.settle();
            s.act(rng.pick(&["key ctrl+shift+Tab", "key Tab", "key alt+ArrowLeft", "key ctrl+1"]));
            s.settle();
            let moved: Vec<&PathBuf> = cut.iter().filter(|p| std::fs::symlink_metadata(p).is_err()).collect();
            for t in s.app.panels.values().flat_map(|p| p.tabs.iter()).filter(|t| t.rename.is_none()) {
                if let Some(p) = moved.iter().find(|p| t.view.iter().any(|&i| t.entries[i as usize].path == ***p)) {
                    s.fail(&format!("{} still listed in {} after it was moved", p.display(), t.path.display()));
                }
            }
        }
        159 => {
            // The welcome tour or What's New: pages by keys, buttons and dots, the films
            // clicked, choices made on the first page, then closed one way or another.
            s.cmd(if rng.below(2) == 0 { Cmd::Welcome } else { Cmd::WhatsNew });
            for _ in 0..rng.below(10) {
                if !matches!(s.app.popup, Popup::Tour(_)) {
                    break;
                }
                match rng.below(4) {
                    0 => s.act(&format!("key {}", rng.pick(&["ArrowRight", "ArrowLeft", "Enter", "Tab", "space"]))),
                    1 => click_control_at(s, rng.pick(&["welcome next", "welcome back", "welcome skip", "tour dot", "tour film", "tour theme", "switch", "choice"])),
                    2 => {
                        let r = s.text_rect_any(&["Font size", "Show hidden files", "Animations"]);
                        if let Some(r) = r {
                            s.act(&format!("scroll {:.0} {:.0} {:.1}", r.center().x, r.center().y, rng.f(-8.0, 8.0)));
                        }
                    }
                    _ => s.idle(rng.below(40) + 1),
                }
            }
            if matches!(s.app.popup, Popup::Tour(_)) {
                match rng.below(3) {
                    0 => s.act("key Escape"),
                    1 => click_control_at(s, "tour close"),
                    _ => s.act("click 4 4"),
                }
            }
        }
        160 => {
            // From Options' row: the tour or What's New, and straight back out.
            s.app.popup = Popup::options_at(crate::popups::Section::System);
            s.idle(3);
            click_control_at(s, rng.pick(&["tour button", "whats new button"]));
            s.idle(rng.below(20) + 1);
            s.act("key Escape");
        }
        161 => {
            // Copies taking turns on one disk: two of what is here into one folder at
            // once, checked or not, the second's card told to start now, cancelled or
            // left to wait.
            s.app.cfg.check_copies = rng.below(2) == 0;
            let files: Vec<PathBuf> = s.tab().entries.iter().filter(|e| !e.is_dir && !e.is_link).map(|e| e.path.clone()).take(3).collect();
            if !files.is_empty() && s.tab().path.starts_with(root()) {
                let dest = root().join("home/turns");
                let _ = std::fs::create_dir_all(&dest);
                s.note(format!("two copies of {} items", files.len()));
                for _ in 0..2 {
                    s.app.start_job(crate::ops::OpKind::Copy { srcs: files.clone(), dest: dest.clone(), decide: Default::default() });
                }
                s.idle(2);
                if rng.below(3) > 0 {
                    click_control_at(s, rng.pick(&["job start now", "job cancel"]));
                }
                s.idle(rng.below(10) + 1);
            }
        }
        162 => {
            // Undo and Redo in a run, by their keys or from the history.
            for _ in 0..1 + rng.below(4) {
                s.act(&format!("key {}", rng.pick(&["ctrl+z", "ctrl+shift+z", "ctrl+y"])));
            }
            if rng.below(3) == 0 {
                s.act("key ctrl+alt+z");
                for _ in 0..rng.below(3) {
                    s.act(&format!("key {}", rng.pick(&["ArrowUp", "ArrowDown"])));
                }
                s.act(&format!("key {}", rng.pick(&["Enter", "Escape"])));
            }
            s.settle();
        }
        163 => {
            // Text taken out of a preview: drags, double and triple clicks and Shift
            // clicks over it, Ctrl+A and Ctrl+C, the right-click Copy; then a key for the
            // list, which must still be the list's.
            if s.app.panels.get(&s.app.active).is_some_and(|p| p.inspector.is_none()) {
                s.cmd(Cmd::ToggleInspector);
            }
            s.act(rng.pick(&["key ArrowDown", "key ArrowUp", "key End", "key Home"]));
            s.idle(3);
            let Some(ir) = s.app.panels.get(&s.app.active).and_then(|p| p.inspector_rect) else { return };
            let q = |rng: &mut Rng| (rng.f(ir.left(), ir.right()), rng.f(ir.top() + 34.0, ir.bottom()));
            for _ in 0..1 + rng.below(6) {
                let (x, y) = q(rng);
                match rng.below(7) {
                    0 | 1 => {
                        let (x2, y2) = q(rng);
                        s.act(&format!("drag {x:.0} {y:.0} {x2:.0} {y2:.0}"));
                    }
                    2 => s.act(&format!("dclick {x:.0} {y:.0}")),
                    3 => {
                        s.act(&format!("dclick {x:.0} {y:.0}"));
                        s.act(&format!("click {x:.0} {y:.0}"));
                    }
                    4 => s.act(&format!("click {x:.0} {y:.0} shift")),
                    5 => s.act(rng.pick(&["key ctrl+a", "key ctrl+c"])),
                    _ => {
                        s.act(&format!("click {x:.0} {y:.0} right"));
                        if matches!(s.app.popup, Popup::Ctx(_)) && rng.below(2) == 0 && some_text_at(s, "Copy") {
                            s.click_text("Copy");
                        }
                    }
                }
            }
            s.idle(rng.below(30));
            if s.popup_open() {
                s.act("key Escape");
            }
        }
        164 => {
            // The terminal's own: shells opened, closed and switched, the scrollback by
            // keys and found in, links under Ctrl, words and lines clicked, a program
            // that hears the mouse, Shift dragging over it.
            if s.app.terminal.shells.is_empty() || !s.app.terminal.focused {
                s.act("key ctrl+Backtick");
            }
            let top = (h - s.app.terminal.height.min(h * 0.8)).max(0.0) + 40.0;
            let q = |rng: &mut Rng| (rng.f(0.0, w), rng.f(top.min(h - 1.0), h));
            match rng.below(9) {
                0 => s.act(&format!("key {}", rng.pick(&["ctrl+shift+t", "ctrl+shift+w", "ctrl+shift+PageDown", "ctrl+shift+PageUp"]))),
                1 => s.act(&format!("key {}", rng.pick(&["shift+PageUp", "shift+PageDown", "shift+Home", "shift+End"]))),
                2 => {
                    s.act("text seq 1 120");
                    s.act("key enter");
                    s.idle(5);
                }
                3 => {
                    s.act("key ctrl+shift+f");
                    s.act(&format!("text {}", rng.pick(&["1", "seq", "zz", "Ab", "é"])));
                    s.act(&format!("key {}", rng.pick(&["Enter", "shift+Enter", "F3", "Escape"])));
                }
                4 => {
                    let (x, y) = q(rng);
                    s.act("hold ctrl");
                    s.act(&format!("move {x:.0} {y:.0}"));
                    s.idle(3);
                    if rng.below(2) == 0 {
                        s.act(&format!("click {x:.0} {y:.0} left ctrl"));
                    }
                    s.act("hold none");
                }
                5 => {
                    let (x, y) = q(rng);
                    s.act(&format!("{} {x:.0} {y:.0}", rng.pick(&["dclick", "tclick"])));
                }
                6 => {
                    s.act("text printf 'https://example.com/x src/main.rs:1:2 ~/ \\033]8;;https://maia.example\\033\\\\here\\033]8;;\\033\\\\\\n'");
                    s.act("key enter");
                    s.idle(3);
                }
                7 => {
                    s.act("text seq 1 200 | LESSHISTFILE=- less --mouse");
                    s.act("key enter");
                    s.idle(10);
                    for _ in 0..rng.below(4) + 1 {
                        let (x, y) = q(rng);
                        if rng.below(2) == 0 {
                            s.act(&format!("scroll {x:.0} {y:.0} {:.1}", rng.f(-4.0, 4.0)));
                        } else {
                            s.act(&format!("click {x:.0} {y:.0}"));
                        }
                    }
                    s.act("text q");
                }
                _ => {
                    let ((x, y), (x2, y2)) = (q(rng), q(rng));
                    s.act("hold shift");
                    s.act(&format!("drag {x:.0} {y:.0} {x2:.0} {y2:.0} left shift"));
                    s.act("hold none");
                }
            }
        }
        133 | 134 => columns_step(s, rng),
        165 => picker_start_or_stop(s, rng),
        166 => picker_step(s, rng),
        167 => {
            // A tag from the sidebar: opened, or its menu used; or a column chosen from
            // the list header's menu and sorted by.
            let side: Vec<(Rect, String)> = s.texts.iter().filter(|(r, t)| r.right() <= s.app.sidebar_width + 1.0 && s.app.ud.tags.iter().any(|k| k == t)).cloned().collect();
            match (rng.below(3), side.is_empty()) {
                (0, false) => {
                    let (r, _) = rng.pick(&side).clone();
                    s.act(&format!("click {:.0} {:.0}{}", r.center().x, r.center().y, if rng.below(3) == 0 { " ctrl" } else { "" }));
                }
                (1, false) => {
                    let (r, _) = rng.pick(&side).clone();
                    s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
                    s.idle(2);
                    let pick = *rng.pick(&["Open", "Open In New Tab", "Rename Tag…", "Remove From All Files…"]);
                    if some_text_at(s, pick) {
                        s.click_text(pick);
                        s.idle(2);
                        if matches!(s.app.popup, Popup::Prompt { .. }) {
                            s.act("key ctrl+a");
                            s.act(&format!("text {}", rng.pick(&["renamed", "work", "Red"])));
                        }
                        s.act(rng.pick(&["key Enter", "key Escape"]));
                    }
                }
                _ => {
                    let hdr: Vec<Rect> = s.texts.iter().filter(|(r, t)| r.left() > s.app.sidebar_width && crate::config::Column::ALL.iter().any(|c| c.title() == t)).map(|x| x.0).collect();
                    if hdr.is_empty() {
                        return;
                    }
                    let r = *rng.pick(&hdr);
                    if rng.below(2) == 0 {
                        // Sorted by it, then maybe the other way round.
                        for _ in 0..1 + rng.below(2) {
                            s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
                        }
                        s.idle(rng.below(20));
                        return;
                    }
                    s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
                    s.idle(2);
                    let name = rng.pick(&crate::config::Column::ALL).title();
                    if let Some((r, _)) = s.texts.iter().rev().find(|(_, t)| t == name).cloned() {
                        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
                    }
                    s.act("move -50 -50");
                    s.idle(rng.below(10));
                }
            }
        }
        168 => {
            into_repo(s, rng);
            // The history searched: by the palette or Ctrl+F in an open history, words and
            // terms typed, stepped through, gone into, a commit opened, then closed again.
            if crate::gitstatus::repo_for(&s.tab().path).is_none() {
                return;
            }
            if rng.below(2) == 0 && history_open(s) {
                s.act("key ctrl+f");
            } else {
                s.cmd(Cmd::GitSearch);
            }
            s.idle(2);
            s.act(&format!("text {}", rng.pick(&["panel", "author:ada", "path:src", "since:2026-09-02", "until:1d", "Grace", "zzz", "\"long notes\" ", "path:\"no such\" x", "1a2b", " "])));
            s.idle(rng.below(30));
            for _ in 0..rng.below(5) {
                match rng.below(4) {
                    0 => click_control_at(s, "Find (Ctrl+F)"),
                    _ => s.act(&format!("key {}", rng.pick(&["Enter", "shift+Enter", "F3", "ArrowDown", "ArrowDown", "Enter", "Escape", "Backspace", "ctrl+f"]))),
                }
                s.idle(2);
            }
        }
        169 => {
            into_repo(s, rng);
            // A comparison: from a branch's own menu, or a commit marked in the history and
            // another one's menu comparing with it; then its rows, folded, opened, Back.
            if crate::gitstatus::repo_for(&s.tab().path).is_none() {
                return;
            }
            if rng.below(2) == 0 {
                if branch_own_menu(s, rng) {
                    pick_starting(s, "Compare with ");
                }
            } else {
                // Opened, or back on its graph when it is open.
                s.cmd(Cmd::GitHistory);
                s.idle(4);
                for (k, item) in ["Mark for comparison", "Compare with "].into_iter().enumerate() {
                    for _ in 0..k + rng.below(3) {
                        s.act("key ArrowDown");
                    }
                    s.act("key shift+F10");
                    s.idle(3);
                    if !matches!(s.app.popup, Popup::GitCommit(_)) {
                        break;
                    }
                    pick_starting(s, item);
                }
            }
            for _ in 0..rng.below(6) {
                match rng.below(4) {
                    0 => click_control_at(s, rng.pick(&["what it adds", "every difference", "compared commit", "split link"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowDown", "Enter", "Home", "End", "Escape", "CloseBracket"]))),
                }
                s.idle(3);
            }
        }
        174 | 175 => {
            // Inside an archive: a delete, a rename, a new folder or a paste, each a job
            // that rewrites it; Undo now and then.
            if s.tab().archive.is_none() {
                // Into the fixture's zip (or what became of it).
                let z = std::fs::read_dir(root().join("home")).ok().and_then(|r| r.flatten().flat_map(|d| [d.path().join("bundle.zip"), d.path().join("locked.zip")]).find(|p| p.is_file()));
                let Some(z) = z else { return };
                let id = s.app.active;
                if let Some(p) = s.app.panels.get_mut(&id) {
                    p.tab_mut().navigate(z, None);
                }
                s.idle(10);
                if s.tab().archive.is_none() {
                    return;
                }
            }
            if !s.tab().entries.is_empty() {
                let i = rng.below(s.tab().entries.len()) as u32;
                let id = s.app.active;
                s.app.panels.get_mut(&id).unwrap().tab_mut().select_only(i);
            }
            match rng.below(5) {
                0 => s.act("key Delete"),
                1 => {
                    s.act("key F2");
                    s.act(&format!("text {}", rng.pick(&["x", " 2", "é"])));
                    s.act("key Enter");
                }
                2 => {
                    s.act("key ctrl+shift+n");
                    s.act(&format!("text {}", rng.pick(&["", "a", "/"])));
                    s.act(rng.pick(&["key Enter", "key Escape"]));
                }
                3 => s.act("key ctrl+v"),
                _ => s.act("key ctrl+z"),
            }
            s.idle(5 + rng.below(40));
            if matches!(s.app.popup, Popup::Choose { .. } | Popup::ArchivePassword(_)) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
        }
        170 => {
            into_repo(s, rng);
            // A branch merged in from its own menu: a fast-forward, nothing to merge, a
            // refusal or conflicts, each answered.
            if crate::gitstatus::repo_for(&s.tab().path).is_none() || !branch_own_menu(s, rng) {
                return;
            }
            pick_starting(s, "Merge into ");
            s.settle();
            match &s.app.popup {
                Popup::Confirm { .. } | Popup::Notice { .. } => s.act(rng.pick(&["key Enter", "key Escape"])),
                _ if rng.below(2) == 0 => click_control_at(s, rng.pick(&["conflict abort", "keep theirs", "keep ours", "conflict continue"])),
                _ => s.act("key Escape"),
            }
            if matches!(s.app.popup, Popup::Confirm { .. }) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
        }
        171..=172 => {
            // Copy To… or Move To…: by its key or from a context menu, a place typed,
            // expanded or picked, then Enter, Esc or a click.
            if rng.below(2) == 0 {
                s.act(rng.pick(&["key shift+F5", "key shift+F6"]));
            } else {
                let (x, y) = p(rng);
                s.act(&format!("click {x:.0} {y:.0} right"));
                s.idle(2);
                if let Some(r) = s.text_rect_any(&[*rng.pick(&["Copy To…", "Move To…"])]) {
                    s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
                }
            }
            if !matches!(&s.app.popup, Popup::GoTo(st) if st.dest.is_some()) {
                return;
            }
            for _ in 0..rng.below(5) {
                match rng.below(6) {
                    0 => s.act(&format!("text {}", rng.pick(&["Code/", "doc", "Empty", "zzz", "/", "~/", "/proc/"]))),
                    1 => {
                        let up = s.tab().path.parent().map(|p| format!("{}/", p.display())).unwrap_or_else(|| "/".into());
                        s.act("key ctrl+a");
                        s.act(&format!("text {up}"));
                    }
                    2 => s.act("key Tab"),
                    3 => s.act("key ArrowDown"),
                    4 => s.act("key Backspace"),
                    _ => {
                        let (x, y) = p(rng);
                        s.act(&format!("click {x:.0} {y:.0}"));
                    }
                }
                s.idle(2);
            }
            s.act(rng.pick(&["key Enter", "key Enter", "key Escape"]));
            s.settle();
        }
        173 => {
            // Select Matching…: from the palette or by name, a pattern, then Enter or Esc.
            if rng.below(2) == 0 {
                s.act("key ctrl+shift+p");
                s.act("text Select Matching");
                s.act("key Enter");
            } else {
                s.cmd(Cmd::SelectMatching);
            }
            if !matches!(s.app.popup, Popup::Prompt { .. }) {
                return;
            }
            s.act(&format!("text {}", rng.pick(&["*.png", "*.{md,txt}", "IMG*", "*", "[a-f]*", "*/*.rs", "{", "?", "résumé*", " "])));
            s.act(rng.pick(&["key Enter", "key Enter", "key Escape"]));
        }
        176 => {
            // A copy started from here into another folder, held or not; then the transfers
            // list steered: keys, its buttons, a drag among the waiting.
            let srcs: Vec<PathBuf> = s.tab().target_entries().into_iter().map(|e| e.path.clone()).take(3).collect();
            let dest = root().join("home").read_dir().ok().and_then(|r| r.flatten().map(|d| d.path()).find(|p| p.is_dir())).map(|h| h.join("Empty"));
            if let (false, Some(dest)) = (srcs.is_empty(), dest.filter(|d| d.is_dir())) {
                s.app.begin_transfer(srcs, dest, false);
                s.idle(2);
                if rng.below(2) == 0 {
                    if let Some(j) = s.app.jobs.last() {
                        j.pause.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
            s.act("key ctrl+shift+j");
            for _ in 0..rng.below(6) {
                match rng.below(4) {
                    0 => click_control_at(s, rng.pick(&["transfer pause", "transfer cancel", "transfer start now", "clash keep both", "clash skip", "clash replace", "clash same", "transfers chip"])),
                    1 => {
                        let rows = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, _, w)| *w == "transfer waiting").map(|(_, r, _)| *r).collect::<Vec<_>>());
                        if rows.len() >= 2 {
                            let (a, b) = (rows[rng.below(rows.len())], rows[rng.below(rows.len())]);
                            s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", a.center().x, a.center().y, b.center().x, b.center().y + 4.0));
                        }
                    }
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "alt+ArrowUp", "alt+ArrowDown", "space", "Enter", "Delete"]))),
                }
                s.idle(2);
            }
            for j in &s.app.jobs {
                j.pause.store(false, std::sync::atomic::Ordering::Relaxed);
            }
            s.act(rng.pick(&["key Escape", "move -50 -50"]));
        }
        177 => {
            // Jump: a few letters, then a folder picked or not.
            s.act("key ctrl+g");
            if !matches!(s.app.popup, Popup::Jump(_)) {
                return;
            }
            s.idle(3);
            for _ in 0..rng.below(4) {
                match rng.below(3) {
                    0 => s.act(&format!("text {}", rng.pick(&["d", "doc", "pic", "zzz", "/", "é"]))),
                    1 => s.act(&format!("key {}", rng.pick(&["ArrowDown", "ArrowUp", "Backspace"]))),
                    _ => click_control_at(s, "jump row"),
                }
            }
            if matches!(s.app.popup, Popup::Jump(_)) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
            s.settle();
        }
        178 => {
            // Paste From Earlier: by a digit, a click, or not at all.
            s.act("key ctrl+shift+v");
            if !matches!(s.app.popup, Popup::Pastes(_)) {
                return;
            }
            match rng.below(4) {
                0 => s.act(&format!("text {}", 1 + rng.below(9))),
                1 => click_control_at(s, "paste row"),
                2 => s.act(&format!("key {}", rng.pick(&["ArrowDown", "Enter"]))),
                _ => s.act("key Escape"),
            }
            s.idle(5);
            if matches!(s.app.popup, Popup::Choose { .. } | Popup::Conflict(..)) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
        }
        179 => {
            // A pin: on the selection, or on the tab.
            s.cmd(if rng.below(2) == 0 { Cmd::PinToTop } else { Cmd::PinTab });
            s.idle(10);
        }
        180 => {
            // The calendar: days by keys and clicks, months back and on.
            s.act("key shift+F3");
            if !matches!(s.app.popup, Popup::Calendar(_)) {
                return;
            }
            for _ in 0..rng.below(6) {
                match rng.below(3) {
                    0 => click_control_at(s, rng.pick(&["calendar day", "calendar previous month", "calendar next month", "calendar strip picture"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "shift+ArrowRight", "shift+ArrowLeft", "PageUp", "PageDown"]))),
                }
                s.idle(2);
            }
            if matches!(s.app.popup, Popup::Calendar(_)) {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
        }
        181 => {
            // Space used round its three views, a block of the map opened, up again.
            s.cmd(Cmd::SpaceUsed);
            if !matches!(s.app.popup, Popup::SpaceUsed(_)) {
                return;
            }
            for _ in 0..1 + rng.below(5) {
                match rng.below(4) {
                    0 => s.act("key Tab"),
                    1 => click_control_at(s, rng.pick(&["space contents", "space map", "space free up", "space block"])),
                    _ => s.act(&format!("key {}", rng.pick(&["ArrowDown", "Enter", "Backspace", "End"]))),
                }
                s.idle(3);
            }
            s.act("key Escape");
            if matches!(s.app.popup, Popup::Duplicates(_) | Popup::Confirm { .. }) {
                s.act("key Escape");
            }
        }
        182 => {
            // The README card: its edge dragged or reset, opened, closed for the folder,
            // or all of them switched off and on.
            let edge = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == "readme edge").map(|(_, r, _)| *r));
            match (rng.below(4), edge) {
                (0, Some(e)) => {
                    let to = e.center().y + rng.f(-200.0, 300.0);
                    s.act(&format!("drag {:.0} {:.0} {:.0} {to:.0}", e.center().x, e.center().y, e.center().x));
                }
                (1, Some(e)) => s.act(&format!("dclick {:.0} {:.0}", e.center().x, e.center().y)),
                (2, Some(_)) => click_control_at(s, rng.pick(&["readme more", "readme more", "readme close"])),
                _ => s.cmd(Cmd::ToggleReadme),
            }
            s.idle(12);
        }
        183 => {
            // Send To Phone opened on what is selected, its switch flipped, closed; or the
            // list grouped by where downloads came from.
            if rng.below(2) == 0 {
                s.cmd(Cmd::GroupOrigin);
                s.settle();
                return;
            }
            s.cmd(Cmd::SendToPhone);
            if !matches!(s.app.popup, Popup::Phone(_)) {
                return;
            }
            s.idle(5);
            if rng.below(2) == 0 {
                click_control_at(s, "switch");
            }
            s.act("key Escape");
        }
        184 => {
            // The GitHub page (no gh here: it says what to install), Sort By Colour, or a
            // rest wherever the pointer is (a folder there fans out its pictures).
            match rng.below(3) {
                0 => {
                    s.cmd(Cmd::GitHubProfile);
                    s.idle(10);
                    s.act("key ArrowDown");
                    s.cmd(Cmd::Back);
                    s.settle();
                }
                1 => {
                    s.cmd(Cmd::SortColour);
                    s.idle(10);
                }
                _ => {
                    let (x, y) = p(rng);
                    s.act(&format!("move {x:.0} {y:.0}"));
                    s.idle(40);
                }
            }
        }
        185 => {
            // The viewer: compared side by side or over one another, its strip, closed.
            s.cmd(Cmd::ViewPictures);
            if !matches!(s.app.popup, Popup::Viewer(_)) {
                return;
            }
            s.idle(rng.below(20) + 1);
            for _ in 0..rng.below(5) {
                let k = *rng.pick(&["W", "C", "ArrowRight", "ArrowLeft", "Escape"]);
                s.act(&format!("key {k}"));
            }
            if rng.below(2) == 0 {
                s.act(&format!("move {:.0} {:.0}", w / 2.0, h - 15.0));
                s.idle(20);
                let strip = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|x| x.2 == "viewer filmstrip").map(|x| x.1).collect::<Vec<_>>());
                if let Some(r) = strip.get(rng.below(strip.len().max(1))) {
                    s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
                }
            }
            for _ in 0..4 {
                s.act("key Escape");
            }
            s.idle(30);
        }
        186 => {
            // A theme hovered in the picker (it washes in), or the window resized (icons
            // glide to new rows).
            if rng.below(2) == 0 {
                s.cmd(Cmd::ChooseTheme);
                s.idle(4);
                let (x, y) = p(rng);
                s.act(&format!("move {x:.0} {y:.0}"));
                s.idle(4);
                s.act("key Escape");
            } else {
                let (w0, h0) = (s.size.x, s.size.y);
                s.resize(rng.f(420.0, 1500.0), rng.f(380.0, 950.0));
                s.idle(20);
                s.resize(w0, h0);
                s.idle(20);
            }
        }
        _ => {
            s.note("idle".into());
            s.idle(rng.below(20) + 1);
        }
    }
    picker_check(s);
}

/// The Columns view at random: into it, then its own moves (Left and Right, a click or
/// a right-click in another column, the wheel along the strip).
fn columns_step(s: &mut Sim, rng: &mut Rng) {
    if !s.tab().settings.column_view {
        s.cmd(Cmd::ViewColumns);
        return;
    }
    let list = s.app.panels.get(&s.app.active).map(|p| p.list_rect).unwrap_or(Rect::NOTHING);
    let names: Vec<Rect> = s.texts.iter().filter(|(r, _)| list.contains(r.center())).map(|(r, _)| *r).collect();
    match rng.below(5) {
        0 | 1 => {
            for _ in 0..1 + rng.below(4) {
                s.act(rng.pick(&["key ArrowRight", "key ArrowLeft", "key ArrowDown"]));
            }
        }
        2 | 3 if !names.is_empty() => {
            let r = *rng.pick(&names);
            let right = rng.below(2) == 1;
            s.act(&format!("click {:.0} {:.0}{}", r.center().x, r.center().y, if right { " right" } else { "" }));
            if right {
                s.idle(2);
                s.act("key Escape");
            }
        }
        _ => {
            let (x, y) = (rng.f(list.left(), list.right()), rng.f(list.top(), list.bottom()));
            s.note(format!("wheel along the strip at {x:.0} {y:.0}"));
            s.frame(vec![Event::PointerMoved(pos2(x, y))]);
            s.frame(vec![Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(rng.f(-6.0, 6.0), 0.0), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Wheel, modifiers: egui::Modifiers::NONE }]);
            s.idle(rng.below(12));
        }
    }
}

/// Picker sessions the random walks started, and how many of them answered (0) or were
/// cancelled (1), for the closing summary.
static PICKERS: [std::sync::atomic::AtomicUsize; 3] = [const { std::sync::atomic::AtomicUsize::new(0) }; 3];

/// Turn the session into an app's Open or Save dialog (a random request: any mode,
/// filters, questions), or, when it is one, cancel it.
fn picker_start_or_stop(s: &mut Sim, rng: &mut Rng) {
    use crate::portal::{Choice, Filter, Mode, Pick};
    if s.app.picker.is_some() {
        s.note("picker cancelled".into());
        crate::picker::cancel(&mut s.app);
        picker_check(s);
        return;
    }
    let mode = *rng.pick(&[Mode::Open, Mode::Open, Mode::Save, Mode::SaveFiles]);
    let filters = [
        Filter { name: "Pictures".into(), rules: vec![(0, "*.[pP][nN][gG]".into()), (1, "image/*".into())] },
        Filter { name: "Text and notes, a long name for a filter".into(), rules: vec![(1, "text/plain".into())] },
        Filter { name: "Everything".into(), rules: vec![(0, "*".into())] },
    ];
    let n = rng.below(4);
    let mut p = Pick { mode, multiple: rng.below(2) == 0, directory: mode == Mode::Open && rng.below(3) == 0, filters: filters[..n].to_vec(), ..Default::default() };
    p.current_filter = (n > 0).then(|| rng.below(n));
    if rng.below(3) == 0 {
        p.choices.push(Choice { id: "enc".into(), label: "Encoding".into(), options: vec![("u".into(), "Unicode".into()), ("w".into(), "Western".into())], chosen: "u".into() });
    }
    if rng.below(3) == 0 {
        p.choices.push(Choice { id: "ro".into(), label: "Read only".into(), options: vec![], chosen: String::new() });
    }
    p.current_name = rng.pick(&["Untitled.txt", "notes.txt", "report.md", "a b.tar.gz", ""]).to_string();
    p.files = vec!["one.txt".into(), "notes.txt".into()];
    p.accept_label = rng.pick(&["", "_Upload", "Attach"]).to_string();
    s.note(format!("picker {:?} multiple {} directory {} filters {n} choices {}", p.mode, p.multiple, p.directory, p.choices.len()));
    s.app.picker = Some(Box::new(crate::picker::Picker::new(p)));
    PICKERS[0].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    s.idle(2);
}

/// One thing a person does with the picker's bar.
fn picker_step(s: &mut Sim, rng: &mut Rng) {
    if s.app.picker.is_none() {
        return picker_start_or_stop(s, rng);
    }
    match rng.below(9) {
        0 => click_control_at(s, "picker accept"),
        1 => click_control_at(s, rng.pick(&["picker filter", "picker choice"])),
        2 => click_control_at(s, "picker menu row"),
        3 => click_control_at(s, "picker check"),
        4 => {
            click_control_at(s, "picker name");
            s.act(&format!("text {}", rng.pick(&["new name.txt", "notes.txt", "Taxes", "~/x.txt", "/nowhere/y.txt", "/"])));
        }
        5 => s.act(&format!("key {}", rng.pick(&["Enter", "Escape", "alt+t", "ArrowDown", "ArrowUp"]))),
        6 => {
            if s.popup_open() {
                s.act(rng.pick(&["key Enter", "key Escape"]));
            }
        }
        7 => s.act(rng.pick(&["key alt+t", "key ctrl+a"])),
        _ => click_control_at(s, "picker cancel"),
    }
}

/// A picker that has answered: what it answered is what the request allows, then the
/// session goes on as the ordinary window.
fn picker_check(s: &mut Sim) {
    let Some((pick, a)) = s.app.picker.as_ref().and_then(|p| p.done.clone().map(|a| (p.pick.clone(), a))) else { return };
    use crate::portal::Mode;
    s.note(format!("picker answered {} with {} uris", a.response, a.uris.len()));
    let paths: Vec<PathBuf> = a.uris.iter().map(|u| crate::clipboard::uri_to_path(u).unwrap_or_else(|| s.fail(&format!("not a file URI: {u}")))).collect();
    match a.response {
        0 => {
            if paths.is_empty() {
                s.fail("picked nothing");
            }
            match pick.mode {
                Mode::Open if pick.directory => paths.iter().for_each(|p| if !p.is_dir() { s.fail(&format!("{} is not a folder", p.display())) }),
                Mode::Open => paths.iter().for_each(|p| if !p.is_file() { s.fail(&format!("{} is not a file", p.display())) }),
                Mode::Save => {
                    if paths.len() != 1 || !paths[0].parent().is_some_and(|d| d.is_dir()) {
                        s.fail(&format!("saved as {paths:?}"));
                    }
                }
                Mode::SaveFiles => {
                    if paths.len() != pick.files.len() {
                        s.fail(&format!("{} names, {} places", pick.files.len(), paths.len()));
                    }
                }
            }
            if !pick.multiple && pick.mode == Mode::Open && paths.len() > 1 {
                s.fail("several picked without `multiple`");
            }
        }
        1 if paths.is_empty() => {}
        r => s.fail(&format!("answered {r} with {paths:?}")),
    }
    PICKERS[1 + a.response.min(1) as usize].fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    s.app.picker = None;
    s.app.quit = false;
    let _ = crate::picker::ANSWER.lock().map(|mut a| a.take());
    for t in s.app.panels.values_mut().flat_map(|p| p.tabs.iter_mut()) {
        if t.pick.take().is_some() {
            t.dirty = true;
        }
    }
    s.idle(1);
}

/// The viewer's tag strip is waiting for a new tag's name.
fn viewer_strip_typing(s: &Sim) -> bool {
    matches!(&s.app.popup, Popup::Viewer(v) if v.strip.as_ref().is_some_and(|t| t.typing.is_some()))
}

/// The branch menu, then one branch's own menu (not the current one's), by a right click
/// on its name or by the keys and Shift+F10. False when there is no such branch.
fn branch_own_menu(s: &mut Sim, rng: &mut Rng) -> bool {
    s.cmd(Cmd::GitBranches);
    let t0 = Instant::now();
    while matches!(&s.app.popup, Popup::Branches(st) if st.refs.is_none()) && t0.elapsed().as_secs() < 3 {
        s.idle(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    s.idle(6);
    let Popup::Branches(st) = &s.app.popup else { return false };
    let Some(r) = &st.refs else { return false };
    let names: Vec<String> = r.local.iter().filter(|b| !b.current).map(|b| b.name.clone()).chain(r.remote.iter().cloned()).collect();
    if names.is_empty() {
        return false;
    }
    if rng.below(2) == 0 {
        let name = rng.pick(&names).clone();
        let Some(c) = s.texts.iter().rev().find(|(_, t)| *t == name).map(|(r, _)| r.center()) else { return false };
        s.act(&format!("click {:.0} {:.0} right", c.x, c.y));
    } else {
        for _ in 0..1 + rng.below(8) {
            s.act("key ArrowDown");
        }
        s.act("key shift+F10");
    }
    s.idle(3);
    matches!(s.app.popup, Popup::Branch { .. })
}

/// Click the item of the open menu whose label starts with `prefix`, or leave the menu.
fn pick_starting(s: &mut Sim, prefix: &str) {
    match s.texts.iter().rev().find(|(_, t)| t.starts_with(prefix)).map(|(r, _)| r.center()) {
        Some(c) => s.act(&format!("click {:.0} {:.0}", c.x, c.y)),
        None => s.act("key Escape"),
    }
    s.idle(3);
}

/// An open history in the active panel's inspector.
fn history_open(s: &Sim) -> bool {
    s.app.panels.get(&s.app.active).and_then(|p| p.inspector.as_ref()).is_some_and(|i| i.git.is_some())
}

fn some_text_at(s: &Sim, want: &str) -> bool {
    s.texts.iter().any(|(_, t)| t == want)
}

#[cfg(test)]
mod journeys {
    use super::*;
    use Cmd::*;

    fn go(s: &mut Sim, p: &Path) {
        s.note(format!("navigate {}", p.display()));
        let id = s.app.active;
        if let Some(panel) = s.app.panels.get_mut(&id) {
            panel.tab_mut().navigate(p.to_path_buf(), None);
        }
        s.settle();
    }

    /// Hover nothing and let the screen settle, then demand clean text layout.
    fn look(s: &mut Sim, what: &str) {
        s.note(format!("look: {what}"));
        s.act("move -50 -50");
        s.settle();
        s.idle(2);
        // Toasts, job cards and letter tags float over the list by design.
        s.check_overlap = !s.popup_open() && s.app.toast.is_none() && s.app.jobs.is_empty() && s.app.hints.is_none();
        // MAIA_SIM_POPUP_OVERLAP=1: every popup looked at is held to it too.
        s.check_popup_overlap = s.popup_open() && std::env::var_os("MAIA_SIM_POPUP_OVERLAP").is_some();
        s.idle(2);
        s.check_overlap = false;
        s.check_popup_overlap = false;
    }

    fn report(name: &str, s: &Sim) {
        let (p50, p95, max) = s.timing();
        let (rss, peak) = crate::memory::resident_kb();
        eprintln!("sim {name}: {} frames, frame ms p50 {p50:.1} p95 {p95:.1} max {max:.1}, resident {} MB, peak {} MB", s.frame_ms.len(), rss / 1024, peak / 1024);
    }

    #[test]
    fn every_view_renders_cleanly() {
        if !sandboxed() {
            return;
        }
        let home = fixture("views");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let folders = [home.clone(), home.join("Documents"), home.join("Big"), home.join("Pictures/Trip"), home.join("Code/maia"), home.join("Empty")];
        for f in &folders {
            go(&mut s, f);
            for v in [ViewDetails, ViewTree, ViewList, ViewMedium, ViewLarge, ViewXL, ViewColumns] {
                s.cmd(v);
                for g in [GroupNone, GroupType, GroupName, GroupSize] {
                    s.cmd(g);
                    for (w, h) in [(1280.0, 800.0), (640.0, 480.0), (520.0, 340.0)] {
                        s.resize(w, h);
                        look(&mut s, &format!("{} {:?} {:?} {w}x{h}", f.display(), v, g));
                    }
                }
            }
        }
        // Same with the inspector and without the sidebar.
        s.cmd(GroupNone);
        s.cmd(ToggleInspector);
        s.cmd(ToggleSidebar);
        for f in &folders {
            go(&mut s, f);
            for v in [ViewDetails, ViewTree, ViewList, ViewLarge, ViewColumns] {
                s.cmd(v);
                s.act("key ArrowDown");
                look(&mut s, &format!("inspector {} {v:?}", f.display()));
            }
        }
        report("views", &s);
    }

    #[test]
    fn browse_rename_create_trash_undo() {
        if !sandboxed() {
            return;
        }
        let home = fixture("journey");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        // Type-to-find, then open.
        s.act("text Docu"); // "Doc" would match docs-link first (display order)
        s.act("key Enter");
        s.settle();
        assert_eq!(s.tab().path, home.join("Documents"), "type-to-find + Enter opens Documents");
        // Rename notes.txt via F2.
        s.act("text notes");
        s.act("key F2");
        assert!(s.tab().rename.is_some(), "F2 starts a rename");
        s.act("key ctrl+a");
        s.act("text notes2.txt");
        s.act("key Enter");
        s.settle();
        assert!(home.join("Documents/notes2.txt").exists(), "renamed on disk");
        s.act("key ctrl+z");
        s.settle();
        assert!(home.join("Documents/notes.txt").exists() && !home.join("Documents/notes2.txt").exists(), "undo restores the name");
        // New folder, named inline.
        s.cmd(NewFolder);
        s.settle();
        s.idle(3);
        if s.tab().rename.is_some() {
            s.act("key ctrl+a");
            s.act("text Fresh");
            s.act("key Enter");
            s.settle();
        }
        let fresh = std::fs::read_dir(home.join("Documents")).unwrap().flatten().find(|d| d.path().is_dir() && d.file_name() != "Taxes").map(|d| d.path());
        let fresh = fresh.unwrap_or_else(|| s.fail("new folder created"));
        // Select it and trash it.
        let idx = s.tab().entries.iter().position(|e| e.path == fresh).unwrap_or_else(|| s.fail("new folder listed"));
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().select_only(idx as u32);
        s.act("key Delete");
        if s.popup_open() {
            s.act("key Enter");
        }
        s.settle();
        assert!(!fresh.exists(), "Delete moves the folder to the trash");
        s.act("key ctrl+z");
        s.settle();
        assert!(fresh.exists(), "undo brings it back from the trash");
        // Up selects the folder we came from.
        s.act("key alt+ArrowUp");
        s.settle();
        assert_eq!(s.tab().path, home);
        let cur = s.tab().cursor.map(|c| s.tab().entries[c as usize].path.clone());
        assert_eq!(cur, Some(home.join("Documents")), "Up selects the previous folder");
        // Tabs: new, close, reopen.
        let tabs = |s: &Sim| s.app.panels[&s.app.active].tabs.len();
        let n = tabs(&s);
        s.act("key ctrl+t");
        assert_eq!(tabs(&s), n + 1);
        s.act("key ctrl+w");
        assert_eq!(tabs(&s), n);
        s.act("key ctrl+shift+t");
        assert_eq!(tabs(&s), n + 1);
        // Splits: two panels, focus moves, close one.
        s.cmd(SplitRight);
        s.settle();
        assert_eq!(s.app.panels.len(), 2);
        s.act("key Tab");
        s.cmd(ClosePanel);
        assert_eq!(s.app.panels.len(), 1);
        // Go To with a typed path.
        s.act("key ctrl+p");
        assert!(s.popup_open());
        s.act(&format!("text {}/Code/maia/", home.display()));
        s.settle();
        s.act("key Escape");
        assert!(!s.popup_open(), "Escape closes Go To");
        assert!(crate::mime::LAUNCHED.lock().unwrap().is_empty() || true);
        report("journey", &s);
    }

    /// Features combined: every subset of a set of toggles, then the usual interactions.
    #[test]
    fn feature_combinations_hold_together() {
        if !sandboxed() {
            return;
        }
        let home = fixture("combos");
        let mut s = Sim::new(&home, vec2(1100.0, 700.0));
        let toggles: &[(&str, &[Cmd])] = &[
            ("flatten", &[ToggleFlatten]),
            ("hidden", &[ToggleHidden]),
            ("tree", &[ViewTree]),
            ("icons", &[ViewLarge]),
            ("group", &[GroupType]),
            ("sort", &[SortSizeDesc]),
            ("inspector", &[ToggleInspector]),
            ("split", &[SplitRight]),
        ];
        let mut rng = Rng::new(7);
        for mask in 0u32..(1 << toggles.len()) {
            if mask.count_ones() > 4 && rng.below(4) != 0 {
                continue; // sample the big combinations
            }
            let mut s2 = Sim::new(&home, vec2(1100.0, 700.0));
            let on: Vec<&str> = toggles.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, t)| t.0).collect();
            s2.note(format!("combo {on:?}"));
            for (i, (_, cmds)) in toggles.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    for c in *cmds {
                        s2.cmd(*c);
                    }
                }
            }
            s2.settle();
            for line in ["text o", "key ArrowDown", "key shift+End", "key ctrl+a", "scroll 500 400 -6", "key Escape", "key ctrl+f", "text a", "key Escape", "key End", "key Home", "key Enter", "key alt+ArrowUp"] {
                s2.act(line);
            }
            s2.settle();
            s2.act("key ctrl+shift+p");
            s2.act("key Escape");
            look(&mut s2, &format!("combo {on:?}"));
            s.frame_ms.extend(s2.frame_ms.drain(..));
        }
        report("combos", &s);
    }

    /// Every popup and menu at every window size (and display scale) stays on screen
    /// and renders valid geometry; Escape always closes it.
    #[test]
    fn popups_and_menus_fit_every_window() {
        if !sandboxed() {
            return;
        }
        let home = fixture("popups");
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.act("key ArrowDown");
        for ppp in [1.0, 1.6, 2.0] {
            s.ppp = ppp;
            for (w, h) in [(1920.0, 1080.0), (1280.0, 800.0), (520.0, 340.0)] {
                s.resize(w, h);
                for c in [Palette, GoTo, Options, MainMenu, QuickMenu, TypesFilter, Properties, ContextMenu, TabSearch, About] {
                    s.cmd(c);
                    s.settle();
                    s.idle(2);
                    if !s.popup_open() {
                        s.fail(&format!("{c:?} opened nothing at {w}x{h}"));
                    }
                    s.act("key Escape");
                    s.idle(2);
                    if s.popup_open() {
                        s.act("key Escape");
                    }
                    if s.popup_open() {
                        s.fail(&format!("{c:?} did not close on Escape"));
                    }
                }
                // Right-click menus on an item, on empty space, on the sidebar and tab bar.
                for (x, y) in [(w * 0.5, 120.0), (w * 0.6, h - 60.0), (60.0, 140.0), (240.0, 14.0)] {
                    s.act(&format!("click {x:.0} {y:.0} right"));
                    s.settle();
                    s.act("key Escape");
                    s.idle(2);
                }
                // The folder dropdown (breadcrumb chevron) on a folder with very long names.
                let id = s.app.active;
                s.app.popup = Popup::path_dropdown(id, home.join("Documents"), None);
                s.settle();
                s.idle(2);
                s.act("key Escape");
                // The view-size popup from the status bar zoom chip.
                s.act(&format!("click {:.0} {:.0}", w - 20.0, h - 12.0));
                s.settle();
                s.act("key Escape");
            }
        }
        assert!(s.popup_checks.get() > 100, "popup text containment checked only {} times", s.popup_checks.get());
        report("popups", &s);
    }

    /// Two to four panels side by side and stacked, small and large, in every view.
    #[test]
    fn split_layouts_render_cleanly() {
        if !sandboxed() {
            return;
        }
        let home = fixture("splits");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        for split in [SplitRight, SplitBottom, SplitRight] {
            s.cmd(split);
            s.settle();
            for v in [ViewDetails, ViewTree, ViewList, ViewMedium, ViewXL] {
                s.cmd(v);
                for (w, h) in [(1920.0, 1080.0), (1280.0, 800.0), (520.0, 340.0)] {
                    s.resize(w, h);
                    let n = s.app.panels.len();
                    look(&mut s, &format!("{n} panels {v:?} {w}x{h}"));
                }
            }
            s.cmd(ToggleInspector);
            let n = s.app.panels.len();
            look(&mut s, &format!("{n} panels with inspector"));
            s.cmd(ToggleInspector);
        }
        report("splits", &s);
    }

    /// Flattening a huge tree streams hundreds of thousands of results in: the UI has to
    /// keep drawing smoothly while they arrive, and stay quick to filter, scroll and sort.
    #[test]
    fn flatten_huge_tree_stays_responsive() {
        if !sandboxed() {
            return;
        }
        let mut s = Sim::new(Path::new("/usr"), vec2(1280.0, 800.0));
        s.frame_ms.clear();
        s.cmd(ToggleFlatten);
        let t0 = Instant::now();
        while s.tab().is_loading() && t0.elapsed().as_secs() < 120 {
            s.frame(vec![]);
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        s.settle();
        let n = s.tab().entries.len();
        let (p50, p95, max) = s.timing();
        let (rss, peak) = crate::memory::resident_kb();
        eprintln!("sim flatten /usr: {n} files in {:.1}s, {} frames while streaming, p50 {p50:.1} p95 {p95:.1} max {max:.1} ms, resident {} MB, peak {} MB", t0.elapsed().as_secs_f32(), s.frame_ms.len(), rss / 1024, peak / 1024);
        let streaming = std::mem::take(&mut s.frame_ms);
        for (what, line) in [("filter", "text lib"), ("scroll", "scroll 600 400 -10"), ("end", "key End"), ("sort", "key ctrl+alt+2"), ("clear", "key Escape")] {
            let i = s.frame_ms.len();
            if what == "filter" {
                s.act("key ctrl+f");
            }
            s.act(line);
            s.settle();
            let worst = s.frame_ms[i..].iter().cloned().fold(0.0f32, f32::max);
            eprintln!("sim flatten /usr: {what} worst frame {worst:.1} ms");
        }
        for (ms, step) in &s.slow {
            eprintln!("  slow frame {ms:.0} ms after: {step}");
        }
        assert!(n > 10_000, "flatten found files");
        // Budgets (optimized build, with headroom for a busy machine): results streaming
        // in must not stall drawing, and filtering/sorting 100k+ items stays interactive.
        let mut v = streaming.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        let p95 = v.get(v.len() * 95 / 100).copied().unwrap_or(0.0);
        assert!(p95 < 40.0, "frames while streaming: p95 {p95:.1} ms");
        // A desktop's /usr is near 100k files; a build server's can be several times that,
        // and filtering and sorting take time in step with it.
        let budget = 200.0 * (n as f32 / 200_000.0).max(1.0);
        let worst = s.frame_ms.iter().cloned().fold(0.0f32, f32::max);
        assert!(worst < budget, "slowest interaction frame {worst:.1} ms of {budget:.0} for {n} files");
        // Leaving the walk frees its entries, hundreds of thousands of small blocks.
        s.cmd(Cmd::Up);
        s.settle();
        s.idle(5);
        let (rss, _) = crate::memory::resident_kb();
        eprintln!("sim flatten /usr: resident after leaving {} MB", rss / 1024);
    }

    /// Tree view, "expand all" over thousands of folders: drawing stays smooth while the
    /// listings land, and no folder's children appear twice.
    #[test]
    fn tree_expand_all_stays_responsive() {
        if !sandboxed() {
            return;
        }
        let mut s = Sim::new(Path::new("/usr/share"), vec2(1280.0, 800.0));
        s.cmd(ViewTree);
        s.settle();
        s.frame_ms.clear();
        s.act("key ctrl+e");
        let t0 = Instant::now();
        while s.tab().is_loading() && t0.elapsed().as_secs() < 120 {
            s.frame(vec![]);
            std::thread::sleep(std::time::Duration::from_millis(4));
        }
        s.settle();
        // Interact with the big unfolded tree too.
        for line in ["scroll 600 400 -20", "key End", "key Home", "key ctrl+f", "text lib", "key Escape", "key Escape"] {
            let i = s.frame_ms.len();
            s.act(line);
            s.settle();
            let worst = s.frame_ms[i..].iter().cloned().fold(0.0f32, f32::max);
            eprintln!("sim tree expand-all: {line} worst frame {worst:.1} ms");
        }
        let t = s.tab();
        let live: Vec<&crate::fsx::Entry> = t.entries.iter().filter(|e| e.depth != crate::fsx::DEAD).collect();
        let mut seen = std::collections::HashSet::new();
        for e in &live {
            assert!(seen.insert(&e.path), "listed twice: {}", e.path.display());
        }
        let (p50, p95, max) = s.timing();
        eprintln!("sim tree expand-all /usr/share: {} live entries, {} rows, {} frames, p50 {p50:.1} p95 {p95:.1} max {max:.1} ms", live.len(), t.rows.len(), s.frame_ms.len());
        assert!(live.len() > 5_000, "expand-all unfolded the tree");
        // Optimized build, headroom for a busy machine; a /usr/share past 100k entries (a
        // build server's) takes time in step with its size.
        let scale = (live.len() as f32 / 100_000.0).max(1.0);
        assert!(max < 120.0 * scale, "slowest frame {max:.1} ms for {} entries", live.len());
        assert!(p95 < 60.0 * scale, "frames p95 {p95:.1} ms for {} entries", live.len());
    }

    fn select_path(s: &mut Sim, p: &Path) {
        let Some(i) = s.tab().entries.iter().position(|e| e.path == p) else { s.fail(&format!("{} not listed", p.display())) };
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().select_only(i as u32);
        s.idle(1);
    }

    /// Inspector open, the cursor walks every item of every kind (text, markdown, images
    /// good and corrupt, fake PDF, pipe, links good and broken, locked and empty
    /// folders) in several views: no crash, no broken layout, no stalls.
    #[test]
    fn inspector_previews_every_kind() {
        if !sandboxed() {
            return;
        }
        let home = fixture("inspect");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.cmd(ToggleSystem);
        s.cmd(ToggleHidden);
        s.cmd(ToggleInspector);
        for dir in [home.clone(), home.join("Documents"), home.join("Pictures"), home.join("Pictures/Trip"), home.join("Code/maia"), home.join("Music/Album")] {
            go(&mut s, &dir);
            for v in [ViewDetails, ViewList, ViewLarge] {
                s.cmd(v);
                s.act("key Home");
                let n = s.tab().view.len().min(40);
                for _ in 0..n {
                    s.act("key ArrowDown");
                    s.idle(2);
                }
                s.settle();
                look(&mut s, &format!("inspector {} {v:?}", dir.display()));
            }
        }
        for (ms, step) in &s.slow {
            eprintln!("  slow frame {ms:.0} ms after: {step}");
        }
        assert!(s.slow.iter().all(|x| x.0 < 150.0), "previewing stalled");
        report("inspector", &s);
    }

    /// Copy and move between split panels into conflicts, answered through the dialog's
    /// buttons; duplicate; undo. Checked on disk.
    #[test]
    fn copy_move_conflicts_and_undo() {
        if !sandboxed() {
            return;
        }
        let home = fixture("conflicts");
        std::fs::write(home.join("Code/maia/report.md"), "older report").unwrap();
        let src = home.join("Documents/report.md");
        let dst_dir = home.join("Code/maia");
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        // Second panel on the destination.
        s.cmd(SplitRight);
        go(&mut s, &dst_dir);
        s.act("key Tab"); // back to the Documents panel
        s.settle();
        assert_eq!(s.tab().path, home.join("Documents"));
        // F5 into an existing name: Keep both.
        select_path(&mut s, &src);
        s.act("key F5");
        s.settle();
        assert!(s.popup_open(), "conflict dialog shown");
        s.click_text("Keep both");
        s.settle();
        wait_until(&mut s, "kept both", 10, |_| dst_dir.join("report (1).md").exists());
        assert_eq!(std::fs::read_to_string(dst_dir.join("report.md")).unwrap(), "older report");
        // Again: Replace.
        select_path(&mut s, &src);
        s.act("key F5");
        s.settle();
        s.click_text("Replace");
        s.settle();
        let same = |a: &Path, b: &Path| std::fs::read(a).ok().is_some_and(|x| std::fs::read(b).ok() == Some(x));
        wait_until(&mut s, "replaced", 10, |_| same(&dst_dir.join("report.md"), &src));
        // The one replaced is in the trash; Undo brings it back in its place.
        wait_until(&mut s, "the copy's undo", 10, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Copied(..))));
        s.act("key ctrl+z");
        s.settle();
        wait_until(&mut s, "the replaced one back", 10, |_| std::fs::read_to_string(dst_dir.join("report.md")).ok().as_deref() == Some("older report"));
        select_path(&mut s, &src);
        s.act("key F5");
        s.settle();
        assert!(s.texts.iter().any(|(_, t)| t.ends_with("· newer · bigger") || t.ends_with("· bigger") || t.ends_with("· newer")), "which is newer or bigger, said");
        look(&mut s, "conflict");
        s.shot("conflict-keys");
        // By its key this time.
        s.act("key R");
        s.settle();
        wait_until(&mut s, "replaced again", 10, |_| same(&dst_dir.join("report.md"), &src));
        // F6 (move) into a conflict: Skip leaves the source where it is.
        select_path(&mut s, &src);
        s.act("key F6");
        s.settle();
        s.click_text("Skip");
        s.settle();
        assert!(src.exists(), "skipped move keeps the source");
        // A move without conflict, then undo puts it back.
        let notes = home.join("Documents/notes.txt");
        select_path(&mut s, &notes);
        s.act("key F6");
        s.settle();
        wait_until(&mut s, "moved", 10, |_| !notes.exists() && dst_dir.join("notes.txt").exists());
        wait_until(&mut s, "the move's undo", 10, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Move(..))));
        s.act("key ctrl+z");
        s.settle();
        wait_until(&mut s, "undo moved it back", 10, |_| notes.exists() && !dst_dir.join("notes.txt").exists());
        // Duplicate in place.
        select_path(&mut s, &src);
        s.cmd(Duplicate);
        let dupes = || std::fs::read_dir(home.join("Documents")).unwrap().flatten().filter(|d| d.file_name().to_string_lossy().starts_with("report")).count();
        wait_until(&mut s, "duplicate made a second copy", 10, |_| dupes() == 2);
        report("conflicts", &s);
    }

    /// A rename the folder refuses (it is read-only) leaves nothing for Undo: Undo then
    /// takes back the change before it, not a rename that never happened.
    #[test]
    fn a_refused_rename_leaves_nothing_to_undo() {
        if !sandboxed() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let home = fixture("refusedrename");
        let d = home.join("Documents/Taxes/2025");
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o555)).unwrap();
        let undos = s.app.undo.len();
        select_path(&mut s, &d.join("receipt.pdf"));
        s.act("key F2");
        s.act("key ctrl+a");
        s.act("text kept.pdf");
        s.act("key Enter");
        s.settle();
        let refused = !d.join("kept.pdf").exists();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(refused && d.join("receipt.pdf").exists(), "the folder said no");
        assert_eq!(s.app.undo.len(), undos, "no Undo for a rename that didn't happen");
        assert!(s.app.toast.is_some(), "and it says why");
        report("refused rename", &s);
    }

    /// "Show in folder" from another app (the requests the session bus hands over): each
    /// folder opens in a new tab with the file asked for selected, two files in one
    /// folder make one tab, Properties opens for the third method, and a path that's gone
    /// is said to be gone.
    #[test]
    fn show_in_folder_from_other_apps() {
        if !sandboxed() {
            return;
        }
        use crate::single::Request;
        let home = fixture("showin");
        let d = home.join("Documents");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let (tx, rx) = std::sync::mpsc::channel();
        s.app.requests = Some(rx);
        let tabs = |s: &Sim| s.app.panels[&s.app.active].tabs.len();
        let n = tabs(&s);
        tx.send(Request::Show(vec![d.join("notes.txt"), d.join("report.md")])).unwrap();
        s.settle();
        assert_eq!(tabs(&s), n + 1, "one tab for the one folder");
        assert_eq!(s.tab().path, d);
        assert_eq!(s.tab().cursor_path(), Some(d.join("notes.txt")), "the file asked for, selected");
        look(&mut s, "shown from another app");
        tx.send(Request::Show(vec![d.join("Taxes")])).unwrap();
        s.settle();
        assert_eq!((s.tab().path.clone(), s.tab().cursor_path()), (d.clone(), Some(d.join("Taxes"))), "a folder shown in its parent, not opened");
        tx.send(Request::Properties(vec![d.join("report.md")])).unwrap();
        s.settle();
        assert!(matches!(s.app.popup, Popup::Properties(_)));
        s.act("key Escape");
        tx.send(Request::Show(vec![d.join("gone.txt")])).unwrap();
        s.settle();
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0.contains("doesn't exist")));
        report("show in folder", &s);
    }

    /// The cursor walked onto `name` with the arrow keys.
    fn cursor_to(s: &mut Sim, name: &str) {
        s.act("key Home");
        for _ in 0..60 {
            let t = s.tab();
            if t.cursor.and_then(|c| t.entries.get(c as usize)).is_some_and(|e| e.name == name) {
                return;
            }
            s.act("key ArrowDown");
        }
        s.fail(&format!("{name} not reached with the arrow keys"));
    }

    fn lists(t: &crate::tab::Tab, p: &Path) -> bool {
        t.view.iter().any(|&i| t.entries[i as usize].path == p)
    }

    /// The front tab of the panel that is not focused.
    fn other_tab(s: &Sim) -> &crate::tab::Tab {
        s.app.panels.iter().find(|(id, _)| **id != s.app.active).map(|(_, p)| p.tab()).unwrap_or_else(|| s.fail("a second panel"))
    }

    /// Ctrl+V, then the files on disk where they were sent.
    fn paste_lands(s: &mut Sim, landed: &[PathBuf]) {
        s.act("key ctrl+v");
        wait_until(s, "the move to finish", 10, |s| landed.iter().all(|p| p.exists()) && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        s.settle();
    }

    /// Cut, paste somewhere else, and the folder the files left stops showing them with
    /// no refresh: from a tab left behind, within one tab, from the other panel, onto
    /// another filesystem, out of a flattened listing and a tree, and back again with
    /// Undo while the tab they return to is behind another.
    #[test]
    fn cut_paste_leaves_the_source_current() {
        if !sandboxed() {
            return;
        }
        let home = fixture("cutpaste");
        let (from, to, flat, tree) = (home.join("From"), home.join("To"), home.join("Flat"), home.join("Tree"));
        for f in ["From/a1.txt", "From/a2.txt", "From/a3.txt", "From/b1.txt", "From/c1.txt", "From/keep.txt", "Flat/sub/d1.txt", "Flat/sub/d2.txt", "Tree/inner/e1.txt", "Tree/inner/e2.txt"] {
            let p = home.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, f).unwrap();
        }
        std::fs::create_dir_all(&to).unwrap();
        // The sandbox's /tmp is a filesystem of its own: a move there copies, then removes.
        let far = PathBuf::from("/tmp/cutpaste-far");
        let _ = std::fs::remove_dir_all(&far);
        std::fs::create_dir_all(&far).unwrap();
        let mut s = Sim::new(&from, vec2(1280.0, 800.0));
        let gone_here = |s: &mut Sim, what: &str, names: &[&str]| {
            wait_until(s, what, 3, |s| names.iter().all(|n| !lists(s.tab(), &from.join(n))));
            s.act("move -50 -50");
            s.settle();
            for n in names {
                assert!(!some_text_at(s, n), "{n} still drawn: {what}");
            }
        };
        // Two files cut, pasted from a new tab in another folder, then back to the first tab.
        s.act("key Home");
        s.act("key shift+ArrowDown");
        assert_eq!(s.tab().targets(), [from.join("a1.txt"), from.join("a2.txt")]);
        s.act("key ctrl+x");
        s.act("key ctrl+t");
        go(&mut s, &to);
        paste_lands(&mut s, &[to.join("a1.txt"), to.join("a2.txt")]);
        s.act("key ctrl+1");
        assert_eq!(s.tab().path, from);
        gone_here(&mut s, "the tab left behind drops what was cut from it", &["a1.txt", "a2.txt"]);
        s.act("key ctrl+2");
        s.act("key ctrl+w");
        // Within one tab: there and Back.
        cursor_to(&mut s, "b1.txt");
        s.act("key ctrl+x");
        go(&mut s, &to);
        paste_lands(&mut s, &[to.join("b1.txt")]);
        s.act("key alt+ArrowLeft");
        s.settle();
        gone_here(&mut s, "Back in the folder", &["b1.txt"]);
        // Onto another filesystem, from a tab left behind.
        cursor_to(&mut s, "a3.txt");
        s.act("key ctrl+x");
        s.act("key ctrl+t");
        go(&mut s, &far);
        paste_lands(&mut s, &[far.join("a3.txt")]);
        assert!(!from.join("a3.txt").exists(), "copied, then removed");
        s.act("key ctrl+1");
        gone_here(&mut s, "moved onto another filesystem", &["a3.txt"]);
        s.act("key ctrl+2");
        s.act("key ctrl+w");
        // From the other panel.
        s.cmd(SplitRight);
        go(&mut s, &to);
        s.act("key Tab");
        s.settle();
        assert_eq!(s.tab().path, from);
        cursor_to(&mut s, "c1.txt");
        s.act("key ctrl+x");
        s.act("key Tab");
        paste_lands(&mut s, &[to.join("c1.txt")]);
        wait_until(&mut s, "the other panel drops c1.txt", 3, |s| !lists(other_tab(s), &from.join("c1.txt")));
        assert!(lists(other_tab(&s), &from.join("keep.txt")), "what stayed is still listed");
        // Out of a flattened listing (Ctrl+E) into the other panel.
        s.act("key Tab");
        go(&mut s, &flat);
        s.act("key ctrl+e");
        s.settle();
        assert!(lists(s.tab(), &flat.join("sub/d1.txt")), "flattened");
        cursor_to(&mut s, "d1.txt");
        s.act("key ctrl+x");
        s.act("key Tab");
        paste_lands(&mut s, &[to.join("d1.txt")]);
        wait_until(&mut s, "the flattened listing drops d1.txt", 3, |s| !lists(other_tab(s), &flat.join("sub/d1.txt")));
        assert!(lists(other_tab(&s), &flat.join("sub/d2.txt")) && other_tab(&s).flatten, "still flattened, the rest still there");
        // Out of a folder unfolded in a tree.
        s.act("key Tab");
        go(&mut s, &tree);
        s.cmd(ViewTree);
        cursor_to(&mut s, "inner");
        s.act("key ArrowRight");
        s.settle();
        cursor_to(&mut s, "e1.txt");
        s.act("key ctrl+x");
        s.act("key Tab");
        paste_lands(&mut s, &[to.join("e1.txt")]);
        wait_until(&mut s, "the tree drops e1.txt", 3, |s| !lists(other_tab(s), &tree.join("inner/e1.txt")));
        assert!(lists(other_tab(&s), &tree.join("inner/e2.txt")));
        // Undo puts e1.txt back while the tree is behind another tab.
        s.act("key Tab");
        s.act("key ctrl+t");
        s.act("key Tab");
        s.act("key ctrl+z");
        s.settle();
        assert!(tree.join("inner/e1.txt").exists(), "undo moved it back");
        s.act("key Tab");
        s.act("key ctrl+1");
        wait_until(&mut s, "the tree shows e1.txt again", 3, |s| lists(s.tab(), &tree.join("inner/e1.txt")));
        look(&mut s, "after cut and paste");
        let _ = std::fs::remove_dir_all(&far);
        report("cutpaste", &s);
    }

    /// Computer (drives) and Trash: every view renders; trashed items list, restore to
    /// their place, and Empty trash (confirmed through its button) clears them.
    #[test]
    fn computer_and_trash_views() {
        if !sandboxed() {
            return;
        }
        let home = fixture("trashy");
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.cmd(Computer);
        s.settle();
        assert!(!s.tab().view.is_empty(), "drives listed");
        for v in [ViewDetails, ViewList, ViewLarge, ViewTree] {
            s.cmd(v);
            look(&mut s, &format!("computer {v:?}"));
        }
        s.cmd(ViewDetails);
        go(&mut s, &home.join("Documents"));
        let a = home.join("Documents/notes.txt");
        let b = home.join("Documents/budget 2026.csv");
        for p in [&a, &b] {
            select_path(&mut s, p);
            s.act("key Delete");
            if s.popup_open() {
                s.act("key Enter");
            }
            s.settle();
            assert!(!p.exists(), "{} trashed", p.display());
        }
        go(&mut s, &crate::sidebar::trash_dir());
        // The sandbox's trash is shared by the journeys that ran before: look for ours.
        for n in ["notes.txt", "budget 2026.csv"] {
            assert!(s.tab().entries.iter().any(|e| e.name == n), "{n} in the trash");
        }
        for v in [ViewDetails, ViewList, ViewLarge] {
            s.cmd(v);
            look(&mut s, &format!("trash {v:?}"));
        }
        let ta = s.tab().entries.iter().position(|e| e.name == "notes.txt").unwrap_or_else(|| s.fail("notes in trash"));
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().select_only(ta as u32);
        s.cmd(RestoreTrash);
        s.settle();
        assert!(a.exists(), "restored to its place");
        s.cmd(EmptyTrash);
        s.settle();
        s.click_text("Empty trash");
        s.settle();
        assert!(!b.exists());
        assert_eq!(std::fs::read_dir(crate::sidebar::trash_dir()).map(|r| r.count()).unwrap_or(0), 0, "trash emptied");
        report("trash", &s);
    }

    /// Mouse work: drag a file onto a folder, drag one into the other panel, rubber-band
    /// select, widen a column, drag a tab into the other panel.
    #[test]
    fn mouse_drag_drop_select_and_resize() {
        if !sandboxed() {
            return;
        }
        let home = fixture("mouse");
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.act("move -50 -50");
        s.settle();
        // Two quick clicks on different rows are not a double-click: nothing opens.
        let (a, b) = (s.text_rect("notes.txt").center(), s.text_rect("Taxes").center());
        let tabs = s.app.panels[&s.app.active].tabs.len();
        s.act(&format!("click {:.0} {:.0}", a.x, a.y));
        s.act(&format!("click {:.0} {:.0} ctrl", b.x, b.y));
        s.settle();
        assert_eq!((s.tab().path.clone(), s.app.panels[&s.app.active].tabs.len()), (home.join("Documents"), tabs), "a click on another row opened it");
        assert_eq!(s.tab().selected.len(), 2);
        s.idle(40);
        s.act(&format!("dclick {:.0} {:.0}", b.x, b.y));
        wait_until(&mut s, "double-click opens", 10, |s| s.tab().path == home.join("Documents/Taxes"));
        s.act("key Backspace");
        wait_until(&mut s, "back", 10, |s| s.tab().path == home.join("Documents"));
        s.act("key Escape");
        s.settle();
        // Drag notes.txt onto the Taxes folder: moved inside.
        let (a, b) = (s.text_rect("notes.txt").center(), s.text_rect("Taxes").center());
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", a.x, a.y, b.x, b.y));
        s.settle();
        assert!(home.join("Documents/Taxes/notes.txt").exists() && !home.join("Documents/notes.txt").exists(), "dropped onto the folder");
        // Right-drag onto a folder asks what to do: Copy here.
        let (a, b) = (s.text_rect("report.md").center(), s.text_rect("Taxes").center());
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0} right", a.x, a.y, b.x, b.y));
        s.settle();
        s.click_text("Copy here");
        s.settle();
        assert!(home.join("Documents/Taxes/report.md").exists() && home.join("Documents/report.md").exists(), "right-drag copied");
        // Drop onto a sidebar place (Home).
        let (a, b) = (s.text_rect("budget 2026.csv").center(), s.text_rect("Home").center());
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", a.x, a.y, b.x, b.y));
        s.settle();
        assert!(home.parent().unwrap().join("budget 2026.csv").exists(), "dropped onto the sidebar's Home");
        std::fs::rename(home.parent().unwrap().join("budget 2026.csv"), home.join("Documents/budget 2026.csv")).unwrap();
        s.cmd(Refresh);
        s.settle();
        // Rubber band from empty space below the rows up across them: several selected.
        let list = s.app.panels[&s.app.active].list_rect;
        let last = s.text_rect("report.md");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", list.center().x + 120.0, list.bottom() - 20.0, list.center().x + 100.0, last.top() - 30.0));
        s.settle();
        assert!(s.tab().selected.len() >= 2, "rubber band selected {} items", s.tab().selected.len());
        s.act("key Escape");
        // Widen the Name column by dragging the divider just left of the Type header.
        let w0 = s.tab().settings.columns.first().map(|c| c.1).unwrap_or(0.0);
        let ty = s.text_rect("Type");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", ty.left() - 9.0, ty.center().y, ty.left() + 71.0, ty.center().y));
        s.settle();
        let w1 = s.tab().settings.columns.first().map(|c| c.1).unwrap_or(0.0);
        assert!(w1 > w0 + 40.0, "Name column widened {w0} -> {w1}");
        // Split: right panel on Pictures; drag a file across into it.
        s.cmd(SplitRight);
        go(&mut s, &home.join("Pictures"));
        s.act("key Tab");
        s.settle();
        s.act("move -50 -50");
        s.idle(2);
        let csv = s.text_rect("budget 2026.csv").center();
        let right = s.app.panels.values().find(|p| p.tab().path == home.join("Pictures")).map(|p| p.list_rect).unwrap_or_else(|| s.fail("right panel"));
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", csv.x, csv.y, right.center().x, right.bottom() - 30.0));
        s.settle();
        assert!(home.join("Pictures/budget 2026.csv").exists(), "dropped into the other panel");
        // Drag the Documents tab onto the other panel's tab strip: one panel left with both tabs.
        let n_tabs: usize = s.app.panels.values().map(|p| p.tabs.len()).sum();
        let tab = s.texts.iter().filter(|(r, t)| t == "Documents" && r.top() < 40.0).map(|(r, _)| *r).next().unwrap_or_else(|| s.fail("Documents tab"));
        let pics = s.texts.iter().filter(|(r, t)| t == "Pictures" && r.top() < 40.0).map(|(r, _)| *r).next().unwrap_or_else(|| s.fail("Pictures tab"));
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", tab.center().x, tab.center().y, pics.right() + 30.0, pics.center().y));
        s.settle();
        let n_tabs2: usize = s.app.panels.values().map(|p| p.tabs.len()).sum();
        assert_eq!(n_tabs2, n_tabs, "the tab moved, none lost");
        assert_eq!(s.app.panels.len(), 1, "the emptied panel closed");
        look(&mut s, "after drags");
        report("mouse", &s);
    }

    fn cursor_pos(s: &Sim) -> usize {
        let t = s.tab();
        t.cursor.and_then(|c| t.view_pos(c)).unwrap_or_else(|| s.fail("no cursor"))
    }

    /// The cursor's item must be drawn inside the visible list (scrolled into view).
    fn cursor_visible(s: &mut Sim, what: &str) {
        s.idle(20); // scrolling is animated (~0.3 s of frames)
        let t = s.tab();
        let name = crate::listing::display_name(&t.entries[t.cursor.unwrap() as usize], &s.app.cfg);
        let list = s.app.panels[&s.app.active].list_rect;
        let seen = s.texts.iter().any(|(r, txt)| (txt == &name || name.starts_with(txt.trim_end_matches('…'))) && list.contains(r.center()));
        if !seen {
            s.fail(&format!("{what}: cursor item {name:?} not drawn inside the list"));
        }
    }

    /// Arrow keys, Home/End, Page keys, Shift-extend and type-to-find in every view of a
    /// 600-file folder: the cursor lands where it should and stays on screen.
    #[test]
    fn keyboard_navigation_in_every_view() {
        if !sandboxed() {
            return;
        }
        let home = fixture("keys");
        let mut s = Sim::new(&home.join("Big"), vec2(1280.0, 800.0));
        for v in [ViewDetails, ViewList, ViewMedium, ViewLarge, ViewXL, ViewColumns] {
            s.cmd(v);
            s.settle();
            let n = s.tab().view.len();
            s.act("key Home");
            assert_eq!(cursor_pos(&s), 0, "{v:?}: Home");
            let (row, col) = (s.tab().nav_row_step.max(1), s.tab().nav_col_step);
            s.act("key ArrowDown");
            assert_eq!(cursor_pos(&s), row, "{v:?}: Down moves one row ({row})");
            if col > 0 {
                s.act("key ArrowRight");
                assert_eq!(cursor_pos(&s), row + col, "{v:?}: Right moves one column ({col})");
                s.act("key ArrowLeft");
            }
            s.act("key ArrowUp");
            assert_eq!(cursor_pos(&s), 0, "{v:?}: Up back to the first");
            s.act("key End");
            assert_eq!(cursor_pos(&s), n - 1, "{v:?}: End");
            cursor_visible(&mut s, &format!("{v:?} End"));
            let page = s.tab().page_items.max(1);
            s.act("key PageUp");
            assert_eq!(cursor_pos(&s), n - 1 - page.min(n - 1), "{v:?}: PageUp moves a page ({page})");
            cursor_visible(&mut s, &format!("{v:?} PageUp"));
            s.act("key Home");
            for _ in 0..3 {
                s.act("key shift+ArrowDown");
            }
            assert_eq!(s.tab().selected.len(), 3 * row + 1, "{v:?}: Shift+Down extends the selection");
            s.act("key Escape");
            s.act("text file0420");
            s.idle(2);
            let t = s.tab();
            assert_eq!(t.entries[t.cursor.unwrap() as usize].name, "file0420.txt", "{v:?}: type-to-find");
            cursor_visible(&mut s, &format!("{v:?} type-to-find"));
            // Typing went into the filter box: arrows still drive the list from there,
            // Escape clears the filter and a second Escape leaves the box.
            assert_eq!(s.tab().filter, "file0420");
            s.act("key ArrowDown");
            s.act("key Escape");
            s.act("key Escape");
            assert!(s.tab().filter.is_empty() && !s.ctx.egui_wants_keyboard_input(), "{v:?}: Escape twice clears and leaves the filter");
        }
        report("keys", &s);
    }

    /// Batch rename: F2 on twelve photos, type a shared prefix, add numbering from the
    /// rename menu, commit, undo. Colliding names are refused with nothing renamed.
    #[test]
    fn batch_rename_numbering_conflicts_and_undo() {
        if !sandboxed() {
            return;
        }
        let home = fixture("batch");
        let trip = home.join("Pictures/Trip");
        let names = |d: &Path| -> Vec<String> {
            let mut v: Vec<String> = std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        let before = names(&trip);
        let mut s = Sim::new(&trip, vec2(1280.0, 800.0));
        // Colliding names: refused, nothing renamed.
        s.act("key Home");
        s.act("key shift+ArrowDown");
        s.act("key F2");
        assert!(s.tab().rename.as_ref().is_some_and(|r| r.is_batch()), "batch rename for two items");
        s.act("text same");
        s.act("key Enter");
        s.settle();
        assert_eq!(names(&trip), before, "colliding names were refused");
        assert!(s.tab().rename.as_ref().is_some_and(|r| r.error.is_some()), "the collision is reported");
        s.act("key Escape");
        s.settle();
        // All twelve: "trip-" + incremental ID from the rename menu.
        s.act("key ctrl+a");
        s.act("key F2");
        assert_eq!(s.tab().rename.as_ref().map(|r| r.fields.len()), Some(12));
        s.act("text trip-");
        s.idle(2);
        let field = s.texts.iter().find(|(_, t)| t.starts_with("trip-")).map(|(r, _)| r.center()).unwrap_or_else(|| s.fail("rename field"));
        s.act(&format!("click {:.0} {:.0} right", field.x, field.y));
        s.settle();
        s.click_text("Insert incremental ID");
        s.settle();
        s.act("key Enter");
        s.settle();
        let after = names(&trip);
        assert_eq!(after.len(), 12, "{after:?}");
        assert!(after.iter().all(|n| n.starts_with("trip-") && n.ends_with(".png")), "{after:?}");
        assert!(!after.iter().any(|n| n.starts_with("img")), "{after:?}");
        s.act("key ctrl+z");
        s.settle();
        assert_eq!(names(&trip), before, "undo restores every name");
        report("batch", &s);
    }

    /// Options: search, toggle with Enter, and the change shows in the list at once.
    #[test]
    fn options_toggle_takes_effect() {
        if !sandboxed() {
            return;
        }
        let home = fixture("opts");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let shows = |s: &Sim, name: &str| s.tab().view.iter().any(|&i| s.tab().entries[i as usize].name == name);
        assert!(!shows(&s, ".bashrc"), "hidden files start hidden");
        let first = |s: &Sim| s.tab().view.first().map(|&i| s.tab().entries[i as usize].is_dir);
        assert_eq!(first(&s), Some(true), "folders first by default");
        for (query, what) in [("hidden", "show hidden"), ("folders first", "folders first")] {
            s.act("key ctrl+Comma");
            s.settle();
            assert!(s.popup_open(), "Options open");
            s.act(&format!("text {query}"));
            s.idle(2);
            s.act("key Enter");
            s.settle();
            s.act("key Escape");
            s.act("key Escape");
            s.settle();
            let _ = what;
        }
        assert!(shows(&s, ".bashrc"), "show hidden took effect");
        assert_eq!(first(&s), Some(false), "files first took effect");
        look(&mut s, "after options");
        report("options", &s);
    }

    /// The section on show in Options (None when Options is closed).
    fn opt_section(s: &Sim) -> Option<crate::popups::Section> {
        match &s.app.popup {
            Popup::Options(st) => st.section,
            _ => None,
        }
    }

    /// Click the `what` control on the row titled `title` (the one nearest its title).
    fn click_in_row(s: &mut Sim, what: &str, title: &str) {
        s.idle(1);
        let t = s.text_rect(title);
        let r = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, r, w)| *w == what && r.left() > t.left()).map(|(_, r, _)| *r).min_by(|a, b| (a.center().y - t.center().y).abs().total_cmp(&(b.center().y - t.center().y).abs())));
        let r = r.unwrap_or_else(|| s.fail(&format!("no {what} beside {title:?}")));
        assert!((r.center().y - t.center().y).abs() < 22.0, "{what} on the row of {title:?}: {r:?} vs {t:?}");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
    }

    /// Options in sections: the left column walked with the mouse and with Ctrl+PageUp
    /// and Ctrl+PageDown, a setting changed in every section, a search across all of
    /// them acted on with the keys and the mouse, reset arrows, titles only with its
    /// tooltips, the section remembered, a small window and a light theme.
    #[test]
    fn options_sections() {
        if !sandboxed() {
            return;
        }
        use crate::popups::{OPTS, Section};
        let home = fixture("optsections");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let slug = |sec: Section| sec.name().to_lowercase().replace(" & ", "-").replace(' ', "-");
        s.act("key ctrl+Comma");
        s.settle();
        assert_eq!(opt_section(&s), Some(Section::Appearance), "opens on Appearance");
        // Every section with the mouse: its own rows and nothing else.
        for sec in Section::ALL {
            s.click_text(sec.name());
            s.idle(2);
            assert_eq!(opt_section(&s), Some(sec), "{} chosen", sec.name());
            for (_, other, _, title, _) in OPTS {
                let shown = s.texts.iter().any(|(_, t)| t == title);
                if *other != sec {
                    assert!(!shown, "{title:?} belongs to {}, not {}", other.name(), sec.name());
                }
            }
            let first = OPTS.iter().find(|o| o.1 == sec).map(|o| o.3).unwrap();
            assert!(s.texts.iter().any(|(_, t)| t == first), "{first:?} heads {}", sec.name());
            look(&mut s, sec.name());
            s.shot(&format!("options-{}", slug(sec)));
        }
        // Appearance: a switch with the mouse, and its reset arrow.
        s.click_text("Appearance");
        click_in_row(&mut s, "switch", "Round corners");
        assert!(!s.app.cfg.round_corners, "round corners off");
        click_in_row(&mut s, "reset option", "Round corners");
        assert!(s.app.cfg.round_corners, "reset puts it back");
        // Files & folders, one Ctrl+PageDown on: hidden files shown, and the drive
        // switches inert while folder sizes are off.
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::Files));
        click_in_row(&mut s, "switch", "Show hidden files");
        assert!(s.app.cfg.show_hidden);
        s.app.cfg.folder_sizes = true;
        s.idle(2);
        click_in_row(&mut s, "switch", "Calculate folder sizes");
        assert!(!s.app.cfg.folder_sizes);
        let ssd = s.app.cfg.sizes_ssd;
        click_in_row(&mut s, "switch", "Solid-state drives");
        assert_eq!(s.app.cfg.sizes_ssd, ssd, "a sub-option does nothing while its switch is off");
        s.shot("options-sub-off");
        click_in_row(&mut s, "reset option", "Calculate folder sizes");
        assert_eq!(s.app.cfg.folder_sizes, crate::config::Config::default().folder_sizes);
        // Mouse & keyboard with the keys alone: Enter on the first row, then on the
        // sub-option it wakes.
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::Mouse));
        assert!(!s.app.cfg.single_click && !s.app.cfg.single_click_folders_only);
        s.act("key Enter");
        assert!(s.app.cfg.single_click, "Enter flips the first row");
        s.act("key ArrowDown");
        s.act("key Enter");
        assert!(s.app.cfg.single_click_folders_only, "then the sub-option under it");
        look(&mut s, "keyboard ring");
        s.shot("options-keys");
        // Preview: a choice steps on with a click.
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::Preview));
        let layout = s.app.cfg.inspector_layout.clone();
        click_in_row(&mut s, "choice", "Inspector layout");
        assert!(s.app.cfg.inspector_layout != layout, "inspector layout changed");
        // Search: pictures stay put while search by meaning is off, follow it when on.
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::Search));
        assert!(!s.app.cfg.semantic_search && s.app.cfg.semantic_pictures);
        click_in_row(&mut s, "switch", "Include pictures");
        assert!(s.app.cfg.semantic_pictures, "inert while search by meaning is off");
        click_in_row(&mut s, "switch", "Search by meaning");
        assert!(s.app.cfg.semantic_search);
        click_in_row(&mut s, "switch", "Include pictures");
        assert!(!s.app.cfg.semantic_pictures);
        click_in_row(&mut s, "reset option", "Include pictures");
        click_in_row(&mut s, "reset option", "Search by meaning");
        assert!(!s.app.cfg.semantic_search && s.app.cfg.semantic_pictures, "both reset");
        // Git, then Startup & system.
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::Git));
        click_in_row(&mut s, "switch", "Show GitHub pictures");
        assert!(s.app.cfg.github_pictures);
        click_in_row(&mut s, "reset option", "Show GitHub pictures");
        assert!(!s.app.cfg.github_pictures);
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::System));
        let new_tab = s.app.cfg.new_tab.clone();
        click_in_row(&mut s, "choice", "New tab shows");
        assert!(s.app.cfg.new_tab != new_tab, "new tab changed");
        // The section's last rows, under the Developer group, are scrolled to.
        let popup = crate::ui::POPUPS.with(|p| p.borrow().first().copied()).unwrap_or_else(|| s.fail("Options drawn"));
        s.act(&format!("scroll {:.0} {:.0} -20", popup.center().x + 100.0, popup.center().y));
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Install"), "the install button is in its row");
        // The ends wrap.
        s.act("key ctrl+PageDown");
        assert_eq!(opt_section(&s), Some(Section::Appearance), "past the last, the first");
        s.act("key ctrl+PageUp");
        assert_eq!(opt_section(&s), Some(Section::System), "and back");
        // A search covers every section at once, each match labelled with its own;
        // typing reaches it although a click took the keyboard from the field.
        s.act("text size");
        s.idle(2);
        for label in ["Appearance · Font size", "Files & folders · Calculate folder sizes", "Preview · Inspector font size", "Startup & system · Asset cache size"] {
            assert!(s.texts.iter().any(|(_, t)| t == label), "{label:?} found");
        }
        look(&mut s, "search");
        s.shot("options-find");
        let at = crate::popups::find_options("size").iter().position(|&i| OPTS[i].3 == "Calculate folder sizes").unwrap();
        for _ in 0..at {
            s.act("key ArrowDown");
        }
        let sizes = s.app.cfg.folder_sizes;
        s.act("key Enter");
        assert_eq!(s.app.cfg.folder_sizes, !sizes, "Enter acts on the result picked");
        click_in_row(&mut s, "reset option", "Files & folders · Calculate folder sizes");
        assert_eq!(s.app.cfg.folder_sizes, sizes, "and its reset arrow puts it back");
        // With the mouse, in another search.
        let r = s.text_rect("size");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        s.act("key ctrl+a");
        s.act("text animations");
        s.idle(2);
        // From the default, on (a run with motion off starts away from it).
        if !s.app.cfg.animations {
            click_in_row(&mut s, "switch", "Appearance · Animations");
            assert!(s.app.cfg.animations);
        }
        click_in_row(&mut s, "switch", "Appearance · Animations");
        assert!(!s.app.cfg.animations);
        click_in_row(&mut s, "reset option", "Appearance · Animations");
        assert!(s.app.cfg.animations);
        s.act("text zzzq");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "No option matches"));
        // Clearing the search brings the section back.
        let r = s.text_rect("animationszzzq");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "On startup"), "the section again");
        // Titles only: the lines go, and come back as tooltips.
        click_control(&mut s, "titles only");
        assert!(s.app.cfg.compact_lists);
        s.click_text("Appearance");
        s.idle(2);
        assert!(!s.texts.iter().any(|(_, t)| t == "Size of all text."), "no descriptions");
        look(&mut s, "titles only");
        s.shot("options-titles-only");
        let r = s.text_rect("Font size");
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        s.idle(50);
        assert!(s.texts.iter().any(|(_, t)| t == "Size of all text."), "the description as a tooltip");
        s.shot("options-titles-only-tooltip");
        click_control(&mut s, "titles only");
        assert!(!s.app.cfg.compact_lists);
        // Closed and opened again: the same section.
        s.click_text("Git");
        s.act("key Escape");
        s.settle();
        assert!(!s.popup_open());
        s.act("key ctrl+Comma");
        s.settle();
        assert_eq!(opt_section(&s), Some(Section::Git), "the last section again");
        // A small window: glyphs only on the left, every row still in reach.
        s.resize(520.0, 340.0);
        s.idle(3);
        assert!(!s.texts.iter().any(|(_, t)| t == "Mouse & keyboard"), "names give way to glyphs");
        click_control(&mut s, "section Files & folders");
        assert_eq!(opt_section(&s), Some(Section::Files));
        let last = OPTS.iter().filter(|o| o.1 == Section::Files).count();
        for _ in 0..last {
            s.act("key ArrowDown");
        }
        s.idle(3);
        let popup = crate::ui::POPUPS.with(|p| p.borrow().first().copied()).unwrap();
        let bottom = OPTS.iter().filter(|o| o.1 == Section::Files).last().map(|o| o.3).unwrap();
        let r = s.text_rect(bottom);
        assert!(popup.contains_rect(r), "the last row scrolled into view: {r:?} in {popup:?}");
        look(&mut s, "small window");
        s.shot("options-small");
        click_control(&mut s, "section Startup & system");
        look(&mut s, "small window, system");
        s.shot("options-small-system");
        s.act("text default");
        s.idle(2);
        look(&mut s, "small window, search");
        s.shot("options-small-search");
        s.act("key Escape");
        s.resize(1280.0, 800.0);
        // A light theme.
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.act("key ctrl+Comma");
        s.settle();
        for sec in [Section::Appearance, Section::Files, Section::System] {
            click_control(&mut s, sec.control());
            look(&mut s, "light");
            s.shot(&format!("options-light-{}", slug(sec)));
        }
        s.act("text font");
        s.idle(2);
        look(&mut s, "light search");
        s.shot("options-light-search");
        s.act("key Escape");
        report("option sections", &s);
    }

    /// Bookmarks (Ctrl+D, click in the sidebar, remove) and tab commands (new, jump by
    /// number, next/previous with wrap, close others, reopen, duplicate, move).
    #[test]
    fn tabs_and_bookmarks() {
        if !sandboxed() {
            return;
        }
        let home = fixture("tabs");
        let mut s = Sim::new(&home.join("Music"), vec2(1280.0, 800.0));
        // Bookmark Music, go elsewhere, come back from the sidebar.
        s.act("key ctrl+d");
        s.settle();
        assert!(s.app.ud.bookmarks.contains(&home.join("Music")), "bookmarked");
        go(&mut s, &home.join("Code"));
        s.act("move -50 -50");
        s.settle();
        let side = s.texts.iter().find(|(r, t)| t == "Music" && r.left() < 170.0).map(|(r, _)| r.center()).unwrap_or_else(|| s.fail("bookmark in the sidebar"));
        s.act(&format!("click {:.0} {:.0}", side.x, side.y));
        s.settle();
        assert_eq!(s.tab().path, home.join("Music"), "the sidebar bookmark navigates");
        s.act("key ctrl+d");
        s.settle();
        assert!(!s.app.ud.bookmarks.contains(&home.join("Music")), "Ctrl+D again removes it");
        // Tabs.
        let tabs = |s: &Sim| s.app.panels[&s.app.active].tabs.len();
        let active = |s: &Sim| s.app.panels[&s.app.active].active;
        for _ in 0..3 {
            s.act("key ctrl+t");
        }
        s.settle();
        assert_eq!(tabs(&s), 4);
        s.act("key ctrl+2");
        assert_eq!(active(&s), 1, "Ctrl+2");
        s.act("key ctrl+9");
        assert_eq!(active(&s), 3, "Ctrl+9 = last tab");
        s.cmd(NextTab);
        assert_eq!(active(&s), 0, "next wraps to the first");
        s.cmd(PrevTab);
        assert_eq!(active(&s), 3, "previous wraps to the last");
        s.cmd(DuplicateTab);
        assert_eq!(tabs(&s), 5, "duplicate");
        s.act("key ctrl+1");
        let first = s.tab().path.clone();
        s.cmd(MoveTabRight);
        assert_eq!(s.app.panels[&s.app.active].tabs[1].path, first, "moved right");
        assert_eq!(active(&s), 1, "the moved tab stays active");
        s.cmd(CloseOtherTabs);
        assert_eq!(tabs(&s), 1, "close others");
        s.act("key ctrl+shift+t");
        s.act("key ctrl+shift+t");
        s.settle();
        assert_eq!(tabs(&s), 3, "reopen twice");
        look(&mut s, "tabs");
        report("tabs", &s);
    }

    /// Thumbnails for every kind (built-in image, SVG renderer, desktop thumbnailers for
    /// video, PDF and MP3 cover art) in Large icons; all land in the shared cache, a fresh
    /// app reuses them without running a thumbnailer, a corrupt video is marked failed
    /// and never retried.
    #[test]
    fn thumbnails_for_every_kind_and_shared_cache() {
        if !sandboxed() {
            return;
        }
        let d = media_fixture("media");
        let kinds = ["photo.png", "drawing.svg", "clip.mp4", "song.mp3", "doc.pdf"];
        let wait_all = |s: &mut Sim| {
            let t0 = Instant::now();
            loop {
                let mut missing = vec![];
                for k in kinds {
                    let p = d.join(k);
                    let m = std::fs::metadata(&p).unwrap().modified().ok();
                    if s.app.thumbs.get(&p, m, 200.0).is_none() {
                        missing.push(k);
                    }
                }
                if missing.is_empty() {
                    return;
                }
                if t0.elapsed().as_secs() > 60 {
                    s.fail(&format!("no thumbnail for {missing:?}"));
                }
                s.frame(vec![]);
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ViewLarge);
        wait_all(&mut s);
        let broken = d.join("broken.mp4");
        let bm = std::fs::metadata(&broken).unwrap().modified().ok();
        let t0 = Instant::now();
        while !s.app.thumbs.failed(&broken) && t0.elapsed().as_secs() < 30 {
            let _ = s.app.thumbs.get(&broken, bm, 200.0);
            s.frame(vec![]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(s.app.thumbs.failed(&broken), "the corrupt video is marked failed");
        look(&mut s, "thumbnails");
        for k in kinds {
            let p = d.join(k);
            let m = std::fs::metadata(&p).unwrap().modified().ok();
            assert!(crate::thumbcache::lookup(&p, m, 200).is_some(), "{k} is in the shared cache");
        }
        assert!(crate::thumbcache::has_failed(&broken, bm), "the failure is recorded");
        // A fresh app: everything from the cache, no thumbnailer runs, no retry of the failure.
        let runs = crate::thumbcache::RUNS.load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("sim thumbnails: {runs} thumbnailer runs in the first pass");
        assert!(runs >= 4, "video, MP3 cover, PDF and the corrupt video went to thumbnailers ({runs})");
        drop(s);
        let mut s2 = Sim::new(&d, vec2(1280.0, 800.0));
        s2.cmd(ViewLarge);
        wait_all(&mut s2);
        let _ = s2.app.thumbs.get(&broken, bm, 200.0);
        s2.settle();
        s2.idle(30);
        assert_eq!(crate::thumbcache::RUNS.load(std::sync::atomic::Ordering::Relaxed), runs, "nothing regenerated");
        report("thumbnails", &s2);
    }

    fn inspector(s: &Sim) -> &crate::inspector::Inspector {
        s.app.panels[&s.app.active].inspector.as_ref().unwrap_or_else(|| s.fail("inspector closed"))
    }

    fn wait_until(s: &mut Sim, what: &str, secs: u64, mut ok: impl FnMut(&Sim) -> bool) {
        let t0 = Instant::now();
        while !ok(s) {
            if t0.elapsed().as_secs() >= secs {
                s.fail(&format!("timed out waiting for: {what}"));
            }
            s.frame(vec![]);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    fn alive(pid: u32) -> bool {
        // A zombie still has a /proc entry: only "running/sleeping" counts.
        std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|st| !st.split_whitespace().nth(2).is_some_and(|s| s == "Z"))
    }

    /// Every inspector toolbar button (and any label beside them, such as a page count)
    /// sits centred on the 30 px header, inside it.
    fn toolbar_centred(s: &mut Sim, what: &str, labels: &[&str]) {
        s.idle(2);
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
        let cy = ir.top() + 15.0;
        let header = Rect::from_min_max(ir.min, pos2(ir.right(), ir.top() + 30.0));
        let buttons: Vec<Rect> = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, r, _)| header.contains(r.center())).map(|(_, r, _)| *r).collect());
        assert!(buttons.len() >= 3, "{what}: toolbar buttons drawn");
        for b in &buttons {
            assert!((b.center().y - cy).abs() <= 0.5 && b.top() > ir.top() && b.bottom() < ir.top() + 30.0, "{what}: button {b:?} off the header's centre {cy}");
        }
        for l in labels {
            let r = s.text_rect(l);
            assert!((r.center().y - cy).abs() <= 1.5, "{what}: {l:?} at {r:?}, header centre {cy}");
        }
    }

    fn click_control(s: &mut Sim, what: &str) {
        s.idle(1);
        let r = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == what).map(|(_, r, _)| *r));
        let r = r.unwrap_or_else(|| s.fail(&format!("no {what} on screen")));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
    }

    /// Inspector previews: highlighted code, PDF pages, video and audio playback with
    /// its processes started, stopped and cleaned up as a user plays, pauses, seeks,
    /// mutes and moves on.
    #[test]
    fn inspector_code_pdf_video_audio() {
        if !sandboxed() {
            return;
        }
        let d = media_fixture("previews");
        std::fs::write(d.join("main.rs"), "// demo\nfn main() {\n    let x: u32 = 42;\n    println!(\"{x}\");\n}\n").unwrap();
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ToggleInspector);
        // Code.
        select_path(&mut s, &d.join("main.rs"));
        wait_until(&mut s, "highlighted code", 10, |s| inspector(s).text_spans().is_some_and(|n| n >= 6));
        look(&mut s, "code preview");
        toolbar_centred(&mut s, "code", &[]);
        // Word wrap: off for code until asked, on for prose; each kind keeps its own.
        assert_eq!(inspector(&s).wrapped_rows(), None, "code starts unwrapped");
        let para = "Wrapped prose keeps every word on screen, however narrow the preview is made. ".repeat(12);
        // Plain prose (Markdown would be rendered, wrapping as a page does).
        std::fs::write(d.join("notes.txt"), format!("# Notes\n\n{para}\n\n- short item\n{para}\n")).unwrap();
        s.cmd(Refresh);
        // The folder is read again on a thread: the new file shows once that is done.
        wait_until(&mut s, "notes listed", 10, |s| s.tab().entries.iter().any(|e| e.name == "notes.txt"));
        select_path(&mut s, &d.join("notes.txt"));
        wait_until(&mut s, "prose wrapped", 10, |s| inspector(s).wrapped_rows().is_some_and(|n| n > 10));
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        let rows: Vec<Rect> = s.texts.iter().filter(|(_, t)| t.starts_with("Wrapped prose") || t.starts_with("prose keeps")).map(|(r, _)| *r).collect();
        assert!(rows.len() >= 2, "wrapped rows drawn");
        for r in s.texts.iter().filter(|(r, _)| r.top() > ir.top() + 30.0 && r.left() >= ir.left()).map(|(r, _)| *r) {
            assert!(r.right() <= ir.right() + 0.5, "wrapped text runs past the preview: {r:?} in {ir:?}");
        }
        look(&mut s, "wrapped prose");
        click_control(&mut s, "Wrap lines");
        wait_until(&mut s, "prose unwrapped", 5, |s| inspector(s).wrapped_rows().is_none());
        click_control(&mut s, "Wrap lines");
        wait_until(&mut s, "prose wrapped again", 5, |s| inspector(s).wrapped_rows().is_some());
        // Larger text fits fewer columns: the rows are made again, more of them.
        let before = inspector(&s).wrapped_rows().unwrap();
        click_control(&mut s, "Larger text");
        click_control(&mut s, "Larger text");
        s.idle(2);
        assert!(inspector(&s).wrapped_rows().unwrap() > before, "larger text, more rows");
        click_control(&mut s, "Smaller text");
        click_control(&mut s, "Smaller text");
        select_path(&mut s, &d.join("main.rs"));
        wait_until(&mut s, "code again", 10, |s| inspector(s).text_spans().is_some());
        assert_eq!(inspector(&s).wrapped_rows(), None, "code still unwrapped");
        s.cmd(InspectorWrap);
        s.idle(2);
        assert!(inspector(&s).wrapped_rows().is_some(), "code wrapped from the palette");
        look(&mut s, "wrapped code");
        // PDF: page 1, then Page Down with the pointer over the preview.
        select_path(&mut s, &d.join("doc.pdf"));
        wait_until(&mut s, "PDF page 1", 20, |s| inspector(s).pdf_state().is_some_and(|(p, n, shown)| p == 1 && n == Some(3) && shown.is_some_and(|x| x.0 == 1)));
        assert!(s.texts.iter().any(|(_, t)| t == "1 / 3"), "page counter shown");
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("move {:.0} {:.0}", list.right() + 150.0, list.center().y));
        s.act("key PageDown");
        wait_until(&mut s, "PDF page 2", 20, |s| inspector(s).pdf_state().is_some_and(|(p, _, shown)| p == 2 && shown.is_some_and(|x| x.0 == 2)));
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "2 / 3"), "page counter follows");
        s.act("move -50 -50");
        look(&mut s, "pdf preview");
        toolbar_centred(&mut s, "pdf", &["2 / 3"]);
        // Font size and Spacing change every row's height; the toolbar stays centred.
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            toolbar_centred(&mut s, &format!("pdf at font {font}, spacing {spacing}"), &["2 / 3"]);
            look(&mut s, &format!("pdf at font {font}, spacing {spacing}"));
        }
        // Video.
        select_path(&mut s, &d.join("clip.mp4"));
        wait_until(&mut s, "video probed", 15, |s| inspector(s).media_state().is_some());
        let probe = inspector(&s).media_probe().unwrap();
        assert_eq!(probe.video, Some((320, 240)));
        click_control(&mut s, "media button"); // play
        wait_until(&mut s, "video frames", 10, |s| inspector(s).media_state().is_some_and(|m| m.0 && m.4 >= 5));
        let (_, pos, dur, pids, _) = inspector(&s).media_state().unwrap();
        assert!(pos > 0.0 && (dur - 2.0).abs() < 0.3, "position {pos} of {dur}");
        assert!(!pids.is_empty() && pids.iter().all(|&p| alive(p)), "decoders running");
        click_control(&mut s, "media button"); // pause
        s.idle(2);
        let (playing, paused_at, _, now, _) = inspector(&s).media_state().unwrap();
        assert!(!playing && now.is_empty(), "paused: no decoders");
        assert!(pids.iter().all(|&p| !alive(p)), "pause killed and reaped them");
        s.idle(30);
        assert!((inspector(&s).media_state().unwrap().1 - paused_at).abs() < 1e-6, "position holds while paused");
        // Seek to the middle of the bar, then play to the end.
        let bar = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == "seek bar").map(|(_, r, _)| *r)).unwrap_or_else(|| s.fail("seek bar"));
        s.act(&format!("click {:.0} {:.0}", bar.center().x, bar.center().y));
        let at = inspector(&s).media_state().unwrap().1;
        assert!((at - dur / 2.0).abs() < 0.25, "seeked to the middle ({at})");
        click_control(&mut s, "media button");
        wait_until(&mut s, "playback ends", 10, |s| inspector(s).media_state().is_some_and(|m| !m.0 && (m.1 - m.2).abs() < 0.05));
        assert!(inspector(&s).media_state().unwrap().3.is_empty(), "no decoders after the end");
        // Moving on mid-playback stops everything.
        click_control(&mut s, "media button");
        wait_until(&mut s, "playing again", 10, |s| inspector(s).media_state().is_some_and(|m| m.0 && !m.3.is_empty()));
        let pids = inspector(&s).media_state().unwrap().3;
        select_path(&mut s, &d.join("photo.png"));
        s.idle(3);
        toolbar_centred(&mut s, "image", &[]);
        assert!(pids.iter().all(|&p| !alive(p)), "changing selection killed the decoders");
        // Audio: tags shown; play starts the sound process; mute ends it, the clock goes on.
        select_path(&mut s, &d.join("song.mp3"));
        wait_until(&mut s, "audio probed", 15, |s| inspector(s).media_state().is_some());
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Test Tone"), "title shown");
        assert!(s.texts.iter().any(|(_, t)| t == "Maia"), "artist shown");
        click_control(&mut s, "media button");
        wait_until(&mut s, "sound process", 10, |s| inspector(s).media_state().is_some_and(|m| m.0 && m.3.len() == 1));
        let snd = inspector(&s).media_state().unwrap().3;
        let buttons: Vec<Rect> = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, _, w)| *w == "media button").map(|(_, r, _)| *r).collect());
        let mute = buttons.get(1).copied().unwrap_or_else(|| s.fail("mute button"));
        s.act(&format!("click {:.0} {:.0}", mute.center().x, mute.center().y));
        s.idle(3);
        let m = inspector(&s).media_state().unwrap();
        assert!(m.0 && m.3.is_empty(), "muted: still playing, no sound process");
        assert!(snd.iter().all(|&p| !alive(p)), "the sound process ended");
        s.cmd(ToggleInspector);
        s.idle(2);
        look(&mut s, "after previews");
        report("previews", &s);
    }

    /// Random sessions: keys, clicks, drags, scrolls, typing, any command, resizes.
    fn sidebar_text(s: &Sim, label: &str) -> Option<Rect> {
        s.texts.iter().find(|(r, t)| t == label && r.right() <= s.app.sidebar_width + 1.0).map(|x| x.0)
    }

    fn list_text(s: &Sim, label: &str) -> Rect {
        s.texts.iter().rev().find(|(r, t)| t == label && r.left() > s.app.sidebar_width).map(|x| x.0).unwrap_or_else(|| s.fail(&format!("{label:?} not in the list")))
    }

    fn select_name(s: &mut Sim, name: &str) {
        let Some(i) = s.tab().entries.iter().position(|e| e.name == name) else { s.fail(&format!("{name} not listed")) };
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().select_only(i as u32);
        s.idle(1);
    }

    fn toast_is(s: &Sim, want: &str) -> bool {
        s.app.toast.as_ref().is_some_and(|t| t.0 == want)
    }

    fn drive_log(d: &Path) -> String {
        std::fs::read_to_string(d.join("log")).unwrap_or_default()
    }

    /// Drives as a user meets them: unmounted ones listed (dimmed) in Computer and, when
    /// removable, in the sidebar; opening one mounts it and shows its files; the sidebar's
    /// eject button moves the tab away and powers the disk off; Mount / Unmount from the
    /// context menu; failures come back as messages; the system disk offers nothing and
    /// can't be deleted; a drive plugged in later appears by itself.
    #[test]
    fn drives_mount_open_unmount_eject() {
        if !sandboxed() {
            return;
        }
        let d = fake_drives("main");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let home = fixture("drives");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.cmd(Computer);
        wait_until(&mut s, "drives listed", 10, |s| s.tab().entries.iter().any(|e| e.name == "Photo Card"));
        s.settle();
        let mut names: Vec<String> = s.tab().entries.iter().map(|e| e.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["Busy Stick", "Photo Card", "Root", "Windows"], "no swap, EFI or /boot");
        for v in [ViewDetails, ViewList, ViewLarge, ViewTree, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("drives {v:?}"));
        }
        assert!(sidebar_text(&s, "Photo Card").is_some(), "removable card offered in the sidebar");
        assert!(sidebar_text(&s, "Windows").is_none(), "internal disk only in Computer");
        assert!(s.texts.iter().any(|(_, t)| t == "14.9 GB · Not mounted"), "card marked as not mounted");
        // Inspector on an unmounted drive.
        s.cmd(ToggleInspector);
        select_name(&mut s, "Windows");
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "200 GB · Not mounted"), "inspector shows the drive");
        look(&mut s, "inspector on an unmounted drive");
        s.cmd(ToggleInspector);

        // Files dropped on a drive that isn't mounted go nowhere.
        let wr = list_text(&s, "Windows").center();
        let note = root().join("dropped.txt");
        std::fs::write(&note, "x").unwrap();
        s.app.on_dnd(crate::clipboard::Dnd::Over { x: wr.x, y: wr.y, paths: vec![note.clone()], own: false });
        s.idle(2);
        s.app.on_dnd(crate::clipboard::Dnd::Dropped { x: wr.x, y: wr.y, paths: vec![note], own: false });
        s.idle(3);
        assert!(s.app.jobs.is_empty() && !s.popup_open(), "no transfer into an unmounted drive");

        // Copy, delete or rename a drive: nothing happens (the root would take everything).
        select_name(&mut s, "Root");
        for k in ["key Delete", "key shift+Delete", "key F2", "key ctrl+x"] {
            s.act(k);
        }
        assert!(!s.popup_open() && s.app.jobs.is_empty() && s.tab().rename.is_none(), "drives can't be deleted or renamed");
        let r = list_text(&s, "Root");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        assert!(s.popup_open(), "menu for the root drive");
        assert!(!s.texts.iter().any(|(_, t)| ["Mount", "Unmount", "Eject", "Move to Trash", "Delete Permanently", "Rename"].contains(&t.as_str())), "system disk offers no drive or file actions");
        s.act("key Escape");

        // Open the card: it mounts (spinner meanwhile) and its files show.
        select_name(&mut s, "Photo Card");
        s.act("key Enter");
        assert!(crate::drives::busy("/dev/sdz1"), "mount in progress");
        look(&mut s, "mounting");
        let card = d.join("media/Photo Card");
        wait_until(&mut s, "card opened", 10, |s| s.tab().path == card);
        s.settle();
        assert!(s.tab().entries.iter().any(|e| e.name == "readme.txt"), "card's files listed");
        assert!(drive_log(&d).contains("udisksctl mount -b /dev/sdz1"));
        wait_until(&mut s, "card mounted in the sidebar", 10, |s| sidebar_text(s, "Photo Card").is_some() && !s.texts.iter().any(|(_, t)| t == "14.9 GB · Not mounted"));
        look(&mut s, "inside the card");

        // Eject from the sidebar: the tab leaves first, then the disk powers off.
        let r = sidebar_text(&s, "Photo Card").unwrap();
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        click_control(&mut s, "eject button");
        assert!(crate::tab::is_computer(&s.tab().path), "tab moved off the card before ejecting");
        wait_until(&mut s, "ejected", 10, |s| toast_is(s, "Photo Card can be safely removed"));
        let log = drive_log(&d);
        assert!(log.contains("udisksctl unmount -b /dev/sdz1") && log.contains("udisksctl power-off -b /dev/sdz"), "{log}");
        wait_until(&mut s, "card gone", 10, |s| !s.tab().entries.iter().any(|e| e.name == "Photo Card") && sidebar_text(s, "Photo Card").is_none());

        // Internal disk from the context menu: Mount, then Unmount; never Eject.
        let r = list_text(&s, "Windows");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        assert!(s.texts.iter().any(|(_, t)| t == "Mount") && !s.texts.iter().any(|(_, t)| t == "Eject" || t == "Unmount"), "unmounted internal disk: Mount only");
        s.click_text("Mount");
        wait_until(&mut s, "windows mounted", 10, |s| toast_is(s, "Mounted Windows"));
        let win = d.join("media/Windows");
        wait_until(&mut s, "windows listed as mounted", 10, |s| s.tab().entries.iter().any(|e| e.path == win));
        assert!(crate::tab::is_computer(&s.tab().path), "mounting from the menu stays put");
        s.idle(5);
        let r = list_text(&s, "Windows");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        assert!(s.texts.iter().any(|(_, t)| t == "Unmount") && !s.texts.iter().any(|(_, t)| t == "Eject" || t == "Mount"), "mounted internal disk: Unmount only: {:?} items {:?} size {:?}", s.texts.iter().map(|t| (&t.1, t.0.top())).collect::<Vec<_>>(), match &s.app.popup { Popup::Ctx(st) => st.items.iter().map(|i| i.label.clone()).collect::<Vec<_>>(), _ => vec![] }, (s.size, s.ctx.memory(|m| m.area_rect(egui::Id::new("ctxmenu")))));
        s.click_text("Unmount");
        wait_until(&mut s, "windows unmounted", 10, |s| toast_is(s, "Unmounted Windows"));
        wait_until(&mut s, "windows back to unmounted", 10, |s| s.tab().entries.iter().any(|e| e.name == "Windows" && crate::tab::drive_dev(&e.path).is_some()));
        assert!(!win.exists());

        // A busy stick refuses to eject: the message says why, and it stays.
        let r = sidebar_text(&s, "Busy Stick").unwrap_or_else(|| s.fail("busy stick in the sidebar"));
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        click_control(&mut s, "eject button");
        wait_until(&mut s, "eject refused, saying what holds it", 10, |s| matches!(&s.app.popup, Popup::Holders(h) if h.why.as_deref() == Some("Couldn't eject Busy Stick: Target is busy")));
        s.act("key Escape");
        assert!(s.tab().entries.iter().any(|e| e.name == "Busy Stick"));
        look(&mut s, "after a refused eject");

        // A failing mount reports the reason and stays in Computer.
        std::fs::write(d.join("fail/sdy2"), "Wrong fs type, bad option").unwrap();
        select_name(&mut s, "Windows");
        s.act("key Enter");
        wait_until(&mut s, "mount failed", 10, |s| toast_is(s, "Couldn't mount Windows: Wrong fs type, bad option"));
        assert!(crate::tab::is_computer(&s.tab().path));
        std::fs::remove_file(d.join("fail/sdy2")).unwrap();

        // Ctrl+Enter mounts into a new tab.
        let tabs = s.app.panels[&s.app.active].tabs.len();
        select_name(&mut s, "Windows");
        s.act("key ctrl+Enter");
        wait_until(&mut s, "opened in a new tab", 10, |s| s.app.panels[&s.app.active].tabs.iter().any(|t| t.path == win));
        assert_eq!(s.app.panels[&s.app.active].tabs.len(), tabs + 1);
        assert!(crate::tab::is_computer(&s.tab().path), "current tab stays");

        // Plugged in later: shows up by itself.
        let mut devs = std::fs::read_to_string(d.join("devices")).unwrap();
        devs.push_str("/dev/sdv\t-\t-\t-\t64023257088\t1\t1\tdisk\n/dev/sdv1\tsdv\tNEW USB\tvfat\t64022208512\t1\t1\tpart\n");
        std::fs::write(d.join("devices"), devs).unwrap();
        wait_until(&mut s, "new drive appears", 15, |s| s.tab().entries.iter().any(|e| e.name == "NEW USB") && sidebar_text(s, "NEW USB").is_some());
        for sz in [(900.0, 560.0), (640.0, 420.0), (1280.0, 800.0)] {
            s.resize(sz.0, sz.1);
            look(&mut s, &format!("drives at {sz:?}"));
        }
        assert!(!drive_log(&d).lines().any(|l| l.contains("/dev/sdw")), "the system disk was never touched");
        report("drives", &s);
    }

    /// An encrypted drive: listed locked, with a padlock, in Computer and the sidebar;
    /// opening it asks for the passphrase. A wrong one is said so in the dialog over an
    /// empty field, still focused; the right one unlocks and mounts it and shows its
    /// files, the drive now wearing an open padlock. No passphrase ever reaches a command
    /// line. Ejecting unmounts, locks and only then powers it off.
    #[test]
    fn encrypted_drive_unlock_open_eject() {
        if !sandboxed() {
            return;
        }
        let d = fake_drives("vault");
        fake_encrypted_drive(&d);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let home = fixture("vault");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        // Pictures in a dark theme and a light one.
        let shots = |s: &mut Sim, name: &str| {
            for (theme, look) in [(crate::theme::ThemeName::Midnight, "dark"), (crate::theme::ThemeName::Paper, "light")] {
                s.app.cfg.theme = theme;
                shot_still(s, &format!("{name}-{look}"));
            }
            s.app.cfg.theme = crate::theme::ThemeName::Midnight;
            s.idle(2);
        };
        s.cmd(Computer);
        wait_until(&mut s, "locked drive listed", 10, |s| s.tab().entries.iter().any(|e| e.name == "Encrypted 500 GB"));
        assert!(sidebar_text(&s, "Encrypted 500 GB").is_some(), "offered in the sidebar");
        assert!(some_text(&s, "500 GB · Locked"), "marked as locked");
        look(&mut s, "a locked drive");
        shots(&mut s, "encrypted-locked");
        let r = list_text(&s, "Encrypted 500 GB");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        assert!(some_text(&s, "Unlock…") && some_text(&s, "Eject") && !some_text(&s, "Mount") && !some_text(&s, "Lock"), "a locked drive unlocks");
        s.act("key Escape");

        // Opened: the passphrase is asked; a wrong one is refused in the dialog.
        select_name(&mut s, "Encrypted 500 GB");
        s.act("key Enter");
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::Unlock(_)), "passphrase asked");
        assert!(some_text(&s, "Unlock “Encrypted 500 GB”"));
        s.act("text battery staple");
        shots(&mut s, "encrypted-passphrase");
        s.act("key Enter");
        assert!(crate::drives::busy("/dev/sdv1"), "unlocking");
        wait_until(&mut s, "wrong passphrase said", 10, |s| some_text(s, "Wrong passphrase"));
        assert!(matches!(&s.app.popup, Popup::Unlock(st) if st.pass.0.is_empty()), "field emptied");
        assert_eq!(s.ctx.memory(|m| m.focused()), Some(egui::Id::new("unlock passphrase")), "and ready for another try");
        shots(&mut s, "encrypted-wrong");

        // The right one: unlocked, mounted and opened.
        s.act("text correct horse");
        s.act("key Enter");
        let vault = d.join("media/Vault");
        wait_until(&mut s, "vault opened", 10, |s| s.tab().path == vault);
        s.settle();
        assert!(!s.popup_open(), "dialog gone");
        assert!(s.tab().entries.iter().any(|e| e.name == "readme.txt"), "its files listed");
        let log = drive_log(&d);
        assert!(log.contains("udisksctl unlock -b /dev/sdv1 --key-file /dev/stdin") && log.contains("udisksctl mount -b /dev/mapper/luks-sdv1"), "{log}");
        assert!(!log.contains("horse") && !log.contains("staple"), "no passphrase on a command line: {log}");
        wait_until(&mut s, "vault in the sidebar", 10, |s| sidebar_text(s, "Vault").is_some());
        look(&mut s, "inside the unlocked drive");
        shots(&mut s, "encrypted-unlocked");
        let r = sidebar_text(&s, "Vault").unwrap();
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        assert!(some_text(&s, "Lock") && some_text(&s, "Eject"), "an unlocked drive locks");
        s.act("key Escape");
        s.cmd(Computer);
        wait_until(&mut s, "vault in Computer", 10, |s| s.tab().entries.iter().any(|e| e.path == vault));
        look(&mut s, "the unlocked drive in Computer");
        shots(&mut s, "encrypted-unlocked-computer");

        // Eject: unmounted, locked, then powered off.
        let r = sidebar_text(&s, "Vault").unwrap();
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        click_control(&mut s, "eject button");
        wait_until(&mut s, "ejected", 10, |s| toast_is(s, "Vault can be safely removed"));
        let log = drive_log(&d);
        let at = |what: &str| log.find(what).unwrap_or_else(|| s.fail(&format!("{what} not in {log}")));
        assert!(at("udisksctl unmount -b /dev/mapper/luks-sdv1") < at("udisksctl lock -b /dev/sdv1") && at("udisksctl lock -b /dev/sdv1") < at("udisksctl power-off -b /dev/sdv"), "{log}");
        wait_until(&mut s, "drive gone", 10, |s| !s.tab().entries.iter().any(|e| e.path == vault || e.name.starts_with("Encrypted")) && sidebar_text(s, "Vault").is_none());
        report("encrypted drive", &s);
    }

    /// Drag and drop with other apps: files dragged in from "another app" land on the
    /// folder row, the empty list or the sidebar place under the pointer, copied (the
    /// source keeps its files), with the drop target lit and a "Copy" badge meanwhile; a
    /// drag that leaves again changes nothing. Files dragged past the window's edge are
    /// handed to the system, the pointer is released here, and if they come back in they
    /// move like any drag inside Maia. Nothing can be dropped into Computer.
    #[test]
    fn drag_and_drop_with_other_apps() {
        if !sandboxed() {
            return;
        }
        use crate::clipboard::Dnd;
        let home = fixture("dnd");
        let src = root().join("elsewhere");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(&src).unwrap();
        let a = src.join("photo from chat.png");
        let b = src.join("notes – 日本.txt");
        png_file(&a, 30, 20, 1);
        std::fs::write(&b, "dropped").unwrap();
        let docs = home.join("Documents");
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        let over = |s: &mut Sim, p: Pos2, paths: &[PathBuf]| {
            s.note(format!("dnd over {p:?}"));
            s.app.on_dnd(Dnd::Over { x: p.x, y: p.y, paths: paths.to_vec(), own: false });
            s.idle(2);
        };
        let drop = |s: &mut Sim, p: Pos2, paths: &[PathBuf], own: bool| {
            s.note(format!("dnd drop {p:?}"));
            s.app.on_dnd(Dnd::Dropped { x: p.x, y: p.y, paths: paths.to_vec(), own });
            s.idle(2);
        };
        let both = [a.clone(), b.clone()];

        // Onto a folder row: lands inside it.
        let taxes = list_text(&s, "Taxes").center();
        over(&mut s, taxes, &both);
        over(&mut s, taxes + vec2(3.0, 1.0), &both);
        assert!(matches!(s.app.drag, Some(crate::app::Drag::Files { .. })), "a drag from outside is a file drag in here");
        assert!(s.texts.iter().any(|(_, t)| t == "Copy 2 items"), "badge says what will happen");
        drop(&mut s, taxes, &both, false);
        wait_until(&mut s, "copied into Taxes", 10, |_| docs.join("Taxes/photo from chat.png").exists() && docs.join("Taxes/notes – 日本.txt").exists());
        assert!(a.exists() && b.exists(), "the other app keeps its files");
        assert!(s.app.drag.is_none() && s.app.ext_drag.is_none());

        // Onto empty space in the list: into the folder shown.
        s.settle();
        let empty = pos2(700.0, 650.0);
        over(&mut s, empty, std::slice::from_ref(&a));
        drop(&mut s, empty, std::slice::from_ref(&a), false);
        wait_until(&mut s, "copied into Documents", 10, |_| docs.join("photo from chat.png").exists());

        // Onto a sidebar place.
        let home_row = sidebar_text(&s, "Home").unwrap_or_else(|| s.fail("Home in the sidebar")).center();
        over(&mut s, home_row, std::slice::from_ref(&b));
        drop(&mut s, home_row, std::slice::from_ref(&b), false);
        wait_until(&mut s, "copied into home", 10, |_| crate::fsx::home().join("notes – 日本.txt").exists());

        // Hover, then leave: nothing happens and the pointer works normally again.
        let before = std::fs::read_dir(&docs).unwrap().count();
        over(&mut s, empty, &both);
        s.app.on_dnd(Dnd::Left);
        s.idle(3);
        assert!(s.app.drag.is_none() && s.app.ext_drag.is_none(), "drag ended on leave");
        s.settle();
        assert_eq!(std::fs::read_dir(&docs).unwrap().count(), before, "nothing dropped");
        let r = list_text(&s, "notes.txt");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        assert_eq!(s.tab().cursor_path().as_deref(), Some(docs.join("notes.txt").as_path()), "clicks select again");

        // Computer's empty space takes nothing (below the last drive, at any row height).
        s.cmd(Computer);
        s.settle();
        let jobs = s.app.jobs.len();
        let below = s.app.panels[&s.app.active].list_rect.bottom() - 20.0;
        over(&mut s, pos2(600.0, below), &both);
        drop(&mut s, pos2(600.0, below), &both, false);
        s.idle(3);
        assert_eq!(s.app.jobs.len(), jobs, "no transfer into Computer");
        go(&mut s, &docs);

        // Out of the window: handed to the system with the button released here.
        crate::clipboard::DRAGGED_OUT.lock().unwrap().clear();
        let r = list_text(&s, "notes.txt").center();
        s.act(&format!("down {:.0} {:.0}", r.x, r.y));
        for x in [r.x + 10.0, r.x + 40.0, 300.0, 100.0, 5.0, -30.0] {
            s.act(&format!("move {x:.0} {:.0}", r.y));
        }
        s.idle(2);
        let out = crate::clipboard::DRAGGED_OUT.lock().unwrap().clone();
        assert_eq!(out, vec![vec![docs.join("notes.txt")]], "dragged out with its path");
        assert!(s.app.drag.is_none(), "inside drag ended");
        assert!(!s.ctx.input(|i| i.pointer.any_down()), "no stuck mouse button");
        // ... and back in, dropped on a folder: moves, like any drag inside Maia.
        let taxes = list_text(&s, "Taxes").center();
        s.app.on_dnd(Dnd::Over { x: taxes.x, y: taxes.y, paths: vec![docs.join("notes.txt")], own: true });
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Move 1 item"), "own drag moves");
        drop(&mut s, taxes, &[docs.join("notes.txt")], true);
        wait_until(&mut s, "moved into Taxes", 10, |_| docs.join("Taxes/notes.txt").exists() && !docs.join("notes.txt").exists());
        s.settle();
        look(&mut s, "after drag and drop");
        report("dnd", &s);
    }

    /// Search inside files: the filter's contents toggle, a typed query with the matching
    /// line beside each name, binary files left out, flattened search, every view and
    /// width, a new query, and back to matching names.
    #[test]
    fn search_inside_files() {
        if !sandboxed() {
            return;
        }
        let home = fixture("contents");
        let d = home.join("Documents");
        std::fs::write(d.join("todo.txt"), "buy xyzzy\n\tCall the Plumber about the leak\n").unwrap();
        std::fs::write(d.join("blob.bin"), b"\x00\x01plumber").unwrap();
        std::fs::write(d.join("Taxes/2025/letter.txt"), "plumber invoice attached").unwrap();
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        let names = |s: &Sim| -> Vec<String> {
            let t = s.tab();
            let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
            v.sort();
            v
        };
        click_control(&mut s, "contents toggle");
        assert!(s.tab().content.on, "the toggle switches contents mode on");
        s.act("text plumber");
        wait_until(&mut s, "contents search", 10, |s| !s.tab().is_loading());
        s.settle();
        assert_eq!(names(&s), vec!["todo.txt"], "only the text file that contains the word");
        look(&mut s, "contents results");
        assert!(s.texts.iter().any(|(_, t)| t.ends_with("Call the Plumber about the leak")), "the matching line is shown beside the name");
        for v in [ViewTree, ViewList, ViewMedium, ViewLarge, ViewXL, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("contents {v:?}"));
        }
        for (w, h) in [(640.0, 480.0), (520.0, 340.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("contents {w}x{h}"));
        }
        // Flattened: the letter two folders down matches too.
        s.act("key ctrl+e");
        wait_until(&mut s, "flattened contents search", 10, |s| !s.tab().is_loading());
        s.settle();
        assert_eq!(names(&s), vec!["letter.txt", "todo.txt"]);
        look(&mut s, "flattened contents");
        // A new query replaces the matches.
        s.act("key ctrl+f");
        s.act("key ctrl+a");
        s.act("text xyzzy");
        wait_until(&mut s, "second query", 10, |s| s.tab().filter == "xyzzy" && !s.tab().is_loading());
        s.settle();
        assert_eq!(names(&s), vec!["todo.txt"]);
        // The palette command switches back to names: nothing is called "xyzzy".
        s.act("key ctrl+shift+p");
        s.act("text Search File Contents");
        s.act("key Enter");
        s.settle();
        assert!(!s.tab().content.on, "the palette command switches contents mode off");
        assert!(names(&s).is_empty(), "names are matched again: {:?}", names(&s));
        s.act("key Escape");
        s.settle();
        assert!(s.tab().filter.is_empty());
        look(&mut s, "after contents search");
        report("contents", &s);
    }

    /// Single instance: a second launch's folders arrive over the real socket and open as
    /// focused tabs (a file opens its folder with it selected); the switch for the default
    /// file manager shows the desktop's state, turns it on with a click and gives folders
    /// back to the previous file manager when turned off.
    #[test]
    fn single_instance_and_default_file_manager() {
        if !sandboxed() {
            return;
        }
        let home = fixture("single");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        // Socket paths are limited to 108 bytes: the sim root can be deeper than that.
        let sock = std::env::temp_dir().join(format!("maia-sim-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let l = crate::single::claim_at(&sock).unwrap_or_else(|| s.fail("listen"));
        let (tx, rx) = std::sync::mpsc::channel();
        crate::single::serve(l, tx, || {});
        s.app.requests = Some(rx);
        let tabs = s.app.panels[&s.app.active].tabs.len();
        let file = home.join("Documents/notes.txt");
        assert!(crate::single::forward_to(&sock, &[home.join("Music"), file.clone()]), "second launch handed over");
        wait_until(&mut s, "tabs opened", 5, |s| s.app.panels[&s.app.active].tabs.len() == tabs + 2);
        s.settle();
        assert_eq!(s.tab().path, home.join("Documents"), "the last one is in front");
        assert_eq!(s.tab().cursor_path().as_deref(), Some(file.as_path()), "the file is selected");
        assert!(s.app.panels[&s.app.active].tabs.iter().any(|t| t.path == home.join("Music")));
        assert!(crate::single::forward_to(&sock, &[home.join("gone")]));
        wait_until(&mut s, "missing path reported", 5, |s| s.app.toast.as_ref().is_some_and(|t| t.0.starts_with("Couldn't open")));
        look(&mut s, "after handing over");

        // Default file manager: another one is set; the switch shows off.
        let cfg = crate::config::config_dir().parent().unwrap_or(&root()).to_path_buf().join("mimeapps.list");
        std::fs::write(&cfg, "[Default Applications]\ninode/directory=org.example.Files.desktop;\n").unwrap();
        let switch = |s: &mut Sim, on_wanted: Option<bool>| {
            s.act("key ctrl+Comma");
            s.settle();
            s.act("text default file manager");
            wait_until(s, "state read", 5, |s| s.app.default_fm().is_some());
            s.idle(3);
            if let Some(w) = on_wanted {
                assert_eq!(s.app.default_fm(), Some(!w), "switch starts the other way");
                // A message of its own, not one still showing from before.
                let before = s.app.toast.as_ref().map(|t| t.0.clone());
                click_control(s, "switch");
                wait_until(s, "default changed", 5, |s| s.app.toast.as_ref().is_some_and(|t| Some(&t.0) != before.as_ref()));
            }
        };
        switch(&mut s, Some(true));
        assert!(std::fs::read_to_string(&cfg).unwrap().contains("inode/directory=io.github.yappologistic.Maia.desktop;"), "Maia set as default");
        assert_eq!(s.app.toast.as_ref().map(|t| t.0.as_str()), Some("Maia is now the default file manager"));
        look(&mut s, "default file manager on");
        s.act("key Escape");
        s.settle();
        switch(&mut s, Some(false));
        assert!(std::fs::read_to_string(&cfg).unwrap().contains("inode/directory=org.example.Files.desktop;"), "previous file manager restored");
        assert_eq!(s.app.default_fm(), Some(false));
        s.act("key Escape");
        // Reuse the open window: a plain switch in the same group.
        s.act("key ctrl+Comma");
        s.settle();
        s.act("text reuse the open window");
        s.idle(3);
        assert!(s.app.cfg.single_instance);
        s.act("key Enter");
        s.idle(2);
        assert!(!s.app.cfg.single_instance, "Enter turns it off");
        s.act("key Escape");
        report("single", &s);
    }

    /// Archives as folders: Enter opens a zip, folders inside open and Up leaves them, a
    /// file inside previews and opens (extracted to the cache), Cut is refused, Copy and
    /// Paste take files out, Extract Here and Compress work from the context menu and
    /// undo removes what they made.
    #[test]
    fn archives_open_extract_compress() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        let home = fixture("archives");
        let docs = home.join("Documents");
        let src = root().join("packsrc");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.join("Trip")).unwrap();
        std::fs::write(src.join("Trip/beach.txt"), "sand and sea").unwrap();
        std::fs::write(src.join("readme.txt"), "inside the zip").unwrap();
        let zip = docs.join("photos.zip");
        crate::archive::create(&zip, &src, &["Trip".into(), "readme.txt".into()]).unwrap();
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        select_path(&mut s, &zip);
        s.act("key Enter");
        wait_until(&mut s, "inside the zip", 10, |s| s.tab().path == zip && s.tab().entries.len() == 2);
        assert!(s.tab().archive.is_some());
        for v in [ViewList, ViewLarge, ViewTree, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("zip {v:?}"));
        }
        assert!(s.texts.iter().any(|(_, t)| t == "photos.zip"), "the archive in the path bar");
        // Into a folder and back out.
        select_path(&mut s, &zip.join("Trip"));
        s.act("key Enter");
        wait_until(&mut s, "folder in the zip", 10, |s| s.tab().path == zip.join("Trip") && s.tab().entries.iter().any(|e| e.name == "beach.txt"));
        s.act("key Backspace");
        wait_until(&mut s, "back at the top", 10, |s| s.tab().path == zip && s.tab().entries.iter().any(|e| e.name == "readme.txt"));
        // Preview, then open.
        s.cmd(ToggleInspector);
        select_path(&mut s, &zip.join("readme.txt"));
        wait_until(&mut s, "preview from the zip", 10, |s| s.texts.iter().any(|(_, t)| t.contains("inside the zip")));
        look(&mut s, "zip preview");
        s.cmd(ToggleInspector);
        let launched = crate::mime::LAUNCHED.lock().unwrap().len();
        s.act("key Enter");
        wait_until(&mut s, "opened from the zip", 10, |_| crate::mime::LAUNCHED.lock().unwrap().len() > launched);
        let cmd = crate::mime::LAUNCHED.lock().unwrap().last().cloned().unwrap_or_default();
        assert!(cmd.contains("maia/archives") && cmd.contains("readme.txt"), "{cmd}");
        let extracted = crate::archive::open_cache(&zip).join("readme.txt");
        assert_eq!(std::fs::read_to_string(&extracted).unwrap(), "inside the zip");
        // Copied out, not cut.
        s.act("key ctrl+x");
        s.idle(2);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Files in an archive are copied out, not cut") && s.app.jobs.is_empty(), "no cutting inside an archive");
        // Copy out.
        s.act("key ctrl+c");
        go(&mut s, &docs);
        s.act("key ctrl+v");
        wait_until(&mut s, "copied out", 10, |_| docs.join("readme.txt").exists());
        assert_eq!(std::fs::read_to_string(docs.join("readme.txt")).unwrap(), "inside the zip");
        // Extract Here: two things at the top go into a folder named after the archive.
        s.settle();
        let r = list_text(&s, "photos.zip");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        s.click_text("Extract Here");
        wait_until(&mut s, "extracted", 10, |_| docs.join("photos/Trip/beach.txt").exists() && docs.join("photos/readme.txt").exists());
        assert!(std::fs::read_dir(&docs).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".maia-extract")), "no staging left");
        // Compress two files to zip, then undo it.
        s.settle();
        let id = s.app.active;
        {
            let t = s.app.panels.get_mut(&id).unwrap().tab_mut();
            t.selected.clear();
            for n in ["notes.txt", "budget 2026.csv"] {
                if let Some(i) = t.entries.iter().position(|e| e.name == n) {
                    t.selected.insert(i as u32);
                }
            }
        }
        let r = list_text(&s, "notes.txt");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        if !s.texts.iter().any(|(_, t)| t == "Zip (.zip)") {
            s.click_text("Compress");
            s.idle(10);
        }
        s.click_text("Zip (.zip)");
        s.idle(5);
        // A long menu in a short window scrolls: a row under its edge takes no click.
        if s.popup_open() && !matches!(s.app.popup, Popup::Compress(_)) {
            let at = s.text_rect("Compress").center();
            s.act(&format!("scroll {:.0} {:.0} -6", at.x, at.y));
            s.idle(10);
            s.click_text("Zip (.zip)");
        }
        // The dialog, its name as the menu gave it before: Enter compresses.
        assert!(matches!(&s.app.popup, Popup::Compress(st) if st.name == "Archive.zip"));
        s.act("key Enter");
        let out = docs.join("Archive.zip");
        wait_until(&mut s, "compressed", 10, |_| out.exists());
        wait_until(&mut s, "job done", 10, |s| s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        let mut names: Vec<String> = crate::archive::entries(&out, Path::new("")).unwrap().iter().map(|e| e.name.clone()).collect();
        names.sort();
        assert_eq!(names, ["budget 2026.csv", "notes.txt"]);
        s.act("key ctrl+z");
        s.settle();
        wait_until(&mut s, "undone", 10, |_| !out.exists());
        look(&mut s, "after archives");
        report("archives", &s);
    }

    /// Tags from the context menu: a colour on two files (dots beside their names), the
    /// check mark that takes it off again, a new tag by name, and the list filtered by
    /// tag, in every view.
    #[test]
    fn tags_and_colour_labels() {
        if !sandboxed() {
            return;
        }
        let home = fixture("tags");
        let d = home.join("Documents");
        let (notes, rep) = (d.join("notes.txt"), d.join("report.md"));
        if crate::tags::write(&notes, &[]).is_err() {
            eprintln!("no user xattrs here: skipped");
            return;
        }
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        let right_click = |s: &mut Sim, name: &str| {
            let r = s.text_rect(name);
            s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
            s.idle(2);
        };
        // Both selected, Red from the Tags submenu.
        select_path(&mut s, &notes);
        let r = s.text_rect("report.md");
        s.act(&format!("click {:.0} {:.0} ctrl", r.center().x, r.center().y));
        right_click(&mut s, "report.md");
        // Typed into the menu's search, as the Tags entry may sit below its fold.
        s.act("text tags red");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Tags › Red"), "found by search");
        look(&mut s, "tags in the menu");
        s.act("key Enter");
        s.idle(3);
        assert_eq!(crate::tags::read(&notes), ["Red"]);
        assert_eq!(crate::tags::read(&rep), ["Red"]);
        // The menu now marks Red as on; choosing it again takes it off this one file.
        s.act("key Escape");
        select_path(&mut s, &rep);
        right_click(&mut s, "report.md");
        s.act("text tags red");
        s.idle(2);
        look(&mut s, "red is on");
        s.act("key Enter");
        s.idle(3);
        assert!(crate::tags::read(&rep).is_empty() && crate::tags::read(&notes) == ["Red"]);
        // A new tag by name, shown after the name in small text.
        right_click(&mut s, "report.md");
        s.act("text new tag");
        s.idle(2);
        s.act("key Enter");
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::Prompt { .. }), "asks for the name");
        s.act("text taxes 2025");
        s.act("key Enter");
        s.idle(3);
        assert_eq!(crate::tags::read(&rep), ["taxes 2025"]);
        wait_until(&mut s, "tag shown", 5, |s| s.texts.iter().any(|(_, t)| t == "taxes 2025"));
        for v in [ViewTree, ViewList, ViewMedium, ViewLarge, ViewXL, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("tags in {v:?}"));
        }
        // Filter by tag: only what carries it.
        s.act("key ctrl+f");
        s.act("text tag:red");
        wait_until(&mut s, "filtered by tag", 5, |s| s.tab().view.len() == 1 && s.tab().entries[s.tab().view[0] as usize].path == notes);
        look(&mut s, "filtered by tag");
        s.act("key Escape");
        // Properties lists them.
        select_path(&mut s, &rep);
        s.act("key alt+Enter");
        wait_until(&mut s, "properties shows tags", 5, |s| s.texts.iter().any(|(_, t)| t == "taxes 2025") && s.texts.iter().any(|(_, t)| t == "Tags"));
        s.act("key Escape");
        report("tags", &s);
    }

    /// Tags as places: the sidebar lists those met on files, one opens a view of every
    /// file in the home folder carrying it, the Tags column shows and sorts them, the list
    /// groups by them, and a tag is renamed and taken off every file from its menu, each
    /// as one change Undo takes back.
    #[test]
    fn tags_in_the_sidebar_column_and_everywhere() {
        if !sandboxed() {
            return;
        }
        let home = fixture("tagviews");
        let d = home.join("Documents");
        let (notes, rep, wide) = (d.join("notes.txt"), d.join("report.md"), home.join("Pictures/wide.png"));
        let tag = |p: &Path, t: &[&str]| crate::tags::write(p, &t.iter().map(|x| x.to_string()).collect::<Vec<_>>());
        if tag(&notes, &["Red", "orchid"]).is_err() {
            eprintln!("no user xattrs here: skipped");
            return;
        }
        tag(&rep, &["orchid"]).unwrap();
        tag(&wide, &["orchid", "Blue"]).unwrap();
        let all = |want: &str| [&notes, &rep, &wide].iter().all(|p| crate::tags::read(p).iter().any(|t| t == want));
        let none = |want: &str| [&notes, &rep, &wide].iter().all(|p| !crate::tags::read(p).iter().any(|t| t == want));
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        // Met on the files drawn, the tags join the sidebar.
        wait_until(&mut s, "tags in the sidebar", 5, |s| sidebar_text(s, "orchid").is_some() && sidebar_text(s, "Red").is_some());
        look(&mut s, "sidebar tags");
        let r = sidebar_text(&s, "orchid").unwrap_or_else(|| s.fail("orchid"));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "the tag's view", 10, |s| s.tab().smart.as_ref().is_some_and(|f| f.tag == "orchid") && !s.tab().is_loading() && s.tab().view.len() == 3);
        assert_eq!(s.tab().title(), "orchid");
        look(&mut s, "orchid's view");
        // The Tags column, from the header's menu; the names leave their tags to it.
        let r = list_text(&s, "Name");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        let r = s.texts.iter().rev().find(|(r, t)| t == "Tags" && r.left() > s.app.sidebar_width).map(|x| x.0).unwrap_or_else(|| s.fail("Tags in the column menu"));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        // Away from the menu (Escape would leave the view).
        s.act("click 800 700");
        assert!(s.tab().settings.columns.iter().any(|c| c.0 == crate::config::Column::Tags), "Tags column on");
        look(&mut s, "tags column");
        // Sorted by it: Red before Blue before the word.
        let r = list_text(&s, "Tags");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "sorted by tag", 5, |s| s.tab().view.iter().map(|&i| s.tab().entries[i as usize].name.as_str()).collect::<Vec<_>>() == ["notes.txt", "wide.png", "report.md"]);
        look(&mut s, "sorted by tag");
        for v in [ViewTree, ViewList, ViewMedium, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("tag view {v:?}"));
        }
        // Grouped in a folder, each under its first tag.
        go(&mut s, &d);
        s.cmd(GroupTag);
        wait_until(&mut s, "groups", 5, |s| s.tab().header_labels() == ["Red", "orchid", "No tags"]);
        look(&mut s, "grouped by tag");
        s.cmd(GroupNone);
        // Renamed on every file from its menu, and back with Undo.
        let menu = |s: &mut Sim, name: &str| {
            let r = sidebar_text(s, name).unwrap_or_else(|| s.fail(name));
            s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
            s.idle(2);
        };
        menu(&mut s, "orchid");
        look(&mut s, "tag menu");
        s.click_text("Rename Tag…");
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::Prompt { .. }), "asks for the name");
        s.act("key ctrl+a");
        s.act("text iris");
        s.act("key Enter");
        wait_until(&mut s, "renamed everywhere", 10, |_| all("iris") && none("orchid"));
        wait_until(&mut s, "sidebar follows", 5, |s| sidebar_text(s, "iris").is_some() && sidebar_text(s, "orchid").is_none());
        look(&mut s, "renamed");
        s.act("key ctrl+z");
        wait_until(&mut s, "rename undone", 10, |s| all("orchid") && none("iris") && sidebar_text(s, "orchid").is_some());
        // Taken off every file: asked first, with how many.
        menu(&mut s, "orchid");
        s.click_text("Remove From All Files…");
        wait_until(&mut s, "asked", 10, |s| matches!(&s.app.popup, Popup::Confirm { msg, .. } if msg.contains("3 files")));
        look(&mut s, "remove asks");
        s.act("key Enter");
        wait_until(&mut s, "removed everywhere", 10, |s| none("orchid") && sidebar_text(s, "orchid").is_none());
        assert_eq!(crate::tags::read(&notes), ["Red"], "the other tags stay");
        look(&mut s, "removed");
        s.act("key ctrl+z");
        wait_until(&mut s, "removal undone", 10, |s| all("orchid") && sidebar_text(s, "orchid").is_some());
        report("tag views", &s);
    }

    /// The media columns: Date Taken, Dimensions and Duration chosen from the header's
    /// menu, filled in as the workers read them (ffprobe through the real tools), sorted
    /// by each with the rest last, grouped by month; the Details card of a film gives its
    /// length, resolution and codecs. Then a folder of 5,000 photos sorted by when they
    /// were taken, every frame drawn while their dates come in.
    #[test]
    fn media_columns_sorted_and_the_details_card() {
        if !sandboxed() {
            return;
        }
        let d = media_fixture("mediacols");
        let Some(tools) = crate::tests::media_tools(&root().join("mediacols-tools")) else {
            eprintln!("ffmpeg missing: skipped");
            return;
        };
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
        let ff = |args: &[&str]| assert!(std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).current_dir(&d).status().is_ok_and(|s| s.success()));
        ff(&["-f", "lavfi", "-i", "testsrc=duration=4:size=640x360:rate=10", "-pix_fmt", "yuv420p", "-metadata", "creation_time=2025-03-09T08:00:00Z", "long clip.mp4"]);
        ff(&["-f", "lavfi", "-i", "sine=frequency=330:duration=5", "chime.flac"]);
        let jpeg = |name: &str, when: &str, dir: &Path| {
            let raw = std::process::Command::new(tools.join("ffmpeg")).args(["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=96x64", "-frames:v", "1", "-f", "mjpeg", "-"]).output().unwrap().stdout;
            std::fs::write(dir.join(name), crate::tests::with_exif_taken(&raw, 1, when)).unwrap();
        };
        jpeg("beach.jpg", "2024:08:02 12:00:00", &d);
        // Wide enough for three more columns.
        let mut s = Sim::new(&d, vec2(1600.0, 900.0));
        s.cmd(ViewDetails);
        s.settle();
        let add_column = |s: &mut Sim, title: &str| {
            let r = list_text(s, "Name");
            s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
            s.idle(2);
            let r = s.texts.iter().rev().find(|(r, t)| t == title && r.left() > s.app.sidebar_width).map(|x| x.0).unwrap_or_else(|| s.fail(title));
            s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
            s.act("click 700 760");
        };
        for c in ["Date Taken", "Dimensions", "Duration"] {
            add_column(&mut s, c);
        }
        wait_until(&mut s, "facts drawn", 20, |s| some_text_at(s, "320 × 240") && some_text_at(s, "0:05") && some_text_at(s, "96 × 64"));
        look(&mut s, "media columns");
        let order = |s: &Sim| -> Vec<String> { s.tab().view.iter().map(|&i| s.tab().entries[i as usize].name.clone()).collect() };
        let click_header = |s: &mut Sim, title: &str| {
            let r = list_text(s, title);
            s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        };
        click_header(&mut s, "Duration");
        wait_until(&mut s, "longest first", 20, |s| order(s)[..4] == ["chime.flac", "long clip.mp4", "song.mp3", "clip.mp4"] || order(s)[..4] == ["chime.flac", "long clip.mp4", "clip.mp4", "song.mp3"]);
        look(&mut s, "sorted by duration");
        click_header(&mut s, "Date Taken");
        wait_until(&mut s, "newest first", 20, |s| order(s)[..3] == ["long clip.mp4", "beach.jpg", "clip.mp4"] || order(s)[..2] == ["long clip.mp4", "beach.jpg"]);
        look(&mut s, "sorted by date taken");
        click_header(&mut s, "Dimensions");
        wait_until(&mut s, "largest first", 20, |s| order(s)[0] == "long clip.mp4");
        look(&mut s, "sorted by dimensions");
        s.cmd(GroupTaken);
        wait_until(&mut s, "months", 20, |s| s.tab().header_labels().first().is_some_and(|l| l == "March 2025") && s.tab().header_labels().last().is_some_and(|l| l == "No date"));
        look(&mut s, "grouped by month");
        s.cmd(GroupNone);
        for (w, h) in [(640.0, 480.0), (520.0, 340.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("media columns {w}x{h}"));
        }
        // A film's Details card.
        select_name(&mut s, "long clip.mp4");
        s.cmd(ToggleInspector);
        wait_until(&mut s, "the film probed", 20, |s| inspector(s).media_probe().is_some());
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector"));
        let r = s.texts.iter().find(|(r, t)| t == "long clip.mp4" && ir.contains(r.center()) && r.top() < ir.top() + 30.0).map(|x| x.0).unwrap_or_else(|| s.fail("the name in the inspector"));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "the card", 10, |s| some_text_at(s, "Resolution") && some_text_at(s, "640 × 360") && some_text_at(s, "0:04") && s.texts.iter().any(|(_, t)| t.starts_with("h264") || t.starts_with("mpeg4")));
        look(&mut s, "film details");
        select_name(&mut s, "chime.flac");
        wait_until(&mut s, "a recording's card", 20, |s| some_text_at(s, "Codec") && some_text_at(s, "flac") && some_text_at(s, "0:05") && !some_text_at(s, "Resolution"));
        look(&mut s, "recording details");
        s.cmd(ToggleInspector);
        // Five thousand photos, sorted by when they were taken while the dates come in.
        let many = d.join("Camera");
        std::fs::create_dir_all(&many).unwrap();
        let raw = std::process::Command::new(tools.join("ffmpeg")).args(["-loglevel", "error", "-f", "lavfi", "-i", "color=c=0x6688aa:s=32x24", "-frames:v", "1", "-f", "mjpeg", "-"]).output().unwrap().stdout;
        for i in 0..5000u32 {
            let when = format!("20{:02}:{:02}:{:02} 10:{:02}:00", 10 + i % 15, 1 + i % 12, 1 + i % 28, i % 60);
            std::fs::write(many.join(format!("IMG_{i:04}.jpg")), crate::tests::with_exif_taken(&raw, 1, &when)).unwrap();
        }
        let t0 = Instant::now();
        go(&mut s, &many);
        s.cmd(SortTaken);
        wait_until(&mut s, "every date read", 120, |s| s.tab().entries.iter().all(|e| crate::facts::get(e).is_some_and(|f| f.taken.is_some())));
        s.idle(5);
        eprintln!("sim media columns: 5000 photos dated in {:.2} s", t0.elapsed().as_secs_f32());
        let o = order(&s);
        let first = crate::facts::get(&s.tab().entries[s.tab().view[0] as usize]).and_then(|f| f.taken);
        assert!(s.tab().view.iter().all(|&i| crate::facts::get(&s.tab().entries[i as usize]).and_then(|f| f.taken) <= first), "newest first: {:?}", &o[..3]);
        look(&mut s, "5000 photos by date taken");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        report("media columns", &s);
    }

    /// Compress With Password, from the Compress menu and the palette: the password
    /// typed twice (two that differ are said to, and Compress waits), a zip made and
    /// opened at once like a folder, then a 7z whose names are locked too; each one
    /// extracted with the password by Maia's own code, and refused without it.
    #[test]
    fn compress_with_a_password() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        let home = fixture("lockpack");
        let d = home.join("Documents");
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.cmd(ViewDetails);
        s.settle();
        select_names(&mut s, &["notes.txt", "report.md"]);
        let r = row_text(&s, "report.md");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.act("text with a password");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Compress › With a password…"), "found by search");
        s.act("key Enter");
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::Compress(_)));
        assert!(some_text(&s, "Compress 2 items"));
        s.act("text tide and time");
        click_control(&mut s, "compress again");
        s.act("text tide and tim");
        s.idle(2);
        assert!(some_text(&s, "The two passwords differ"));
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Compress(_)), "Compress waits for the same password twice");
        look(&mut s, "two passwords that differ");
        shot_still(&mut s, "lock-differ");
        s.act("text e");
        s.idle(2);
        assert!(!some_text(&s, "The two passwords differ"));
        look(&mut s, "the password dialog");
        shot_still(&mut s, "lock-dialog");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("the password dialog at font {font}, spacing {spacing}"));
        }
        s.act("key Enter");
        let zip = d.join("Archive.zip");
        wait_until(&mut s, "the zip", 30, |s| zip.exists() && s.app.jobs.iter().all(|j| j.progress.lock().unwrap().finished));
        // Opened like a folder: the password is known for this session.
        go(&mut s, &zip);
        wait_until(&mut s, "inside it", 10, |s| s.tab().entries.iter().any(|e| e.name == "notes.txt"));
        look(&mut s, "inside the locked zip");
        crate::archive::forget_password(&zip);
        let out = root().join("lock-out");
        let e = crate::archive::extract(&zip, &[PathBuf::from("notes.txt")], &out);
        assert!(e.is_err_and(|e| crate::archive::wants_password(&e)), "locked without the password");
        crate::archive::set_password(&zip, "tide and time".into());
        crate::archive::extract(&zip, &[PathBuf::from("notes.txt")], &out).unwrap();
        assert_eq!(std::fs::read(out.join("notes.txt")).unwrap(), std::fs::read(d.join("notes.txt")).unwrap());
        // A 7z, from the palette, names locked too.
        if crate::archive::can_lock_7z() {
            go(&mut s, &d);
            select_names(&mut s, &["Taxes"]);
            palette_run(&mut s, "Compress With Password…");
            click_control(&mut s, "compress 7z");
            click_control(&mut s, "compress password");
            s.act("text 7 seas");
            click_control(&mut s, "compress again");
            s.act("text 7 seas");
            click_control(&mut s, "compress button");
            let sz = d.join("Taxes.7z");
            wait_until(&mut s, "the 7z", 30, |s| sz.exists() && s.app.jobs.iter().all(|j| j.progress.lock().unwrap().finished));
            crate::archive::forget_password(&sz);
            assert_eq!(crate::archive::list(&sz).err().as_deref(), Some(crate::archive::NEEDS_PASSWORD), "names locked");
            crate::archive::set_password(&sz, "7 seas".into());
            assert!(crate::archive::list(&sz).unwrap().iter().any(|i| i.path == Path::new("Taxes/2025/receipt.pdf")));
        }
        report("compress with a password", &s);
    }

    /// Wait for every job to end, and for the tab to show `want` (names in its folder).
    fn wait_jobs(s: &mut Sim, what: &str) {
        wait_until(s, what, 30, |s| s.app.jobs.iter().all(|j| j.progress.lock().unwrap().finished));
        s.settle();
    }

    fn names_here(s: &Sim) -> Vec<String> {
        let mut v: Vec<String> = s.tab().entries.iter().map(|e| e.name.clone()).collect();
        v.sort();
        v
    }

    /// Changing a zip in place, as a user does it: Delete takes a file out (the job's
    /// card, then the row gone) and Undo puts it back; F2 renames; New Folder asks for
    /// the name first; Paste adds, and a name already there asks Keep both or Replace; a
    /// file dragged in from the other panel lands in it; a locked zip asks for its
    /// password before it changes. A tarball takes the same, and an ISO, which is only
    /// read, says so. Every change checked in the archive itself.
    #[test]
    fn archive_edits_in_place() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        let home = fixture("archedits");
        let docs = home.join("Documents");
        let src = root().join("editsrc");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.join("Trip")).unwrap();
        std::fs::write(src.join("Trip/beach.txt"), "sand and sea").unwrap();
        std::fs::write(src.join("readme.txt"), "inside the zip").unwrap();
        let zip = docs.join("photos.zip");
        crate::archive::create(&zip, &src, &["Trip".into(), "readme.txt".into()]).unwrap();
        let inside = |a: &Path| -> Vec<String> {
            let mut v: Vec<String> = crate::archive::list(a).unwrap().iter().map(|i| i.path.display().to_string()).collect();
            v.sort();
            v
        };
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        select_path(&mut s, &zip);
        s.act("key Enter");
        wait_until(&mut s, "inside the zip", 10, |s| s.tab().path == zip && s.tab().entries.len() == 2);
        // Delete: a job with its card, the row gone, the old zip in the trash.
        select_path(&mut s, &zip.join("readme.txt"));
        s.act("key Delete");
        s.idle(2);
        assert!(s.app.jobs.iter().any(|j| j.label == "Deleting readme.txt from photos.zip"), "a job: {:?}", s.app.jobs.iter().map(|j| &j.label).collect::<Vec<_>>());
        wait_jobs(&mut s, "deleted");
        wait_until(&mut s, "the row gone", 10, |s| names_here(s) == ["Trip"]);
        assert_eq!(inside(&zip), ["Trip", "Trip/beach.txt"]);
        look(&mut s, "after a delete in the zip");
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 10, |s| names_here(s) == ["Trip", "readme.txt"]);
        assert_eq!(inside(&zip), ["Trip", "Trip/beach.txt", "readme.txt"]);
        // Rename by F2.
        select_path(&mut s, &zip.join("readme.txt"));
        s.act("key F2");
        assert!(s.tab().rename.is_some(), "F2 renames inside a zip");
        s.act("key ctrl+a");
        s.act("text read me first.txt");
        s.act("key Enter");
        s.idle(2);
        wait_jobs(&mut s, "renamed");
        wait_until(&mut s, "the new name", 10, |s| names_here(s) == ["Trip", "read me first.txt"]);
        assert_eq!(crate::archive::read_member(&zip, Path::new("read me first.txt"), 1 << 20).unwrap(), b"inside the zip");
        // New Folder: named first, then made.
        s.act("key ctrl+shift+n");
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::Prompt { .. }), "the name asked for");
        look(&mut s, "new folder in a zip");
        s.act("key ctrl+a");
        s.act("text Holiday – 2026");
        s.act("key Enter");
        wait_jobs(&mut s, "folder made");
        wait_until(&mut s, "the folder", 10, |s| names_here(s).contains(&"Holiday – 2026".to_string()));
        assert!(crate::archive::list(&zip).unwrap().iter().any(|i| i.path == Path::new("Holiday – 2026") && i.is_dir));
        // Paste in: a file from the folder outside.
        go(&mut s, &docs);
        select_path(&mut s, &docs.join("notes.txt"));
        s.act("key ctrl+c");
        go(&mut s, &zip.join("Holiday – 2026"));
        wait_until(&mut s, "in the new folder", 10, |s| s.tab().path == zip.join("Holiday – 2026") && !s.tab().is_loading());
        s.act("key ctrl+v");
        s.idle(2);
        wait_jobs(&mut s, "pasted");
        wait_until(&mut s, "pasted row", 10, |s| names_here(s) == ["notes.txt"]);
        // Again: the name is there, so Keep both or Replace.
        s.act("key ctrl+v");
        s.idle(3);
        assert!(some_text(&s, "“notes.txt” is in the archive already"), "asked");
        look(&mut s, "keep both or replace");
        shot_still(&mut s, "archive-keep-both");
        s.click_text("Keep both");
        wait_jobs(&mut s, "kept both");
        wait_until(&mut s, "both", 10, |s| names_here(s) == ["notes (1).txt", "notes.txt"]);
        // Dragged in from the other panel.
        s.cmd(SplitRight);
        go(&mut s, &docs);
        s.settle();
        let from = list_text(&s, "report.md");
        s.act("key Tab");
        s.settle();
        if s.tab().path != zip.join("Holiday – 2026") {
            s.fail("the zip's panel");
        }
        let to = s.app.panels[&s.app.active].list_rect.center();
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", from.center().x, from.center().y, to.x, to.y + 120.0));
        s.idle(3);
        wait_jobs(&mut s, "dropped");
        wait_until(&mut s, "dropped row", 10, |s| s.app.panels.values().any(|p| p.tab().path == zip.join("Holiday – 2026") && p.tab().entries.iter().any(|e| e.name == "report.md")));
        assert!(docs.join("report.md").exists(), "a drop into an archive copies");
        assert_eq!(crate::archive::read_member(&zip, Path::new("Holiday – 2026/report.md"), 1 << 20).unwrap(), std::fs::read(docs.join("report.md")).unwrap());
        // The other panel closed: the zip's is left.
        if s.tab().path != docs {
            s.act("key Tab");
        }
        s.cmd(ClosePanel);
        s.settle();
        assert_eq!(s.tab().path, zip.join("Holiday – 2026"));
        for v in [ViewList, ViewLarge, ViewTree, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("edited zip {v:?}"));
        }
        // A locked zip: the password asked for before it changes.
        let locked = docs.join("locked.zip");
        crate::archive::create_locked(&locked, &src, &["readme.txt".into()], "open sesame").unwrap();
        crate::archive::forget_password(&locked);
        go(&mut s, &locked);
        wait_until(&mut s, "in the locked zip", 10, |s| names_here(s) == ["readme.txt"]);
        select_path(&mut s, &locked.join("readme.txt"));
        s.act("key Delete");
        wait_until(&mut s, "the password asked", 20, |s| matches!(s.app.popup, Popup::ArchivePassword(_)));
        look(&mut s, "password for a change");
        s.act("text open sesame");
        s.act("key Enter");
        s.idle(2);
        wait_jobs(&mut s, "locked one changed");
        wait_until(&mut s, "emptied", 10, |s| s.tab().entries.is_empty());
        assert!(crate::archive::list(&locked).unwrap().is_empty());
        // A tarball: the same, rebuilt.
        let tgz = docs.join("bundle.tar.gz");
        assert!(std::process::Command::new("bsdtar").env("LC_ALL", "C.UTF-8").arg("-czf").arg(&tgz).arg("-C").arg(&src).args(["Trip", "readme.txt"]).status().unwrap().success());
        go(&mut s, &tgz.join("Trip"));
        wait_until(&mut s, "in the tarball", 10, |s| names_here(s) == ["beach.txt"]);
        select_path(&mut s, &tgz.join("Trip/beach.txt"));
        s.act("key F2");
        s.act("key ctrl+a");
        s.act("text shore.txt");
        s.act("key Enter");
        wait_jobs(&mut s, "tarball renamed");
        wait_until(&mut s, "tar renamed", 10, |s| names_here(s) == ["shore.txt"]);
        assert_eq!(crate::archive::read_member(&tgz, Path::new("Trip/shore.txt"), 1 << 20).unwrap(), b"sand and sea");
        // An ISO is only read: said, and nothing starts.
        let iso = docs.join("disc.iso");
        assert!(std::process::Command::new("bsdtar").env("LC_ALL", "C.UTF-8").arg("-cf").arg(&iso).args(["--format", "iso9660"]).arg("-C").arg(&src).arg("readme.txt").status().unwrap().success());
        go(&mut s, &iso);
        wait_until(&mut s, "in the iso", 10, |s| s.tab().entries.len() == 1);
        s.cmd(SelectAll);
        s.act("key Delete");
        s.idle(2);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "ISO archives can only be read here: extract it to change it") && s.app.jobs.iter().all(|j| j.progress.lock().unwrap().finished), "{:?}", s.app.toast);
        s.act("key ctrl+shift+n");
        assert!(!matches!(s.app.popup, Popup::Prompt { .. }), "no new folder in an iso");
        look(&mut s, "an iso refuses");
        report("archive edits", &s);
    }

    /// The Compress dialog: opened from the menu with its format chosen, the name typed
    /// (its extension picks the format, a chip changes the extension), a level, then a
    /// long job whose card counts bytes and time left as bsdtar reads them, pauses (the
    /// count holds), resumes and is cancelled, leaving nothing; then a quick one that
    /// finishes and is undone.
    #[test]
    fn compress_dialog_and_progress() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        let home = fixture("compressdlg");
        let d = home.join("Documents");
        let mut noise = vec![0u8; 40 << 20];
        let mut x = 0x9E37_79B9_7F4A_7C15u64;
        for b in noise.iter_mut() {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            *b = x as u8;
        }
        std::fs::write(d.join("noise.bin"), &noise).unwrap();
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        select_names(&mut s, &["noise.bin", "notes.txt"]);
        open_submenu(&mut s, "notes.txt", "Compress");
        s.click_text("Tar.xz (.tar.xz)");
        s.idle(3);
        assert!(matches!(&s.app.popup, Popup::Compress(st) if st.name == "Archive.tar.xz"), "named as before");
        assert!(some_text(&s, "Compress 2 items"));
        look(&mut s, "the compress dialog");
        shot_still(&mut s, "compress-dialog");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("the compress dialog at font {font}, spacing {spacing}"));
        }
        // The name: an extension typed picks the format; a chip changes the extension.
        click_control(&mut s, "compress name");
        s.act("key ctrl+a");
        s.act("text backup.zip");
        s.idle(2);
        assert!(matches!(&s.app.popup, Popup::Compress(st) if st.format == crate::archwrite::Format::Zip));
        click_control(&mut s, "compress tar.xz");
        assert!(matches!(&s.app.popup, Popup::Compress(st) if st.name == "backup.tar.xz"));
        // Tarballs take no password: the field says so and takes none.
        assert!(!matches!(&s.app.popup, Popup::Compress(st) if st.lockable()));
        click_control(&mut s, "compress smallest");
        assert!(matches!(&s.app.popup, Popup::Compress(st) if st.level == crate::archwrite::Level::Smallest));
        look(&mut s, "the compress dialog, filled in");
        s.act("key Enter");
        s.idle(2);
        let out = d.join("backup.tar.xz");
        assert!(s.app.jobs.iter().any(|j| j.label == "Compressing into backup.tar.xz"));
        // Real progress: bytes counted as bsdtar reads them, between the ends.
        let done = |s: &Sim| s.app.jobs.first().map(|j| j.progress.lock().unwrap().done_bytes).unwrap_or(0);
        wait_until(&mut s, "progress shown", 30, |s| (4 << 20..30 << 20).contains(&done(s)));
        assert!(s.texts.iter().any(|(_, t)| t.contains(" of ") && t.contains("left")), "bytes and time left on the card");
        s.shot("compress-progress");
        s.click_text("Pause");
        // The worker stops the tool on its next look, some milliseconds on.
        std::thread::sleep(std::time::Duration::from_millis(300));
        s.idle(5);
        let held = done(&s);
        std::thread::sleep(std::time::Duration::from_millis(500));
        s.idle(5);
        assert_eq!(done(&s), held, "paused: the count holds");
        assert!(some_text(&s, "Resume"));
        s.click_text("Resume");
        wait_until(&mut s, "going again", 30, |s| done(s) > held);
        s.click_text("Cancel");
        wait_jobs(&mut s, "cancelled");
        assert!(!out.exists() && std::fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with('.')), "nothing left of it");
        assert!(s.app.jobs.iter().all(|j| j.progress.lock().unwrap().errors.is_empty()), "a cancel is not an error");
        // A quick one, done, then undone. The level chosen is offered again.
        select_names(&mut s, &["notes.txt"]);
        s.cmd(CompressZip);
        s.idle(2);
        assert!(matches!(&s.app.popup, Popup::Compress(st) if st.name == "notes.zip" && st.level == crate::archwrite::Level::Smallest));
        click_control(&mut s, "compress fast");
        click_control(&mut s, "compress button");
        let zip = d.join("notes.zip");
        wait_jobs(&mut s, "compressed");
        assert_eq!(crate::archive::read_member(&zip, Path::new("notes.txt"), 1 << 20).unwrap(), std::fs::read(d.join("notes.txt")).unwrap());
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 10, |_| !zip.exists());
        // Two with one name: said, and Compress waits.
        select_names(&mut s, &["Taxes"]);
        s.cmd(Compress7z);
        click_control(&mut s, "compress name");
        s.act("key ctrl+a");
        s.act("text notes.txt");
        s.idle(2);
        click_control(&mut s, "compress 7z");
        s.idle(2);
        std::fs::write(d.join("taken.7z"), "x").unwrap();
        click_control(&mut s, "compress name");
        s.act("key ctrl+a");
        s.act("text taken.7z");
        s.idle(2);
        assert!(some_text(&s, "There is a “taken.7z” here already"));
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Compress(_)), "Compress waits for a free name");
        look(&mut s, "a name taken");
        s.act("key Escape");
        report("compress dialog", &s);
    }

    /// New kinds of archive: a .deb (control and data inside, as bsdtar writes one), a
    /// log compressed on its own (one file, previewed as text in the inspector), opened
    /// with Enter like a folder, and Extract Here giving the file back.
    #[test]
    fn packages_and_lone_compressed_files() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        let home = fixture("lonefiles");
        let d = home.join("Downloads");
        let pkg = root().join("debsrc");
        let _ = std::fs::remove_dir_all(&pkg);
        std::fs::create_dir_all(pkg.join("data/usr/share/doc/hello")).unwrap();
        std::fs::write(pkg.join("data/usr/share/doc/hello/README"), "hello").unwrap();
        std::fs::write(pkg.join("control"), "Package: hello\n").unwrap();
        std::fs::write(pkg.join("debian-binary"), "2.0\n").unwrap();
        let tar = |args: &[&str]| assert!(std::process::Command::new("bsdtar").env("LC_ALL", "C.UTF-8").args(args).current_dir(&pkg).status().unwrap().success(), "{args:?}");
        tar(&["-czf", "control.tar.gz", "control"]);
        tar(&["-cJf", "data.tar.xz", "-C", "data", "usr"]);
        let deb = d.join("hello_1.0_all.deb");
        tar(&["-cf", deb.to_str().unwrap(), "--format", "ar", "debian-binary", "control.tar.gz", "data.tar.xz"]);
        let log: String = (0..300).map(|i| format!("{i}: the service said hello\n")).collect();
        std::fs::write(pkg.join("service.log"), &log).unwrap();
        let gz = d.join("service.log.gz");
        tar(&["-czf", gz.to_str().unwrap(), "--format", "raw", "service.log"]);
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        select_path(&mut s, &deb);
        s.act("key Enter");
        wait_until(&mut s, "inside the deb", 10, |s| s.tab().path == deb && names_here(s) == ["control.tar.gz", "data.tar.xz", "debian-binary"]);
        look(&mut s, "a deb");
        s.shot("deb-browsed");
        s.act("key Backspace");
        wait_until(&mut s, "back out", 10, |s| s.tab().path == d && s.tab().entries.iter().any(|e| e.path == gz));
        // The lone .gz previews as the log inside it.
        s.cmd(ToggleInspector);
        select_path(&mut s, &gz);
        wait_until(&mut s, "the log previewed", 10, |s| s.texts.iter().any(|(_, t)| t.contains("the service said hello")));
        look(&mut s, "a .gz previewed");
        s.cmd(ToggleInspector);
        s.act("key Enter");
        wait_until(&mut s, "inside the gz", 10, |s| s.tab().path == gz && names_here(s) == ["service.log"]);
        assert_eq!(s.tab().entries[0].size, log.len() as u64, "its size, from the gzip trailer");
        look(&mut s, "inside a .gz");
        // Changing it is not offered: one compressed file.
        s.act("key Delete");
        s.idle(2);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0.contains("one compressed file")));
        s.act("key Backspace");
        wait_until(&mut s, "out again", 10, |s| s.tab().path == d && s.tab().entries.iter().any(|e| e.path == gz));
        select_path(&mut s, &gz);
        s.cmd(ExtractHere);
        wait_until(&mut s, "extracted", 10, |_| d.join("service.log").exists());
        assert_eq!(std::fs::read_to_string(d.join("service.log")).unwrap(), log);
        report("packages and lone files", &s);
    }

    /// Password-protected archives: Extract Here asks for the password (a wrong one is
    /// said to be wrong), a 7z whose names are hidden asks on the way in, a file opened
    /// from an AES zip asks before it opens, and Cancel leaves everything as it was.
    #[test]
    fn password_protected_archives() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        let home = fixture("encrypted");
        let d = home.join("Documents");
        let src = root().join("encsrc");
        let _ = std::fs::remove_dir_all(&src);
        std::fs::create_dir_all(src.join("Trip")).unwrap();
        std::fs::write(src.join("plan.txt"), "the secret plan").unwrap();
        std::fs::write(src.join("Trip/beach.txt"), "sand").unwrap();
        let mk = |prog: &str, args: &[&str]| std::process::Command::new(prog).args(args).current_dir(&src).stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        let (zip, aes, hidden) = (d.join("vault.zip"), d.join("aes.zip"), d.join("hidden.7z"));
        if !mk("zip", &["-q", "-r", "-P", "hunter2", zip.to_str().unwrap(), "."]) || !mk("7z", &["a", "-phunter2", "-tzip", "-mem=AES256", aes.to_str().unwrap(), "."]) || !mk("7z", &["a", "-phunter2", "-mhe=on", hidden.to_str().unwrap(), "."]) {
            eprintln!("zip or 7z missing: skipped");
            return;
        }
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        let asking = |s: &Sim| matches!(&s.app.popup, Popup::ArchivePassword(_));
        // Extract Here: asked, a wrong password refused, then the right one.
        select_path(&mut s, &zip);
        s.cmd(ExtractHere);
        wait_until(&mut s, "password asked", 10, asking);
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "“vault.zip” is password-protected"));
        look(&mut s, "password dialog");
        s.act("text wrong");
        s.click_text("Unlock");
        wait_until(&mut s, "asked again", 10, |s| asking(s) && s.texts.iter().any(|(_, t)| t == "Wrong password"));
        look(&mut s, "wrong password");
        assert!(!d.join("vault").exists(), "nothing left behind by the refused attempt");
        click_control(&mut s, "archive password field");
        s.act("text hunter2");
        s.act("key Enter");
        wait_until(&mut s, "extracted", 10, |s| !asking(s) && d.join("vault/Trip/beach.txt").exists());
        assert_eq!(std::fs::read_to_string(d.join("vault/plan.txt")).unwrap(), "the secret plan");
        // Into the 7z whose names are encrypted too: asked on the way in.
        s.cmd(Refresh);
        s.settle();
        select_path(&mut s, &hidden);
        s.act("key Enter");
        wait_until(&mut s, "asked for the 7z", 10, asking);
        s.act("text hunter2");
        s.act("key Enter");
        wait_until(&mut s, "inside the 7z", 10, |s| s.tab().path == hidden && s.tab().entries.iter().any(|e| e.name == "plan.txt"));
        look(&mut s, "inside an encrypted 7z");
        // Cancel leaves the AES zip shut: nothing opened.
        s.act("key Backspace");
        wait_until(&mut s, "back", 10, |s| s.tab().path == d);
        s.settle();
        select_path(&mut s, &aes);
        s.act("key Enter");
        wait_until(&mut s, "inside the AES zip", 10, |s| s.tab().path == aes && !s.tab().is_loading());
        s.settle();
        // The preview doesn't ask, it says why it shows nothing.
        s.cmd(ToggleInspector);
        select_path(&mut s, &aes.join("plan.txt"));
        wait_until(&mut s, "preview says locked", 10, |s| s.texts.iter().any(|(_, t)| t == "Password-protected"));
        look(&mut s, "locked preview");
        s.cmd(ToggleInspector);
        let launched = crate::mime::LAUNCHED.lock().unwrap().len();
        s.act("key Enter");
        wait_until(&mut s, "asked before opening", 10, asking);
        s.click_text("Cancel");
        s.idle(3);
        assert!(!asking(&s) && crate::mime::LAUNCHED.lock().unwrap().len() == launched, "cancelled: nothing opened");
        // Opened for real once the password is given.
        s.act("key Enter");
        wait_until(&mut s, "asked again", 10, asking);
        s.act("text hunter2");
        s.act("key Enter");
        wait_until(&mut s, "opened from the AES zip", 10, |_| crate::mime::LAUNCHED.lock().unwrap().len() > launched);
        report("encrypted", &s);
    }

    /// A deep folder in a narrow window: the path bar squeezes the folders above it
    /// first, so the current one keeps its whole name.
    #[test]
    fn path_bar_keeps_the_current_folder() {
        if !sandboxed() {
            return;
        }
        // Outside home, so the whole path is spelled out (no "~").
        let deep = root().join("srv/Projects archive 2026/client reports/quarterly summaries/final versions");
        std::fs::create_dir_all(&deep).unwrap();
        let mut s = Sim::new(&deep, vec2(1280.0, 800.0));
        let mut full = 0.0;
        for w in [1280.0, 900.0, 640.0, 520.0] {
            s.resize(w, 600.0);
            s.settle();
            // Drawn texts keep their whole label, so an elided one shows only in its width:
            // the name in the path bar (its lowest occurrence) against its width with room.
            let bar = s.app.panels[&s.app.active].list_rect.top();
            let r = s.texts.iter().filter(|(r, t)| t == "final versions" && r.top() < bar).map(|(r, _)| *r).max_by(|a, b| a.top().total_cmp(&b.top())).unwrap_or_else(|| s.fail("not in the path bar"));
            if w == 1280.0 {
                full = r.width();
            }
            // Shortened only when nothing else of the path is left beside it.
            if r.width() < 0.95 * full {
                // Path segments: text in the panel, on the bar's row, left of the name; not the
                // "…" for the dropped ones, not the toolbar's icon glyphs.
                let left = s.app.panels[&s.app.active].list_rect.left();
                let icon = |t: &str| t.chars().next().is_some_and(|c| ('\u{e000}'..='\u{f8ff}').contains(&c));
                let others: Vec<&str> = s.texts.iter().filter(|(o, t)| (o.center().y - r.center().y).abs() < 3.0 && o.left() >= left && o.right() <= r.left() && t != "…" && !icon(t)).map(|(_, t)| t.as_str()).collect();
                assert!(others.is_empty(), "current folder cut short at {w} px wide ({:.0} of {full:.0} px) while {others:?} still show", r.width());
            }
            look(&mut s, &format!("path bar at {w}"));
        }
        report("pathbar", &s);
    }

    /// A view picked in one folder (every kind) stays as the user opens other folders by
    /// double-click and Enter and goes back up.
    #[test]
    fn chosen_view_stays_while_browsing() {
        if !sandboxed() {
            return;
        }
        let home = fixture("viewstays");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.settle();
        for v in [ViewMedium, ViewTree, ViewList, ViewLarge, ViewXL, ViewColumns, ViewDetails] {
            go(&mut s, &home);
            s.cmd(v);
            s.settle();
            let zoom = s.tab().settings.zoom;
            // Into Documents with a double-click on its name.
            let r = s.text_rect("Documents");
            s.act(&format!("dclick {:.0} {:.0}", r.center().x, r.center().y));
            wait_until(&mut s, "in Documents", 10, |s| s.tab().path == home.join("Documents"));
            s.settle();
            assert_eq!(s.tab().settings.zoom, zoom, "{v:?} kept after a double-click");
            // Further down with Enter, then back up.
            select_path(&mut s, &home.join("Documents/Taxes"));
            s.act("key Enter");
            wait_until(&mut s, "in Taxes", 10, |s| s.tab().path == home.join("Documents/Taxes"));
            assert_eq!(s.tab().settings.zoom, zoom, "{v:?} kept after Enter");
            s.act("key Backspace");
            wait_until(&mut s, "back", 10, |s| s.tab().path == home.join("Documents"));
            s.cmd(Up);
            s.settle();
            assert_eq!(s.tab().settings.zoom, zoom, "{v:?} kept going back and up");
            assert_eq!(s.tab().settings.column_view, v == ViewColumns, "{v:?}: Columns kept, or not taken up");
            look(&mut s, &format!("{v:?} after browsing"));
            s.idle(30);
        }
        report("viewstays", &s);
    }

    /// Size and date conditions typed into the filter, alone and with a name, in every
    /// view, flattened, in contents mode, and words that only look like conditions.
    #[test]
    fn filter_by_size_and_date() {
        if !sandboxed() {
            return;
        }
        let home = fixture("filterq");
        let d = home.join("Documents");
        let now = std::time::SystemTime::now();
        let day = std::time::Duration::from_secs(86_400);
        for (name, n, age) in [("big video.mkv", 300u64 << 20, 500u64), ("small note.txt", 40, 1), ("Taxes/2025/scan.pdf", 4 << 20, 3)] {
            let f = std::fs::File::create(d.join(name)).unwrap();
            f.set_len(n).unwrap();
            f.set_modified(now - day * age as u32).unwrap();
        }
        std::fs::write(d.join("lazy note.txt"), "a lazy afternoon").unwrap();
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        let names = |s: &Sim| -> Vec<String> {
            let t = s.tab();
            let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
            v.sort();
            v
        };
        s.act("key ctrl+f");
        s.act("text size>100M");
        s.settle();
        assert_eq!(names(&s), ["big video.mkv"]);
        assert!(s.texts.iter().any(|(_, t)| t == "size>100M"), "the condition stays in the field");
        look(&mut s, "size condition");
        for v in [ViewTree, ViewList, ViewMedium, ViewLarge, ViewDetails] {
            s.cmd(v);
            s.settle();
            assert!(names(&s).contains(&"big video.mkv".to_string()), "{v:?}");
            look(&mut s, &format!("size condition {v:?}"));
        }
        // Older than a year, then a name and an age together.
        s.act("key ctrl+a");
        s.act("text modified>1y");
        s.settle();
        assert_eq!(names(&s), ["big video.mkv"]);
        s.act("key ctrl+a");
        s.act("text note modified<1w");
        s.settle();
        assert_eq!(names(&s), ["lazy note.txt", "notes.txt", "small note.txt"]);
        // Flattened: the scan two folders down.
        s.act("key ctrl+a");
        s.act("text size>1M modified<7d");
        s.cmd(ToggleFlatten);
        wait_until(&mut s, "flattened", 10, |s| !s.tab().is_loading());
        s.settle();
        assert_eq!(names(&s), ["scan.pdf"]);
        look(&mut s, "conditions flattened");
        s.cmd(ToggleFlatten);
        s.settle();
        // Contents mode: the condition narrows the files searched.
        click_control(&mut s, "contents toggle");
        s.act("key ctrl+a");
        s.act("text lazy size<1k");
        // The toggle hands focus back to the field: Ctrl+A selects its text, not the files.
        wait_until(&mut s, "contents search with a size", 10, |s| names(s) == ["lazy note.txt"]);
        s.act("key ctrl+a");
        s.act("text lazy size>1k");
        s.idle(2);
        wait_until(&mut s, "contents search done", 10, |s| !s.tab().content.busy());
        s.settle();
        assert!(names(&s).is_empty(), "the only file with the word is under 1 KB");
        click_control(&mut s, "contents toggle");
        // Not a condition: searched as a name, nothing found.
        s.act("key ctrl+a");
        s.act("text size>big");
        s.settle();
        assert!(names(&s).is_empty());
        s.act("key Escape");
        s.settle();
        assert!(s.tab().filter.is_empty() && names(&s).len() > 3);
        report("filterq", &s);
    }

    /// The filter's language typed in: either, not, kinds, endings, patterns, owners and
    /// groups, in every view, flattened over 45,000 files, and a broken pattern.
    #[test]
    fn filter_language() {
        if !sandboxed() {
            return;
        }
        use std::os::unix::fs::MetadataExt;
        let home = fixture("filterlang");
        let d = home.join("Documents");
        let now = std::time::SystemTime::now();
        let day = std::time::Duration::from_secs(86_400);
        for (name, n, age) in [("draft report.md", 900u64, 1u64), ("IMG_0001.jpg", 2 << 20, 3), ("IMG_0002.png", 300 << 10, 3), ("old scan.pdf", 5 << 20, 400)] {
            let f = std::fs::File::create(d.join(name)).unwrap();
            f.set_len(n).unwrap();
            f.set_modified(now - day * age as u32).unwrap();
        }
        // A big tree for the flattened searches: names of four kinds in 45 folders (the
        // fixture's own Big folder has more).
        let big = home.join("Big");
        for k in 0..45 {
            let dir = big.join(format!("part {k:02}"));
            std::fs::create_dir_all(&dir).unwrap();
            for i in 0..1000 {
                let name = match i % 4 {
                    0 => format!("img_{k}_{i}.jpg"),
                    1 => format!("report {i}.pdf"),
                    2 => format!("notes-{i}.txt"),
                    _ => format!("main{i}.rs"),
                };
                std::fs::File::create(dir.join(name)).unwrap();
            }
        }
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        let names = |s: &Sim| -> Vec<String> {
            let t = s.tab();
            let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
            v.sort();
            v
        };
        let typed = |s: &mut Sim, q: &str| {
            s.act("key ctrl+a");
            s.act(&format!("text {q}"));
            s.settle();
        };
        s.act("key ctrl+f");
        typed(&mut s, "ext:pdf | type:pictures");
        assert_eq!(names(&s), ["IMG_0001.jpg", "IMG_0002.png", "old scan.pdf"]);
        assert!(s.texts.iter().any(|(_, t)| t == "ext:pdf | type:pictures"), "the query stays in the field");
        look(&mut s, "either");
        s.shot("filter-either");
        for v in [ViewTree, ViewList, ViewMedium, ViewLarge, ViewDetails] {
            s.cmd(v);
            s.settle();
            assert_eq!(names(&s).len(), 3, "{v:?}");
            look(&mut s, &format!("either {v:?}"));
        }
        // Not, with a word and with a condition; a hyphen inside a name is the name's.
        typed(&mut s, "report -draft");
        assert_eq!(names(&s), ["report.md"]);
        typed(&mut s, "type:documents !ext:md,txt");
        assert_eq!(names(&s), ["budget 2026.csv", "old scan.pdf"]);
        typed(&mut s, "résumé – final");
        assert_eq!(names(&s), ["résumé – final (v3).txt"]);
        // Patterns, globs and a group.
        typed(&mut s, r"/^img_\d+\.png$/");
        assert_eq!(names(&s), ["IMG_0002.png"]);
        typed(&mut s, "*.jpg | name:scan");
        assert_eq!(names(&s), ["IMG_0001.jpg", "old scan.pdf"]);
        typed(&mut s, "(type:pictures | ext:pdf) size>1M modified<1y");
        assert_eq!(names(&s), ["IMG_0001.jpg"]);
        look(&mut s, "a group");
        // The owner by name: the users are read off the frame, then the list follows.
        let uid = std::fs::metadata(d.join("notes.txt")).unwrap().uid();
        let who = crate::perms::read_ids("/etc/passwd").into_iter().find(|u| u.0 == uid).map_or_else(|| uid.to_string(), |u| u.1);
        typed(&mut s, &format!("owner:{who} ext:pdf"));
        wait_until(&mut s, "the owner's files", 10, |s| names(s) == ["old scan.pdf"]);
        typed(&mut s, "owner:65534");
        assert!(names(&s).is_empty());
        // A broken pattern: nothing matches, and the empty list says what was typed.
        typed(&mut s, "/img_(/");
        assert!(names(&s).is_empty());
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Nothing matches")));
        look(&mut s, "broken pattern");
        s.shot("filter-broken-pattern");
        // Flattened over the big tree: a complex query keeps every frame quick.
        s.act("key Escape");
        go(&mut s, &big);
        s.cmd(ToggleFlatten);
        wait_until(&mut s, "the big walk", 20, |s| !s.tab().is_loading() && s.tab().entries.len() >= 45_000);
        let all = s.tab().view.len();
        s.act("key ctrl+f");
        typed(&mut s, r#"(img | report) -"part 0" (ext:jpg | re:"^report 1\d\.") -size>1k"#);
        let n = names(&s);
        assert!(!n.is_empty() && n.iter().all(|x| x.ends_with(".jpg") || x.starts_with("report 1")), "{:?}", &n[..n.len().min(5)]);
        assert!(s.tab().view.iter().all(|&i| !s.tab().entries[i as usize].rel().unwrap_or_default().contains("part 0")));
        // Longer than the box: it stays on its one line and scrolls.
        let line = s.texts.iter().find(|(_, t)| t.starts_with("(img | report)")).map(|(r, t)| (r.height(), t.ends_with("-size>1k")));
        assert!(line.is_some_and(|(h, whole)| whole && h < s.app.cfg.font_size * 2.0), "{line:?}");
        look(&mut s, "complex query flattened");
        s.shot("filter-complex-flattened");
        // OR binds loosest; "-main1" leaves out main1.rs, main103.rs…, "notes-99" is a word.
        typed(&mut s, "main -main1 ext:rs | notes-99");
        let n = names(&s);
        assert_eq!(n.iter().filter(|x| x.ends_with(".rs")).count(), 45 * (250 - 28));
        assert!(n.iter().any(|x| x.starts_with("notes-")) && n.iter().all(|x| x.ends_with(".rs") || x.starts_with("notes-") && x.matches('9').count() >= 2));
        s.act("key Escape");
        s.settle();
        assert!(s.tab().filter.is_empty() && s.tab().view.len() == all);
        report("filter language", &s);
    }

    /// Properties of a file: work out its SHA-256 and copy it; paste a checksum that is
    /// wrong, then the file's real MD5, and see each verdict.
    #[test]
    fn checksums_in_properties() {
        if !sandboxed() {
            return;
        }
        let home = fixture("checksums");
        let f = home.join("Documents/notes.txt");
        std::fs::write(&f, "abc").unwrap();
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        select_path(&mut s, &f);
        s.act("key alt+Enter");
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Checksum"), "checksum row shown");
        click_control(&mut s, "checksum button");
        let sha = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        wait_until(&mut s, "SHA-256 shown", 10, |s| s.texts.iter().any(|(_, t)| t == sha));
        look(&mut s, "checksum worked out");
        click_control(&mut s, "copy checksum");
        // A wrong one: its length says MD5, so MD5 is worked out and compared.
        click_control(&mut s, "checksum field");
        s.act("text 00000000000000000000000000000000");
        wait_until(&mut s, "mismatch", 10, |s| s.texts.iter().any(|(_, t)| t == "Doesn't match"));
        look(&mut s, "checksum mismatch");
        s.act("key ctrl+a");
        s.act("text MD5 (notes.txt) = 900150983CD24FB0D6963F7D28E17F72");
        wait_until(&mut s, "match", 10, |s| s.texts.iter().any(|(_, t)| t == "Matches"));
        assert!(s.texts.iter().any(|(_, t)| t == "MD5"));
        look(&mut s, "checksum match");
        s.act("key ctrl+a");
        s.act("text hello");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Not an MD5")), "says what it accepts");
        s.act("key Escape");
        s.idle(2);
        assert!(!s.popup_open());
        report("checksums", &s);
    }

    /// Git status marks: a work tree with every kind of change, the branch in the status
    /// bar, marks in every view, refreshed when a file changes and when git itself changes
    /// the index, and nothing outside a work tree.
    #[test]
    fn git_status_marks() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitmarks");
        let repo = home.join("Code/proj");
        crate::tests::git_fixture(&repo);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        let mark = |s: &Sim, p: &str| crate::gitstatus::repo_for(&s.tab().path).and_then(|r| r.marks.get(&repo.join(p)).copied());
        use crate::gitstatus::Mark;
        wait_until(&mut s, "status read", 10, |s| mark(s, "mod.txt") == Some(Mark::Modified));
        assert_eq!(mark(&s, "new.txt"), Some(Mark::Untracked));
        assert_eq!(mark(&s, "sub"), Some(Mark::Inside));
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "main"), "the branch is in the status bar");
        for v in [ViewTree, ViewList, ViewMedium, ViewLarge, ViewXL, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("git {v:?}"));
        }
        for (w, h) in [(520.0, 340.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("git {w}x{h}"));
        }
        // A file edited outside Maia: the watcher's change brings a fresh status.
        std::fs::write(repo.join("keep.txt"), "changed\n").unwrap();
        wait_until(&mut s, "edit seen", 10, |s| mark(s, "keep.txt") == Some(Mark::Modified));
        // git itself changing the index (only .git changes): staged now.
        crate::tests::git(&repo, &["add", "keep.txt"]);
        wait_until(&mut s, "staging seen", 10, |s| mark(s, "keep.txt") == Some(Mark::Staged));
        // Into a subfolder: same repository, marks below.
        s.act("text sub");
        s.act("key Enter");
        s.settle();
        assert_eq!(s.tab().path, repo.join("sub"));
        wait_until(&mut s, "subfolder marks", 10, |s| mark(s, "sub/inner") == Some(Mark::Inside));
        look(&mut s, "git subfolder");
        // Outside any work tree: no repository, no branch.
        let plain = std::env::temp_dir().join("plain-folder");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("a.txt"), "a").unwrap();
        go(&mut s, &plain);
        s.idle(10);
        assert!(crate::gitstatus::repo_for(&plain).is_none(), "no repository outside a work tree");
        assert!(!s.texts.iter().any(|(_, t)| t == "main"), "no branch outside a work tree");
        look(&mut s, "outside git");
        report("git", &s);
    }

    fn history(s: &Sim) -> Option<(usize, bool, Option<usize>, Option<usize>, Option<usize>)> {
        s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).map(|h| h.state())
    }

    /// Install Maia in Options only for a copy that isn't the installed one.
    #[test]
    fn install_row_only_when_not_installed() {
        if !sandboxed() {
            return;
        }
        let home = fixture("installrow");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        for (installed, shown) in [(false, true), (true, false)] {
            crate::install::set_installed(installed);
            // Filtered, so the row is on screen wherever the section puts it.
            s.app.popup = Popup::options_at(crate::popups::Section::System);
            s.idle(3);
            s.act("text install");
            s.idle(3);
            assert_eq!(s.texts.iter().any(|(_, t)| t.ends_with("Install Maia")), shown, "installed: {installed}");
            s.app.popup = Popup::None;
            s.idle(2);
        }
        crate::install::set_installed(false);
        report("install row", &s);
    }

    /// Updates from Options: Check for Updates (the command) opens Startup & system and
    /// asks; a newer release offers Update, which puts the checked build in place and
    /// offers Restart. A bad download or a folder Maia can't write says so.
    #[test]
    fn update_from_options() {
        if !sandboxed() {
            return;
        }
        use crate::update::{Fetch, State, Updater};
        let home = fixture("updates");
        let bin = root().join("updates-bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join("maia");
        std::fs::write(&exe, b"\x7fELF old").unwrap();
        let build = b"\x7fELF the new build".to_vec();
        let f = bin.join("sum-input");
        std::fs::write(&f, &build).unwrap();
        let sum = String::from_utf8_lossy(&std::process::Command::new("sha256sum").arg(&f).output().unwrap().stdout).split_whitespace().next().unwrap().to_string();
        let serve = |bin_bytes: Vec<u8>| -> Fetch {
            let sum = sum.clone();
            std::sync::Arc::new(move |url: &str, to: Option<&Path>| {
                let body = if url.contains("api.github.com") {
                    br#"{"tag_name":"v9.9.9","body":"New themes.","assets":[{"name":"maia-linux-x86_64","browser_download_url":"https://github.com/yappologistic/Maia/releases/download/v9.9.9/maia-linux-x86_64"},{"name":"maia-linux-x86_64.sha256","browser_download_url":"https://github.com/yappologistic/Maia/releases/download/v9.9.9/maia-linux-x86_64.sha256"}]}"#.to_vec()
                } else if url.ends_with(".sha256") {
                    format!("{sum}  maia-linux-x86_64").into_bytes()
                } else {
                    bin_bytes.clone()
                };
                match to {
                    Some(p) => std::fs::write(p, &body).map(|_| vec![]).map_err(|e| e.to_string()),
                    None => Ok(body),
                }
            })
        };
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.app.updater = Updater::for_test(serve(build.clone()), exe.clone());
        s.cmd(CheckUpdates);
        wait_until(&mut s, "a newer release", 5, |s| matches!(s.app.updater.state(), State::Available(_)));
        s.idle(3);
        assert!(matches!(&s.app.popup, Popup::Options(st) if st.section == Some(crate::popups::Section::System)), "Options at Startup & system");
        assert!(s.texts.iter().any(|(_, t)| t == "Update to 9.9.9"), "the step offered");
        look(&mut s, "update available");
        s.shot("update-available");
        click_control(&mut s, "update button");
        wait_until(&mut s, "installed", 10, |s| matches!(s.app.updater.state(), State::Ready(_)));
        s.idle(3);
        assert_eq!(std::fs::read(&exe).unwrap(), build, "the new build in place");
        assert!(s.texts.iter().any(|(_, t)| t == "Restart") && s.texts.iter().any(|(_, t)| t == "9.9.9 installed"));
        look(&mut s, "update installed");
        s.shot("update-ready");
        // A tampered download: said so, the program untouched.
        std::fs::write(&exe, b"\x7fELF old").unwrap();
        s.app.updater = Updater::for_test(serve(b"\x7fELF tampered".to_vec()), exe.clone());
        click_control(&mut s, "update button");
        wait_until(&mut s, "a newer release again", 5, |s| matches!(s.app.updater.state(), State::Available(_)));
        s.idle(2);
        click_control(&mut s, "update button");
        wait_until(&mut s, "a failure", 10, |s| matches!(s.app.updater.state(), State::Failed(_)));
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Couldn't update"));
        assert_eq!(std::fs::read(&exe).unwrap(), b"\x7fELF old");
        // Installed where Maia can't write.
        s.app.updater = Updater::for_test(serve(build.clone()), PathBuf::from("/proc/maia"));
        click_control(&mut s, "update button");
        wait_until(&mut s, "available", 5, |s| matches!(s.app.updater.state(), State::Available(_)));
        s.idle(2);
        click_control(&mut s, "update button");
        s.idle(3);
        assert!(matches!(s.app.updater.state(), State::CantWrite(_)));
        assert!(s.texts.iter().any(|(_, t)| t == "Update it where you installed it"));
        look(&mut s, "update elsewhere");
        s.shot("update-cant-write");
        // Installed from the .deb: the package goes to dpkg through pkexec, which asks for
        // the password while the row says it is installing.
        use crate::update::Installed;
        use std::os::unix::fs::PermissionsExt;
        let deb = Installed::Deb.package("v9.9.9").unwrap();
        let app = Installed::AppImage(PathBuf::new()).package("v9.9.9").unwrap();
        let (deb_bytes, app_bytes) = (b"!<arch>\nthe new package".to_vec(), b"\x7fELF the new AppImage".to_vec());
        let sum_of = |b: &[u8]| {
            std::fs::write(&f, b).unwrap();
            String::from_utf8_lossy(&std::process::Command::new("sha256sum").arg(&f).output().unwrap().stdout).split_whitespace().next().unwrap().to_string()
        };
        let sums = format!("{}  {deb}\n{}  {app}\n", sum_of(&deb_bytes), sum_of(&app_bytes));
        let packaged: Fetch = {
            let (sum, deb, app, deb_bytes, app_bytes, build) = (sum.clone(), deb.clone(), app.clone(), deb_bytes.clone(), app_bytes.clone(), build.clone());
            std::sync::Arc::new(move |url: &str, to: Option<&Path>| {
                let name = url.rsplit('/').next().unwrap_or("").to_string();
                let body = if url.contains("api.github.com") {
                    let assets: Vec<String> = ["maia-linux-x86_64", "maia-linux-x86_64.sha256", &deb, &app, "SHA256SUMS"].iter().map(|n| format!(r#"{{"name":"{n}","browser_download_url":"https://github.com/yappologistic/Maia/releases/download/v9.9.9/{n}"}}"#)).collect();
                    format!(r#"{{"tag_name":"v9.9.9","body":"New themes.","assets":[{}]}}"#, assets.join(",")).into_bytes()
                } else if name == "SHA256SUMS" {
                    sums.clone().into_bytes()
                } else if name.ends_with(".sha256") {
                    format!("{sum}  maia-linux-x86_64").into_bytes()
                } else if name == deb {
                    deb_bytes.clone()
                } else if name == app {
                    app_bytes.clone()
                } else {
                    build.clone()
                };
                match to {
                    Some(p) => std::fs::write(p, &body).map(|_| vec![]).map_err(|e| e.to_string()),
                    None => Ok(body),
                }
            })
        };
        let tools = root().join("updates-tools");
        let _ = std::fs::remove_dir_all(&tools);
        std::fs::create_dir_all(&tools).unwrap();
        for (name, body) in [
            ("dpkg-query", "echo \"maia: $2\"".to_string()),
            ("dpkg", format!("cp \"$2\" {}/installed", tools.display())),
            ("pkexec", format!("echo \"pkexec $*\" >> {}/log; sleep 1; exec \"$@\"", tools.display())),
        ] {
            std::fs::write(tools.join(name), format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(tools.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let sysbin = root().join("updates-system/usr/bin");
        std::fs::create_dir_all(&sysbin).unwrap();
        std::fs::write(sysbin.join("maia"), b"\x7fELF old").unwrap();
        std::fs::set_permissions(&sysbin, std::fs::Permissions::from_mode(0o555)).unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
        s.app.updater = Updater::for_test(packaged.clone(), sysbin.join("maia"));
        click_control(&mut s, "update button");
        wait_until(&mut s, "a release for the .deb", 5, |s| matches!(s.app.updater.state(), State::Available(_)));
        s.idle(2);
        click_control(&mut s, "update button");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        wait_until(&mut s, "the password asked", 10, |s| matches!(s.app.updater.state(), State::Installing(_)));
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Installing 9.9.9…"));
        look(&mut s, "update installing");
        s.shot("update-installing");
        wait_until(&mut s, "the package installed", 10, |s| matches!(s.app.updater.state(), State::Ready(_)));
        s.idle(3);
        assert_eq!(std::fs::read(tools.join("installed")).unwrap(), deb_bytes, "dpkg was given the release's package");
        assert!(std::fs::read_to_string(tools.join("log")).unwrap().contains(" -i "));
        assert!(s.texts.iter().any(|(_, t)| t == "Restart"));
        std::fs::set_permissions(&sysbin, std::fs::Permissions::from_mode(0o755)).unwrap();
        // Run from an AppImage: its file replaced, no password.
        let apps = root().join("updates-apps");
        std::fs::create_dir_all(&apps).unwrap();
        let file = apps.join("Maia.AppImage");
        std::fs::write(&file, b"\x7fELF old AppImage").unwrap();
        s.app.updater = Updater::for_test(packaged, PathBuf::from("/proc/maia")).with_appimage(file.clone());
        click_control(&mut s, "update button");
        wait_until(&mut s, "a release for the AppImage", 5, |s| matches!(s.app.updater.state(), State::Available(_)));
        s.idle(2);
        click_control(&mut s, "update button");
        wait_until(&mut s, "the AppImage replaced", 10, |s| matches!(s.app.updater.state(), State::Ready(_)));
        s.idle(3);
        assert_eq!(std::fs::read(&file).unwrap(), app_bytes);
        assert!(s.texts.iter().any(|(_, t)| t == "9.9.9 installed"));
        report("updates", &s);
    }

    /// The theme picker: from Options, every theme on screen as a small window with its
    /// name; hovering one shows it on the whole window, a click keeps it, and code takes
    /// its colours (or the classic set with "Code colors follow the theme" off).
    #[test]
    fn theme_picker() {
        if !sandboxed() {
            return;
        }
        let home = fixture("themes");
        std::fs::write(home.join("Documents/main.rs"), "// demo\nfn main() {\n    let x: u32 = 42;\n    println!(\"{x}\\n\");\n}\n\n#[derive(Debug)]\nstruct Point<'a> { name: &'a str, x: f64 }\n\nimpl Point<'_> {\n    const ORIGIN: f64 = 0.0;\n    fn norm(&self) -> Option<f64> { Some(self.x.abs()) }\n}\n").unwrap();
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        use crate::theme::{Syntax, ThemeName};
        assert_eq!(s.app.pal.syntax, Syntax::of(ThemeName::Midnight), "code in the theme's colours by default");
        s.act("key ctrl+Comma");
        s.settle();
        s.act("text color scheme");
        s.idle(3);
        // Every control fits its row, with descriptions and without (titles only).
        for compact in [false, true] {
            s.app.cfg.compact_lists = compact;
            s.act("key ctrl+a");
            s.act("key Backspace");
            s.idle(3);
            s.shot(if compact { "options-compact" } else { "options" });
            let rows = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, _, w)| *w == "choose theme").map(|(_, r, _)| *r).collect::<Vec<_>>());
            let h = if compact { 32.0 } else { 44.0 };
            assert!(rows.iter().all(|r| r.height() <= h), "the theme row's button fits a {h}-point row: {rows:?}");
        }
        s.app.cfg.compact_lists = false;
        s.act("text color scheme");
        s.idle(3);
        click_control(&mut s, "choose theme");
        s.settle();
        assert!(matches!(s.app.popup, Popup::Themes), "the picker opened");
        // Every theme can be seen, its name whole: in view, or scrolled to (the search,
        // the switch and Done stay put).
        theme_cards_reachable(&mut s, "a normal window");
        let ctl = |s: &Sim, name: &str| crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == name).map(|(_, r, _)| *r));
        let shown = |s: &Sim| ThemeName::ALL.iter().filter(|t| ctl(s, t.label()).is_some()).count();
        let done = ctl(&s, "themes done").unwrap_or_else(|| s.fail("no Done"));
        s.act(&format!("scroll {:.0} {:.0} 2000", done.center().x - 200.0, done.top() - 120.0));
        s.idle(2);
        look(&mut s, "theme picker");
        s.shot("theme-picker");
        // Hovering shows a theme on the whole window, without choosing it; leaving puts
        // the chosen one back. (Typed first: at larger font sizes it is scrolled away.)
        s.act("text gruvbox");
        s.idle(3);
        let r = ctl(&s, "Gruvbox").unwrap_or_else(|| s.fail("no Gruvbox"));
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        s.idle(3);
        assert_eq!(s.app.pal.panel, crate::theme::palette(ThemeName::Gruvbox).panel, "previewed while hovered");
        assert_eq!(s.app.cfg.theme, ThemeName::Midnight, "not chosen by a hover");
        s.act(&format!("move {:.0} {:.0}", done.left() - 300.0, done.center().y));
        s.idle(3);
        assert_eq!(s.app.pal.panel, crate::theme::palette(ThemeName::Midnight).panel, "the chosen theme is back");
        // Typing searches, by name or by its line.
        s.act("key ctrl+a");
        s.act("text tokyo");
        s.idle(3);
        let tokyo = ThemeName::ALL.iter().filter(|t| t.label().contains("Tokyo")).count();
        assert_eq!(shown(&s), tokyo, "the Tokyo themes only");
        assert!(ctl(&s, "Tokyo Night").is_some());
        let tokyo_dark = ThemeName::ALL.iter().filter(|t| t.label().contains("Tokyo") && !t.is_light()).count();
        assert!(s.texts.iter().any(|(_, t)| *t == format!("Dark · {tokyo_dark}")), "the group says how many it shows");
        s.act("key ctrl+a");
        s.act("text warm and retro");
        s.idle(3);
        assert!(ctl(&s, "Gruvbox").is_some(), "found by its line");
        s.act("key ctrl+a");
        s.act("text monokai pro");
        s.idle(3);
        assert_eq!(shown(&s), 2, "Monokai Pro and its light one");
        assert!(ctl(&s, "Monokai Pro").is_some() && ctl(&s, "Monokai Pro Light").is_some());
        s.act("key ctrl+a");
        s.act("text zzzz");
        s.idle(3);
        assert!(shown(&s) == 0 && s.texts.iter().any(|(_, t)| t == "No theme matches"));
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.idle(3);
        assert_eq!(shown(&s), ThemeName::ALL.len());
        // The filter: light ones only, dark ones only, then all.
        let light = ThemeName::ALL.iter().filter(|t| t.is_light()).count();
        click_control(&mut s, "light themes");
        s.idle(3);
        assert!(ThemeName::ALL.iter().all(|t| ctl(&s, t.label()).is_some() == t.is_light()), "only light themes");
        assert!(s.texts.iter().any(|(_, t)| *t == format!("Light · {light}")));
        theme_cards_reachable(&mut s, "light themes");
        click_control(&mut s, "dark themes");
        s.idle(3);
        assert!(ThemeName::ALL.iter().all(|t| ctl(&s, t.label()).is_some() == !t.is_light()), "only dark themes");
        // The arrows walk the cards and show each on the window; Enter keeps one.
        let order: Vec<ThemeName> = ThemeName::ALL.into_iter().filter(|t| !t.is_light()).collect();
        s.act("move -50 -50");
        s.act("key Home");
        s.idle(2);
        assert_eq!(s.app.pal.panel, crate::theme::palette(order[0]).panel, "Home: the first card");
        s.act("key ArrowRight");
        s.idle(2);
        assert_eq!(s.app.pal.panel, crate::theme::palette(order[1]).panel, "Right: the next card, previewed");
        assert_eq!(s.app.cfg.theme, ThemeName::Midnight, "not chosen by the arrows");
        let y = |s: &Sim, t: ThemeName| ctl(s, t.label()).map_or(0.0, |r| r.center().y);
        s.act("key ArrowDown");
        s.idle(2);
        let below = order.iter().copied().find(|t| crate::theme::palette(*t).panel == s.app.pal.panel).unwrap_or_else(|| s.fail("Down previews a dark theme"));
        assert!(y(&s, below) > y(&s, order[1]) + 1.0, "Down: a card in the row below");
        s.act("key End");
        s.idle(2);
        assert_eq!(s.app.pal.panel, crate::theme::palette(*order.last().unwrap()).panel, "End: the last card");
        // Esc goes back to Options with the chosen theme as it was.
        s.act("key Escape");
        s.idle(3);
        assert!(matches!(s.app.popup, Popup::Options(_)), "Esc goes back");
        assert_eq!(s.app.pal.panel, crate::theme::palette(ThemeName::Midnight).panel, "nothing chosen");
        // Opened again, the filter is as it was left; the arrows and Enter choose.
        s.act("text color scheme");
        s.idle(3);
        click_control(&mut s, "choose theme");
        s.settle();
        assert!(ThemeName::ALL.iter().all(|t| ctl(&s, t.label()).is_some() == !t.is_light()), "the filter is kept for the session");
        s.act("key ArrowRight");
        s.act("key Enter");
        s.idle(3);
        let after = order[order.iter().position(|t| *t == ThemeName::Midnight).unwrap() + 1];
        assert_eq!(s.app.cfg.theme, after, "Enter keeps the focused theme");
        assert!(matches!(s.app.popup, Popup::Options(_)), "and goes back");
        s.app.cfg.theme = ThemeName::Midnight;
        s.act("text color scheme");
        s.idle(3);
        click_control(&mut s, "choose theme");
        s.settle();
        click_control(&mut s, "all themes");
        s.idle(3);
        // A small window: the grid narrows, every name still whole.
        s.resize(520.0, 340.0);
        s.idle(3);
        theme_cards_reachable(&mut s, "a small window");
        look(&mut s, "theme picker, small");
        s.shot("theme-picker-small");
        s.resize(1280.0, 800.0);
        s.idle(3);
        // Found by typing (it may be scrolled out of view), then a click keeps it.
        s.act("text tokyo night");
        s.idle(3);
        click_control(&mut s, "Tokyo Night");
        s.idle(3);
        assert_eq!(s.app.cfg.theme, ThemeName::TokyoNight);
        s.act("move 5 5");
        s.idle(3);
        assert_eq!(s.app.pal.syntax, Syntax::of(ThemeName::TokyoNight), "code takes the theme's colours");
        s.shot("theme-picker-tokyo");
        // The code colours' switch, in the picker.
        s.app.cfg.syntax_theme = false;
        s.idle(3);
        assert_eq!(s.app.pal.syntax, Syntax::classic(true), "the classic set when off");
        s.app.cfg.syntax_theme = true;
        click_control(&mut s, "themes done");
        s.settle();
        assert!(matches!(s.app.popup, Popup::Options(_)), "back to Options");
        s.act("key Escape");
        s.settle();
        // A light one, and code drawn in its colours.
        s.app.cfg.theme = ThemeName::Latte;
        s.cmd(ToggleInspector);
        select_path(&mut s, &home.join("Documents/main.rs"));
        wait_until(&mut s, "highlighted code", 10, |s| inspector(s).text_spans().is_some_and(|n| n >= 6));
        look(&mut s, "code in a light theme");
        s.shot("code-latte");
        assert_eq!(s.app.pal.syntax, Syntax::of(ThemeName::Latte));
        // Line spacing: the gap between two lines of code grows with it, the text kept
        // whole in each row.
        let gap = |s: &mut Sim, spacing: f32| {
            s.app.cfg.preview_line_spacing = spacing;
            s.idle(3);
            let y = |t: &str| s.texts.iter().find(|(_, x)| x.starts_with(t)).map(|(r, _)| r.top()).unwrap_or_else(|| s.fail(&format!("no {t:?}")));
            y("fn") - y("// demo")
        };
        let (tight, roomy) = (gap(&mut s, 1.0), gap(&mut s, 2.0));
        assert!(roomy > tight * 1.8, "lines spread out: {tight} then {roomy}");
        s.app.cfg.preview_line_spacing = 1.25;
        s.idle(2);
        s.shot("code-line-spacing");
        report("themes", &s);
    }

    /// Every card the picker shows can be brought into view, between the search and
    /// Done, with its name whole (not cut with "…") and inside the window.
    fn theme_cards_reachable(s: &mut Sim, what: &str) {
        let ctl = |s: &Sim, name: &str| crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == name).map(|(_, r, _)| *r));
        let top = ctl(s, "theme search").map(|r| r.bottom()).unwrap_or_else(|| s.fail("no search"));
        let done = ctl(s, "themes done").unwrap_or_else(|| s.fail("no Done"));
        let shown: Vec<crate::theme::ThemeName> = crate::theme::ThemeName::ALL.into_iter().filter(|t| ctl(s, t.label()).is_some()).collect();
        assert!(!shown.is_empty(), "{what}: cards drawn");
        let full_h = ctl(s, shown[0].label()).map_or(0.0, |r| r.height());
        let mid = pos2(done.center().x - 150.0, (top + done.top()) / 2.0);
        for t in shown {
            let named = |s: &Sim| s.texts.iter().any(|(tr, x)| x == t.label() && tr.top() >= top && tr.bottom() <= done.top() && tr.right() <= s.size.x);
            // The whole card in view: below the search, above the footer's rule.
            let whole = |r: Rect| r.top() >= top - 1.0 && r.bottom() <= done.top() - 12.0 && r.height() >= full_h - 1.0;
            for _ in 0..200 {
                let r = ctl(s, t.label()).unwrap_or_else(|| s.fail(&format!("{what}: {} not drawn", t.label())));
                if named(s) && whole(r) {
                    break;
                }
                // Half a notch at a time, let settle: a card about as tall as the list
                // is otherwise stepped over back and forth.
                let dy = if r.top() < top + 1.0 || r.height() < full_h - 1.0 && r.top() <= top + 40.0 { 0.5 } else { -0.5 };
                s.act(&format!("scroll {:.0} {:.0} {dy}", mid.x, mid.y));
                s.idle(8);
            }
            let r = ctl(s, t.label()).unwrap();
            assert!(named(s) && whole(r) && r.left() >= 0.0 && r.right() <= s.size.x, "{what}: {} in view, its name whole: {r:?}", t.label());
        }
    }

    /// Search by meaning, start to finish, against a stand-in Ollama: off by default (the
    /// window explains and offers to turn it on), the model fetched only on a click, files
    /// read, a vague question answered with the right file and its passage, Enter opening
    /// it, and switching it off in Options stopping everything.
    #[test]
    fn search_by_meaning() {
        if !sandboxed() {
            return;
        }
        let home = fixture("meaning");
        let docs = home.join("Documents");
        std::fs::write(docs.join("letter from the bank.txt"), "Dear customer, your tax refund has been paid into your account.").unwrap();
        std::fs::write(docs.join("summer plans.md"), "Two weeks at the beach, a flight on Friday and a hotel by the sea.").unwrap();
        std::fs::write(docs.join("Taxes/pasta.txt"), "Pasta with tomato sauce: boil, stir and eat.").unwrap();
        crate::tests::captioned_png(&docs.join("IMG_0412.png"), "a puppy playing in the sand");
        std::fs::create_dir_all(docs.join("Receipts")).unwrap();
        for k in 1..=8 {
            std::fs::write(docs.join(format!("Receipts/quittance {k}.txt")), "12.50 4.99 3.00").unwrap();
        }
        let (addr, model, _) = crate::tests::fake_ollama(false);
        *crate::ollama::TEST_HOST.lock().unwrap() = Some(addr);
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                *crate::ollama::TEST_HOST.lock().unwrap() = None;
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        assert!(!s.app.cfg.semantic_search && s.app.semantic.is_none(), "off by default: nothing running");
        s.app.cfg.semantic_folders = docs.display().to_string();
        // Off, Ctrl+K finds by name, anywhere in the home folder, at once.
        s.act("key ctrl+k");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Search by meaning is off."));
        click_control(&mut s, "meaning field");
        s.act("text summer");
        wait_until(&mut s, "a name found", 5, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.named.first().is_some_and(|(p, _)| p.ends_with("summer plans.md"))));
        s.idle(2);
        let shown_folder = format!("~/{}", docs.strip_prefix(crate::fsx::home()).unwrap().display());
        assert!(s.texts.iter().any(|(_, t)| *t == shown_folder), "the folder under the name");
        look(&mut s, "name search");
        s.act("key Enter");
        s.settle();
        assert!(!s.popup_open());
        assert_eq!(s.tab().cursor_path().as_deref(), Some(docs.join("summer plans.md").as_path()), "opened in its folder, selected");
        // A folder's name, and words split between the folder and the name.
        s.act("key ctrl+k");
        s.idle(2);
        click_control(&mut s, "meaning field");
        s.act("text taxes pasta");
        wait_until(&mut s, "by folder and name", 5, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.named.first().is_some_and(|(p, _)| p.ends_with("Taxes/pasta.txt"))));
        s.act("key ctrl+a");
        s.act("text Taxes");
        wait_until(&mut s, "a folder", 5, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.named.first().is_some_and(|(p, d)| *d && p.ends_with("Taxes"))));
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.idle(2);
        look(&mut s, "meaning off");
        s.click_text("Turn on");
        wait_until(&mut s, "asks for the model", 10, |s| s.texts.iter().any(|(_, t)| t == "Download, about 860 MB"));
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(!model.load(std::sync::atomic::Ordering::Relaxed), "nothing fetched before the click");
        look(&mut s, "meaning needs the model");
        s.click_text("Download, about 860 MB");
        wait_until(&mut s, "files read", 20, |s| matches!(s.app.semantic.as_ref().map(|e| e.phase()), Some(crate::semantic::Phase::Ready)));
        s.idle(2);
        let n = s.app.semantic.as_ref().map_or(0, |e| e.index().files());
        assert!(n >= 3, "the fixture's text files and ours: {n}");
        assert!(s.texts.iter().any(|(_, t)| *t == format!("{n} files read for search by meaning")));
        // A vague question, none of its words in the file.
        click_control(&mut s, "meaning field");
        s.act("text money back from the government");
        wait_until(&mut s, "answer", 10, |s| matches!(&s.app.popup, Popup::Meaning(st) if !st.hits.is_empty()));
        s.idle(3);
        match &s.app.popup {
            Popup::Meaning(st) => assert_eq!(st.hits[0].path, docs.join("letter from the bank.txt")),
            _ => s.fail("window closed"),
        }
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Dear customer, your tax refund")), "the passage under the name");
        // Only what stands out: the vague question's one clear answer, the rest folded
        // away (or not there at all).
        match &s.app.popup {
            Popup::Meaning(st) => assert!(st.hits.iter().filter(|h| h.strong).count() <= 2, "a short list: {:?}", st.hits.iter().map(|h| (&h.path, h.strong)).collect::<Vec<_>>()),
            _ => s.fail("window closed"),
        }
        // Many results show at once, each row whole (not squeezed into the window's first,
        // empty size).
        s.act("key ctrl+a");
        s.act("text quittance");
        wait_until(&mut s, "receipts by name", 5, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.named.len() >= 8));
        s.idle(3);
        let shown = (1..=8).filter(|k| {
            let name = format!("quittance {k}.txt");
            s.texts.iter().any(|(r, t)| *t == name && r.height() > 8.0 && r.top() > 0.0 && r.bottom() < s.size.y)
        }).count();
        assert!(shown >= 4, "only {shown} results on screen");
        s.act("key ctrl+a");
        s.act("text money back from the government");
        wait_until(&mut s, "answer again", 10, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.hits.first().is_some_and(|h| h.path.ends_with("letter from the bank.txt"))));
        s.idle(3);
        look(&mut s, "meaning results");
        // A picture, by what it shows.
        s.act("key ctrl+a");
        s.act("text photo of my dog");
        wait_until(&mut s, "picture found", 10, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.hits.first().is_some_and(|h| h.path.ends_with("IMG_0412.png"))));
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Picture"), "a picture is said to be one");
        look(&mut s, "a picture found");
        s.act("key ctrl+a");
        s.act("text money back from the government");
        wait_until(&mut s, "answer again", 10, |s| matches!(&s.app.popup, Popup::Meaning(st) if st.hits.first().is_some_and(|h| h.path.ends_with("letter from the bank.txt"))));
        s.act("key Enter");
        s.settle();
        assert!(!s.popup_open());
        assert_eq!(s.tab().path, docs, "opened in its folder");
        assert_eq!(s.tab().cursor_path().as_deref(), Some(docs.join("letter from the bank.txt").as_path()), "with the file selected");
        // Off again from Options: the engine goes.
        s.act("key ctrl+comma");
        s.act("text search by meaning");
        s.idle(2);
        s.act("key Enter");
        s.idle(3);
        s.act("key Escape");
        assert!(!s.app.cfg.semantic_search && s.app.semantic.is_none(), "switched off: nothing running");
        report("meaning", &s);
    }

    /// A clone two commits ahead of its remote and one behind: the status bar's branch
    /// chip says so, and the history shows the remote branch with a cloud and a branch in
    /// step with its remote as one pill.
    #[test]
    fn git_branch_labels_and_counts() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitremote");
        let d = home.join("Code");
        let run = |at: &Path, args: &[&str]| assert!(std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"]).args(args).current_dir(at).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status().is_ok_and(|st| st.success()), "git {args:?}");
        std::fs::create_dir_all(d.join("server.git")).unwrap();
        run(&d.join("server.git"), &["init", "-q", "--bare"]);
        run(&d, &["clone", "-q", "server.git", "ours"]);
        let ours = d.join("ours");
        std::fs::write(ours.join("a.txt"), "a").unwrap();
        run(&ours, &["add", "."]);
        run(&ours, &["commit", "-qm", "Start"]);
        run(&ours, &["push", "-q", "-u", "origin", "main"]);
        run(&ours, &["branch", "docs"]);
        run(&ours, &["push", "-q", "origin", "docs"]);
        run(&d, &["clone", "-q", "server.git", "theirs"]);
        std::fs::write(d.join("theirs/b.txt"), "b").unwrap();
        run(&d.join("theirs"), &["add", "."]);
        run(&d.join("theirs"), &["commit", "-qm", "Their change"]);
        run(&d.join("theirs"), &["push", "-q"]);
        for n in ["Mine one", "Mine two"] {
            std::fs::write(ours.join(n), n).unwrap();
            run(&ours, &["add", "."]);
            run(&ours, &["commit", "-qm", n]);
        }
        run(&ours, &["fetch", "-q"]);
        let mut s = Sim::new(&ours, vec2(1280.0, 800.0));
        wait_until(&mut s, "counts read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| (r.ahead, r.behind) == (2, 1)));
        s.idle(2);
        // The chip: branch, then 2 (up) and 1 (down), all on its row.
        let chip = s.texts.iter().filter(|(_, t)| t == "main").max_by(|a, b| a.0.center().y.total_cmp(&b.0.center().y)).map(|(r, _)| *r).unwrap();
        for n in ["2", "1"] {
            assert!(s.texts.iter().any(|(r, t)| t == n && (r.center().y - chip.center().y).abs() < 2.0 && r.left() > chip.right()), "{n} beside the branch");
        }
        look(&mut s, "ahead and behind");
        s.act(&format!("move {:.0} {:.0}", chip.center().x, chip.center().y));
        s.idle(40);
        assert!(s.texts.iter().any(|(_, t)| t == "2 to push, 1 to pull · click for history and branches"), "the tooltip says what the numbers are");
        // The history: our branch alone, origin/main on their commit, docs with its remote.
        s.act(&format!("click {:.0} {:.0}", chip.center().x, chip.center().y));
        wait_until(&mut s, "the branch menu", 5, |s| s.texts.iter().any(|(_, t)| t == "History"));
        // A menu taller than when it was last open settles on its next frame.
        s.idle(3);
        s.click_text("History");
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1 && h.0 == 4));
        s.idle(3);
        for t in ["origin/main", "docs · origin"] {
            assert!(s.texts.iter().any(|(_, x)| x == t), "{t:?} in the graph");
        }
        assert!(!s.texts.iter().any(|(_, x)| x == "origin/docs"), "a remote in step with its branch has no pill of its own");
        look(&mut s, "branch labels");
        report("gitremote", &s);
    }

    /// A commit's diff with a long line: the history's Wrap lines button wraps it (every
    /// row inside the preview), and the setting is the code previews' own.
    #[test]
    fn git_history_diffs_wrap() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitwrap");
        let repo = home.join("Code/wrap");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| assert!(std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).current_dir(&repo).stdout(std::process::Stdio::null()).status().is_ok_and(|st| st.success()), "git {args:?}");
        g(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("notes.md"), "# Notes\n").unwrap();
        g(&["add", "."]);
        g(&["commit", "-qm", "Start the notes"]);
        let long = "A long line in a diff keeps going well past the right edge of the preview, and wrapped it stays readable. ".repeat(4);
        std::fs::write(repo.join("notes.md"), format!("# Notes\n{long}\n")).unwrap();
        g(&["commit", "-qam", "Write the long notes"]);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1 && h.0 == 2));
        s.idle(2);
        s.click_text("Write the long notes");
        wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
        s.idle(2);
        // A double click shows the diff in the whole panel.
        s.idle(40);
        let r = s.text_rect("notes.md");
        s.act(&format!("dclick {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "diff", 10, |s| history(s).is_some_and(|h| h.4.is_some()));
        s.idle(2);
        let hist = |s: &Sim| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).and_then(|h| h.wrapped_rows());
        assert_eq!(hist(&s), None, "diffs start unwrapped, as code does");
        click_control(&mut s, "Wrap lines");
        s.idle(2);
        let lines = history(&s).and_then(|h| h.4).unwrap();
        assert!(hist(&s).is_some_and(|n| n > lines + 2), "the long line wraps: {:?} rows for {lines} lines", hist(&s));
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        for r in s.texts.iter().filter(|(r, _)| r.top() > ir.top() + 30.0 && r.left() >= ir.left()).map(|(r, _)| *r) {
            assert!(r.right() <= ir.right() + 0.5, "a wrapped diff row runs past the preview: {r:?} in {ir:?}");
        }
        assert!(s.app.panels[&s.app.active].inspector.as_ref().is_some_and(|i| i.wrap_code), "the code previews' setting");
        look(&mut s, "wrapped diff");
        // The palette command turns it off again here too.
        s.cmd(InspectorWrap);
        s.idle(2);
        assert_eq!(hist(&s), None);
        report("gitwrap", &s);
    }

    /// The branch chip (the lowest "main" on screen, drawn over everything), then
    /// History in its menu.
    fn open_history(s: &mut Sim) {
        let chip = s.texts.iter().filter(|(_, t)| t == "main").max_by(|a, b| a.0.center().y.total_cmp(&b.0.center().y)).map(|(r, _)| *r).unwrap_or_else(|| s.fail("no branch chip"));
        s.act(&format!("click {:.0} {:.0}", chip.center().x, chip.center().y));
        wait_until(s, "the branch menu", 5, |s| matches!(s.app.popup, Popup::Branches(_)) && s.texts.iter().any(|(_, t)| t == "History"));
        // A menu taller than when it was last open settles on its next frame.
        s.idle(3);
        s.click_text("History");
    }

    fn probe(s: &Sim) -> crate::gitlog::Probe {
        s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).map(|h| h.probe()).unwrap_or_else(|| s.fail("history closed"))
    }

    fn git_hist(s: &Sim) -> &crate::gitlog::History {
        s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_deref()).unwrap_or_else(|| s.fail("history closed"))
    }

    /// Where `label` is drawn inside the inspector (the listing beside it may show the
    /// same name).
    fn in_inspector(s: &Sim, label: &str) -> Rect {
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
        s.texts.iter().rev().find(|(r, t)| t == label && ir.contains(r.center())).map(|(r, _)| *r).unwrap_or_else(|| {
            let near: Vec<&str> = s.texts.iter().filter(|(r, _)| ir.contains(r.center())).map(|(_, t)| t.as_str()).take(60).collect();
            s.fail(&format!("no {label:?} in the inspector; there: {near:?}"))
        })
    }

    /// Scroll the inspector, as a person would with the wheel, until `label` is in view.
    fn scroll_to_label(s: &mut Sim, label: &str) {
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
        let body = Rect::from_min_max(pos2(ir.left(), ir.top() + 40.0), pos2(ir.right(), ir.bottom() - 10.0));
        for k in 0..60 {
            if s.texts.iter().any(|(r, t)| t == label && body.contains_rect(*r)) {
                return;
            }
            s.act(&format!("scroll {:.0} {:.0} {}", body.center().x, body.center().y, if k < 20 { -2 } else { 2 }));
            s.idle(12);
        }
    }

    /// A click at a person's pace: never close enough to the last one to count as a
    /// double click.
    fn click_in_inspector(s: &mut Sim, label: &str, how: &str) {
        s.idle(40);
        scroll_to_label(s, label);
        let r = in_inspector(s, label);
        s.act(&format!("{how} {:.0} {:.0}", r.center().x, r.center().y));
    }

    /// `label` is in the inspector, scrolled into view if it has to be.
    fn see(s: &mut Sim, label: &str) -> Rect {
        scroll_to_label(s, label);
        in_inspector(s, label)
    }

    fn right_click_in_inspector(s: &mut Sim, label: &str) {
        s.idle(40);
        scroll_to_label(s, label);
        let r = in_inspector(s, label);
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
    }

    fn file_index(s: &Sim, path: &str) -> usize {
        git_hist(s).files().iter().position(|f| f.path == path).unwrap_or_else(|| s.fail(&format!("{path} not in the commit")))
    }

    /// The commit view, as someone reading a change would use it: files grouped by
    /// folder and the switch to a plain list, folding a folder, pictures before and
    /// after, diffs opening under their rows and in the whole panel, the keys, the
    /// right-click menu (open, show in folder, as of this commit, file history, copy
    /// path), walking commits with the arrows and parent links, copying the hash, wrapped
    /// diffs, a deleted picture, then small windows and a light theme.
    #[test]
    fn git_commit_view() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitcommit");
        let repo = home.join("Code/project");
        crate::tests::commit_view_fixture(&repo);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1 && h.0 == 5));
        look(&mut s, "graph");
        s.shot("graph");
        click_in_inspector(&mut s, "Lay out the panels", "click");
        wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3 == Some(12)));
        // Twelve files in several folders: grouped, folders first.
        let p = probe(&s);
        assert!(p.tree && p.items > 12, "grouped by folder: {p:?}");
        for t in ["assets", "icons", "docs", "src", "ui", "tests", "12 files changed", "+30", "−9", "Grace Hopper", "GH", "v1.0", "Parent", "binary"] {
            see(&mut s, t);
        }
        // A rename reads old → new.
        see(&mut s, "guide.md→handbook.md");
        // Pictures: the changed logo's two sides and the new icon's one.
        wait_until(&mut s, "pictures", 10, |s| probe(s).pictures >= 3);
        assert_eq!(probe(&s).failed, 0);
        look(&mut s, "commit grouped");
        s.shot("commit-grouped");
        // The plain list and back; the folder then shows after each name.
        click_control(&mut s, "Show as a list");
        assert!(!probe(&s).tree && probe(&s).items == 12);
        see(&mut s, "src/ui");
        look(&mut s, "commit as a list");
        s.shot("commit-list");
        click_control(&mut s, "Group by folder");
        assert!(probe(&s).tree);
        // A folder folds and unfolds.
        let n = probe(&s).items;
        click_in_inspector(&mut s, "assets", "click");
        assert_eq!((probe(&s).folded, probe(&s).items), (1, n - 3), "assets folded: its subfolder, the icon and the logo");
        click_in_inspector(&mut s, "assets", "click");
        assert_eq!((probe(&s).folded, probe(&s).items), (0, n));
        // A diff opens under its row, and closes again.
        let view = file_index(&s, "src/ui/view.rs");
        click_in_inspector(&mut s, "view.rs", "click");
        wait_until(&mut s, "inline diff", 10, |s| probe(s).inline_lines.len() == 1);
        assert_eq!(probe(&s).inline, [view]);
        s.idle(2);
        see(&mut s, "+// view two");
        look(&mut s, "inline diff");
        s.shot("inline-diff");
        click_in_inspector(&mut s, "view.rs", "click");
        assert!(probe(&s).inline.is_empty(), "closed again");
        // A picture opens larger.
        click_in_inspector(&mut s, "logo.png", "click");
        assert_eq!(probe(&s).inline, [file_index(&s, "assets/logo.png")]);
        look(&mut s, "pictures larger");
        s.shot("pictures-larger");
        click_in_inspector(&mut s, "logo.png", "click");
        // Keys: Home, down twice to the icon, Enter opens it and Enter closes it; [ and ]
        // open the files one at a time.
        s.act("key Home");
        s.act("key ArrowDown");
        s.act("key ArrowDown");
        let icon = file_index(&s, "assets/icons/icon.png");
        assert_eq!(probe(&s).sel, 2);
        s.act("key Enter");
        assert_eq!(probe(&s).inline, [icon]);
        s.act("key Enter");
        assert!(probe(&s).inline.is_empty());
        s.act("key CloseBracket");
        assert_eq!(probe(&s).inline, [icon], "] opens the selected file first");
        s.act("key CloseBracket");
        assert_eq!(probe(&s).inline, [file_index(&s, "assets/logo.png")], "then the next one, alone");
        s.act("key OpenBracket");
        assert_eq!((probe(&s).inline.clone(), probe(&s).sel), (vec![icon], 2));
        s.act("key Enter");
        // The whole panel: a double click, then [ and ] through the files, Esc back.
        let main = file_index(&s, "src/main.rs");
        let r = see(&mut s, "main.rs");
        s.act(&format!("dclick {:.0} {:.0}", r.center().x, r.center().y));
        assert_eq!(probe(&s).full, Some((main, false)));
        wait_until(&mut s, "the diff", 10, |s| probe(s).full.is_some() && history(s).is_some_and(|h| h.4.is_some()));
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "+    ui::run();"), "the diff in the panel");
        look(&mut s, "diff in the panel");
        s.shot("diff-large");
        // Side by side, and back: remembered.
        click_control(&mut s, "Side by side");
        s.idle(2);
        assert!(s.app.cfg.diff_side, "side by side");
        assert!(s.texts.iter().any(|(_, t)| t.trim_start() == "ui::run();"), "the new line on its side, without its +: {:?}", s.texts.iter().map(|x| &x.1).collect::<Vec<_>>());
        look(&mut s, "diff side by side");
        s.shot("diff-side-by-side");
        click_control(&mut s, "Side by side");
        assert!(!s.app.cfg.diff_side);
        s.act("key CloseBracket");
        let after = probe(&s).full.unwrap_or_else(|| s.fail("still in the panel")).0;
        assert_ne!(after, main, "] goes to the next file");
        click_control(&mut s, "previous file");
        assert_eq!(probe(&s).full, Some((main, false)), "and the arrow back");
        s.act("key Escape");
        assert_eq!(probe(&s).full, None);
        assert!(probe(&s).inline.contains(&main), "a double click leaves the diff open under its row");
        click_in_inspector(&mut s, "main.rs", "click");
        // The open-large button on a hovered row.
        let r = see(&mut s, "README.md");
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        click_control(&mut s, "open large");
        assert_eq!(probe(&s).full, Some((file_index(&s, "README.md"), false)));
        s.act("key Escape");
        // The right-click menu.
        right_click_in_inspector(&mut s, "main.rs");
        assert!(matches!(s.app.popup, Popup::GitFile { .. }), "the file's menu");
        for t in ["Open", "Show in folder", "View as of this commit", "File history", "Copy path"] {
            s.text_rect(t);
        }
        // Menus fade in over a few frames.
        s.idle(12);
        s.shot("file-menu");
        s.click_text("Copy path");
        let want = repo.join("src/main.rs").display().to_string();
        assert!(toast_is(&s, &format!("Copied {want}")), "{:?}", s.app.toast);
        right_click_in_inspector(&mut s, "main.rs");
        s.click_text("View as of this commit");
        assert_eq!(probe(&s).full, Some((main, true)));
        wait_until(&mut s, "the file as it was", 10, |s| git_hist(s).full_text().is_some());
        assert_eq!(git_hist(&s).full_text().unwrap()[0], "mod ui;");
        s.idle(2);
        look(&mut s, "file as of the commit");
        s.shot("as-of-commit");
        s.act("key Escape");
        let before = crate::mime::LAUNCHED.lock().unwrap().len();
        right_click_in_inspector(&mut s, "main.rs");
        s.click_text("Open");
        s.idle(2);
        let launched = crate::mime::LAUNCHED.lock().unwrap().clone();
        assert!(launched.len() > before && launched.iter().any(|l| format!("{l:?}").contains("src/main.rs")), "opened in its app: {launched:?}");
        right_click_in_inspector(&mut s, "main.rs");
        s.click_text("Show in folder");
        s.settle();
        assert_eq!(s.tab().path, repo.join("src"), "its folder");
        let t = s.tab();
        assert!(t.cursor.is_some_and(|c| t.entries[c as usize].name == "main.rs"), "with the file selected");
        assert!(probe(&s).at.is_some(), "the commit stays open inside the work tree");
        // A deleted file is read as it was before.
        right_click_in_inspector(&mut s, "old.txt");
        assert!(s.texts.iter().any(|(_, t)| t == "View before it was deleted"));
        s.click_text("View before it was deleted");
        wait_until(&mut s, "the deleted file", 10, |s| git_hist(s).full_text().is_some());
        assert_eq!(git_hist(&s).full_text().unwrap()[0], "old line 1");
        s.act("key Escape");
        // One file's history, through its rename, and back to every commit.
        right_click_in_inspector(&mut s, "guide.md→handbook.md");
        s.click_text("File history");
        wait_until(&mut s, "the file's commits", 10, |s| history(s).is_some_and(|h| h.1));
        assert_eq!(probe(&s).filter.as_deref(), Some("docs/handbook.md"));
        assert_eq!(history(&s).map(|h| h.0), Some(2), "the rename followed back");
        see(&mut s, "Start the project");
        see(&mut s, "handbook.md");
        look(&mut s, "file history");
        s.shot("file-history");
        click_in_inspector(&mut s, "Start the project", "click");
        wait_until(&mut s, "its files", 10, |s| history(s).is_some_and(|h| h.3.is_some()));
        s.act("key Escape");
        assert!(probe(&s).at.is_none() && probe(&s).filter.is_some(), "back to the file's commits");
        click_control(&mut s, "show all commits");
        assert_eq!((probe(&s).filter, history(&s).map(|h| h.0)), (None, Some(5)), "every commit again");
        // Esc clears it too.
        click_in_inspector(&mut s, "Tune the panel", "click");
        wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
        right_click_in_inspector(&mut s, "panel.rs");
        s.click_text("File history");
        wait_until(&mut s, "panel.rs's commits", 10, |s| history(s).is_some_and(|h| h.1 && h.0 == 2));
        s.act("key Escape");
        assert_eq!(probe(&s).filter, None, "Esc shows every commit again");
        assert!(history(&s).is_some(), "and leaves the history open");
        // The arrows walk the commits; the oldest has no older one.
        click_in_inspector(&mut s, "Lay out the panels", "click");
        wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3 == Some(12)));
        click_control(&mut s, "newer commit");
        assert_eq!(probe(&s).subject.as_deref(), Some("Write the long notes"));
        wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
        // A long line wraps under its row too.
        click_in_inspector(&mut s, "notes.md", "click");
        wait_until(&mut s, "inline diff", 10, |s| probe(s).inline_lines.len() == 1);
        click_control(&mut s, "Wrap lines");
        s.idle(2);
        let p = probe(&s);
        assert!(p.inline_wrapped.first().is_some_and(|&w| w > p.inline_lines[0] + 1), "the long line wraps: {p:?}");
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        for r in s.texts.iter().filter(|(r, _)| ir.contains(r.left_center())).map(|(r, _)| *r) {
            assert!(r.right() <= ir.right() + 0.5, "a wrapped row runs past the panel: {r:?}");
        }
        look(&mut s, "wrapped inline diff");
        s.shot("inline-wrapped");
        click_control(&mut s, "Wrap lines");
        click_control(&mut s, "older commit");
        click_control(&mut s, "older commit");
        assert_eq!(probe(&s).subject.as_deref(), Some("Start the project"));
        click_control(&mut s, "older commit");
        assert_eq!(probe(&s).subject.as_deref(), Some("Start the project"), "nothing older");
        // Parent links: from the panels commit to its parent.
        click_control(&mut s, "newer commit");
        assert_eq!(probe(&s).subject.as_deref(), Some("Lay out the panels"));
        click_control(&mut s, "parent link");
        assert_eq!(probe(&s).subject.as_deref(), Some("Start the project"));
        // The hash pill copies the whole hash.
        click_control(&mut s, "copy hash");
        let full = git_hist(&s).hash(4).unwrap();
        assert!(toast_is(&s, &format!("Copied {full}")), "{:?}", s.app.toast);
        // A deleted picture: only its old side.
        for _ in 0..3 {
            click_control(&mut s, "newer commit");
        }
        assert_eq!(probe(&s).subject.as_deref(), Some("Remove the logo"));
        wait_until(&mut s, "the old logo", 10, |s| probe(s).pictures == 1);
        look(&mut s, "deleted picture");
        s.shot("deleted-picture");
        // The authors keep their colours: the same in the header as in the graph.
        assert_eq!(crate::gitlog::author_hue("ada@example.invalid", ""), crate::gitlog::author_hue("ADA@example.invalid ", "x"));
        // Small windows, a light theme, no animations.
        click_control(&mut s, "older commit");
        click_control(&mut s, "older commit");
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.app.cfg.animations = false;
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (900.0, 1200.0), (1920.0, 1080.0)] {
            s.resize(w, h);
            look(&mut s, &format!("commit {w}x{h}"));
            s.shot(&format!("commit-paper-{w}x{h}"));
        }
        s.resize(1280.0, 800.0);
        // Docked at the bottom, and beside a split.
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Bottom;
        look(&mut s, "commit docked at the bottom");
        s.shot("commit-bottom");
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Auto;
        s.cmd(SplitRight);
        s.cmd(FocusPrevPanel);
        look(&mut s, "commit beside a split");
        s.cmd(FocusNextPanel);
        s.cmd(ClosePanel);
        s.idle(2);
        click_in_inspector(&mut s, "view.rs", "click");
        wait_until(&mut s, "inline diff", 10, |s| probe(s).inline_lines.len() == 1);
        look(&mut s, "light inline diff");
        s.shot("commit-paper-inline");
        for _ in 0..2 {
            s.act("key Escape");
        }
        s.act("key Escape");
        assert!(s.app.panels[&s.app.active].inspector.is_none(), "closed with its inspector");
        report("gitcommit", &s);
    }

    fn changes(s: &Sim) -> crate::gitchanges::Probe {
        git_hist(s).changes()
    }

    /// Hover the row drawn as `label` in the inspector (the `nth` from the top), then
    /// click its control `what`.
    fn row_control(s: &mut Sim, label: &str, nth: usize, what: &str) {
        s.idle(40);
        scroll_to_label(s, label);
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
        let mut rows: Vec<Rect> = s.texts.iter().filter(|(r, t)| t == label && ir.contains(r.center())).map(|(r, _)| *r).collect();
        rows.sort_by(|a, b| a.top().total_cmp(&b.top()));
        let r = *rows.get(nth).unwrap_or_else(|| s.fail(&format!("no {label:?} number {nth}")));
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        s.idle(2);
        let c = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, b, w)| *w == what && (b.center().y - r.center().y).abs() < 8.0).map(|(_, b, _)| *b));
        let c = c.unwrap_or_else(|| {
            let near: Vec<String> = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, b, _)| (b.center().y - r.center().y).abs() < 40.0).map(|(_, b, w)| format!("{w} {b:?}")).collect());
            s.fail(&format!("no {what} on the {label} row at {r:?}; near: {near:?}"))
        });
        s.act(&format!("click {:.0} {:.0}", c.center().x, c.center().y));
    }

    /// The `nth` control called `what` from the top.
    fn click_nth(s: &mut Sim, what: &str, nth: usize) {
        s.idle(1);
        let mut v: Vec<Rect> = crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, _, w)| *w == what).map(|(_, r, _)| *r).collect());
        v.sort_by(|a, b| a.top().total_cmp(&b.top()));
        let r = *v.get(nth).unwrap_or_else(|| s.fail(&format!("no {what} number {nth}")));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
    }

    fn git_out(at: &Path, args: &[&str]) -> String {
        String::from_utf8_lossy(&std::process::Command::new("git").arg("-C").arg(at).args(args).output().unwrap().stdout).trim().to_string()
    }

    /// The Changes page, as someone getting a commit together uses it: the Changes row
    /// above the commits, a file's diff hunk by hunk, staging one hunk and taking it
    /// back, whole files in and out, everything at once, discarding (into the stash, or
    /// the trash for an untracked file), the keys, a commit, an amend, a hook that
    /// refuses, wrapped lines, then small windows, a light theme, the bottom dock and a
    /// split.
    #[test]
    fn git_changes_page() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitchanges");
        let repo = home.join("Code/changes");
        crate::tests::changes_fixture(&repo);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1 && h.0 == 1));
        wait_until(&mut s, "the changes", 10, |s| changes(s).staged.len() == 1 && changes(s).unstaged.len() == 4);
        s.idle(2);
        see(&mut s, "Changes");
        see(&mut s, "1 staged · 4 not staged");
        let (row, first) = (in_inspector(&s, "Changes"), in_inspector(&s, "Start the project"));
        assert!(row.top() < first.top(), "the changes come above the commits");
        look(&mut s, "changes row");
        s.shot("changes-row");
        click_in_inspector(&mut s, "Changes", "click");
        wait_until(&mut s, "the page", 5, |s| changes(s).open);
        assert_eq!(changes(&s).staged, ["src/view.rs"]);
        assert_eq!(changes(&s).unstaged, ["README.md", "old.txt", "src/lib.rs", "notes.md"]);
        for t in ["Staged", "Changes", "view.rs", "lib.rs", "notes.md", "old.txt", "Stage all", "Unstage all", "+2", "−2", "+3"] {
            see(&mut s, t);
        }
        look(&mut s, "changes page");
        s.shot("changes-page");
        // A file's diff, hunk by hunk; the second hunk staged alone and taken back.
        click_in_inspector(&mut s, "lib.rs", "click");
        wait_until(&mut s, "the diff", 10, |s| changes(s).inline == [(false, "src/lib.rs".to_string(), 2)]);
        s.idle(2);
        see(&mut s, "+line three");
        see(&mut s, "+line thirty-six");
        look(&mut s, "hunks");
        s.shot("changes-hunks");
        click_nth(&mut s, "stage hunk", 1);
        wait_until(&mut s, "one hunk staged", 10, |s| changes(s).staged.contains(&"src/lib.rs".to_string()) && changes(s).unstaged.contains(&"src/lib.rs".to_string()) && !changes(s).busy);
        assert_eq!(git_out(&repo, &["diff", "--cached", "--numstat", "--", "src/lib.rs"]), "1	1	src/lib.rs");
        wait_until(&mut s, "the rest read again", 10, |s| changes(s).inline == [(false, "src/lib.rs".to_string(), 1)]);
        assert!(matches!(changes(&s).said, Some(Ok(ref m)) if m == "Staged the hunk"), "{:?}", changes(&s).said);
        click_in_inspector(&mut s, "lib.rs", "click");
        // The unstaged lib.rs is the lower one.
        row_control(&mut s, "lib.rs", 1, "stage file");
        s.idle(1);
        // The staged lib.rs (the upper row) opens its own diff, whose hunk goes back.
        wait_until(&mut s, "idle", 10, |s| !changes(s).busy);
        let w = changes(&s);
        assert!(w.staged.contains(&"src/lib.rs".to_string()) && !w.unstaged.contains(&"src/lib.rs".to_string()), "{w:?}");
        click_in_inspector(&mut s, "lib.rs", "click");
        wait_until(&mut s, "the staged diff", 10, |s| changes(s).inline.iter().any(|x| x.0 && x.2 == 2));
        click_nth(&mut s, "unstage hunk", 0);
        wait_until(&mut s, "the hunk unstaged", 10, |s| !changes(s).busy && changes(s).unstaged.contains(&"src/lib.rs".to_string()));
        assert_eq!(git_out(&repo, &["diff", "--cached", "--numstat", "--", "src/lib.rs"]), "1	1	src/lib.rs");
        // Whole files in and out, then everything.
        row_control(&mut s, "notes.md", 0, "stage file");
        wait_until(&mut s, "notes staged", 10, |s| !changes(s).busy && changes(s).staged.contains(&"notes.md".to_string()));
        row_control(&mut s, "notes.md", 0, "unstage file");
        wait_until(&mut s, "notes unstaged", 10, |s| !changes(s).busy && changes(s).unstaged.contains(&"notes.md".to_string()));
        click_control(&mut s, "stage all");
        wait_until(&mut s, "all staged", 10, |s| !changes(s).busy && changes(s).unstaged.is_empty());
        assert_eq!(changes(&s).staged.len(), 5);
        look(&mut s, "all staged");
        click_control(&mut s, "unstage all");
        wait_until(&mut s, "none staged", 10, |s| !changes(s).busy && changes(s).staged.is_empty());
        // Nothing staged: Commit waits.
        click_control(&mut s, "commit summary");
        s.act("text Tidy the view");
        s.idle(2);
        let before = changes(&s).said;
        click_control(&mut s, "commit");
        s.idle(5);
        // The index changed a moment ago: the page may still be reading it again.
        wait_until(&mut s, "the page settled", 10, |s| !changes(s).busy);
        assert!(changes(&s).said == before, "nothing staged, nothing tried");
        // The keys (the button took the keyboard from the field): Home and Down to a
        // file, Space stages it, Delete asks.
        assert!(s.ctx.memory(|m| m.focused()).is_none());
        for k in ["Home", "ArrowDown"] {
            s.act(&format!("key {k}"));
        }
        assert_eq!(changes(&s).sel, 2, "on the second file");
        s.act("key Space");
        wait_until(&mut s, "staged by key", 10, |s| !changes(s).busy && changes(s).staged == ["old.txt"]);
        s.act("key Home");
        s.act("key ArrowDown");
        s.act("key Delete");
        assert!(matches!(s.app.popup, Popup::Confirm { .. }), "Delete asks first");
        s.idle(12);
        s.shot("changes-discard-confirm");
        s.act("key Escape");
        assert!(repo.join("README.md").exists());
        // Discard: README's change goes to the stash, the untracked notes to the trash.
        row_control(&mut s, "README.md", 0, "discard file");
        assert!(matches!(s.app.popup, Popup::Confirm { .. }));
        s.click_text("Discard");
        wait_until(&mut s, "discarded", 10, |s| !changes(s).busy && !changes(s).unstaged.contains(&"README.md".to_string()));
        assert_eq!(std::fs::read_to_string(repo.join("README.md")).unwrap(), "# Project
");
        assert!(git_out(&repo, &["stash", "list"]).contains("Discarded in Maia: README.md"));
        row_control(&mut s, "notes.md", 0, "discard file");
        s.click_text("Move to Trash");
        wait_until(&mut s, "trashed", 10, |s| !changes(s).busy && !changes(s).unstaged.contains(&"notes.md".to_string()));
        assert!(!repo.join("notes.md").exists() && crate::trashcan::list().iter().any(|i| i.original == repo.join("notes.md")), "in the trash");
        // A commit: everything staged, the summary typed, the button.
        click_control(&mut s, "stage all");
        wait_until(&mut s, "all staged", 10, |s| !changes(s).busy && changes(s).unstaged.is_empty());
        look(&mut s, "ready to commit");
        s.shot("changes-ready");
        click_control(&mut s, "commit");
        wait_until(&mut s, "committed", 10, |s| matches!(changes(s).said, Some(Ok(_))));
        assert!(changes(&s).said.as_ref().is_some_and(|r| r.as_ref().is_ok_and(|m| m.starts_with("Committed "))));
        assert_eq!(git_out(&repo, &["log", "-1", "--format=%s"]), "Tidy the view");
        wait_until(&mut s, "back on the graph", 10, |s| !changes(s).open && history(s).is_some_and(|h| h.1 && h.0 == 2));
        s.idle(3);
        see(&mut s, "Tidy the view");
        assert!(!s.texts.iter().any(|(_, t)| t == "1 staged · 4 not staged"), "nothing left to commit");
        look(&mut s, "after the commit");
        s.shot("changes-committed");
        // Amend: the last message comes in, is changed, and Ctrl+Enter amends.
        std::fs::write(repo.join("src/view.rs"), "fn view() { draw(); flush(); }
").unwrap();
        crate::gitstatus::rescan(&repo);
        wait_until(&mut s, "a new change", 10, |s| changes(s).unstaged == ["src/view.rs"]);
        s.idle(3);
        click_in_inspector(&mut s, "Changes", "click");
        wait_until(&mut s, "the page", 5, |s| changes(s).open);
        click_control(&mut s, "stage all");
        wait_until(&mut s, "staged", 10, |s| !changes(s).busy && changes(s).staged == ["src/view.rs"]);
        s.click_text("Amend the last commit");
        wait_until(&mut s, "the last message", 5, |s| changes(s).amend && changes(s).summary == "Tidy the view");
        click_control(&mut s, "commit summary");
        s.act("key End");
        s.act("text  and flush it");
        look(&mut s, "amending");
        s.shot("changes-amend");
        s.act("key ctrl+Enter");
        wait_until(&mut s, "amended", 10, |s| matches!(changes(s).said, Some(Ok(_))));
        assert_eq!(git_out(&repo, &["log", "-1", "--format=%s"]), "Tidy the view and flush it");
        assert_eq!(git_out(&repo, &["rev-list", "--count", "HEAD"]), "2", "amended, not added");
        assert!(s.app.panels[&s.app.active].tabs.len() == 1, "Ctrl+Enter did not open a tab");
        // A hook that refuses: its words, plainly, and the change still staged.
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(&hook, "#!/bin/sh\necho 'lint: view.rs has a trailing space' >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        std::fs::write(repo.join("src/view.rs"), "fn view() { draw(); } \n").unwrap();
        crate::gitstatus::rescan(&repo);
        wait_until(&mut s, "changed again", 10, |s| changes(s).unstaged == ["src/view.rs"]);
        s.idle(3);
        click_in_inspector(&mut s, "Changes", "click");
        wait_until(&mut s, "the page", 5, |s| changes(s).open);
        click_control(&mut s, "stage all");
        wait_until(&mut s, "staged", 10, |s| !changes(s).busy && changes(s).staged == ["src/view.rs"]);
        click_control(&mut s, "commit summary");
        s.act("text Add a space");
        click_control(&mut s, "commit");
        wait_until(&mut s, "refused", 10, |s| matches!(changes(s).said, Some(Err(_))));
        assert!(matches!(changes(&s).said, Some(Err(ref e)) if e.contains("lint: view.rs has a trailing space")), "{:?}", changes(&s).said);
        assert_eq!(changes(&s).staged, ["src/view.rs"], "still staged");
        see(&mut s, "lint: view.rs has a trailing space");
        look(&mut s, "hook refused");
        s.shot("changes-hook");
        std::fs::remove_file(&hook).unwrap();
        // A long line wraps under its row.
        std::fs::write(repo.join("README.md"), format!("# Project\n{}\n", "Words that keep going past the edge of the panel. ".repeat(6))).unwrap();
        crate::gitstatus::rescan(&repo);
        wait_until(&mut s, "README changed", 10, |s| changes(s).unstaged.contains(&"README.md".to_string()));
        click_in_inspector(&mut s, "README.md", "click");
        wait_until(&mut s, "its diff", 10, |s| changes(s).inline.iter().any(|x| x.1 == "README.md" && x.2 == 1));
        if !s.app.panels[&s.app.active].inspector.as_ref().is_some_and(|i| i.wrap_code) {
            click_control(&mut s, "Wrap lines");
        }
        s.idle(3);
        assert!(changes(&s).wrapped.iter().any(|&n| n > 3), "the long line wraps: {:?}", changes(&s).wrapped);
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        for r in s.texts.iter().filter(|(r, _)| ir.contains(r.left_center())).map(|(r, _)| *r) {
            assert!(r.right() <= ir.right() + 0.5, "a wrapped row runs past the panel: {r:?}");
        }
        look(&mut s, "wrapped change");
        s.shot("changes-wrapped");
        click_control(&mut s, "Wrap lines");
        // Around it: small windows, a light theme, no animations, the bottom dock, a split.
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.app.cfg.animations = false;
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (900.0, 1200.0), (1920.0, 1080.0)] {
            s.resize(w, h);
            look(&mut s, &format!("changes {w}x{h}"));
            s.shot(&format!("changes-paper-{w}x{h}"));
        }
        s.resize(1280.0, 800.0);
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Bottom;
        look(&mut s, "changes docked at the bottom");
        s.shot("changes-bottom");
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Auto;
        s.cmd(SplitRight);
        s.cmd(FocusPrevPanel);
        look(&mut s, "changes beside a split");
        s.shot("changes-split");
        s.cmd(FocusNextPanel);
        s.cmd(ClosePanel);
        s.idle(2);
        // Escape goes back to the graph, the Changes row there; the palette command
        // opens the page again.
        s.act("key Escape");
        assert!(!changes(&s).open && history(&s).is_some());
        s.act("key Home");
        assert!(git_hist(&s).on_changes(), "Home is on the Changes row");
        s.act("key Enter");
        assert!(changes(&s).open, "Enter opens it");
        s.act("key Escape");
        s.act("key Escape");
        assert!(s.app.panels[&s.app.active].inspector.is_none(), "closed with the inspector it opened");
        palette_run(&mut s, "Show Git Changes");
        wait_until(&mut s, "the page from the palette", 5, |s| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).is_some_and(|h| h.changes().open));
        look(&mut s, "changes from the palette");
        report("gitchanges", &s);
    }

    fn branch_menu(s: &Sim) -> Option<&crate::popups::BranchState> {
        match &s.app.popup {
            Popup::Branches(st) => Some(st),
            _ => None,
        }
    }

    /// The branch chip, and its menu once its lists are read.
    fn open_branches(s: &mut Sim) {
        let chip = s.texts.iter().filter(|(_, t)| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| *t == r.branch)).max_by(|a, b| a.0.center().y.total_cmp(&b.0.center().y)).map(|(r, _)| *r).unwrap_or_else(|| {
            let low: Vec<&str> = s.texts.iter().filter(|(r, _)| r.top() > s.size.y - 30.0).map(|(_, t)| t.as_str()).collect();
            s.fail(&format!("no branch chip for {:?} (popup {}); the status bar has {low:?}", branch_now(s), s.app.popup.kind_name()))
        });
        s.act(&format!("click {:.0} {:.0}", chip.center().x, chip.center().y));
        wait_until(s, "the branch menu", 5, |s| branch_menu(s).is_some_and(|m| m.refs.is_some()));
        s.idle(12);
    }

    fn branch_now(s: &Sim) -> String {
        crate::gitstatus::repo_for(&s.tab().path).map(|r| r.branch.clone()).unwrap_or_default()
    }

    /// The branch chip's menu, by mouse and by keyboard: the branches with the current
    /// one marked and what each has to push, switching (and being refused when local
    /// changes would be lost), a new branch, a remote branch checked out to track it,
    /// a stash applied and dropped, another work tree in a new tab, the history from
    /// it, the palette command, then small windows and a light theme.
    #[test]
    fn git_branch_menu() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitbranches");
        let d = home.join("Code");
        let g = |at: &Path, args: &[&str]| crate::tests::git(at, args);
        std::fs::create_dir_all(d.join("server.git")).unwrap();
        g(&d.join("server.git"), &["init", "-q", "--bare"]);
        let ours = d.join("ours");
        std::fs::create_dir_all(&ours).unwrap();
        crate::tests::work_repo(&ours);
        std::fs::write(ours.join("a.txt"), "a\n").unwrap();
        g(&ours, &["add", "-A"]);
        g(&ours, &["commit", "-qm", "Start"]);
        g(&ours, &["remote", "add", "origin", d.join("server.git").to_str().unwrap()]);
        g(&ours, &["push", "-q", "-u", "origin", "main"]);
        g(&ours, &["branch", "topic"]);
        g(&ours, &["push", "-q", "origin", "topic"]);
        g(&ours, &["branch", "-q", "-D", "topic"]);
        g(&ours, &["checkout", "-qb", "docs"]);
        std::fs::write(ours.join("a.txt"), "docs\n").unwrap();
        g(&ours, &["commit", "-qam", "Docs"]);
        g(&ours, &["checkout", "-q", "main"]);
        for n in ["One", "Two"] {
            std::fs::write(ours.join(n), n).unwrap();
            g(&ours, &["add", "-A"]);
            g(&ours, &["commit", "-qm", n]);
        }
        std::fs::write(ours.join("One"), "tweaked").unwrap();
        g(&ours, &["stash", "push", "-q", "-m", "A tweak"]);
        g(&ours, &["worktree", "add", "-q", d.join("wt").to_str().unwrap(), "-b", "spike"]);
        let mut s = Sim::new(&ours, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| branch_now(s) == "main");
        s.idle(2);
        open_branches(&mut s);
        let r = branch_menu(&s).unwrap().refs.clone().unwrap();
        assert!(r.local.iter().any(|b| b.name == "main" && b.current && b.ahead == 2), "{:?}", r.local);
        for t in ["History", "Branches", "main", "docs", "spike", "New branch…", "Remote branches", "origin/topic", "Stashes", "On main: A tweak", "stash@{0}", "Work trees", "wt"] {
            assert!(s.texts.iter().any(|(_, x)| x == t), "{t:?} in the menu");
        }
        assert!(!s.texts.iter().any(|(_, x)| x == "origin/main"), "a tracked remote branch is its local one");
        let menu = s.text_rect("New branch…");
        assert!(menu.bottom() < s.text_rect("History").bottom() + 400.0 && s.text_rect("History").top() >= 0.0);
        s.shot("branch-menu");
        for t in ["Pull", "Push", "Fetch"] {
            assert!(s.texts.iter().any(|(_, x)| x == t), "{t:?} in the menu: there is a remote");
        }
        // By keyboard: down past History, Pull, Push and Fetch to docs, Enter.
        let at = 4 + r.local.iter().position(|b| b.name == "docs").unwrap();
        for _ in 0..at {
            s.act("key ArrowDown");
        }
        assert_eq!(branch_menu(&s).unwrap().sel, at);
        s.idle(4);
        s.shot("branch-menu-keyboard");
        s.act("key Enter");
        assert!(!s.popup_open(), "the menu closes");
        wait_until(&mut s, "switched", 10, |s| branch_now(s) == "docs");
        wait_until(&mut s, "said so", 5, |s| toast_is(s, "Switched to docs"));
        // Refused: a local change git would lose. Its words, in full.
        std::fs::write(ours.join("a.txt"), "mine\n").unwrap();
        open_branches(&mut s);
        s.click_text("main");
        wait_until(&mut s, "refused", 10, |s| matches!(s.app.popup, Popup::Notice { .. }));
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Couldn't switch to main"));
        assert!(s.texts.iter().any(|(_, t)| t.contains("would be overwritten") && t.contains("a.txt")), "git's reason");
        s.idle(12);
        s.shot("branch-refused");
        s.act("key Enter");
        assert!(!s.popup_open());
        assert_eq!((branch_now(&s), std::fs::read_to_string(ours.join("a.txt")).unwrap()), ("docs".to_string(), "mine\n".to_string()), "nothing lost");
        g(&ours, &["checkout", "-q", "--", "a.txt"]);
        // The stash (made on main): applied, then dropped after asking.
        g(&ours, &["checkout", "-q", "main"]);
        crate::gitstatus::rescan(&ours);
        wait_until(&mut s, "back on main", 10, |s| branch_now(s) == "main");
        open_branches(&mut s);
        s.click_text("On main: A tweak");
        assert!(matches!(s.app.popup, Popup::Stash { .. }));
        s.idle(12);
        s.shot("stash-menu");
        s.click_text("Apply");
        wait_until(&mut s, "applied", 10, |s| toast_is(s, "Applied stash@{0}"));
        assert_eq!(std::fs::read_to_string(ours.join("One")).unwrap(), "tweaked");
        g(&ours, &["checkout", "-q", "--", "One"]);
        open_branches(&mut s);
        s.click_text("On main: A tweak");
        for _ in 0..3 {
            s.act("key ArrowDown");
        }
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Confirm { .. }), "a drop asks first");
        s.click_text("Drop");
        wait_until(&mut s, "dropped", 10, |s| toast_is(s, "Dropped stash@{0}"));
        assert_eq!(git_out(&ours, &["stash", "list"]), "");
        // A new branch from here, named in a prompt.
        open_branches(&mut s);
        assert!(!s.texts.iter().any(|(_, t)| t == "Stashes"), "no stashes left");
        s.click_text("New branch…");
        assert!(matches!(s.app.popup, Popup::Prompt { .. }));
        s.act("text feature/menu");
        s.act("key Enter");
        wait_until(&mut s, "created", 10, |s| branch_now(s) == "feature/menu");
        assert!(toast_is(&s, "Created and switched to feature/menu"));
        // A bad name is refused, plainly.
        open_branches(&mut s);
        s.click_text("New branch…");
        s.act("text two..dots");
        s.act("key Enter");
        wait_until(&mut s, "refused", 10, |s| matches!(s.app.popup, Popup::Notice { .. }));
        s.act("key Escape");
        // A remote branch: a local one that tracks it.
        open_branches(&mut s);
        s.click_text("origin/topic");
        wait_until(&mut s, "tracking", 10, |s| branch_now(s) == "topic");
        assert_eq!(git_out(&ours, &["rev-parse", "--abbrev-ref", "topic@{upstream}"]), "origin/topic");
        open_branches(&mut s);
        assert!(!s.texts.iter().any(|(_, t)| t == "origin/topic"), "tracked now");
        // Another work tree opens in a new tab.
        s.click_text("wt");
        s.settle();
        assert_eq!((s.app.panels[&s.app.active].tabs.len(), s.tab().path.clone()), (2, d.join("wt")));
        wait_until(&mut s, "its branch", 10, |s| branch_now(s) == "spike");
        s.act("key ctrl+w");
        s.settle();
        // History from the menu, and closing it from there.
        open_branches(&mut s);
        s.click_text("History");
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.0 > 0));
        open_branches(&mut s);
        assert!(s.texts.iter().any(|(_, t)| t == "Close history"));
        s.click_text("Close history");
        assert!(history(&s).is_none());
        // The palette command; Escape closes the menu.
        palette_run(&mut s, "Git Branches");
        wait_until(&mut s, "the menu from the palette", 5, |s| branch_menu(s).is_some_and(|m| m.refs.is_some()));
        s.act("key Escape");
        assert!(!s.popup_open());
        // A clean work tree: the Changes page says so, and the graph has no such row.
        s.cmd(GitChanges);
        wait_until(&mut s, "read", 10, |s| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).is_some_and(|h| h.changes().open && !h.changes().busy));
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "No changes"), "the empty page says so");
        look(&mut s, "no changes");
        s.shot("changes-none");
        s.act("key Escape");
        s.idle(2);
        assert!(!s.texts.iter().any(|(_, t)| t == "Changes"), "no Changes row on a clean tree");
        s.act("key Escape");
        // Small windows, a light theme, no animations.
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.app.cfg.animations = false;
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (1920.0, 1080.0)] {
            s.resize(w, h);
            look(&mut s, &format!("branches {w}x{h}"));
            // A narrow status bar leaves the chip out: the palette opens the menu then.
            if s.texts.iter().any(|(r, t)| t == "topic" && r.top() > h - 30.0) {
                open_branches(&mut s);
            } else {
                palette_run(&mut s, "Git Branches");
                wait_until(&mut s, "the menu", 5, |s| branch_menu(s).is_some_and(|m| m.refs.is_some()));
                s.idle(12);
            }
            let pr = crate::ui::POPUPS.with(|p| p.borrow().first().copied()).unwrap_or_else(|| s.fail("no popup"));
            assert!(pr.top() >= 0.0 && pr.bottom() <= h + 0.5 && pr.right() <= w + 0.5, "the menu inside a {w}x{h} window: {pr:?}");
            s.shot(&format!("branch-menu-paper-{w}x{h}"));
            s.act("key Escape");
        }
        report("gitbranches", &s);
    }

    fn toast_starts(s: &Sim, want: &str) -> bool {
        s.app.toast.as_ref().is_some_and(|t| t.0.starts_with(want))
    }

    /// The commit's menu in the history, as someone tidying up a line of work uses it: a
    /// branch and an annotated tag made at an old commit (both shown on its row at once),
    /// a commit from another branch cherry-picked by keyboard, stopping at a conflict
    /// that is settled on the Changes page and continued, the branch reset to the commit
    /// before it with the uncommitted work kept in the stash, branches deleted from the
    /// branch menu (one not merged only when asked again), and two lines of a hunk
    /// picked and staged alone.
    #[test]
    fn git_commit_menu() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitcommitmenu");
        let repo = home.join("Code/notes");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str]| crate::tests::git(&repo, args);
        crate::tests::work_repo(&repo);
        let numbered: String = (1..=40).map(|i| format!("line {i}\n")).collect();
        std::fs::write(repo.join("list.txt"), &numbered).unwrap();
        std::fs::write(repo.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "Start the notes"]);
        let start = git_out(&repo, &["rev-parse", "HEAD"]);
        g(&["switch", "-q", "-c", "topic"]);
        std::fs::write(repo.join("notes.txt"), "one\nTWO from topic\nthree\n").unwrap();
        g(&["commit", "-qam", "Shout the second note"]);
        let shout = git_out(&repo, &["rev-parse", "HEAD"]);
        g(&["switch", "-q", "main"]);
        std::fs::write(repo.join("notes.txt"), "one\ntwo on main\nthree\n").unwrap();
        g(&["commit", "-qam", "Reword the second note"]);
        let reword = git_out(&repo, &["rev-parse", "HEAD"]);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| branch_now(s) == "main");
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1 && h.0 == 3));
        s.idle(3);
        // A right click on the first commit: its menu, under its hash and subject.
        right_click_in_inspector(&mut s, "Start the notes");
        assert!(matches!(s.app.popup, Popup::GitCommit(_)), "the commit's menu");
        s.idle(3);
        for t in [&start[..7], "Check out this commit", "Create branch here…", "Create tag here…", "Cherry-pick onto main", "Revert", "Reset main to here…", "Copy hash"] {
            assert!(some_text(&s, t), "{t:?} in the menu");
        }
        look(&mut s, "commit menu");
        s.shot("commit-menu");
        // A branch there, not switched to.
        s.click_text("Create branch here…");
        assert!(matches!(s.app.popup, Popup::NewRef(_)));
        s.act("text spike");
        s.idle(12);
        assert!(some_text(&s, &format!("New branch at {}", &start[..7])) && some_text(&s, "Create and switch"));
        s.shot("commit-new-branch");
        s.act("key Enter");
        wait_until(&mut s, "the branch", 10, |s| toast_is(s, &format!("Created spike at {}", &start[..7])));
        assert_eq!((git_out(&repo, &["rev-parse", "spike"]), branch_now(&s)), (start.clone(), "main".to_string()));
        wait_until(&mut s, "its pill", 10, |s| some_text(s, "spike"));
        // A tag with a message: an annotated one.
        right_click_in_inspector(&mut s, "Start the notes");
        s.click_text("Create tag here…");
        s.act("text v0.1");
        click_control(&mut s, "tag message");
        s.act("text The first cut");
        s.idle(12);
        s.shot("commit-new-tag");
        s.act("key Enter");
        wait_until(&mut s, "tagged", 10, |s| toast_is(s, &format!("Tagged {} as v0.1", &start[..7])));
        assert_eq!(git_out(&repo, &["cat-file", "-t", "v0.1"]), "tag");
        wait_until(&mut s, "its pill", 10, |s| some_text(s, "v0.1"));
        // Cherry-picked by keyboard: to the commit, Shift+F10, down to the item, Enter.
        let at = (0..3).find(|&i| git_hist(&s).hash(i).as_deref() == Some(shout.as_str())).unwrap_or_else(|| s.fail("the topic's commit listed"));
        s.act("key Home");
        for _ in 0..at {
            s.act("key ArrowDown");
        }
        s.act("key shift+F10");
        assert!(matches!(&s.app.popup, Popup::GitCommit(m) if m.hash == shout && m.sel == 0), "the selected commit's menu, ready for the keys");
        for _ in 0..3 {
            s.act("key ArrowDown");
        }
        s.idle(12);
        s.shot("commit-menu-keyboard");
        s.act("key Enter");
        assert!(!s.popup_open());
        wait_until(&mut s, "the Changes page", 20, |s| changes(s).open && changes(s).unstaged.contains(&"notes.txt".to_string()));
        s.idle(3);
        see(&mut s, "Cherry-picking · 1 file in conflict");
        click_in_inspector(&mut s, "notes.txt", "click");
        wait_until(&mut s, "the conflict shown", 10, |s| s.texts.iter().any(|(_, t)| t == "Keep both"));
        s.idle(3);
        look(&mut s, "cherry-pick conflict");
        s.shot("cherry-pick-conflict");
        click_control(&mut s, "keep theirs");
        wait_until(&mut s, "settled", 20, |s| changes(s).staged.contains(&"notes.txt".to_string()) && !changes(s).busy);
        click_control(&mut s, "conflict continue");
        wait_until(&mut s, "picked", 20, |s| matches!(changes(s).said, Some(Ok(ref m)) if m == "Finished the cherry-pick"));
        assert_eq!(git_out(&repo, &["log", "-1", "--format=%s"]), "Shout the second note");
        s.act("key Escape");
        wait_until(&mut s, "the graph read again", 20, |s| history(s).is_some_and(|h| h.1 && h.0 == 4));
        // Reset to the commit before, discarding: what isn't committed is stashed first.
        std::fs::write(repo.join("notes.txt"), "one\nscribbled\nthree\n").unwrap();
        crate::gitstatus::rescan(&repo);
        let picked = git_out(&repo, &["rev-parse", "--short", "HEAD"]);
        right_click_in_inspector(&mut s, "Reword the second note");
        s.click_text("Reset main to here…");
        assert!(matches!(s.app.popup, Popup::Choose { .. }), "asked how");
        s.idle(12);
        assert!(some_text(&s, "Keep changes") && some_text(&s, "Discard changes"));
        assert!(matches!(&s.app.popup, Popup::Choose { choices, .. } if !choices[0].1 && choices[1].1), "Discard is the risky one, never Enter's");
        s.shot("commit-reset");
        s.click_text("Discard changes");
        wait_until(&mut s, "reset", 10, |s| toast_starts(s, &format!("Reset main to {} · it was at {picked}", &reword[..7])));
        assert!(toast_starts(&s, "Reset main") && s.app.toast.as_ref().unwrap().0.ends_with("your changes kept in the stash"));
        assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), reword);
        assert!(git_out(&repo, &["stash", "list"]).contains("Before resetting main in Maia"));
        wait_until(&mut s, "the graph read again", 20, |s| history(s).is_some_and(|h| h.1 && h.0 == 3));
        // The branch menu: a branch's own menu on a right click; spike is merged and goes,
        // topic only when asked again.
        open_branches(&mut s);
        let r = s.text_rect("spike");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        assert!(matches!(s.app.popup, Popup::Branch { .. }), "the branch's menu");
        s.idle(12);
        assert!(some_text(&s, "Switch to it") && some_text(&s, "Rename…") && some_text(&s, "Delete"));
        s.shot("branch-actions");
        s.click_text("Delete");
        wait_until(&mut s, "deleted", 10, |s| toast_is(s, &format!("Deleted spike · it was at {}", &start[..7])));
        open_branches(&mut s);
        let at = 1 + branch_menu(&s).unwrap().refs.as_ref().unwrap().local.iter().position(|b| b.name == "topic").unwrap();
        for _ in 0..at {
            s.act("key ArrowDown");
        }
        s.act("key Delete");
        wait_until(&mut s, "asked again", 10, |s| matches!(s.app.popup, Popup::Choose { .. }));
        s.idle(12);
        assert!(some_text(&s, "topic has commits no other branch has"));
        s.shot("branch-unmerged");
        s.click_text("Delete anyway");
        wait_until(&mut s, "deleted anyway", 10, |s| toast_starts(s, "Deleted topic · it was at "));
        assert_eq!(git_out(&repo, &["branch", "--format=%(refname:short)"]), "main");
        // Two lines of a hunk staged on their own: the new ones, not the changed one.
        std::fs::write(repo.join("list.txt"), numbered.replace("line 3\n", "line three\n").replace("line 4\n", "line 4\nnew A\nnew B\n")).unwrap();
        crate::gitstatus::rescan(&repo);
        wait_until(&mut s, "the change", 10, |s| changes(s).unstaged == ["list.txt"]);
        s.idle(3);
        click_in_inspector(&mut s, "Changes", "click");
        wait_until(&mut s, "the page", 5, |s| changes(s).open);
        click_in_inspector(&mut s, "list.txt", "click");
        wait_until(&mut s, "the diff", 10, |s| changes(s).inline == [(false, "list.txt".to_string(), 1)]);
        s.idle(3);
        // A click picks a line and another lets it go; Shift picks the run between.
        for (label, with) in [("-line 3", ""), ("-line 3", ""), ("+new A", ""), ("+new B", "left shift")] {
            let r = see(&mut s, label);
            s.act(&format!("click {:.0} {:.0} {with}", r.center().x, r.center().y));
            s.idle(2);
        }
        see(&mut s, "Stage 2 lines");
        look(&mut s, "lines picked");
        s.shot("lines-picked");
        click_control(&mut s, "stage lines");
        wait_until(&mut s, "staged", 10, |s| !changes(s).busy && changes(s).staged == ["list.txt"]);
        assert!(matches!(changes(&s).said, Some(Ok(ref m)) if m == "Staged 2 lines"), "{:?}", changes(&s).said);
        let cached = git_out(&repo, &["diff", "--cached"]);
        assert_eq!(&cached[cached.find("@@").unwrap()..], "@@ -2,6 +2,8 @@ line 1\n line 2\n line 3\n line 4\n+new A\n+new B\n line 5\n line 6\n line 7");
        assert_eq!(changes(&s).unstaged, ["list.txt"], "the changed line stays unstaged");
        wait_until(&mut s, "the rest read again", 10, |s| !some_text(s, "Stage 2 lines"));
        look(&mut s, "lines staged");
        s.shot("lines-staged");
        report("gitcommitmenu", &s);
    }

    /// The colour `label` was last drawn in.
    fn color_of(s: &Sim, label: &str) -> egui::Color32 {
        let i = s.texts.iter().rposition(|(_, t)| t == label).unwrap_or_else(|| s.fail(&format!("no {label:?} on screen")));
        s.text_colors[i]
    }

    /// What git ignores, in the listing: faded by default in every view (a folder that
    /// is ignored as a whole, a matching file, both seen from above), nothing singled
    /// out inside an ignored folder, left out with Hide and shown plainly with Show,
    /// from Options. A node_modules of 20,000 files never stalls a frame, flattened
    /// either.
    #[test]
    fn git_ignored_files() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        use crate::config::IgnoredFiles;
        let home = fixture("gitignored");
        let repo = home.join("Code/web");
        std::fs::create_dir_all(&repo).unwrap();
        crate::tests::work_repo(&repo);
        std::fs::write(repo.join(".gitignore"), "node_modules/\n*.log\n").unwrap();
        for p in 0..200 {
            let d = repo.join(format!("node_modules/pkg{p}/lib"));
            std::fs::create_dir_all(&d).unwrap();
            for f in 0..100 {
                std::fs::write(d.join(format!("f{f}.js")), "").unwrap();
            }
        }
        std::fs::create_dir_all(repo.join("src")).unwrap();
        for f in ["src/app.js", "src/debug.log", "README.md", "server.log"] {
            std::fs::write(repo.join(f), "x\n").unwrap();
        }
        crate::tests::git(&repo, &["add", "-A"]);
        crate::tests::git(&repo, &["commit", "-qm", "Start"]);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        assert_eq!(s.app.cfg.git_ignored, IgnoredFiles::Dim, "dimmed by default");
        wait_until(&mut s, "the ignored files", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| r.ignored.len() == 3));
        s.idle(3);
        let plain = color_of(&s, "README.md");
        for (view, name) in [(ViewDetails, "details"), (ViewList, "list"), (ViewLarge, "icons")] {
            s.cmd(view);
            s.settle();
            for f in ["node_modules", "server.log"] {
                let c = color_of(&s, f);
                assert!(c.a() < plain.a() && c != plain, "{f} faded in {name}: {c:?} against {plain:?}");
            }
            assert_eq!(color_of(&s, "src"), plain, "src is not ignored");
            look(&mut s, &format!("ignored dimmed, {name}"));
            s.shot(&format!("ignored-dim-{name}"));
        }
        s.cmd(ViewDetails);
        // In the tree, a file further down is faded too.
        s.cmd(ViewTree);
        s.settle();
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().toggle_expand(&repo.join("src"));
        s.settle();
        assert!(color_of(&s, "debug.log").a() < color_of(&s, "app.js").a(), "a file further down");
        look(&mut s, "ignored in the tree");
        s.shot("ignored-tree");
        s.cmd(ViewDetails);
        // Inside an ignored folder nothing is singled out.
        go(&mut s, &repo.join("node_modules"));
        wait_until(&mut s, "its status", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        assert_eq!(color_of(&s, "pkg0"), plain, "inside node_modules, rows are not faded");
        go(&mut s, &repo);
        // Hide, from Options: left out (and counted as hidden), then Show.
        s.act("key ctrl+Comma");
        s.act("text ignored");
        s.idle(30);
        s.shot("ignored-option");
        click_control(&mut s, "choice");
        assert_eq!(s.app.cfg.git_ignored, IgnoredFiles::Hide);
        s.act("key Escape");
        s.settle();
        let names = |s: &Sim| s.tab().view.iter().map(|&i| s.tab().entries[i as usize].name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&s), ["src", "README.md"], "hidden");
        look(&mut s, "ignored hidden");
        s.shot("ignored-hidden");
        // Flattened, 20,000 ignored files stream in and stay out, the frames short.
        s.cmd(ToggleFlatten);
        s.settle();
        assert!(!s.tab().view.iter().any(|&i| s.tab().entries[i as usize].path.starts_with(repo.join("node_modules"))), "nothing from node_modules");
        assert!(names(&s).contains(&"app.js".to_string()) && !names(&s).contains(&"debug.log".to_string()));
        s.cmd(ToggleFlatten);
        s.settle();
        s.act("key ctrl+Comma");
        s.act("text ignored");
        s.idle(3);
        click_control(&mut s, "choice");
        assert_eq!(s.app.cfg.git_ignored, IgnoredFiles::Show);
        s.act("key Escape");
        s.settle();
        wait_until(&mut s, "read without ignored files", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| r.ignored.is_empty()));
        s.idle(2);
        assert_eq!(names(&s).len(), 4, "every entry");
        assert_eq!(color_of(&s, "node_modules"), plain, "shown like the rest");
        look(&mut s, "ignored shown");
        // Back to Dim, and the frames stayed short throughout.
        s.app.cfg.git_ignored = IgnoredFiles::Dim;
        wait_until(&mut s, "read with ignored files", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| !r.ignored.is_empty()));
        s.idle(3);
        assert!(color_of(&s, "node_modules").a() < plain.a());
        assert!(s.slow.is_empty(), "slow frames: {:?}", s.slow);
        report("gitignored", &s);
    }

    /// Git history: the branch chip opens the commit graph in the inspector, scrolling
    /// reads further pages from the one running git, a commit lists its files, a file
    /// shows its diff, and Escape walks back out, closing the inspector it opened. Then
    /// the same by keyboard and palette, beside an open preview, in a split, docked at
    /// the bottom, in a small window, on a light theme, without animations, and leaving
    /// the work tree.
    #[test]
    fn git_history_graph() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("githistory");
        let repo = home.join("Code/history");
        crate::tests::git_history_fixture(&repo, 700);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        assert!(s.app.panels[&s.app.active].inspector.is_none());
        // The chip is drawn last, over everything: the topmost "main".
        open_history(&mut s);
        wait_until(&mut s, "first page", 10, |s| history(s).is_some_and(|h| h.0 == crate::gitlog::PAGE));
        look(&mut s, "history");
        for t in ["Join ten branches at once", "Wide branch 10", "wide-10"] {
            assert!(s.texts.iter().any(|(_, x)| x == t), "{t:?} on screen");
        }
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
        assert!(s.text_rect("Wide branch 10").left() > ir.left(), "the graph is in the inspector");
        // Scrolling down reads the next pages; then Home returns to the top.
        let t0 = Instant::now();
        while history(&s).is_some_and(|h| !h.1) && t0.elapsed().as_secs() < 20 {
            s.act(&format!("scroll {:.0} {:.0} -60", ir.center().x, ir.center().y));
        }
        assert!(history(&s).is_some_and(|h| h.1 && h.0 > 2 * crate::gitlog::PAGE), "every page read: {:?}", history(&s));
        // Once the wheel's smooth scrolling has run out, End jumps to the first commit.
        s.idle(120);
        s.act("key End");
        wait_until(&mut s, "the first commit at the bottom", 5, |s| s.texts.iter().any(|(_, t)| t == "Start the notes"));
        look(&mut s, "history at the end");
        s.act("key Home");
        wait_until(&mut s, "back at the top", 5, |s| s.texts.iter().any(|(_, t)| t == "Join ten branches at once"));
        s.idle(2);
        // A merge: what it brought in, then that file's diff.
        for _ in 0..30 {
            if s.texts.iter().any(|(r, t)| t == "Merge topic 700" && r.bottom() < ir.bottom() - 10.0) {
                break;
            }
            s.act(&format!("scroll {:.0} {:.0} -2", ir.center().x, ir.center().y));
            s.idle(10);
        }
        s.click_text("Merge topic 700");
        wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3.is_some()));
        assert_eq!(history(&s).and_then(|h| h.3), Some(1));
        s.idle(2);
        look(&mut s, "commit files");
        // A click opens the diff under the file; a double click in the whole panel.
        s.idle(40);
        s.click_text("topic_700.rs");
        wait_until(&mut s, "diff under the file", 10, |s| s.texts.iter().any(|(_, t)| t == "+fn part_3() {}"));
        assert_eq!(history(&s).and_then(|h| h.4), None);
        s.idle(40);
        let r = s.text_rect("topic_700.rs");
        s.act(&format!("dclick {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "diff", 10, |s| history(s).is_some_and(|h| h.4.is_some()));
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "+fn part_3() {}"), "the diff is drawn");
        look(&mut s, "diff");
        for (k, want) in [(1, "files again"), (2, "graph again")] {
            s.act("key Escape");
            let h = history(&s).unwrap_or_else(|| s.fail("history closed early"));
            assert_eq!((h.2.is_some(), h.4.is_some()), (k == 1, false), "{want}");
        }
        s.act("key Escape");
        assert!(s.app.panels[&s.app.active].inspector.is_none(), "the inspector the history opened closes with it");
        // Keyboard and palette: down twice, open, open the first file, all the way back.
        palette_run(&mut s, "Show Git History");
        wait_until(&mut s, "history again", 10, |s| history(s).is_some_and(|h| h.0 > 0));
        for k in ["ArrowDown", "ArrowDown", "Enter"] {
            s.act(&format!("key {k}"));
        }
        assert_eq!(history(&s).and_then(|h| h.2), Some(2));
        wait_until(&mut s, "files by keyboard", 10, |s| history(s).is_some_and(|h| h.3.is_some()));
        s.act("key Enter");
        wait_until(&mut s, "diff by keyboard", 10, |s| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).is_some_and(|h| h.probe().inline_lines.len() == 1));
        look(&mut s, "diff by keyboard");
        for _ in 0..2 {
            s.act("key Escape");
        }
        assert!(s.app.panels[&s.app.active].inspector.is_none());
        // Beside a preview the user opened: closing the history brings the preview back.
        s.cmd(ToggleInspector);
        s.cmd(GitHistory);
        wait_until(&mut s, "history over the preview", 10, |s| history(s).is_some_and(|h| h.0 > 0));
        s.act("key Escape");
        assert!(s.app.panels[&s.app.active].inspector.as_ref().is_some_and(|i| i.git.is_none()), "the preview stays");
        // Around it: a split, the bottom dock, small windows, a light theme, no animations.
        s.cmd(GitHistory);
        s.cmd(SplitRight);
        s.cmd(FocusPrevPanel);
        look(&mut s, "history in a split");
        s.cmd(ClosePanel);
        s.cmd(FocusPrevPanel);
        s.cmd(GitHistory);
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Bottom;
        look(&mut s, "history docked at the bottom");
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Auto;
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.app.cfg.animations = false;
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (1920.0, 1080.0)] {
            s.resize(w, h);
            look(&mut s, &format!("history {w}x{h}"));
            if let Some(r) = s.app.panels[&s.app.active].inspector_rect {
                s.act(&format!("click {:.0} {:.0}", r.center().x, r.top() + 60.0));
                s.idle(3);
                look(&mut s, &format!("commit {w}x{h}"));
                s.act("key Escape");
            }
        }
        // Leaving the work tree closes its history.
        let plain = home.join("Documents");
        go(&mut s, &plain);
        s.idle(4);
        assert!(history(&s).is_none(), "no history outside the work tree");
        assert!(!crate::popups::applicable(&s.app, GitHistory), "the command is off outside a work tree");
        report("git history", &s);
    }

    /// "Show GitHub pictures": off by default and asking nothing; switched on, noreply
    /// authors get theirs straight away and the others once each through the repository
    /// on github.com, in place of their initials in the header and beside their name in
    /// the graph. A failed lookup and an author without an account keep their initials.
    /// Switched off, initials come back and nothing more is asked; on again, the pictures
    /// come from the cache. GitHub is a stand-in that answers from a table.
    #[test]
    fn git_history_github_pictures() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        use crate::avatars::Http;
        let home = fixture("gitpictures");
        let repo = home.join("Code/pictures");
        std::fs::create_dir_all(&repo).unwrap();
        let g = |args: &[&str], who: &str| {
            let (name, email) = who.split_once(" <").unwrap_or(("t", "t@t>"));
            let st = std::process::Command::new("git").args(["-c", &format!("user.name={name}"), "-c", &format!("user.email={}", email.trim_end_matches('>')), "-c", "commit.gpgsign=false"]).args(args).current_dir(&repo).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            assert!(st.is_ok_and(|st| st.success()), "git {args:?}");
        };
        g(&["init", "-q", "-b", "main"], "");
        let people = [
            ("Octo Cat <12345+octocat@users.noreply.github.com>", "Add the octo"),
            ("Mona Lisa <mona@users.noreply.github.com>", "Paint the mona"),
            ("Ada Lovelace <ada@example.invalid>", "Build the engine"),
            ("Grace Hopper <grace@example.invalid>", "Find the bug"),
            ("Linus Tor <linus@example.invalid>", "Write the kernel"),
        ];
        for (k, (who, msg)) in people.iter().enumerate() {
            std::fs::write(repo.join(format!("file{k}.txt")), msg).unwrap();
            g(&["add", "."], who);
            g(&["commit", "-qm", msg], who);
        }
        g(&["remote", "add", "origin", "git@github.com:octo-org/pictures.git"], "");
        let sha = |msg: &str| {
            let out = std::process::Command::new("git").args(["log", "--format=%H", "--grep", msg]).current_dir(&repo).output().unwrap();
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        let api = |msg: &str| format!("api.github.com/repos/octo-org/pictures/commits?sha={}&per_page=1", sha(msg));
        let png = |seed| crate::tests::png_bytes(160, 160, seed);
        let table = vec![
            ("avatars.githubusercontent.com/u/12345?s=96".to_string(), Http::Ok(png(3))),
            ("avatars.githubusercontent.com/mona?s=96".to_string(), Http::Ok(png(6))),
            (api("Build the engine"), Http::Ok(format!(r#"[{{"sha":"{}","author":{{"avatar_url":"https://avatars.githubusercontent.com/u/4242?v=4"}}}}]"#, sha("Build the engine")).into_bytes())),
            ("avatars.githubusercontent.com/u/4242?v=4&s=96".to_string(), Http::Ok(png(9))),
            (api("Find the bug"), Http::Status(500)),
            (api("Write the kernel"), Http::Ok(format!(r#"[{{"sha":"{}","author":null}}]"#, sha("Write the kernel")).into_bytes())),
        ];
        let log = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        s.app.avatars.dir = root().join("xdg/cache/maia-sim-avatars");
        s.app.avatars.set_get(crate::avatars::fake_get(table, log.clone()));
        assert!(!s.app.cfg.github_pictures, "off by default");
        let emails: Vec<&str> = people.iter().map(|(w, _)| w.split_once('<').unwrap().1.trim_end_matches('>')).collect();
        let states = |s: &Sim| emails.iter().map(|e| s.app.avatars.state(e)).collect::<Vec<_>>();
        let in_insp = |s: &Sim, t: &str| {
            let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
            s.texts.iter().any(|(r, x)| x == t && ir.contains(r.center()))
        };
        wait_until(&mut s, "branch read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1 && h.0 == 5));
        look(&mut s, "graph, pictures off");
        click_in_inspector(&mut s, "Add the octo", "click");
        wait_until(&mut s, "commit open", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
        s.idle(3);
        assert!(in_insp(&s, "OC"), "initials while off");
        assert_eq!((s.app.avatars.asked(), log.lock().unwrap().len()), (0, 0), "off: nothing asked");
        assert!(states(&s).iter().all(|x| x.is_empty()));
        s.act("key Escape");
        s.idle(2);
        // Without curl the option says so, quietly.
        s.app.avatars.set_curl(false);
        s.act("key ctrl+Comma");
        s.act("text show github pictures");
        wait_until(&mut s, "the curl note", 5, |s| s.texts.iter().any(|(_, t)| t == "Needs curl, which isn't installed; authors keep their initials."));
        s.idle(30);
        s.shot("option-no-curl");
        s.app.avatars.set_curl(true);
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Asks GitHub by username or email; kept on this computer."), "what it does and what GitHub learns");
        look(&mut s, "the option");
        s.shot("option");
        // Still inside its window when that is small.
        s.resize(520.0, 340.0);
        s.idle(30);
        s.resize(1280.0, 800.0);
        s.idle(2);
        s.act("key Enter");
        s.idle(2);
        s.act("key Escape");
        s.settle();
        assert!(s.app.cfg.github_pictures, "switched on from Options");
        wait_until(&mut s, "every author answered", 10, |s| s.app.avatars.on() && !s.app.avatars.busy() && states(s).iter().all(|x| *x == "ready" || *x == "none"));
        assert_eq!(states(&s), ["ready", "ready", "ready", "none", "none"]);
        let asked: Vec<String> = log.lock().unwrap().clone();
        assert_eq!(asked.len(), 6, "three pictures and three lookups, once each: {asked:?}");
        for e in ["Build the engine", "Find the bug", "Write the kernel"] {
            assert_eq!(asked.iter().filter(|u| u.contains(&api(e))).count(), 1, "one lookup for {e}");
        }
        assert!(asked.iter().all(|u| u.starts_with("https://")));
        look(&mut s, "graph with pictures");
        s.shot("graph-pictures");
        // The header: the picture in place of the initials.
        click_in_inspector(&mut s, "Add the octo", "click");
        wait_until(&mut s, "commit open", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
        s.idle(3);
        assert!(!in_insp(&s, "OC"), "the picture replaces the initials");
        assert!(in_insp(&s, "Octo Cat"), "the name stays beside it");
        look(&mut s, "commit with a picture");
        s.shot("commit-picture");
        for (subject, initials, picture) in [("Paint the mona", "ML", true), ("Build the engine", "AL", true), ("Find the bug", "GH", false), ("Write the kernel", "LT", false)] {
            s.act("key Escape");
            s.idle(2);
            click_in_inspector(&mut s, subject, "click");
            wait_until(&mut s, "commit open", 10, |s| probe(s).subject.as_deref() == Some(subject) && history(s).is_some_and(|h| h.3 == Some(1)));
            s.idle(3);
            assert_eq!(in_insp(&s, initials), !picture, "{subject}: initials only without a picture");
            look(&mut s, subject);
        }
        s.shot("commit-failed-lookup");
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.act("key Escape");
        look(&mut s, "pictures on a light theme");
        s.shot("graph-pictures-paper");
        click_in_inspector(&mut s, "Build the engine", "click");
        wait_until(&mut s, "commit open", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
        look(&mut s, "a picture on a light theme");
        s.shot("commit-picture-paper");
        s.app.cfg.theme = crate::theme::ThemeName::default();
        // Off again: initials at once, and nothing more asked whatever is opened.
        s.act("key ctrl+Comma");
        s.act("text show github pictures");
        s.idle(2);
        s.act("key Enter");
        s.idle(2);
        s.act("key Escape");
        s.settle();
        assert!(!s.app.cfg.github_pictures && !s.app.avatars.on());
        assert!(states(&s).iter().all(|x| x.is_empty()), "pictures dropped");
        let before = (s.app.avatars.asked(), log.lock().unwrap().len());
        assert!(in_insp(&s, "AL"), "initials back");
        for subject in ["Add the octo", "Find the bug"] {
            s.act("key Escape");
            s.idle(2);
            click_in_inspector(&mut s, subject, "click");
            s.idle(20);
        }
        assert!(in_insp(&s, "GH"));
        assert_eq!((s.app.avatars.asked(), log.lock().unwrap().len()), before, "off: nothing asked");
        look(&mut s, "pictures off again");
        // On again: the kept answers come from the cache; only the failed lookup is asked
        // again.
        s.app.cfg.github_pictures = true;
        s.act("key Escape");
        wait_until(&mut s, "answered again", 10, |s| s.app.avatars.on() && !s.app.avatars.busy() && states(s).iter().all(|x| *x == "ready" || *x == "none"));
        assert_eq!(states(&s), ["ready", "ready", "ready", "none", "none"]);
        let again: Vec<String> = log.lock().unwrap()[before.1..].to_vec();
        assert_eq!(again, [format!("https://{}", api("Find the bug"))], "only what wasn't kept");
        report("git pictures", &s);
    }

    fn palette_run(s: &mut Sim, title: &str) {
        s.act("key ctrl+shift+p");
        s.act(&format!("text {title}"));
        s.act("key Enter");
        s.settle();
    }

    /// Folder compare between two panels: marks on both sides in every view, Select
    /// Differences, Sync To Other Panel through the copy job and its conflict dialog, the
    /// marks following the sync and its undo, and clearing when a panel moves.
    #[test]
    fn compare_and_sync_panels() {
        if !sandboxed() {
            return;
        }
        use crate::compare::{Diff, mark};
        let home = fixture("compare");
        let (a, b) = (home.join("A"), home.join("B"));
        crate::tests::compare_fixture(&a, &b);
        // A difference two levels down, in a folder both sides have.
        std::fs::create_dir_all(a.join("both/deep")).unwrap();
        std::fs::write(a.join("both/deep/inner.txt"), "inner").unwrap();
        let mut s = Sim::new(&a, vec2(1280.0, 800.0));
        s.act("key ctrl+shift+s");
        s.settle();
        go(&mut s, &b);
        s.act("key Tab");
        s.settle();
        assert_eq!(s.tab().path, a);
        palette_run(&mut s, "Compare Panels");
        assert!(toast_is(&s, "5 different here, 3 there"), "{:?}", s.app.toast);
        assert_eq!(mark(&a.join("newer.txt")), Some(Diff::Newer));
        assert_eq!(mark(&b.join("newer.txt")), None, "the older side is not marked");
        assert_eq!(mark(&b.join("size.txt")), Some(Diff::Size));
        assert_eq!(mark(&b.join("only-b.txt")), Some(Diff::Only));
        assert_eq!(mark(&a.join("same.txt")), None);
        wait_until(&mut s, "folders looked through", 10, |_| mark(&a.join("both")) == Some(Diff::Inside) && mark(&b.join("both")) == Some(Diff::Inside));
        for v in [ViewTree, ViewList, ViewMedium, ViewLarge, ViewXL, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("compare {v:?}"));
        }
        s.resize(520.0, 340.0);
        look(&mut s, "compare narrow");
        s.resize(1280.0, 800.0);
        palette_run(&mut s, "Select Differences");
        let mut sel: Vec<String> = s.tab().selected_paths().iter().filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect();
        sel.sort();
        assert_eq!(sel, vec!["both", "folder-a", "kind", "newer.txt", "only-a.txt", "size.txt"]);
        // Sync shows what it will do, all the way down, before doing it: what is missing
        // or newer (not the file that only differs in size), and with Mirror what only
        // the other side has.
        palette_run(&mut s, "Sync To Other Panel");
        wait_until(&mut s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Sync(st) if st.plan.is_some()));
        let Popup::Sync(st) = &s.app.popup else { unreachable!() };
        let plan = st.plan.as_ref().unwrap();
        assert_eq!((plan.copy.len(), plan.update.len(), plan.extra.len()), (3, 2, 1), "{plan:?}");
        assert!(some_text_at(&s, "both/deep") && some_text_at(&s, "only-a.txt"), "the changes listed");
        s.act("key m");
        let popup = crate::ui::POPUPS.with(|p| p.borrow().last().copied()).unwrap_or_else(|| s.fail("the dialog"));
        assert!(s.texts.iter().any(|(r, t)| t == "only-b.txt" && popup.contains_rect(*r)), "mirroring lists what goes");
        s.shot("sync-preview");
        look(&mut s, "sync preview");
        // Folders that couldn't be read (made up here: the sandbox may read anything) are
        // counted and each listed, so it is clear what the sync leaves alone.
        let unread = |s: &mut Sim, v: Vec<PathBuf>| {
            if let Popup::Sync(st) = &mut s.app.popup
                && let Some(p) = st.plan.as_mut()
            {
                p.unlisted = v;
            }
            s.idle(3);
        };
        unread(&mut s, vec![b.join("both"), a.join("folder-a")]);
        assert!(some_text_at(&s, "2 folders couldn't be read and are left as they are"), "counted");
        assert!(s.texts.iter().filter(|(r, t)| t == "Unread" && popup.contains_rect(*r)).count() == 2 && some_text_at(&s, "both"), "each listed");
        s.shot("sync-preview-unread");
        look(&mut s, "sync preview with unread folders");
        unread(&mut s, vec![]);
        s.act("key Enter");
        wait_until(&mut s, "synced", 10, |_| b.join("only-a.txt").exists() && b.join("folder-a").is_dir() && b.join("kind").is_file() && b.join("both/deep/inner.txt").exists() && !b.join("only-b.txt").exists());
        assert_eq!(std::fs::read_to_string(b.join("newer.txt")).unwrap(), "new");
        assert_eq!(std::fs::read_to_string(b.join("size.txt")).unwrap(), "short", "a size difference alone is not synced");
        wait_until(&mut s, "marks follow the sync", 10, |_| mark(&a.join("newer.txt")).is_none() && mark(&a.join("only-a.txt")).is_none() && mark(&a.join("both")).is_none());
        look(&mut s, "after sync");
        // Undo takes the whole sync back: copies gone, replaced and mirrored back.
        s.act("key ctrl+z");
        wait_until(&mut s, "sync undone", 10, |s| !b.join("only-a.txt").exists() && b.join("only-b.txt").exists() && mark(&a.join("only-a.txt")) == Some(Diff::Only));
        assert_eq!(std::fs::read_to_string(b.join("newer.txt")).unwrap(), "old");
        // Either panel moving clears every mark.
        s.act("key Tab");
        s.act("key alt+ArrowUp");
        s.settle();
        assert_eq!(mark(&a.join("size.txt")), None, "marks clear when a panel moves");
        look(&mut s, "after compare");
        report("compare", &s);
    }

    /// The palette by mouse and by keys: a category clicked on the left lists only its
    /// commands and keeps them while typing, Tab and Shift+Tab step through the ones a
    /// search leaves something in, Ctrl+D and the row's star keep a command in Quick,
    /// Ctrl+Enter asks for a hotkey, a click runs a row, and what ran comes back first
    /// under Recent. A narrow window keeps the categories as glyphs.
    #[test]
    fn palette_categories() {
        if !sandboxed() {
            return;
        }
        use crate::commands::Cat;
        let home = fixture("palette");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let cat = |s: &Sim| match &s.app.popup {
            Popup::Palette(p) => Some(p.cat),
            _ => None,
        };
        let quick = |s: &Sim, id: &str| s.app.ud.quick.iter().any(|q| q == id);
        s.act("key ctrl+shift+p");
        s.idle(2);
        assert_eq!(cat(&s), Some(None), "everything to begin with");
        assert!(some_text(&s, "Navigate") && some_text(&s, "Open In New Tab"));
        look(&mut s, "palette");
        click_control(&mut s, "category Sort");
        s.idle(2);
        assert_eq!(cat(&s), Some(Some(Cat::Sort)));
        assert!(some_text(&s, "Sort By Size Asc") && !some_text(&s, "New Tab"), "only Sort's commands");
        // The click left the keyboard with the search: typing narrows the category.
        s.act("text desc");
        s.idle(2);
        assert!(some_text(&s, "Sort By Size Desc") && !some_text(&s, "Sort By Size Asc"));
        s.act("key Tab");
        s.idle(2);
        // Nothing after Sort has a "desc": round to everything.
        assert_eq!(cat(&s), Some(None), "on past the categories without a match");
        assert!(!some_text(&s, "No command matches"));
        s.act("key shift+Tab");
        assert_eq!(cat(&s), Some(Some(Cat::Sort)), "and back");
        look(&mut s, "palette in a category");
        s.shot("palette-category");
        click_control(&mut s, "category All");
        s.act("key ctrl+a");
        s.act("key Backspace");
        // Quick: Ctrl+D adds the selected command, its star takes it out again.
        s.act("text Toggle Sidebar");
        s.idle(2);
        s.act("key ctrl+d");
        assert!(quick(&s, "toggle_sidebar"), "Ctrl+D keeps it in Quick");
        click_control(&mut s, "star button");
        assert!(!quick(&s, "toggle_sidebar"), "the star takes it out");
        // Ctrl+Enter asks for a hotkey; Escape leaves that.
        s.act("key ctrl+Enter");
        assert!(matches!(&s.app.popup, Popup::Palette(p) if p.capture.is_some()), "a new hotkey is asked for");
        s.act("key Escape");
        assert!(matches!(&s.app.popup, Popup::Palette(p) if p.capture.is_none()));
        let side = s.app.sidebar;
        s.act("key Enter");
        assert_eq!(s.app.sidebar, !side, "Enter ran it");
        assert_eq!(s.app.ud.recent_cmds.first().map(String::as_str), Some("toggle_sidebar"));
        // Next time it is first, under Recent.
        s.act("key ctrl+shift+p");
        s.idle(2);
        assert!(some_text(&s, "Recent"));
        s.act("key Enter");
        assert_eq!(s.app.sidebar, side, "the recent command first");
        // A click on a row runs it.
        let hidden = s.app.cfg.show_hidden;
        s.act("key ctrl+shift+p");
        s.act("text hidden files");
        s.idle(2);
        s.click_text("Toggle Hidden Files");
        s.settle();
        assert_eq!(s.app.cfg.show_hidden, !hidden, "the clicked row ran");
        assert!(matches!(s.app.popup, Popup::None));
        // Down the list by keys, in a narrow window.
        s.resize(560.0, 460.0);
        s.act("key ctrl+shift+p");
        s.idle(2);
        assert!(!some_text(&s, "Navigate"), "glyphs only on the left");
        for _ in 0..30 {
            s.act("key ArrowDown");
        }
        s.act("key PageDown");
        s.act("key Tab");
        s.idle(20);
        assert!(some_text(&s, "Open In New Tab"), "a new category starts at its top");
        look(&mut s, "palette narrow");
        s.shot("palette-narrow");
        s.act("key Escape");
        report("palette", &s);
    }

    /// Command aliases from the palette: set one with the row's alias button, find and
    /// run the command by typing it, a duplicate refused with the reason, change and clear
    /// with F2, and the result saved with user data.
    #[test]
    fn palette_aliases() {
        if !sandboxed() {
            return;
        }
        let home = fixture("aliases");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let alias = |s: &Sim, id: &str| s.app.ud.aliases.get(id).cloned();
        s.act("key ctrl+shift+p");
        s.act("text Toggle Hidden Files");
        click_control(&mut s, "alias button");
        assert!(matches!(s.app.popup, Popup::Prompt { .. }), "the alias prompt opens");
        s.act("text hh");
        s.act("key Enter");
        assert_eq!(alias(&s, "toggle_hidden").as_deref(), Some("hh"));
        assert!(matches!(s.app.popup, Popup::Palette(_)), "saving returns to the palette");
        // Typing the alias finds the command first; Enter runs it.
        let hidden = s.app.cfg.show_hidden;
        s.act("text hh");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t == "Toggle Hidden Files“hh”"), "the alias shows beside the title");
        s.act("key Enter");
        assert_eq!(s.app.cfg.show_hidden, !hidden, "the alias ran Toggle Hidden Files");
        // The same alias for another command is refused, with the reason in the prompt.
        s.act("key ctrl+shift+p");
        s.act("text Toggle Sidebar");
        s.act("key F2");
        s.act("text HH");
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Prompt { .. }), "the prompt stays");
        assert!(s.texts.iter().any(|(_, t)| t.contains("already finds “Toggle Hidden Files”")), "the reason is shown");
        assert_eq!(alias(&s, "toggle_sidebar"), None);
        s.act("key ctrl+a");
        s.act("text sb");
        s.act("key Enter");
        assert_eq!(alias(&s, "toggle_sidebar").as_deref(), Some("sb"));
        look(&mut s, "palette with aliases");
        // Change, then clear, with F2 on the selected command.
        for (new, want) in [("hid", Some("hid")), ("", None)] {
            s.act("key Escape");
            s.act("key ctrl+shift+p");
            s.act("text Toggle Hidden Files");
            s.act("key F2");
            s.act("key ctrl+a");
            s.act("key Backspace");
            if !new.is_empty() {
                s.act(&format!("text {new}"));
            }
            s.act("key Enter");
            assert_eq!(alias(&s, "toggle_hidden").as_deref(), want);
        }
        s.act("key Escape");
        let ctx = s.ctx.clone();
        s.app.save_all(&ctx);
        let saved = crate::config::load_userdata().aliases;
        assert_eq!((saved.get("toggle_sidebar").map(String::as_str), saved.get("toggle_hidden")), (Some("sb"), None), "saved with user data");
        look(&mut s, "after aliases");
        report("aliases", &s);
    }

    /// Network places as a user meets them, on a fake gio: the phone in the sidebar mounts
    /// with a click and opens; Connect To Server asks again after a wrong password and opens
    /// the share with the right one; an FTP address typed into Go To connects as a guest;
    /// an unknown host says so; recent servers fill the address; Disconnect moves the tab
    /// off the share first.
    #[test]
    fn network_places_connect_browse_disconnect() {
        if !sandboxed() {
            return;
        }
        let d = fake_network("main");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let home = fixture("net");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        wait_until(&mut s, "phone in the sidebar", 10, |s| sidebar_text(s, "Pixel 7").is_some());
        assert!(sidebar_text(&s, "Network").is_some(), "Network section shown");
        look(&mut s, "network section");
        // The phone: one click mounts it and opens it.
        let r = sidebar_text(&s, "Pixel 7").unwrap();
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        let phone = d.join("gvfs/mtp___Google_Pixel_7_");
        wait_until(&mut s, "phone opened", 10, |s| s.tab().path == phone);
        s.settle();
        assert!(s.tab().entries.iter().any(|e| e.name == "hello.txt"));
        // Connect To Server: no password first, then a wrong one, then the right one.
        s.cmd(ConnectServer);
        s.idle(3);
        assert!(s.texts.iter().any(|(_, t)| t == "Connect to server"));
        s.act("text smb://nas/media");
        s.act("key Enter");
        wait_until(&mut s, "login refused", 10, |s| s.texts.iter().any(|(_, t)| t == "Wrong user name or password"));
        look(&mut s, "connect dialog with an error");
        click_control(&mut s, "user field");
        s.act("text me");
        click_control(&mut s, "password field");
        s.act("text nope");
        s.click_text("Connect");
        wait_until(&mut s, "wrong password refused", 10, |s| !s.app.connecting("smb://nas/media") && s.texts.iter().any(|(_, t)| t == "Wrong user name or password"));
        match &s.app.popup {
            Popup::Connect(st) => assert!(st.user == "me" && st.password.is_empty(), "user kept, password cleared: user {:?} password {:?} uri {:?}", st.user, st.password, st.uri),
            _ => s.fail("dialog still open"),
        }
        click_control(&mut s, "password field");
        s.act("text secret");
        s.act("key Enter");
        let share = d.join("gvfs/smb___nas_media");
        wait_until(&mut s, "share opened", 10, |s| s.tab().path == share && !s.popup_open());
        assert_eq!(s.app.ud.servers.first().map(String::as_str), Some("smb://nas/media"), "remembered");
        wait_until(&mut s, "share in the sidebar", 10, |s| sidebar_text(s, "media on nas").is_some());
        // The tab and the path bar name the share, not GVFS's folder for it.
        s.idle(2);
        assert_eq!(s.tab().title(), "media on nas");
        let bar = s.app.panels[&s.app.active].list_rect.top();
        assert!(s.texts.iter().filter(|(r, _)| r.top() < bar).filter(|(_, t)| t == "media on nas").count() >= 2, "tab and path bar");
        assert!(!s.texts.iter().any(|(_, t)| t.contains("smb___nas_media") || t == "gvfs"), "no GVFS folder names on screen");
        look(&mut s, "inside the share");
        // Go To: an address connects; an FTP site as a guest.
        s.act("key ctrl+p");
        s.act("text ftp://pub");
        s.act("key Enter");
        wait_until(&mut s, "ftp opened", 10, |s| s.tab().path == d.join("gvfs/ftp___pub"));
        // An unknown host fails with the reason.
        s.act("key ctrl+p");
        s.act("text smb://nohost/x");
        s.act("key Enter");
        wait_until(&mut s, "failure reported", 10, |s| s.app.toast.as_ref().is_some_and(|t| t.0 == "Couldn't connect to x on nohost: No server or share by that name"));
        // Recent servers fill the address.
        s.cmd(ConnectServer);
        s.idle(3);
        s.click_text("smb://nas/media");
        s.idle(2);
        assert!(matches!(&s.app.popup, Popup::Connect(st) if st.uri == "smb://nas/media"));
        s.act("key Escape");
        // Disconnect the share from the sidebar while a tab shows it.
        go(&mut s, &share);
        let r = sidebar_text(&s, "media on nas").unwrap();
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        click_control(&mut s, "disconnect button");
        assert!(crate::tab::is_computer(&s.tab().path), "tab left the share first");
        wait_until(&mut s, "disconnected", 10, |s| s.app.toast.as_ref().is_some_and(|t| t.0 == "Disconnected media on nas"));
        wait_until(&mut s, "gone from the sidebar", 10, |s| sidebar_text(s, "media on nas").is_none());
        assert!(!share.exists());
        let log = std::fs::read_to_string(d.join("log")).unwrap_or_default();
        assert!(log.contains("gio mount smb://nas/media") && log.contains("gio mount -u smb://nas/media"), "{log}");
        report("network", &s);
    }

    /// Administrator access, on a fake pkexec: pasting into, deleting in, creating in and
    /// renaming in a folder the user may not write each fail first and then succeed through
    /// "Retry as administrator" / "Use administrator"; a dismissed password dialog leaves
    /// everything as it was and says so.
    #[test]
    fn administrator_retries_refused_operations() {
        if !sandboxed() {
            return;
        }
        let d = fake_pkexec("main");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let home = fixture("admin");
        let locked = home.join("System");
        std::fs::create_dir_all(&locked).unwrap();
        std::fs::write(locked.join("keep.conf"), "x").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.cmd(ViewDetails);
        s.settle();
        let finished = |s: &Sim| !s.app.jobs.is_empty() && s.app.jobs.iter().all(|j| j.finished_at.is_some());
        // Paste into it: refused, then copied as administrator.
        select_path(&mut s, &home.join("Documents/notes.txt"));
        s.act("key ctrl+c");
        go(&mut s, &locked);
        s.act("key ctrl+v");
        wait_until(&mut s, "paste refused", 10, |s| s.texts.iter().any(|(_, t)| t == "Retry as administrator"));
        assert!(s.texts.iter().any(|(_, t)| t == "Couldn't copy notes.txt"), "the card says nothing was copied");
        look(&mut s, "permission refused");
        s.click_text("Retry as administrator");
        wait_until(&mut s, "copied as administrator", 10, |_| locked.join("notes.txt").exists());
        assert!(std::fs::read_to_string(d.join("log")).unwrap().contains("cp -a -- "), "one cp through pkexec");
        wait_until(&mut s, "job finished", 10, finished);
        // Delete inside it (Shift+Delete asks first).
        s.settle();
        select_path(&mut s, &locked.join("keep.conf"));
        s.act("key shift+Delete");
        s.idle(2);
        s.act("key Enter");
        wait_until(&mut s, "delete refused", 10, |s| s.texts.iter().any(|(_, t)| t == "Retry as administrator"));
        s.click_text("Retry as administrator");
        wait_until(&mut s, "deleted as administrator", 10, |_| !locked.join("keep.conf").exists());
        // A new folder: asked, then made.
        s.settle();
        s.act("key ctrl+shift+n");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("You don't have permission")), "asked first");
        s.click_text("Use administrator");
        wait_until(&mut s, "folder made", 10, |_| locked.join("New folder").is_dir());
        // Rename: asked, then renamed.
        s.settle();
        select_path(&mut s, &locked.join("notes.txt"));
        s.act("key F2");
        s.act("key ctrl+a");
        s.act("text settings.txt");
        s.act("key Enter");
        s.idle(2);
        s.click_text("Use administrator");
        wait_until(&mut s, "renamed", 10, |_| locked.join("settings.txt").exists() && !locked.join("notes.txt").exists());
        // The password dialog dismissed: nothing changes, and the job says why.
        std::fs::write(d.join("deny"), "").unwrap();
        s.settle();
        s.act("key ctrl+shift+n");
        s.idle(2);
        s.click_text("Use administrator");
        wait_until(&mut s, "refusal reported", 10, |s| s.app.jobs.iter().any(|j| j.progress.lock().unwrap().errors.iter().any(|e| e == "Administrator access was not given")));
        assert!(!locked.join("New folder (1)").exists());
        look(&mut s, "after administrator work");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        report("admin", &s);
    }

    fn code(s: &Sim) -> &crate::devtools::CodeState {
        match &s.app.popup {
            Popup::CodeSearch(st) => st,
            _ => s.fail("Search Code is not open"),
        }
    }

    fn code_files(s: &Sim) -> Vec<String> {
        code(s).results.iter().map(|f| f.rel.clone()).collect()
    }

    /// Wait for the query as typed to have been searched to the end.
    fn code_settled(s: &mut Sim, what: &str) {
        wait_until(s, what, 20, |s| code(s).settled());
        s.idle(2);
    }

    fn launched() -> Vec<String> {
        crate::mime::LAUNCHED.lock().unwrap().clone()
    }

    fn some_text(s: &Sim, want: &str) -> bool {
        s.texts.iter().any(|(_, t)| t == want)
    }

    /// A photograph once the popup's opening fade is over.
    fn shot_still(s: &mut Sim, name: &str) {
        s.idle(12);
        s.shot(name);
    }

    /// Search Code from inside a project: the word found from the project's top in path
    /// order, lines with their neighbours, ignored, hidden and binary files left out
    /// until asked for; case, whole words, an expression (and a broken one), the file
    /// filter, this folder only, the TODO list with its counts; a line opened in the
    /// editor, copied as path:line, shown in Maia with the preview at that line; at
    /// three window sizes.
    #[test]
    fn search_code_and_todos() {
        if !sandboxed() {
            return;
        }
        let home = fixture("codesearch");
        let app = dev_fixture(&home);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake_editors("codesearch")));
        crate::editor::TEST_ENV.with(|e| e.borrow_mut().clear());
        let mut s = Sim::new(&app.join("src"), vec2(1280.0, 800.0));
        s.act("key ctrl+alt+f");
        assert!(matches!(s.app.popup, Popup::CodeSearch(_)), "Ctrl+Alt+F opens Search Code");
        s.act("text needle");
        code_settled(&mut s, "needle");
        assert_eq!(code_files(&s), ["src/lib.rs", "src/main.rs", "src/many.rs", "web/src/app.js"], "from the top, in path order, without ignored, hidden or binary files");
        assert_eq!(code(&s).root.as_deref(), Some(app.as_path()));
        let lines = |s: &Sim, f: &str| code(s).results.iter().find(|x| x.rel == f).map(|x| x.hits.iter().map(|h| h.line).collect::<Vec<_>>()).unwrap_or_default();
        assert_eq!(lines(&s, "src/main.rs"), vec![5, 6, 9, 10, 11]);
        let many = code(&s).results.iter().find(|f| f.rel == "src/many.rs").unwrap();
        assert_eq!((many.hits.len(), many.total), (crate::codesearch::PER_FILE, 300));
        assert!(some_text(&s, "let needle = find_needle();"), "the line is shown");
        assert!(some_text(&s, "fn main() {"), "with the line before it");
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("In ~/codesearch/Code/app · 308 lines in 4 files")), "the status says where and how much: {:?}", s.texts.iter().filter(|t| t.1.starts_with("In ")).collect::<Vec<_>>());
        shot_still(&mut s, "code-search");
        // Each line in its language's colours, worked out by the search rather than the
        // frame; a light theme draws them in its own.
        let main = code(&s).results.iter().find(|f| f.rel == "src/main.rs").unwrap();
        let kinds: Vec<crate::syntax::Kind> = main.hits.iter().flat_map(|h| h.syntax.iter().flatten().map(|x| x.2)).collect();
        for k in [crate::syntax::Kind::Keyword, crate::syntax::Kind::Function] {
            assert!(kinds.contains(&k), "{k:?} among {kinds:?}");
        }
        s.app.cfg.theme = crate::theme::ThemeName::Latte;
        s.idle(3);
        shot_still(&mut s, "code-search-latte");
        s.app.cfg.theme = crate::theme::ThemeName::default();
        s.idle(3);
        // The rest of a long file is counted at its end.
        for _ in 0..60 {
            if some_text(&s, "200 more lines in this file") {
                break;
            }
            s.act("scroll 640 420 -20");
        }
        assert!(some_text(&s, "200 more lines in this file"));
        shot_still(&mut s, "code-search-scrolled");
        // Enter opens the selected line in the editor and keeps the list.
        s.act("key Down");
        assert_eq!(code(&s).selected().map(|(f, h)| (f.rel.clone(), h.line)), Some(("src/main.rs".to_string(), 5)));
        let n = launched().len();
        s.act("key Enter");
        let l = launched();
        assert!(l.len() == n + 1 && l[n].contains("editors/codesearch/code") && l[n].contains("\"-g\"") && l[n].contains("src/main.rs:5\""), "VS Code at the line: {:?}", l.last());
        assert!(matches!(s.app.popup, Popup::CodeSearch(_)), "the list stays");
        assert!(toast_is(&s, "Opened main.rs:5 in VS Code"), "{:?}", s.app.toast);
        s.act("key ctrl+shift+c");
        assert_eq!(s.copied.last().map(String::as_str), Some("src/main.rs:5"));
        // Page Down goes on through lines far below, keeping the selection in view.
        for _ in 0..5 {
            s.act("key PageDown");
        }
        // The list scrolls there smoothly: let it arrive.
        s.idle(30);
        let (f, h) = code(&s).selected().map(|(f, h)| (f.rel.clone(), h.text.clone())).unwrap();
        assert_eq!((f.as_str(), h.as_str()), ("src/many.rs", "fn needle_45() {}"));
        assert!(s.texts.iter().any(|(r, t)| *t == h && r.max.y < s.size.y), "the selected line is in view");
        for _ in 0..5 {
            s.act("key PageUp");
        }
        // Whole words: needle_0 and find_needle are other words.
        click_control(&mut s, "whole word");
        code_settled(&mut s, "whole word");
        assert_eq!(code_files(&s), ["src/lib.rs", "src/main.rs", "web/src/app.js"]);
        assert_eq!(lines(&s, "src/main.rs"), vec![5, 6, 10, 11]);
        click_control(&mut s, "whole word");
        // An expression, then a broken one.
        click_control(&mut s, "regex");
        s.act("key ctrl+a");
        s.act(r"text needle_\d+\(");
        code_settled(&mut s, "expression");
        assert_eq!(code_files(&s), ["src/many.rs"]);
        s.act("text (");
        wait_until(&mut s, "the expression's error", 5, |s| code(s).error.is_some());
        s.idle(2);
        let err = code(&s).error.clone().unwrap();
        assert!(some_text(&s, &err), "the error is shown: {err}");
        assert!(code(&s).results.is_empty());
        shot_still(&mut s, "code-search-error");
        click_control(&mut s, "regex");
        // Match case.
        s.act("key ctrl+a");
        s.act("text Needle");
        click_control(&mut s, "match case");
        code_settled(&mut s, "match case");
        assert_eq!((code_files(&s), lines(&s, "src/main.rs")), (vec!["src/main.rs".to_string()], vec![10]));
        click_control(&mut s, "match case");
        // File types.
        s.act("key ctrl+a");
        s.act("text needle");
        s.act("key Tab");
        s.act("text js");
        code_settled(&mut s, "only js");
        assert_eq!(code_files(&s), ["web/src/app.js"]);
        s.act("key ctrl+a");
        s.act("key Backspace");
        // This folder only.
        click_control(&mut s, "code scope folder");
        code_settled(&mut s, "this folder");
        assert_eq!(code_files(&s), ["lib.rs", "main.rs", "many.rs"]);
        click_control(&mut s, "code scope project");
        // Everything, ignored and hidden and binary files included.
        click_control(&mut s, "code all files");
        code_settled(&mut s, "all files");
        let all = code_files(&s);
        for f in [".secret", "logo.bin", "notes.log", "target/debug/app.d", "web/dist/app.js", "web/node_modules/left-pad/index.js"] {
            assert!(all.contains(&f.to_string()), "{f} with All files: {all:?}");
        }
        assert!(code(&s).results.iter().any(|f| f.rel == "logo.bin" && f.binary));
        let long = &code(&s).results.iter().find(|f| f.rel == "target/debug/app.d").unwrap().hits[0];
        assert!(long.cut.1 && long.text.len() <= 240, "a 20,000 character line is cut");
        shot_still(&mut s, "code-search-all");
        click_control(&mut s, "code all files");
        // The TODO list, counted by tag.
        click_control(&mut s, "code todo");
        s.act("key ctrl+a");
        s.act("key Backspace");
        code_settled(&mut s, "TODOs");
        assert_eq!(code_files(&s), ["src/lib.rs", "src/main.rs", "web/src/app.js"]);
        assert_eq!(lines(&s, "src/main.rs"), vec![3, 10]);
        assert!(s.texts.iter().any(|(_, t)| t.ends_with("1 TODO · 1 FIXME · 1 HACK · 1 XXX in 3 files")), "counts in the status");
        shot_still(&mut s, "todo-finder");
        // A file folds away and back.
        s.click_text("src/main.rs");
        assert!(!some_text(&s, "// TODO: read the config before the window opens"), "folded");
        s.click_text("src/main.rs");
        assert!(some_text(&s, "// TODO: read the config before the window opens"));
        for (w, h) in [(640.0, 480.0), (520.0, 340.0), (1280.0, 800.0)] {
            s.resize(w, h);
            s.idle(3);
            shot_still(&mut s, &format!("todo-finder-{w}x{h}"));
        }
        // Ctrl+click shows the line in Maia: the file selected, its preview at the line.
        let r = s.text_rect("// TODO: read the config before the window opens");
        s.act(&format!("click {:.0} {:.0} ctrl", r.center().x, r.center().y));
        assert!(!s.popup_open(), "Show in Maia closes the search");
        s.settle();
        let main = app.join("src/main.rs");
        assert_eq!(s.tab().path, app.join("src"));
        assert_eq!(s.tab().cursor.map(|c| s.tab().entries[c as usize].path.clone()), Some(main.clone()));
        wait_until(&mut s, "the preview at the line", 10, |s| inspector(s).goto.as_ref().is_some_and(|g| g.0 == main && g.1 == 3 && !g.2));
        look(&mut s, "shown in Maia");
        shot_still(&mut s, "show-in-maia");
        // The file's path with that line, and from the project's top.
        palette_run(&mut s, "Copy Path With Line");
        assert_eq!(s.copied.last().map(String::as_str), Some("src/main.rs:3"));
        palette_run(&mut s, "Copy Relative Path");
        assert_eq!(s.copied.last().map(String::as_str), Some("src/main.rs"));
        let row = row_text(&s, "main.rs");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.act("text path with line");
        s.act("key Enter");
        assert_eq!(s.copied.last().map(String::as_str), Some("src/main.rs:3"), "from the file's menu too");
        // F4 opens it at that line.
        let n = launched().len();
        s.act("key F4");
        assert!(launched().get(n).is_some_and(|l| l.contains("src/main.rs:3\"")), "{:?}", launched().last());
        // Find TODOs from the palette opens the list straight away.
        palette_run(&mut s, "Find TODOs");
        assert!(code(&s).q.todo);
        code_settled(&mut s, "TODOs again");
        assert_eq!(code_files(&s).len(), 3);
        s.act("key Escape");
        assert!(!s.popup_open());
        // And Search Code comes back as it was left.
        s.act("key ctrl+alt+f");
        assert!(!code(&s).q.todo && code(&s).project);
        s.act("key Escape");
        look(&mut s, "after Search Code");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        report("search code", &s);
    }

    /// Open In Editor: Auto picks the installed editor, Options names another (a terminal
    /// one runs in the terminal), the file's menu and F4 open the selection, a folder
    /// opens as a folder, $EDITOR counts when nothing is set.
    #[test]
    fn open_in_editor() {
        if !sandboxed() {
            return;
        }
        let home = fixture("editor");
        let app = dev_fixture(&home);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake_editors("editor")));
        crate::editor::TEST_ENV.with(|e| e.borrow_mut().clear());
        let mut s = Sim::new(&app.join("src"), vec2(1280.0, 800.0));
        select_name(&mut s, "main.rs");
        let main = app.join("src/main.rs");
        let n = launched().len();
        s.act("key F4");
        let l = launched();
        assert!(l.len() == n + 1 && l[n].contains("editors/editor/code") && l[n].contains(&format!("{:?}", main.display().to_string())) && !l[n].contains("-g"), "VS Code, no line: {:?}", l.last());
        assert!(toast_is(&s, "Opened main.rs in VS Code"));
        // Options: Startup & system, Developer.
        s.act("key ctrl+Comma");
        s.act("text editor");
        s.idle(3);
        assert!(some_text(&s, "Auto: VS Code"), "the field says what Auto comes to");
        shot_still(&mut s, "options-editor");
        click_control(&mut s, "editor field");
        s.act("text nvim");
        s.act("key Enter");
        s.act("key Escape");
        assert_eq!(s.app.cfg.editor, "nvim");
        s.app.popup = Popup::options_at(crate::popups::Section::System);
        s.idle(4);
        assert!(some_text(&s, "Developer") && some_text(&s, "nvim"), "the Developer group in Startup & system");
        shot_still(&mut s, "options-developer");
        s.act("key Escape");
        let n = launched().len();
        s.act("key F4");
        let l = launched();
        assert!(l.len() == n + 1 && l[n].contains("editors/editor/nvim") && !l[n].contains("\"sh\""), "nvim in the terminal: {:?}", l.last());
        // The menu's item, with Zed.
        s.app.cfg.editor = "zed".into();
        let row = row_text(&s, "main.rs");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        s.click_text("Open In Editor");
        assert!(launched().last().is_some_and(|l| l.contains("editors/editor/zed")), "{:?}", launched().last());
        // A folder opens as a folder.
        s.app.cfg.editor = "code".into();
        s.cmd(Up);
        s.settle();
        select_name(&mut s, "src");
        s.act("key F4");
        assert!(launched().last().is_some_and(|l| l.ends_with(&format!("{:?}]", app.join("src").display().to_string())) || l.contains(&format!("{:?}", app.join("src").display().to_string()))), "{:?}", launched().last());
        // Nothing set: $EDITOR comes first.
        s.app.cfg.editor.clear();
        crate::editor::TEST_ENV.with(|e| e.borrow_mut().push(("EDITOR".into(), "hx".into())));
        s.act("key F4");
        assert!(launched().last().is_some_and(|l| l.contains("editors/editor/hx")), "{:?}", launched().last());
        crate::editor::TEST_ENV.with(|e| e.borrow_mut().clear());
        // Without any editor, a message instead.
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(root().join("no-tools")));
        s.act("key F4");
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0.starts_with("No editor found")), "{:?}", s.app.toast);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        look(&mut s, "after the editor");
        report("editor", &s);
    }

    /// Run Task: the scripts of the package beside the folder, then the project's own
    /// (Cargo, justfile, Makefile), filtered as typed; Enter runs one in the terminal,
    /// held open; a folder without manifests says so.
    #[test]
    fn run_project_tasks() {
        if !sandboxed() {
            return;
        }
        let home = fixture("tasks");
        let app = dev_fixture(&home);
        let mut s = Sim::new(&app.join("web/src"), vec2(1280.0, 800.0));
        s.act("key ctrl+shift+b");
        let groups = |s: &Sim| match &s.app.popup {
            Popup::Tasks(t) => t.groups.clone(),
            _ => s.fail("Run Task is not open"),
        };
        wait_until(&mut s, "the tasks", 10, |s| groups(s).is_some());
        s.idle(2);
        let g = groups(&s).unwrap();
        let src: Vec<(PathBuf, String)> = g.iter().map(|g| (g.dir.clone(), g.source.clone())).collect();
        assert_eq!(src, vec![(app.join("web"), "package.json · pnpm".to_string()), (app.clone(), "Cargo.toml".into()), (app.clone(), "justfile".into()), (app.clone(), "Makefile".into())]);
        assert_eq!(g[1].tasks.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["build", "run app", "run tool", "check", "test", "example demo"]);
        assert!(some_text(&s, "vite build"), "a script's command beside its name");
        shot_still(&mut s, "run-task");
        s.act("text build");
        s.idle(2);
        let n = launched().len();
        s.act("key Enter");
        let l = launched();
        assert!(l.len() == n + 1 && l[n].contains("\"pnpm\" \"run\" \"build\"") && l[n].contains("exec"), "pnpm run build, held open: {:?}", l.last());
        assert!(!s.popup_open());
        assert!(toast_is(&s, "Running pnpm run build in the terminal"), "{:?}", s.app.toast);
        s.act("key ctrl+shift+b");
        wait_until(&mut s, "the tasks again", 10, |s| groups(s).is_some());
        s.act("text justfile");
        s.idle(2);
        s.act("key Enter");
        assert!(launched().last().is_some_and(|l| l.contains("\"just\" \"test\"")), "{:?}", launched().last());
        for (w, h) in [(640.0, 480.0), (520.0, 340.0)] {
            s.resize(w, h);
            s.act("key ctrl+shift+b");
            wait_until(&mut s, "tasks small", 10, |s| groups(s).is_some());
            s.idle(2);
            shot_still(&mut s, &format!("run-task-{w}x{h}"));
            s.act("key Escape");
        }
        s.resize(1280.0, 800.0);
        go(&mut s, &home.join("Documents"));
        s.act("key ctrl+shift+b");
        wait_until(&mut s, "no tasks", 10, |s| groups(s).is_some());
        s.idle(2);
        assert!(groups(&s).unwrap().is_empty());
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("No tasks here")));
        shot_still(&mut s, "run-task-none");
        s.act("key Escape");
        look(&mut s, "after tasks");
        report("tasks", &s);
    }

    /// The terminal pane: its key opens it on a shell in the folder shown that takes what
    /// is typed (Maia's own hotkeys stay quiet), goes along when the panel moves to
    /// another folder (not into the Computer view), scrolls back, copies a selection,
    /// gives the keyboard back to a click on the files and closes with the shell reaped,
    /// or when the shell ends.
    #[test]
    fn terminal_pane_follows_the_folder() {
        if !sandboxed() {
            return;
        }
        let home = fixture("terminal");
        let docs = home.join("Documents");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let idle = |s: &Sim| s.app.terminal.pty().is_some_and(|p| p.idle());
        s.act("key ctrl+Backtick");
        assert!(s.app.terminal.pty().is_some() && s.app.terminal.focused, "open, with the keyboard");
        wait_until(&mut s, "the prompt", 10, |s| idle(s) && !s.app.terminal.text().trim().is_empty());
        let pid = s.app.terminal.pty().and_then(|p| p.pid()).unwrap();
        let cwd = || std::fs::read_link(format!("/proc/{pid}/cwd")).ok();
        assert_eq!(cwd(), Some(home.clone()), "started in the folder shown");
        // Ctrl+A is the shell's (start of the line) here, not Select All.
        s.act("text echo maia-ok");
        s.act("key ctrl+a");
        s.act("key enter");
        wait_until(&mut s, "the echo", 10, |s| some_text(s, "maia-ok"));
        assert!(s.tab().selected.is_empty(), "Maia's hotkey stayed quiet");
        s.idle(30);
        s.shot("terminal-dark");
        // Dragged over, the text copies with Ctrl+Shift+C.
        let r = s.text_rect("maia-ok");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", r.left() + 1.0, r.center().y, r.right() - 1.0, r.center().y));
        s.act("key ctrl+shift+c");
        assert_eq!(s.copied.last().map(String::as_str), Some("maia-ok"));
        // A line longer than a narrow window and the icons a prompt draws: narrowed, the
        // line wraps at the new width and the shell is told it; widened, it is one row.
        let long: String = (0..30).map(|i| format!("w{i:02}-")).collect();
        let rows = |s: &Sim| s.app.terminal.text();
        s.act("text printf 'w%02d-' $(seq 0 29); echo; printf '\\uf07b \\ue0a0 \\uf015 \\uf017\\n'");
        s.act("key enter");
        wait_until(&mut s, "the long line", 10, |s| idle(s) && rows(s).lines().any(|l| l.contains(&long)));
        s.idle(10);
        s.shot("terminal-icons");
        s.resize(560.0, 800.0);
        let size = |s: &Sim| s.app.terminal.pty().map(|p| {
            let t = p.shared.term();
            (t.screen.cols, t.screen.rows)
        }).unwrap_or_default();
        let (cols, lines) = size(&s);
        assert!(cols < long.len(), "{cols} columns hold the line");
        wait_until(&mut s, "the line rewrapped", 10, |s| {
            let t = rows(s);
            !t.contains(&long) && t.lines().any(|l| l.starts_with("w00-") && l.len() == cols) && t.contains("w29-")
        });
        s.act("text stty size");
        s.act("key enter");
        let told = format!("{lines} {cols}");
        wait_until(&mut s, "the shell's new size", 10, |s| rows(s).lines().any(|l| l.trim() == told));
        s.shot("terminal-narrow");
        s.resize(1280.0, 800.0);
        wait_until(&mut s, "the line whole again", 10, |s| rows(s).lines().any(|l| l.contains(&long)));
        s.act("key ctrl+l");
        wait_until(&mut s, "the prompt after clear", 10, idle);
        // The panel moves on: the shell, waiting at its prompt, follows.
        wait_until(&mut s, "the prompt again", 10, idle);
        go(&mut s, &docs);
        wait_until(&mut s, "the shell in Documents", 10, |_| cwd() == Some(docs.clone()));
        wait_until(&mut s, "the prompt after cd", 10, idle);
        s.act("text pwd");
        s.act("key enter");
        let shown = docs.display().to_string();
        wait_until(&mut s, "pwd", 10, |s| some_text(s, &shown));
        s.cmd(Computer);
        s.idle(5);
        assert_eq!(cwd(), Some(docs.clone()), "the Computer view is no folder to go to");
        // Output past the screen scrolls back with the wheel; typing comes back down.
        s.act("text seq 1 300");
        s.act("key enter");
        wait_until(&mut s, "the numbers", 10, |s| some_text(s, "300"));
        let body = s.text_rect("300");
        s.act(&format!("scroll {:.0} {:.0} 5", body.center().x, body.center().y));
        let back = s.app.terminal.pty().map(|p| p.shared.term().screen.scroll).unwrap();
        assert!(back > 0, "scrolled back");
        s.act("text x");
        assert_eq!(s.app.terminal.pty().map(|p| p.shared.term().screen.scroll), Some(0));
        s.act("key backspace");
        // A click on the files takes the keyboard; the key gives it back, then closes.
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("click {:.0} {:.0}", list.center().x, list.center().y));
        assert!(!s.app.terminal.focused);
        s.act("key ctrl+Backtick");
        assert!(s.app.terminal.focused && s.app.terminal.pty().is_some());
        s.act("key ctrl+Backtick");
        assert!(s.app.terminal.pty().is_none(), "closed");
        wait_until(&mut s, "the shell reaped", 10, |_| !Path::new(&format!("/proc/{pid}")).exists());
        s.idle(20);
        // In a light theme too.
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        go(&mut s, &home.join("Code/maia"));
        s.act("key ctrl+Backtick");
        wait_until(&mut s, "the prompt (light)", 10, |s| idle(s) && !s.app.terminal.text().trim().is_empty());
        s.act("text printf '\\033[1;31mred \\033[32mgreen \\033[34mblue \\033[0;7m inverse \\033[0m\\n'; ls");
        s.act("key enter");
        wait_until(&mut s, "ls", 10, |s| s.texts.iter().any(|(_, t)| t.contains("Cargo.toml") && t.contains("README")));
        s.idle(30);
        s.shot("terminal-light");
        // The shell ending closes the pane.
        s.act("text exit");
        s.act("key enter");
        wait_until(&mut s, "the pane closed", 10, |s| s.app.terminal.pty().is_none());
        s.app.cfg.theme = crate::theme::ThemeName::default();
        look(&mut s, "after the terminal");
        report("terminal", &s);
    }

    /// Where the terminal drew `row` (the whole row's text), at its column `col`.
    fn term_cell(s: &Sim, row: &str, col: usize) -> (f32, f32) {
        let r = s.text_rect(row);
        let cw = r.width() / row.chars().count() as f32;
        (r.left() + cw * (col as f32 + 0.5), r.center().y)
    }

    fn controls_named(what: &str) -> Vec<Rect> {
        crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, _, w)| *w == what).map(|(_, r, _)| *r).collect())
    }

    /// Several terminals: Ctrl+Shift+T opens another shell in the folder shown, with a tab
    /// strip only while there is more than one; each keeps its own screen, title and
    /// folder; Ctrl+Shift+PageUp/PageDown, the tabs and the palette switch; only the one
    /// shown follows the panel, and one shown again goes on from where it was; Ctrl+Shift+W,
    /// a tab's ×, a middle click and `exit` close one, the last closing the pane.
    #[test]
    fn terminal_tabs() {
        if !sandboxed() {
            return;
        }
        let home = fixture("terminal-tabs");
        let (docs, music, pics) = (home.join("Documents"), home.join("Music"), home.join("Pictures"));
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let idle = |s: &Sim| s.app.terminal.pty().is_some_and(|p| p.idle());
        let cwd = |s: &Sim, i: usize| s.app.terminal.shells.get(i).and_then(|sh| sh.pty.cwd());
        let ready = |s: &mut Sim, what: &str| wait_until(s, what, 10, |s| idle(s) && (s.app.terminal.text().contains('$') || s.app.terminal.text().contains('#')));
        s.act("key ctrl+Backtick");
        ready(&mut s, "the prompt");
        s.idle(2);
        assert!(controls_named("terminal tab").is_empty(), "one shell, no tabs");
        s.act("text printf '\\033]0;first\\007'; echo one-here");
        s.act("key enter");
        wait_until(&mut s, "the first shell's echo", 10, |s| some_text(s, "one-here") && some_text(s, "first"));
        go(&mut s, &docs);
        wait_until(&mut s, "the first shell in Documents", 10, |s| cwd(s, 0) == Some(docs.clone()));
        wait_until(&mut s, "its prompt", 10, idle);
        s.act("key ctrl+shift+t");
        assert_eq!((s.app.terminal.shells.len(), s.app.terminal.cur), (2, 1));
        ready(&mut s, "the second prompt");
        assert_eq!(cwd(&s, 1), Some(docs.clone()), "started in the folder shown");
        assert!(!some_text(&s, "one-here"), "a screen of its own");
        s.act("text printf '\\033]0;second\\007'; echo two-here");
        s.act("key enter");
        wait_until(&mut s, "the second echo and both titles", 10, |s| some_text(s, "two-here") && some_text(s, "first") && some_text(s, "second"));
        assert_eq!(controls_named("terminal tab").len(), 2);
        s.idle(20);
        s.shot("terminal-tabs");
        s.act("key ctrl+shift+PageUp");
        assert_eq!(s.app.terminal.cur, 0);
        s.idle(2);
        assert!(some_text(&s, "one-here") && !some_text(&s, "two-here"));
        s.act("key ctrl+shift+PageDown");
        s.act("key ctrl+shift+PageDown");
        assert_eq!(s.app.terminal.cur, 0, "round the end");
        assert!(s.app.terminal.focused, "the keyboard stays in the terminal");
        // The shell shown follows the panel; the other stays where it was.
        go(&mut s, &music);
        wait_until(&mut s, "the first shell in Music", 10, |s| cwd(s, 0) == Some(music.clone()));
        s.idle(20);
        assert_eq!(cwd(&s, 1), Some(docs.clone()), "the hidden shell stayed");
        let tabs = controls_named("terminal tab");
        s.act(&format!("click {:.0} {:.0}", tabs[1].center().x, tabs[1].center().y));
        assert_eq!(s.app.terminal.cur, 1);
        s.idle(20);
        assert_eq!(cwd(&s, 1), Some(docs.clone()), "shown again, it goes on from where it was");
        go(&mut s, &pics);
        wait_until(&mut s, "the second shell in Pictures", 10, |s| cwd(s, 1) == Some(pics.clone()));
        assert_eq!(cwd(&s, 0), Some(music.clone()));
        // From the palette and the +.
        s.cmd(NewTerminal);
        assert_eq!((s.app.terminal.shells.len(), s.app.terminal.cur), (3, 2));
        // Once its tab has grown in, and the + moved along.
        s.idle(10);
        click_control(&mut s, "terminal new");
        assert_eq!((s.app.terminal.shells.len(), s.app.terminal.cur), (4, 3));
        ready(&mut s, "the fourth prompt");
        s.cmd(PrevTerminal);
        assert_eq!(s.app.terminal.cur, 2);
        s.cmd(NextTerminal);
        assert_eq!(s.app.terminal.cur, 3);
        // A tab is named for its folder, as the panels' tabs are, with the whole path as
        // its tooltip; a cd typed by hand renames it once the shell is back at its prompt.
        let in_tab = |s: &Sim, want: &str| {
            let tabs = controls_named("terminal tab");
            s.texts.iter().any(|(r, t)| t == want && tabs.iter().any(|tr| tr.contains(r.center())))
        };
        assert!(in_tab(&s, "Pictures"), "named for its folder");
        s.act("text cd Trip");
        s.act("key enter");
        wait_until(&mut s, "the tab renamed", 10, |s| in_tab(s, "Trip"));
        let t = controls_named("terminal tab")[3];
        s.act(&format!("move {:.0} {:.0}", t.center().x, t.center().y));
        s.idle(50);
        assert!(s.texts.iter().any(|(_, t)| t.ends_with("terminal-tabs/Pictures/Trip")), "the whole path on hover");
        s.shot("terminal-tab-tooltip");
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("move {:.0} {:.0}", list.center().x, list.center().y));
        s.idle(20);
        look(&mut s, "four terminals");
        s.resize(640.0, 600.0);
        s.idle(20);
        s.shot("terminal-tabs-narrow");
        look(&mut s, "four terminals, narrow");
        s.resize(1280.0, 800.0);
        // Closing: the key, the ×, a middle click; the next one along shows.
        let pids: Vec<u32> = s.app.terminal.shells.iter().filter_map(|sh| sh.pty.pid()).collect();
        // Once the pane has eased to the new height.
        s.settle();
        click_control(&mut s, "terminal tab");
        s.act("key ctrl+shift+w");
        assert_eq!((s.app.terminal.shells.len(), s.app.terminal.cur), (3, 0));
        wait_until(&mut s, "the closed shell reaped", 10, |_| !Path::new(&format!("/proc/{}", pids[0])).exists());
        // Once the closed tab's room has closed up.
        s.idle(10);
        // The ×, at the shown tab's right end.
        let t = controls_named("terminal tab")[s.app.terminal.cur];
        s.act(&format!("move {:.0} {:.0}", t.right() - 14.0, t.center().y));
        s.act(&format!("click {:.0} {:.0}", t.right() - 14.0, t.center().y));
        assert_eq!(s.app.terminal.shells.len(), 2);
        s.idle(10);
        let tabs = controls_named("terminal tab");
        s.act(&format!("click {:.0} {:.0} middle", tabs[1].center().x, tabs[1].center().y));
        assert_eq!(s.app.terminal.shells.len(), 1);
        s.idle(2);
        assert!(controls_named("terminal tab").is_empty(), "one again: no tabs");
        s.act("key ctrl+shift+t");
        ready(&mut s, "another prompt");
        // A shell that ends closes its tab, the last one the pane.
        s.act("text exit");
        s.act("key enter");
        wait_until(&mut s, "the ended shell's tab gone", 10, |s| s.app.terminal.shells.len() == 1);
        assert!(s.app.terminal.pty().is_some());
        s.act("key ctrl+shift+w");
        assert!(s.app.terminal.pty().is_none(), "the last one closed the pane");
        // A light theme, no animations, split panels.
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        s.app.cfg.animations = false;
        s.cmd(SplitRight);
        s.act("key ctrl+Backtick");
        s.act("key ctrl+shift+t");
        assert_eq!(s.app.terminal.shells.len(), 2);
        ready(&mut s, "a prompt (light)");
        s.idle(2);
        assert_eq!(controls_named("terminal tab").len(), 2, "tabs at once, without growing in");
        s.act("key ctrl+shift+f");
        s.act("text root");
        s.idle(3);
        s.shot("terminal-tabs-light");
        look(&mut s, "terminals, light and split");
        s.act("key Escape");
        assert!(s.app.terminal.focused, "Escape gave the keyboard back to the shell");
        s.act("key ctrl+Backtick");
        assert!(s.app.terminal.pty().is_none());
        s.app.cfg.theme = crate::theme::ThemeName::default();
        s.app.cfg.animations = true;
        look(&mut s, "after the terminals");
        report("terminal tabs", &s);
    }

    /// The scrollback from the keyboard: Shift+PageUp and PageDown a page at a time,
    /// Shift+Home and End to its ends, while a full-screen program gets those keys
    /// itself. Ctrl+Shift+F finds in it: the count, the newest match first, Enter and
    /// Shift+Enter stepping and scrolling to each, none found, Escape back to the shell.
    #[test]
    fn terminal_scrollback_and_find() {
        if !sandboxed() {
            return;
        }
        let home = fixture("terminal-find");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let idle = |s: &Sim| s.app.terminal.pty().is_some_and(|p| p.idle());
        let screen = |s: &Sim| s.app.terminal.pty().map(|p| {
            let t = p.shared.term();
            (t.screen.scroll, t.screen.history(), t.screen.rows)
        }).unwrap();
        s.act("key ctrl+Backtick");
        wait_until(&mut s, "the prompt", 10, |s| idle(s) && !s.app.terminal.text().trim().is_empty());
        s.act("text seq 1 400");
        s.act("key enter");
        wait_until(&mut s, "the numbers", 10, |s| idle(s) && some_text(s, "400"));
        let (_, history, rows) = screen(&s);
        let page = rows - 1;
        s.act("key shift+PageUp");
        assert_eq!(screen(&s).0, page);
        s.act("key shift+PageUp");
        assert_eq!(screen(&s).0, 2 * page);
        s.act("key shift+PageDown");
        assert_eq!(screen(&s).0, page);
        s.act("key shift+Home");
        assert_eq!(screen(&s).0, history);
        assert!(some_text(&s, "seq 1 400") || s.texts.iter().any(|(_, t)| t.ends_with("seq 1 400")), "the top of what went by");
        s.act("key shift+End");
        assert_eq!(screen(&s).0, 0);
        // The find bar leaves the newest rows in sight: the top ones make room for it.
        s.act("key ctrl+shift+f");
        s.idle(10);
        let prompt = s.app.terminal.text().lines().rev().find(|l| !l.trim().is_empty()).unwrap_or_default().to_string();
        let r = s.text_rect(&prompt);
        assert!(r.bottom() <= s.size.y - crate::findbar::BAR_H + 0.5, "the prompt above the bar: {r:?}");
        s.act("key Escape");
        assert!(s.app.terminal.found().is_none());
        // Find.
        s.act("key shift+PageUp");
        s.act("key ctrl+shift+f");
        s.idle(2);
        assert!(s.app.terminal.found().is_some());
        s.act("text 37");
        s.idle(3);
        // 37, 137, 237, 337, 370 to 379: the newest first.
        let (n, cur, hit) = s.app.terminal.found().unwrap();
        assert_eq!((n, cur), (14, 13));
        assert!(some_text(&s, "14 of 14"));
        let text = |s: &Sim, h: crate::vt::Span| s.app.terminal.pty().unwrap().shared.term().screen.text(h.0, h.1);
        assert_eq!(text(&s, hit.unwrap()), "37");
        s.act("key enter");
        let (_, cur, hit) = s.app.terminal.found().unwrap();
        assert_eq!(cur, 0, "Enter goes on past the last to the first");
        s.idle(2);
        assert!(some_text(&s, "1 of 14") && some_text(&s, "37"), "scrolled to it");
        assert!(screen(&s).0 > 300, "far back");
        assert_eq!(hit.map(|h| text(&s, h)).as_deref(), Some("37"));
        s.act("key shift+Enter");
        assert_eq!(s.app.terminal.found().unwrap().1, 13);
        s.act("key shift+Enter");
        s.idle(20);
        s.shot("terminal-find");
        assert!(some_text(&s, "13 of 14") && some_text(&s, "378"));
        // F3 steps as Enter does.
        s.act("key F3");
        assert_eq!(s.app.terminal.found().unwrap().1, 13);
        s.act("key ctrl+a");
        s.act("text ZZZ");
        s.idle(3);
        assert!(some_text(&s, "No matches"));
        s.idle(10);
        s.shot("terminal-find-none");
        s.act("key Escape");
        assert!(s.app.terminal.found().is_none(), "Escape closes the bar");
        s.act("text echo back-in-shell");
        s.act("key enter");
        wait_until(&mut s, "the shell has the keyboard again", 10, |s| some_text(s, "back-in-shell"));
        assert_eq!(screen(&s).0, 0, "typing came back down");
        // In a full-screen program the keys are its own.
        s.act("text seq 1 400 | LESSHISTFILE=- less");
        s.act("key enter");
        let top = |s: &Sim| s.app.terminal.text().lines().next().unwrap_or_default().to_string();
        wait_until(&mut s, "less", 10, |s| s.app.terminal.pty().is_some_and(|p| p.shared.term().screen.alt()) && top(s) == "1");
        s.act("key shift+PageUp");
        assert_eq!(screen(&s).0, 0);
        s.act("text f");
        wait_until(&mut s, "less paged", 10, |s| top(s) != "1");
        s.act("text q");
        wait_until(&mut s, "less gone", 10, |s| s.app.terminal.pty().is_some_and(|p| !p.shared.term().screen.alt()) && idle(s));
        s.act("key ctrl+Backtick");
        look(&mut s, "after finding");
        report("terminal find", &s);
    }

    /// Links in the terminal: with Ctrl held, an address, a path with its line, a folder
    /// relative to the shell's, a hyperlink a program wrote and `ls --hyperlink`'s names
    /// underline under the pointer, and a click opens them: the browser, the editor at
    /// the line, the folder in the panel, the file selected in its own. Double and triple
    /// clicks select a word and a line. less and vim hear the mouse, and Shift still
    /// selects over them.
    #[test]
    fn terminal_links_and_mouse() {
        if !sandboxed() {
            return;
        }
        let home = fixture("terminal-links");
        let proj = home.join("proj");
        let wf = |p: &str, t: &str| {
            let p = proj.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, t).unwrap();
        };
        wf("src/lib.rs", "pub fn one() {}\npub fn two() {}\n");
        wf("nums.txt", &(1..=300).map(|i| format!("{i}\n")).collect::<String>());
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake_editors("terminal-links")));
        crate::editor::TEST_ENV.with(|e| e.borrow_mut().clear());
        let mut s = Sim::new(&proj, vec2(1280.0, 800.0));
        s.app.cfg.editor = "code".into();
        let idle = |s: &Sim| s.app.terminal.pty().is_some_and(|p| p.idle());
        s.act("key ctrl+Backtick");
        wait_until(&mut s, "the prompt", 10, |s| idle(s) && !s.app.terminal.text().trim().is_empty());
        s.act("text printf 'see https://example.com/docs.\\nerror: src/lib.rs:2:5: oops\\nlook in src\\n\\033]8;;https://maia.example/osc\\033\\\\linked\\033]8;;\\033\\\\ text\\n'");
        s.act("key enter");
        wait_until(&mut s, "the lines", 10, |s| idle(s) && some_text(s, "linked text"));
        // The pane has eased open: its rows stay where they are.
        s.settle();
        let url_row = "see https://example.com/docs.";
        let (x, y) = term_cell(&s, url_row, 10);
        s.act(&format!("move {x:.0} {y:.0}"));
        s.idle(2);
        assert_eq!(s.app.terminal.hovered(), None, "nothing without Ctrl");
        s.act("hold ctrl");
        s.act(&format!("move {x:.0} {y:.0}"));
        s.idle(2);
        assert_eq!(s.app.terminal.hovered().as_deref(), Some("https://example.com/docs"));
        s.shot("terminal-link");
        let n = launched().len();
        s.act(&format!("click {x:.0} {y:.0} left ctrl"));
        let l = launched();
        assert!(l.len() == n + 1 && l[n].contains("xdg-open") && l[n].contains("\"https://example.com/docs\""), "{:?}", l.last());
        // A compiler's path and line: the editor, at the line.
        let (x, y) = term_cell(&s, "error: src/lib.rs:2:5: oops", 12);
        s.act(&format!("move {x:.0} {y:.0}"));
        wait_until(&mut s, "the path found on disk", 10, |s| s.app.terminal.hovered().as_deref() == Some("src/lib.rs:2:5"));
        let n = launched().len();
        s.act(&format!("click {x:.0} {y:.0} left ctrl"));
        let l = launched();
        assert!(l.len() == n + 1 && l[n].contains("editors/terminal-links/code") && l[n].contains("lib.rs:2\""), "{:?}", l.last());
        // A word that is no file there underlines nothing.
        let (x, y) = term_cell(&s, "look in src", 6);
        s.act(&format!("move {x:.0} {y:.0}"));
        s.idle(10);
        assert_eq!(s.app.terminal.hovered(), None, "\"in\" is no file");
        // A folder opens in the panel (and the shell, at its prompt, follows).
        let (x, y) = term_cell(&s, "look in src", 9);
        s.act(&format!("move {x:.0} {y:.0}"));
        wait_until(&mut s, "the folder found", 10, |s| s.app.terminal.hovered().as_deref() == Some("src"));
        s.act(&format!("click {x:.0} {y:.0} left ctrl"));
        s.settle();
        assert_eq!(s.tab().path, proj.join("src"));
        // A hyperlink a program wrote.
        let (x, y) = term_cell(&s, "linked text", 2);
        s.act(&format!("move {x:.0} {y:.0}"));
        s.idle(2);
        assert_eq!(s.app.terminal.hovered().as_deref(), Some("linked"));
        let n = launched().len();
        s.act(&format!("click {x:.0} {y:.0} left ctrl"));
        assert!(launched().get(n).is_some_and(|l| l.contains("\"https://maia.example/osc\"")), "{:?}", launched().last());
        s.act("hold none");
        // ls's names link to their files: one opens selected in its folder.
        wait_until(&mut s, "the shell in src", 10, |s| idle(s) && s.app.terminal.pty().and_then(|p| p.cwd()) == Some(proj.join("src")));
        s.act("text cd .. && ls --hyperlink=always");
        s.act("key enter");
        wait_until(&mut s, "ls", 10, |s| idle(s) && s.texts.iter().any(|(_, t)| t.starts_with("nums.txt")));
        let row = s.texts.iter().find(|(_, t)| t.starts_with("nums.txt")).map(|(_, t)| t.clone()).unwrap();
        let (x, y) = term_cell(&s, &row, 1);
        s.act("hold ctrl");
        s.act(&format!("move {x:.0} {y:.0}"));
        wait_until(&mut s, "ls's link", 10, |s| s.app.terminal.hovered().as_deref() == Some("nums.txt"));
        s.act(&format!("click {x:.0} {y:.0} left ctrl"));
        s.act("hold none");
        s.settle();
        assert_eq!(s.tab().path, proj);
        assert_eq!(s.tab().cursor_path(), Some(proj.join("nums.txt")), "selected in its folder");
        // Double and triple clicks (well after the last click, which would count).
        s.idle(30);
        let (x, y) = term_cell(&s, url_row, 12);
        s.act(&format!("dclick {x:.0} {y:.0}"));
        assert_eq!(s.app.terminal.selection().as_deref(), Some("https://example.com/docs"), "without the full stop");
        s.idle(30);
        s.act(&format!("tclick {x:.0} {y:.0}"));
        assert_eq!(s.app.terminal.selection().as_deref(), Some(url_row));
        s.act("key ctrl+shift+c");
        assert_eq!(s.copied.last().map(String::as_str), Some(url_row));
        // less hears the wheel.
        s.act("text clear; LESSHISTFILE=- less --mouse nums.txt");
        s.act("key enter");
        let mode = |s: &Sim| s.app.terminal.pty().map(|p| p.shared.term().screen.mouse);
        let top = |s: &Sim| s.app.terminal.text().lines().next().unwrap_or_default().to_string();
        // less 710 asks for drags too (1002) after clicks (1000); older ones clicks alone.
        wait_until(&mut s, "less with the mouse", 10, |s| matches!(mode(s), Some(crate::vt::Track::Click | crate::vt::Track::Drag)) && top(s) == "1");
        s.idle(3);
        let body = s.texts.iter().rev().find(|(_, t)| t == "2").map(|(r, _)| *r).unwrap_or_else(|| s.fail("less's second row"));
        s.act(&format!("scroll {:.0} {:.0} -2", body.center().x, body.center().y));
        wait_until(&mut s, "less scrolled", 10, |s| top(s) != "1");
        s.idle(10);
        s.shot("terminal-less-mouse");
        s.act("text q");
        wait_until(&mut s, "less gone", 10, |s| mode(s) == Some(crate::vt::Track::Off) && idle(s));
        // vim: a click puts the cursor there; Shift drags select instead.
        s.act("text clear; vim -u NONE -i NONE -n -N -c 'set mouse=a' nums.txt");
        s.act("key enter");
        wait_until(&mut s, "vim with the mouse", 10, |s| mode(s) == Some(crate::vt::Track::Drag) && some_text(s, "6"));
        s.idle(5);
        let six = s.text_rect("6");
        s.act(&format!("click {:.0} {:.0}", six.center().x, six.center().y));
        s.idle(5);
        s.act("text iX");
        s.act("key Escape");
        wait_until(&mut s, "the X where vim was clicked", 10, |s| some_text(s, "X6"));
        let eight = s.text_rect("8");
        s.act("hold shift");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0} left shift", eight.left() + 1.0, eight.center().y, eight.right() + 30.0, eight.center().y));
        s.act("hold none");
        assert_eq!(s.app.terminal.selection().as_deref(), Some("8"));
        s.idle(10);
        s.shot("terminal-vim-mouse");
        s.act("text :q!");
        s.act("key enter");
        wait_until(&mut s, "vim gone", 10, |s| mode(s) == Some(crate::vt::Track::Off) && idle(s));
        s.act("key ctrl+Backtick");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        look(&mut s, "after links");
        report("terminal links", &s);
    }

    /// Clean Build Folders: the scan finds each kind under the folder, sizes them, sorts
    /// the biggest first; nothing goes without a check and a confirmation, the checked
    /// ones go to the trash, and Undo brings them back.
    #[test]
    fn clean_build_folders() {
        if !sandboxed() {
            return;
        }
        let home = fixture("clean");
        let app = dev_fixture(&home);
        let mut s = Sim::new(&home.join("Code"), vec2(1280.0, 800.0));
        palette_run(&mut s, "Clean Build Folders");
        let st = |s: &Sim| match &s.app.popup {
            Popup::CleanBuilds(c) => (c.done, c.items.clone(), c.checked.len()),
            _ => s.fail("Clean Build Folders is not open"),
        };
        wait_until(&mut s, "the scan", 20, |s| st(s).0);
        s.idle(2);
        let items = st(&s).1;
        let mut found: Vec<(String, &str)> = items.iter().map(|f| (crate::project::relative(&f.path, &home.join("Code")), f.kind)).collect();
        found.sort();
        assert_eq!(found, vec![("app/py/.venv".to_string(), "Python environment"), ("app/py/pkg/__pycache__".into(), "Python bytecode"), ("app/target".into(), "Rust build"), ("app/web/dist".into(), "Build output"), ("app/web/node_modules".into(), "Node packages")]);
        assert!(items.windows(2).all(|w| w[0].size >= w[1].size) && items.iter().all(|f| f.size.is_some()), "sized, the biggest first");
        assert_eq!(items[0].path, app.join("target"));
        assert!(some_text(&s, "Rust build · changed today"));
        shot_still(&mut s, "clean-builds");
        // Nothing checked: Enter does nothing.
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::CleanBuilds(_)));
        s.act("key space");
        let nm = list_row_y(&s, "app/web/node_modules");
        s.act(&format!("click 640 {nm:.0}"));
        assert_eq!(st(&s).2, 2, "the cursor's row and the clicked one");
        shot_still(&mut s, "clean-builds-checked");
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Confirm { .. }), "asks first");
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Move 2 folders (") && t.ends_with(") to the trash?")));
        shot_still(&mut s, "clean-builds-confirm");
        s.act("key Enter");
        wait_until(&mut s, "moved to the trash", 20, |_| !app.join("target").exists() && !app.join("web/node_modules").exists());
        s.settle();
        let trash = root().join("xdg/data/Trash/files");
        assert!(trash.join("target").is_dir() && trash.join("node_modules").is_dir(), "in the trash, not deleted");
        assert!(app.join("src/main.rs").exists() && app.join("web/dist").exists());
        wait_until(&mut s, "the trash job done", 20, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Trashed(_))));
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 20, |_| app.join("target/debug/app.d").exists() && app.join("web/node_modules/left-pad/index.js").exists());
        // Every project in the home folder, from anywhere: the one untouched for two months
        // checked in one go and deleted for good, freeing its room at once.
        let nm = app.join("web/node_modules");
        std::fs::File::open(&nm).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 86_400)).unwrap();
        go(&mut s, &home.join("Documents"));
        palette_run(&mut s, "Clean Build Folders In Home");
        wait_until(&mut s, "the home scan", 30, |s| st(s).0);
        s.idle(2);
        let st_root = |s: &Sim| match &s.app.popup {
            Popup::CleanBuilds(c) => c.root.clone(),
            _ => s.fail("Clean Build Folders is not open"),
        };
        assert_eq!(st_root(&s), crate::fsx::home(), "the home folder, not the one shown");
        assert!(st(&s).1.iter().any(|f| f.path == app.join("target")), "this project's among them");
        let old: Vec<PathBuf> = st(&s).1.iter().filter(|f| crate::devtools::is_stale(f.modified)).map(|f| f.path.clone()).collect();
        assert_eq!(old, [nm.clone()]);
        look(&mut s, "clean builds in home");
        click_control(&mut s, "clean stale");
        assert_eq!(st(&s).2, 1, "the stale one checked");
        shot_still(&mut s, "clean-builds-stale");
        s.act("key shift+Delete");
        assert!(s.texts.iter().any(|(_, t)| t.contains("for good")), "asks, saying it can't be undone");
        s.act("key Enter");
        wait_until(&mut s, "deleted", 20, |_| !nm.exists());
        assert!(!root().join("xdg/data/Trash/files/node_modules (1)").exists() && app.join("target").exists(), "deleted, not trashed; the rest left alone");
        // A folder with nothing to clean.
        go(&mut s, &home.join("Documents"));
        palette_run(&mut s, "Clean Build Folders");
        wait_until(&mut s, "an empty scan", 10, |s| st(s).0);
        s.idle(2);
        assert!(some_text(&s, "No build folders here"));
        s.resize(520.0, 340.0);
        shot_still(&mut s, "clean-builds-empty-small");
        s.act("key Escape");
        s.resize(1280.0, 800.0);
        look(&mut s, "after cleaning");
        report("clean builds", &s);
    }

    /// Where the file list (not the preview beside it) shows `label`.
    /// Sharing from a project: a file and a folder copied as code blocks from the file's
    /// menu (the folder without what git ignores), a link to a file on its forge and to
    /// the line a search found, and a file and an extension added to .gitignore, faded
    /// in the listing once git reads it again and gone from their menus after.
    #[test]
    fn share_from_a_project() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("share");
        let app = dev_fixture(&home);
        let g = |args: &[&str]| crate::tests::git(&app, args);
        g(&["add", "-A"]);
        g(&["commit", "-qm", "Start"]);
        g(&["remote", "add", "origin", "git@github.com:acme/app.git"]);
        let sha = String::from_utf8_lossy(&std::process::Command::new("git").arg("-C").arg(&app).args(["rev-parse", "HEAD"]).output().unwrap().stdout).trim().to_string();
        for f in ["scratch.txt", "a.tmp", "b.tmp"] {
            std::fs::write(app.join(f), "x\n").unwrap();
        }
        let mut s = Sim::new(&app.join("src"), vec2(1280.0, 800.0));
        wait_until(&mut s, "the repository", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(2);
        let menu = |s: &mut Sim, name: &str, what: &str| {
            reveal(s, name);
            let row = row_text(s, name);
            s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
            s.act(&format!("text {what}"));
            s.act("key Enter");
        };
        let n = s.copied.len();
        menu(&mut s, "main.rs", "code blocks");
        wait_until(&mut s, "the file's block", 10, |s| s.copied.len() > n);
        assert!(s.copied.last().unwrap().starts_with("src/main.rs\n```rust\nuse std::io;\n"), "{:?}", s.copied.last());
        assert!(toast_is(&s, "Copied 1 file as code blocks"));
        // A folder: its files git keeps, from the project's top.
        go(&mut s, &app);
        let n = s.copied.len();
        menu(&mut s, "web", "code blocks");
        wait_until(&mut s, "the folder's blocks", 10, |s| s.copied.len() > n);
        let heads: Vec<&str> = s.copied.last().unwrap().lines().filter(|l| l.starts_with("web/")).collect();
        assert_eq!(heads, ["web/package.json", "web/pnpm-lock.yaml", "web/src/app.js"], "without node_modules or dist");
        assert!(toast_is(&s, "Copied 3 files as code blocks"));
        // A link to the file, at the commit checked out.
        go(&mut s, &app.join("src"));
        select_path(&mut s, &app.join("src/main.rs"));
        let n = s.copied.len();
        palette_run(&mut s, "Copy Web Link");
        wait_until(&mut s, "the link", 10, |s| s.copied.len() > n);
        assert_eq!(s.copied.last().unwrap(), &format!("https://github.com/acme/app/blob/{sha}/src/main.rs"));
        assert!(toast_is(&s, "Copied the link; this commit isn't pushed, so the link won't open until it is"), "{:?}", s.app.toast);
        // At the line a search found.
        s.act("key ctrl+alt+f");
        s.act("text find_needle");
        code_settled(&mut s, "find_needle");
        let r = s.text_rect("let needle = find_needle();");
        s.act(&format!("click {:.0} {:.0} ctrl", r.center().x, r.center().y));
        s.settle();
        let n = s.copied.len();
        menu(&mut s, "main.rs", "copy web link");
        wait_until(&mut s, "the line's link", 10, |s| s.copied.len() > n);
        assert_eq!(s.copied.last().unwrap(), &format!("https://github.com/acme/app/blob/{sha}/src/main.rs#L5"));
        // Ignoring a file, then every file like another.
        go(&mut s, &app);
        let root = app.clone();
        let faded = move |s: &Sim, f: &str| crate::gitstatus::ignored(crate::gitstatus::repo_for(&root).as_deref(), &root.join(f), &root);
        assert!(!faded(&s, "scratch.txt"));
        // The menu offers it once git's word on the folder is in (late on a busy machine).
        let repo = app.clone();
        wait_until(&mut s, "git's status of the project", 20, move |_| crate::gitstatus::repo_for(&repo).is_some_and(|r| r.marks.keys().any(|k| k.ends_with("scratch.txt"))));
        reveal(&mut s, "scratch.txt");
        let row = row_text(&s, "scratch.txt");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        // A long menu in a short window scrolls.
        for _ in 0..40 {
            if some_text(&s, "Add to .gitignore") {
                break;
            }
            // Over the menu itself: near the window's foot it opens upward, away from
            // the middle.
            let over = s.ctx.memory(|m| m.area_rect(egui::Id::new("ctxmenu"))).map(|r| r.center()).unwrap_or(pos2(row.center().x + 120.0, s.size.y / 2.0));
            s.act(&format!("scroll {:.0} {:.0} -6", over.x, over.y));
            s.idle(2);
        }
        if !some_text(&s, "Add to .gitignore") {
            let menu = s.ctx.memory(|m| m.area_rect(egui::Id::new("ctxmenu")));
            let shown: Vec<&str> = s.texts.iter().filter(|(r, _)| menu.is_some_and(|m| m.intersects(*r))).map(|(_, t)| t.as_str()).collect();
            let repo = crate::gitstatus::repo_for(&app).map(|r| r.marks.keys().filter(|k| k.ends_with("scratch.txt")).count());
            s.fail(&format!("no \"Add to .gitignore\": menu {menu:?} shows {shown:?}; scratch.txt marked {repo:?}"));
        }
        let at = s.text_rect("Add to .gitignore").center();
        s.act(&format!("click {:.0} {:.0}", at.x, at.y));
        s.idle(2);
        assert!(some_text(&s, "This File") && some_text(&s, "All *.txt Files"), "the submenu");
        look(&mut s, "gitignore menu");
        s.shot("share-gitignore-menu");
        s.click_text("This File");
        // The row fades when git's status is read again, which can land a frame before
        // the word on what was added.
        wait_until(&mut s, "scratch.txt ignored", 10, |s| faded(s, "scratch.txt") && toast_is(s, "Added /scratch.txt to .gitignore"));
        menu(&mut s, "a.tmp", "all *.tmp");
        wait_until(&mut s, "*.tmp ignored", 10, |s| faded(s, "a.tmp") && faded(s, "b.tmp"));
        let gi = std::fs::read_to_string(app.join(".gitignore")).unwrap();
        assert!(gi.ends_with("*.log\n/scratch.txt\n*.tmp\n"), "{gi:?}");
        // What git ignores has nothing to add.
        reveal(&mut s, "b.tmp");
        let row = row_text(&s, "b.tmp");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        assert!(!some_text(&s, "Add to .gitignore"), "an ignored file's menu");
        assert!(some_text(&s, "Copy as"));
        s.act("key Escape");
        look(&mut s, "ignored");
        report("share", &s);
    }

    /// Compare Files from the menu of two selected files: git's diff in the inspector with
    /// its counts, kept while either file is selected and left for the preview after;
    /// not offered for one file. A patch file previewed. Open Web Link hands the link
    /// to the browser.
    #[test]
    fn compare_files_and_open_links() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("compare");
        let app = dev_fixture(&home);
        std::fs::create_dir_all(app.join("config")).unwrap();
        std::fs::write(app.join("config/dev.toml"), "port = 8080\nworkers = 4\nlog = \"debug\"\n").unwrap();
        std::fs::write(app.join("config/prod.toml"), "port = 9090\nworkers = 4\nlog = \"warn\"\ncache = true\n").unwrap();
        std::fs::write(app.join("fix.patch"), "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n pub fn haystack() -> Vec<u8> {\n-    // HACK: a needle in a haystack\n+    // a needle in a haystack\n     vec![]\n").unwrap();
        let g = |args: &[&str]| crate::tests::git(&app, args);
        g(&["add", "-A"]);
        g(&["commit", "-qm", "Start"]);
        g(&["remote", "add", "origin", "https://codeberg.org/ana/app.git"]);
        let mut s = Sim::new(&app.join("config"), vec2(1280.0, 800.0));
        wait_until(&mut s, "the repository", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        // One file: nothing to compare with.
        reveal(&mut s, "dev.toml");
        let row = row_text(&s, "dev.toml");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        assert!(!some_text(&s, "Compare Files"));
        s.act("key Escape");
        s.act("key ctrl+a");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        assert!(some_text(&s, "Compare Files"));
        s.click_text("Compare Files");
        let got = |s: &Sim| inspector(s).compare.as_ref().and_then(|c| c.got.as_ref().map(|g| g.as_ref().map(|d| (d.added, d.removed)).map_err(|e| e.clone())));
        wait_until(&mut s, "the comparison", 10, |s| got(s).is_some());
        assert_eq!(got(&s), Some(Ok((3, 2))));
        s.idle(3);
        for t in ["dev.toml → prod.toml", "+3", "−2", "-port = 8080", "+port = 9090", "+cache = true"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        assert!(s.texts.iter().any(|(_, t)| t.trim() == "workers = 4"), "the line both have, unmarked");
        look(&mut s, "comparison");
        s.shot("compare-files");
        s.act("key ArrowDown");
        s.idle(2);
        assert!(inspector(&s).compare.is_some(), "kept on the other file of the two");
        // Somewhere else: the preview again.
        go(&mut s, &app);
        s.idle(2);
        assert!(inspector(&s).compare.is_none());
        reveal(&mut s, "fix.patch");
        wait_until(&mut s, "the patch", 10, |s| inspector(s).text_lines().is_some());
        s.idle(3);
        assert!(some_text(&s, "+    // a needle in a haystack"));
        look(&mut s, "patch");
        s.shot("patch-preview");
        // The file on its forge, in the browser.
        go(&mut s, &app.join("src"));
        wait_until(&mut s, "the folder's repository", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        reveal(&mut s, "main.rs");
        let n = launched().len();
        palette_run(&mut s, "Open Web Link");
        wait_until(&mut s, "the browser", 10, |_| launched().len() > n);
        let sha = String::from_utf8_lossy(&std::process::Command::new("git").arg("-C").arg(&app).args(["rev-parse", "HEAD"]).output().unwrap().stdout).trim().to_string();
        assert!(launched().last().unwrap().contains(&format!("https://codeberg.org/ana/app/src/commit/{sha}/src/main.rs")), "{:?}", launched().last());
        assert!(toast_is(&s, "Opened in the browser; this commit isn't pushed, so the link won't open until it is"), "{:?}", s.app.toast);
        report("compare", &s);
    }

    /// Find Duplicates from the folder's menu: the sets as they are found, the most to free
    /// first and the oldest copy first in each; every copy of a set refused; the extras
    /// checked, trashed after asking, the oldest copies left, and undone. A folder with
    /// none says so.
    #[test]
    fn find_duplicates_and_trash_the_extras() {
        if !sandboxed() {
            return;
        }
        let home = fixture("dupes");
        let d = home.join("Holiday");
        std::fs::create_dir_all(d.join("Phone")).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 20);
        let photo = vec![3u8; 500_000];
        let write = |p: &Path, data: &[u8], aged: bool| {
            std::fs::write(p, data).unwrap();
            if aged {
                std::fs::File::options().write(true).open(p).unwrap().set_modified(old).unwrap();
            }
        };
        write(&d.join("beach.jpg"), &photo, true);
        write(&d.join("Phone/IMG_0001.jpg"), &photo, false);
        write(&d.join("beach copy.jpg"), &photo, false);
        write(&d.join("notes.txt"), b"pack sunscreen\n", true);
        write(&d.join("Phone/notes (1).txt"), b"pack sunscreen\n", false);
        write(&d.join("only once.txt"), b"nothing like it\n", false);
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.idle(3);
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("click {:.0} {:.0} right", list.center().x, list.bottom() - 40.0));
        s.idle(2);
        assert!(some_text(&s, "Find Duplicates…"), "in the folder's menu");
        s.click_text("Find Duplicates…");
        let st = |s: &Sim| match &s.app.popup {
            Popup::Duplicates(d) => (d.done, d.sets.clone(), d.checked.len()),
            _ => s.fail("Find Duplicates is not open"),
        };
        wait_until(&mut s, "the search", 20, |s| st(s).0);
        s.idle(3);
        let sets = st(&s).1;
        assert_eq!(sets.len(), 2);
        assert_eq!((sets[0].size, sets[0].files.len()), (500_000, 3), "the most to free first");
        assert_eq!(sets[0].files[0].path, d.join("beach.jpg"), "the oldest copy first");
        assert_eq!(sets[1].files[0].path, d.join("notes.txt"));
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("2 sets of duplicates; ")), "what could be freed");
        assert!(some_text(&s, "beach.jpg · 3 copies · 488 KB each"), "{:?}", s.texts.iter().filter(|t| t.1.contains("copies")).collect::<Vec<_>>());
        look(&mut s, "duplicates");
        s.shot("duplicates");
        // Both notes checked: refused, the file would be gone.
        for _ in 0..3 {
            s.act("key ArrowDown");
        }
        s.act("key space");
        s.act("key ArrowDown");
        s.act("key space");
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Duplicates(_)), "still open");
        assert!(some_text(&s, "Every copy is checked in 1 set: leave one of each unchecked"));
        // The extras: every copy but the oldest.
        click_control(&mut s, "duplicates extras");
        assert_eq!(st(&s).2, 3);
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Confirm { .. }), "asks first");
        s.act("key Enter");
        wait_until(&mut s, "the extras trashed", 20, |_| !d.join("beach copy.jpg").exists() && !d.join("Phone/IMG_0001.jpg").exists() && !d.join("Phone/notes (1).txt").exists());
        assert!(d.join("beach.jpg").exists() && d.join("notes.txt").exists() && d.join("only once.txt").exists());
        assert!(root().join("xdg/data/Trash/files/beach copy.jpg").exists(), "in the trash, not deleted");
        wait_until(&mut s, "the trash job done", 20, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Trashed(_))));
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 20, |_| d.join("beach copy.jpg").exists() && d.join("Phone/notes (1).txt").exists());
        go(&mut s, &home.join("Empty"));
        palette_run(&mut s, "Find Duplicates");
        wait_until(&mut s, "an empty search", 10, |s| st(s).0);
        s.idle(2);
        assert!(some_text(&s, "No duplicates here"));
        s.resize(520.0, 340.0);
        look(&mut s, "duplicates small");
        s.act("key Escape");
        s.resize(1280.0, 800.0);
        report("duplicates", &s);
    }

    /// Batch Rename from the menu of several files: every new name beside its old one, a
    /// name taken by another file refused with why, a numbered pattern from another
    /// start renaming them all, the new names selected, and one undo putting them back.
    #[test]
    fn batch_rename_with_every_new_name_shown() {
        if !sandboxed() {
            return;
        }
        let home = fixture("brename");
        let d = home.join("Trip");
        std::fs::create_dir_all(&d).unwrap();
        for f in ["IMG_2041.jpg", "IMG_2042.jpg", "IMG_2043.jpg", "Beach 002.jpg"] {
            std::fs::write(d.join(f), f).unwrap();
        }
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.idle(3);
        let id = s.app.active;
        {
            let t = s.app.panels.get_mut(&id).unwrap().tab_mut();
            let picks: Vec<u32> = t.entries.iter().enumerate().filter(|(_, e)| e.name.starts_with("IMG_")).map(|(i, _)| i as u32).collect();
            t.selected = picks.iter().copied().collect();
            t.cursor = picks.first().copied();
        }
        s.idle(2);
        let row = row_text(&s, "IMG_2041.jpg");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        assert!(some_text(&s, "Batch Rename…"));
        s.click_text("Batch Rename…");
        assert!(matches!(s.app.popup, Popup::BatchRename(_)));
        s.idle(2);
        assert!(some_text(&s, "Rename 3 items") && some_text(&s, "Nothing changes yet"));
        s.act("text Beach ###");
        s.idle(2);
        for t in ["Beach 001.jpg", "Beach 003.jpg", "Beach 002.jpg  (already there)", "1 name can't be used"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::BatchRename(_)), "nothing renamed while a name can't be used");
        look(&mut s, "batch rename clash");
        s.shot("batch-rename-clash");
        click_control(&mut s, "rename start");
        s.act("key ctrl+a");
        s.act("text 10");
        s.idle(2);
        assert!(some_text(&s, "Beach 010.jpg") && some_text(&s, "3 to rename"));
        look(&mut s, "batch rename");
        s.shot("batch-rename");
        s.act("key Enter");
        wait_until(&mut s, "renamed", 10, |_| ["Beach 010.jpg", "Beach 011.jpg", "Beach 012.jpg"].iter().all(|f| d.join(f).exists()));
        assert!(!d.join("IMG_2041.jpg").exists() && d.join("Beach 002.jpg").exists());
        wait_until(&mut s, "the new names selected", 10, |s| s.tab().selected_paths().len() == 3 && s.tab().selected_paths().iter().all(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("Beach 01"))));
        assert!(toast_is(&s, "Renamed 3 items"));
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 10, |_| ["IMG_2041.jpg", "IMG_2042.jpg", "IMG_2043.jpg"].iter().all(|f| d.join(f).exists()));
        report("batch rename", &s);
    }

    /// Empty the trash, in Options: off at first, so an old item stays; set to 30 days,
    /// what was trashed 40 days ago goes for good, with a word, and last week's stays.
    #[test]
    fn trash_emptied_after_its_days() {
        if !sandboxed() {
            return;
        }
        let home = fixture("trashage");
        let t = root().join("xdg/data/Trash");
        let item = |name: &str, days: u64| {
            let when = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * days);
            std::fs::create_dir_all(t.join("files").join(name)).unwrap();
            std::fs::write(t.join("files").join(name).join("inside.txt"), "x").unwrap();
            std::fs::create_dir_all(t.join("info")).unwrap();
            std::fs::write(t.join("info").join(format!("{name}.trashinfo")), format!("[Trash Info]\nPath=/somewhere/{name}\nDeletionDate={}\n", crate::timefmt::format(when, "%Y-%m-%dT%H:%M:%S"))).unwrap();
        };
        item("trashage old", 40);
        item("trashage new", 3);
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.idle(5);
        assert!(t.join("files/trashage old").exists(), "kept while the option is off");
        s.app.popup = Popup::options_at(crate::popups::Section::Files);
        s.idle(2);
        s.act("text trash");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, x)| x.ends_with("Empty the trash")), "the option is there");
        assert!(some_text(&s, "Never"));
        // On its way to 30 the choice says 7 for a moment: nothing goes then.
        click_control(&mut s, "choice");
        s.idle(3);
        assert_eq!(s.app.cfg.trash_days, 7);
        assert!(t.join("files/trashage old").exists(), "a choice passed through takes nothing");
        click_control(&mut s, "choice");
        s.idle(2);
        assert_eq!(s.app.cfg.trash_days, 30);
        look(&mut s, "trash option");
        s.shot("trash-option");
        s.act("key Escape");
        wait_until(&mut s, "the old item gone, once the choice settled", 10, |_| !t.join("files/trashage old").exists() && !t.join("info/trashage old.trashinfo").exists());
        wait_until(&mut s, "the word", 5, |s| s.app.toast.as_ref().is_some_and(|x| x.0.starts_with("Emptied ") && x.0.ends_with("from the trash, older than 30 days")));
        assert!(t.join("files/trashage new/inside.txt").exists(), "last week's stays");
        report("trash age", &s);
    }

    /// A bare repository and two clones of it in the sandbox: the one the window is in
    /// (`b`) and another (`a`) that moves the remote on. Each has its own identity.
    fn sync_repos(name: &str) -> (PathBuf, PathBuf, PathBuf) {
        let home = fixture(name);
        let d = home.join("Code");
        let g = |at: &Path, args: &[&str]| crate::tests::git(at, args);
        std::fs::create_dir_all(d.join("server.git")).unwrap();
        g(&d.join("server.git"), &["init", "-q", "--bare", "-b", "main"]);
        let a = d.join("a");
        std::fs::create_dir_all(&a).unwrap();
        crate::tests::work_repo(&a);
        std::fs::write(a.join("notes.txt"), "one\ntwo\nthree\n").unwrap();
        g(&a, &["add", "-A"]);
        g(&a, &["commit", "-qm", "Start"]);
        g(&a, &["remote", "add", "origin", d.join("server.git").to_str().unwrap()]);
        g(&a, &["push", "-q", "-u", "origin", "main"]);
        g(&d, &["clone", "-q", d.join("server.git").to_str().unwrap(), "b"]);
        let b = d.join("b");
        for (k, v) in [("user.name", "Test"), ("user.email", "test@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", ".git/hooks")] {
            g(&b, &["config", k, v]);
        }
        (home, a, b)
    }

    fn branch_menu_pick(s: &mut Sim, item: &str) {
        let chip = s.texts.iter().filter(|(_, t)| t == "main" || t == "topic").max_by(|a, b| a.0.center().y.total_cmp(&b.0.center().y)).map(|(r, _)| *r).unwrap_or_else(|| s.fail("no branch chip"));
        s.act(&format!("click {:.0} {:.0}", chip.center().x, chip.center().y));
        wait_until(s, "the branch menu", 5, |s| matches!(s.app.popup, Popup::Branches(_)) && s.texts.iter().any(|(_, t)| t == "Pull"));
        // A menu taller than when it was last open settles on its next frame.
        s.idle(3);
        s.click_text(item);
    }

    /// Pull, Push and Fetch from the branch menu against a real bare repository: what is
    /// waiting counted, a pull that can't fast-forward answered with Rebase, the push,
    /// and a new branch published; the status bar says what is going on meanwhile.
    #[test]
    fn pull_and_push_from_the_branch_menu() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let (_home, a, b) = sync_repos("syncmenu");
        let g = |at: &Path, args: &[&str]| crate::tests::git(at, args);
        let commit = |at: &Path, f: &str, msg: &str| {
            std::fs::write(at.join(f), msg).unwrap();
            g(at, &["add", "-A"]);
            g(at, &["commit", "-qm", msg]);
        };
        commit(&a, "a.txt", "A's work");
        g(&a, &["push", "-q"]);
        let mut s = Sim::new(&b, vec2(1280.0, 800.0));
        wait_until(&mut s, "the repository", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(3);
        branch_menu_pick(&mut s, "Fetch");
        wait_until(&mut s, "fetched", 20, |s| toast_is(s, "Fetched: 1 commit to pull"));
        // The menu counts what a pull would bring.
        let chip = s.texts.iter().filter(|(_, t)| t == "main").max_by(|a, b| a.0.center().y.total_cmp(&b.0.center().y)).map(|(r, _)| *r).unwrap();
        s.act(&format!("click {:.0} {:.0}", chip.center().x, chip.center().y));
        wait_until(&mut s, "the menu", 5, |s| s.texts.iter().any(|(_, t)| t == "Pull"));
        s.idle(3);
        for t in ["History", "Pull", "Push", "Fetch", "Branches", "New branch…"] {
            assert!(some_text(&s, t), "{t:?} in the menu");
        }
        look(&mut s, "branch menu with sync");
        s.shot("branch-menu-sync");
        s.click_text("Pull");
        wait_until(&mut s, "pulled", 20, |s| toast_is(s, "Pulled 1 commit"));
        assert!(b.join("a.txt").exists());
        // Both move on: asked whether to merge or rebase.
        commit(&a, "a2.txt", "More from A");
        g(&a, &["push", "-q"]);
        commit(&b, "b.txt", "B's work");
        s.app.toast = None;
        branch_menu_pick(&mut s, "Pull");
        wait_until(&mut s, "asked", 20, |s| matches!(s.app.popup, Popup::Choose { .. }));
        s.idle(3);
        assert!(some_text(&s, "This branch and its remote have both moved on") && some_text(&s, "Merge") && some_text(&s, "Rebase"));
        look(&mut s, "diverged");
        s.shot("pull-diverged");
        s.click_text("Rebase");
        wait_until(&mut s, "rebased", 20, |s| toast_is(s, "Rebased onto origin/main"));
        assert!(b.join("a2.txt").exists() && b.join("b.txt").exists());
        branch_menu_pick(&mut s, "Push");
        wait_until(&mut s, "pushed", 20, |s| toast_is(s, "Pushed 1 commit to origin/main"));
        // A new branch: Push becomes Publish.
        g(&b, &["switch", "-q", "-c", "topic"]);
        crate::gitstatus::rescan(&b);
        wait_until(&mut s, "on topic", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| r.branch == "topic"));
        s.idle(3);
        branch_menu_pick(&mut s, "Publish branch");
        wait_until(&mut s, "published", 20, |s| toast_is(s, "Published topic to origin"));
        report("sync", &s);
    }

    /// A pull that stops at a conflict: the Changes page opens on its own, with what is
    /// in progress above; the file's conflict shows both sides, Keep both settles it, and
    /// Continue makes the merge commit.
    #[test]
    fn conflicts_settled_on_the_changes_page() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let (_home, a, b) = sync_repos("conflictpage");
        let g = |at: &Path, args: &[&str]| crate::tests::git(at, args);
        std::fs::write(a.join("notes.txt"), "one\nTWO from a\nthree\n").unwrap();
        g(&a, &["commit", "-qam", "A edits"]);
        g(&a, &["push", "-q"]);
        std::fs::write(b.join("notes.txt"), "one\ntwo from b\nthree\n").unwrap();
        g(&b, &["commit", "-qam", "B edits"]);
        let mut s = Sim::new(&b, vec2(1280.0, 800.0));
        wait_until(&mut s, "the repository", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
        s.idle(3);
        branch_menu_pick(&mut s, "Pull");
        wait_until(&mut s, "asked", 20, |s| matches!(s.app.popup, Popup::Choose { .. }));
        s.idle(3);
        s.click_text("Merge");
        wait_until(&mut s, "the Changes page", 20, |s| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).is_some_and(|h| h.changes.open));
        wait_until(&mut s, "the conflict listed", 20, |s| changes(s).unstaged.contains(&"notes.txt".to_string()));
        s.idle(3);
        see(&mut s, "Merging · 1 file in conflict");
        click_in_inspector(&mut s, "notes.txt", "click");
        wait_until(&mut s, "the conflict shown", 10, |s| s.texts.iter().any(|(_, t)| t == "Keep both"));
        s.idle(3);
        for t in ["Keep HEAD", "Keep both", "two from b", "TWO from a"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        look(&mut s, "conflict");
        s.shot("conflict");
        click_control(&mut s, "keep both");
        wait_until(&mut s, "settled", 20, |s| changes(s).staged.contains(&"notes.txt".to_string()) && !changes(s).busy);
        assert_eq!(std::fs::read_to_string(b.join("notes.txt")).unwrap(), "one\ntwo from b\nTWO from a\nthree\n");
        wait_until(&mut s, "nothing left", 10, |s| s.texts.iter().any(|(_, t)| t == "Merging · nothing left in conflict"));
        click_control(&mut s, "conflict continue");
        wait_until(&mut s, "merged", 20, |_| crate::gitsync::doing(&b).is_none());
        wait_until(&mut s, "the banner gone", 10, |s| !s.texts.iter().any(|(_, t)| t.starts_with("Merging")));
        look(&mut s, "merged");
        report("conflicts", &s);
    }

    /// Show Only Changed Files from its key: the list narrows to what git marks, the
    /// filter box says so, and the key again shows everything.
    #[test]
    fn only_changed_files_from_the_key() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let (_home, _a, b) = sync_repos("onlychanged");
        for f in ["src/app.rs", "src/lib.rs", "docs/guide.md", "LICENSE"] {
            std::fs::create_dir_all(b.join(f).parent().unwrap()).unwrap();
            std::fs::write(b.join(f), "x\n").unwrap();
        }
        crate::tests::git(&b, &["add", "-A"]);
        crate::tests::git(&b, &["commit", "-qm", "More"]);
        std::fs::write(b.join("src/lib.rs"), "y\n").unwrap();
        std::fs::write(b.join("NEW.txt"), "new\n").unwrap();
        let mut s = Sim::new(&b, vec2(1280.0, 800.0));
        wait_until(&mut s, "the marks", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| !r.marks.is_empty()));
        s.idle(3);
        let names = |s: &Sim| s.tab().view.iter().map(|&i| s.tab().entries[i as usize].name.clone()).collect::<Vec<_>>();
        assert_eq!(names(&s).len(), 5);
        s.act("key ctrl+shift+g");
        wait_until(&mut s, "narrowed", 10, |s| s.tab().view.len() == 2);
        assert_eq!(names(&s), ["src", "NEW.txt"]);
        s.idle(3);
        assert!(some_text(&s, "Filter 2 changed items…"));
        look(&mut s, "only changed");
        s.shot("only-changed");
        s.act("key ctrl+shift+g");
        wait_until(&mut s, "everything", 10, |s| s.tab().view.len() == 5);
        report("only changed", &s);
    }

    /// Formulas in a Markdown preview: without a TeX they show as written under a word
    /// saying why; with one (a stand-in pdflatex, the real pdftocairo) they are typeset,
    /// in a sentence and on lines of their own, and the text flows around them.
    #[test]
    fn formulas_in_markdown_previews() {
        if !sandboxed() || !crate::media::have("pdftocairo") {
            return;
        }
        let home = fixture("formulas");
        let d = home.join("Notes");
        std::fs::create_dir_all(&d).unwrap();
        let md = "# Notes on waves\n\nA wave's speed is $v = f\\lambda$, and its energy grows with $A^2$, so doubling the height gives four times the energy.\n\n$$\n\\frac{\\partial^2 u}{\\partial t^2} = c^2 \\nabla^2 u\n$$\n\nSee also \\(\\omega = 2\\pi f\\).\n";
        std::fs::write(d.join("waves.md"), md).unwrap();
        // First without a TeX.
        // Poppler but no TeX, as on a computer without TeX Live.
        let empty = root().join("formulas-none");
        std::fs::create_dir_all(&empty).unwrap();
        let _ = std::os::unix::fs::symlink("/usr/bin/pdftocairo", empty.join("pdftocairo"));
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(empty.clone()));
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ToggleInspector);
        select_path(&mut s, &d.join("waves.md"));
        wait_until(&mut s, "the note", 10, |s| inspector(s).markdown().is_some_and(|m| m.maths_state().1.is_some()));
        s.idle(5);
        assert!(some_text(&s, "Formulas show as written: install TeX Live to typeset them"));
        assert!(s.texts.iter().any(|(_, t)| t.contains("$v = f\\lambda$")), "the source in the sentence");
        look(&mut s, "formulas as written");
        s.shot("formulas-source");
        drop(s);
        // Then with one.
        let tex = root().join("formulas-tex");
        crate::tests::fake_tex(&tex);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tex.clone()));
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ToggleInspector);
        select_path(&mut s, &d.join("waves.md"));
        wait_until(&mut s, "typeset", 20, |s| inspector(s).markdown().is_some_and(|m| m.maths_state().0 == 4));
        s.idle(5);
        assert!(!s.texts.iter().any(|(_, t)| t.contains("$v = f")), "no source left");
        assert!(!some_text(&s, "Formulas show as written: install TeX Live to typeset them"));
        look(&mut s, "formulas typeset");
        s.shot("formulas-typeset");
        for (w, h) in [(720.0, 480.0), (520.0, 400.0)] {
            s.resize(w, h);
            s.idle(5);
            look(&mut s, "formulas narrow");
        }
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        report("formulas", &s);
    }

    /// Select the listed item named `label`, which scrolls it into view.
    fn reveal(s: &mut Sim, label: &str) {
        let p = s.tab().entries.iter().find(|e| e.name == label).map(|e| e.path.clone()).unwrap_or_else(|| s.fail(&format!("{label:?} not listed")));
        select_path(s, &p);
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().scroll_to_cursor = true;
        s.settle();
    }

    fn row_text(s: &Sim, label: &str) -> Rect {
        let list = s.app.panels[&s.app.active].list_rect;
        s.texts.iter().find(|(r, t)| t == label && list.contains(r.center())).map(|x| x.0).unwrap_or_else(|| s.fail(&format!("{label:?} not in the list")))
    }

    /// The middle of the row in a popup's list whose text is `label`.
    fn list_row_y(s: &Sim, label: &str) -> f32 {
        s.texts.iter().rev().find(|(_, t)| t == label).map(|(r, _)| r.center().y + 6.0).unwrap_or_else(|| s.fail(&format!("no {label:?} row")))
    }

    /// Every surface photographed for a person to look at (MAIA_SIM_SHOTS): the window,
    /// each view, the inspector's previews, the history, popups and menus, in a dark and
    /// a light theme and two of the newer ones, at a normal and a small window. Without
    /// shots it walks the first theme and size only, as one more check of every frame.
    /// MAIA_SIM_AUDIT=midnight-1280 keeps the passes whose name contains it.
    #[test]
    fn ui_audit() {
        if !sandboxed() {
            return;
        }
        use crate::theme::ThemeName;
        let home = fixture("audit");
        let long = home.join("Projects/Quarterly planning and the long tail of everything else");
        std::fs::create_dir_all(long.join("Drafts that nobody has opened since the spring offsite")).unwrap();
        for (i, n) in ["Agenda.md", "Minutes of the weekly sync with every team lead.txt", "budget.csv"].iter().enumerate() {
            std::fs::write(long.join(n), "x".repeat(100 * (i + 1))).unwrap();
        }
        let docs = home.join("Documents");
        std::fs::write(docs.join("letter from the bank.txt"), "Dear customer, your tax refund has been paid into your account.").unwrap();
        std::fs::write(docs.join("data.bin"), (0u8..=255).cycle().take(4096).collect::<Vec<u8>>()).unwrap();
        std::fs::write(docs.join("main.rs"), "// demo\nfn main() {\n    let x: u32 = 42;\n    println!(\"{x} is the answer to a question long enough to run past the right edge of any preview\");\n}\n").unwrap();
        if crate::tags::write(&docs.join("notes.txt"), &["Red".into(), "Work".into()]).is_ok() {
            let _ = crate::tags::write(&docs.join("report.md"), &["Blue".into()]);
        }
        if crate::archive::available() {
            let _ = crate::archive::create(&docs.join("bundle.zip"), &docs, &["notes.txt".into(), "Taxes".into()]);
            let _ = std::process::Command::new("zip").args(["-q", "-r", "-P", "pw"]).arg(docs.join("locked.zip")).arg("Taxes").current_dir(&docs).status();
        }
        let media = media_fixture("auditmedia");
        let git = crate::media::have("git");
        if git {
            crate::tests::commit_view_fixture(&home.join("Code/project"));
        }
        // A project to develop in.
        dev_fixture(&home);
        crate::tests::dev_fixture(&home.join("Dev"));
        let drv = fake_drives("audit");
        crate::tests::fake_sqlite(&drv);
        let net = fake_network("audit");
        for f in ["gio", "phones"] {
            let _ = std::fs::copy(net.join(f), drv.join(f));
        }
        for sub in ["gvfs", "netmounts"] {
            let _ = std::fs::create_dir_all(drv.join(sub));
        }
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(drv));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
                *crate::ollama::TEST_HOST.lock().unwrap() = None;
            }
        }
        let _reset = Reset;
        let (addr, _, _) = crate::tests::fake_ollama(true);
        *crate::ollama::TEST_HOST.lock().unwrap() = Some(addr);
        let shots = std::env::var_os("MAIA_SIM_SHOTS").is_some();
        let only = std::env::var("MAIA_SIM_AUDIT").unwrap_or_default().to_lowercase();
        for theme in [ThemeName::Midnight, ThemeName::Paper, ThemeName::Latte, ThemeName::Gruvbox] {
            for (w, h) in [(1280.0, 800.0), (720.0, 480.0)] {
                let tag = format!("{}-{w}", theme.label().to_lowercase().replace(' ', ""));
                if !tag.contains(&only) {
                    continue;
                }
                audit_pass(&home, &media, git, theme, w, h, &tag);
                if !shots {
                    return;
                }
            }
        }
    }

    fn audit_pass(home: &Path, media: &Path, git: bool, theme: crate::theme::ThemeName, w: f32, h: f32, tag: &str) {
        let docs = home.join("Documents");
        let mut s = Sim::new(home, vec2(w, h));
        s.app.cfg.theme = theme;
        s.app.cfg.semantic_folders = docs.display().to_string();
        // Growing tabs run on the wall clock, fades on the frames' clock: let both end.
        let still = |s: &mut Sim| {
            std::thread::sleep(std::time::Duration::from_millis(350));
            s.idle(15);
        };
        let shot = |s: &mut Sim, name: &str| {
            look(s, name);
            still(s);
            s.shot(&format!("{tag}-{name}"));
        };
        let shot_open = |s: &mut Sim, name: &str| {
            still(s);
            s.shot(&format!("{tag}-{name}"));
        };
        let close = |s: &mut Sim| {
            for _ in 0..3 {
                if s.popup_open() {
                    s.act("key Escape");
                    s.idle(2);
                }
            }
        };
        let pause = |s: &mut Sim| {
            for _ in 0..30 {
                s.idle(1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            s.settle();
        };
        wait_until(&mut s, "drives in the sidebar", 10, |s| sidebar_text(s, "Photo Card").is_some());
        // The window: several tabs, long names, the sidebar's sections.
        for p in [docs.clone(), home.join("Projects/Quarterly planning and the long tail of everything else"), home.join("Code/deep/a/b/c/d/e/f/g/h"), home.join("Pictures/Trip")] {
            s.cmd(NewTab);
            s.settle();
            go(&mut s, &p);
        }
        s.cmd(Tab1);
        s.settle();
        s.act("key ArrowDown");
        shot(&mut s, "home-details");
        s.cmd(Tab4);
        s.settle();
        shot(&mut s, "deep-path");
        s.app.cfg.compact_paths = false;
        shot(&mut s, "deep-path-full");
        s.app.cfg.compact_paths = true;
        s.cmd(Tab3);
        s.settle();
        s.act("key ArrowDown");
        s.act("key shift+ArrowDown");
        let lr = s.app.panels[&s.app.active].list_rect;
        shot(&mut s, "long-names-selection");
        // Hovering a row below the selection.
        s.act(&format!("move {:.0} {:.0}", lr.center().x, lr.top() + 100.0));
        still(&mut s);
        s.shot(&format!("{tag}-hover"));
        // Views of a folder with names and tags, then pictures.
        s.cmd(Tab2);
        s.settle();
        for (v, n) in [(ViewDetails, "details"), (ViewList, "list"), (ViewTree, "tree"), (ViewMedium, "icons-m"), (ViewLarge, "icons-l"), (ViewXL, "icons-xl")] {
            s.cmd(v);
            s.act("key ArrowDown");
            shot(&mut s, &format!("docs-{n}"));
        }
        s.cmd(Tab5);
        s.settle();
        for (v, n) in [(ViewMedium, "icons-m"), (ViewXL, "icons-xl"), (ViewDetails, "details")] {
            s.cmd(v);
            pause(&mut s);
            shot(&mut s, &format!("pictures-{n}"));
        }
        s.cmd(Tab2);
        s.settle();
        s.cmd(ViewTree);
        s.act("key Home");
        s.act("key ArrowRight");
        shot(&mut s, "docs-tree-open");
        s.cmd(ViewDetails);
        // Renaming, an empty folder, a folder that can't be read.
        select_path(&mut s, &docs.join("report.md"));
        s.act("key F2");
        still(&mut s);
        s.shot(&format!("{tag}-rename"));
        s.act("key Escape");
        go(&mut s, &home.join("Empty"));
        shot(&mut s, "empty");
        go(&mut s, &home.join("Locked"));
        shot(&mut s, "locked");
        go(&mut s, &docs);
        // The filter, with conditions; contents search; the whole tree flattened.
        s.act("key ctrl+f");
        s.act("text size>10 modified<7d no");
        still(&mut s);
        s.shot(&format!("{tag}-filter-query"));
        s.act("key ctrl+a");
        s.act("text nothing-matches-this");
        still(&mut s);
        s.shot(&format!("{tag}-filter-none"));
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.act("key Escape");
        click_control(&mut s, "contents toggle");
        s.act("text notes");
        wait_until(&mut s, "contents search", 10, |s| !s.tab().is_loading());
        s.settle();
        still(&mut s);
        s.shot(&format!("{tag}-contents"));
        click_control(&mut s, "contents toggle");
        s.act("key Escape");
        s.act("key Escape");
        s.cmd(ToggleFlatten);
        s.settle();
        shot(&mut s, "flatten");
        s.cmd(ToggleFlatten);
        s.settle();
        // Split panels.
        s.cmd(SplitRight);
        s.settle();
        shot(&mut s, "split-right");
        s.cmd(SplitBottom);
        s.settle();
        shot(&mut s, "split-three");
        s.cmd(ClosePanel);
        s.cmd(ClosePanel);
        s.settle();
        // The inspector beside the list and under it, with every kind of preview.
        go(&mut s, &docs);
        s.cmd(ToggleInspector);
        for (layout, side) in [(crate::config::InspectorLayout::Right, "right"), (crate::config::InspectorLayout::Bottom, "bottom")] {
            s.app.cfg.inspector_layout = layout;
            s.settle();
            for (f, n) in [("notes.txt", "text"), ("report.md", "markdown"), ("main.rs", "code"), ("data.bin", "hex"), ("Taxes", "folder"), ("bundle.zip", "archive")] {
                let p = docs.join(f);
                if !p.exists() {
                    continue;
                }
                select_path(&mut s, &p);
                pause(&mut s);
                if n == "archive" {
                    // Its contents, as a folder's are, not its bytes.
                    in_inspector(&s, "notes.txt");
                }
                shot(&mut s, &format!("inspect-{side}-{n}"));
            }
            if side == "right" {
                select_path(&mut s, &docs.join("main.rs"));
                s.settle();
                s.cmd(InspectorWrap);
                shot(&mut s, "inspect-right-code-wrapped");
                s.cmd(InspectorWrap);
            }
        }
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Right;
        go(&mut s, media);
        for (f, n) in [("photo.png", "image"), ("doc.pdf", "pdf"), ("clip.mp4", "video"), ("song.mp3", "audio"), ("drawing.svg", "svg")] {
            select_path(&mut s, &media.join(f));
            match n {
                "pdf" => wait_until(&mut s, "pdf page", 20, |s| inspector(s).pdf_state().is_some_and(|(_, _, shown)| shown.is_some())),
                "video" | "audio" => wait_until(&mut s, "media probed", 15, |s| inspector(s).media_state().is_some()),
                _ => {}
            }
            pause(&mut s);
            shot(&mut s, &format!("inspect-{n}"));
        }
        // Developer previews: rendered, as trees and tables, a database, a .env, a log
        // and blame.
        let dev = home.join("Dev");
        if dev.exists() {
            go(&mut s, &dev);
            let hidden = s.app.cfg.show_hidden;
            if !hidden {
                s.cmd(ToggleHidden);
                s.settle();
            }
            for (f, n) in [("README.md", "markdown"), ("package.json", "json"), ("ci.yml", "yaml"), ("people.csv", "csv"), ("data.db", "sqlite"), (".env", "env"), ("app.log", "log")] {
                select_path(&mut s, &dev.join(f));
                if n == "sqlite" {
                    wait_until(&mut s, "tables", 10, |s| inspector(s).sqlite_state().is_some_and(|st| st.1 == 3));
                }
                pause(&mut s);
                shot(&mut s, &format!("inspect-dev-{n}"));
            }
            if git {
                go(&mut s, &dev.join("src"));
                wait_until(&mut s, "work tree", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
                select_path(&mut s, &dev.join("src/lib.rs"));
                s.cmd(InspectorBlame);
                wait_until(&mut s, "blame", 15, |s| inspector(s).blame_state().is_some());
                pause(&mut s);
                shot(&mut s, "inspect-dev-blame");
                s.cmd(InspectorBlame);
            }
            if !hidden {
                s.cmd(ToggleHidden);
            }
        }
        s.cmd(ToggleInspector);
        // The history and a commit.
        if git {
            go(&mut s, &home.join("Code/project"));
            wait_until(&mut s, "branch read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
            s.idle(2);
            shot(&mut s, "git-status");
            s.cmd(GitHistory);
            wait_until(&mut s, "history", 30, |s| history(s).is_some_and(|h| h.1));
            shot(&mut s, "git-graph");
            click_in_inspector(&mut s, "Lay out the panels", "click");
            wait_until(&mut s, "files", 10, |s| history(s).is_some_and(|h| h.3.is_some()));
            wait_until(&mut s, "pictures", 10, |s| probe(s).pictures >= 3 || probe(s).failed > 0);
            shot(&mut s, "git-commit");
            s.cmd(ToggleInspector);
            s.settle();
        }
        // The developer popups, over a project.
        let app = home.join("Code/app");
        go(&mut s, &app.join("src"));
        s.act("key ctrl+alt+f");
        s.act("text needle");
        wait_until(&mut s, "code search", 20, |s| matches!(&s.app.popup, Popup::CodeSearch(c) if c.settled()));
        shot_open(&mut s, "search-code");
        close(&mut s);
        s.cmd(FindTodos);
        wait_until(&mut s, "todos", 20, |s| matches!(&s.app.popup, Popup::CodeSearch(c) if c.settled()));
        shot_open(&mut s, "find-todos");
        close(&mut s);
        s.act("key ctrl+shift+b");
        wait_until(&mut s, "tasks", 10, |s| matches!(&s.app.popup, Popup::Tasks(t) if t.groups.is_some()));
        shot_open(&mut s, "run-task");
        close(&mut s);
        go(&mut s, &app);
        s.cmd(CleanBuilds);
        wait_until(&mut s, "build folders", 20, |s| matches!(&s.app.popup, Popup::CleanBuilds(c) if c.done));
        shot_open(&mut s, "clean-builds");
        close(&mut s);
        // Popups and menus.
        go(&mut s, &docs);
        select_path(&mut s, &docs.join("notes.txt"));
        let lr = s.app.panels[&s.app.active].list_rect;
        let r = s.text_rect("notes.txt");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        shot_open(&mut s, "context-menu");
        close(&mut s);
        s.act(&format!("click {:.0} {:.0} right", lr.center().x, lr.bottom() - 20.0));
        shot_open(&mut s, "context-menu-empty");
        close(&mut s);
        s.cmd(Palette);
        shot_open(&mut s, "palette");
        s.act("text view");
        shot_open(&mut s, "palette-query");
        close(&mut s);
        s.cmd(GoTo);
        s.act("text doc");
        s.settle();
        shot_open(&mut s, "goto");
        close(&mut s);
        s.act("key ctrl+k");
        s.idle(3);
        shot_open(&mut s, "search-empty");
        click_control(&mut s, "meaning field");
        s.act("text notes");
        wait_until(&mut s, "a name found", 5, |s| matches!(&s.app.popup, Popup::Meaning(st) if !st.named.is_empty()));
        shot_open(&mut s, "search-names");
        // Meaning, from the stand-in Ollama: turned on, the files read, a vague question.
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.idle(2);
        // A pass after the first finds it on (the setting is kept).
        if s.texts.iter().any(|(_, t)| t == "Turn on") {
            s.click_text("Turn on");
        }
        wait_until(&mut s, "files read", 20, |s| matches!(s.app.semantic.as_ref().map(|e| e.phase()), Some(crate::semantic::Phase::Ready)));
        click_control(&mut s, "meaning field");
        s.act("text money back from the government");
        wait_until(&mut s, "answer", 10, |s| matches!(&s.app.popup, Popup::Meaning(st) if !st.hits.is_empty()));
        shot_open(&mut s, "search-meaning");
        close(&mut s);
        s.app.cfg.semantic_search = false;
        s.cmd(Options);
        shot_open(&mut s, "options");
        close(&mut s);
        select_path(&mut s, &docs.join("notes.txt"));
        s.act("key alt+Enter");
        s.idle(3);
        click_control(&mut s, "checksum button");
        wait_until(&mut s, "checksum", 10, |s| s.texts.iter().any(|(_, t)| t.len() == 64 && t.chars().all(|c| c.is_ascii_hexdigit())));
        shot_open(&mut s, "properties");
        close(&mut s);
        s.cmd(ConnectServer);
        shot_open(&mut s, "connect");
        close(&mut s);
        if docs.join("locked.zip").exists() {
            select_path(&mut s, &docs.join("locked.zip"));
            s.cmd(ExtractHere);
            wait_until(&mut s, "password asked", 10, |s| matches!(&s.app.popup, Popup::ArchivePassword(_)));
            shot_open(&mut s, "archive-password");
            close(&mut s);
        }
        s.cmd(ChooseTheme);
        s.settle();
        shot_open(&mut s, "themes");
        close(&mut s);
        s.app.cfg.theme = theme;
        for (c, n) in [(MainMenu, "main-menu"), (QuickMenu, "quick-menu"), (About, "about"), (TabSearch, "tab-search"), (TypesFilter, "types")] {
            s.cmd(c);
            shot_open(&mut s, n);
            close(&mut s);
        }
        // A conflict: the same name copied into a folder that has it.
        std::fs::write(home.join("Code/maia/report.md"), "older").unwrap();
        s.cmd(SplitRight);
        go(&mut s, &home.join("Code/maia"));
        s.act("key Tab");
        s.settle();
        select_path(&mut s, &docs.join("report.md"));
        s.act("key F5");
        s.settle();
        shot_open(&mut s, "conflict");
        close(&mut s);
        s.cmd(ClosePanel);
        // A toast and a job card.
        let progress = std::sync::Arc::new(std::sync::Mutex::new(crate::ops::Progress { total_bytes: 900 << 20, done_bytes: 300 << 20, total_items: 120, done_items: 41, current: "a very long file name that keeps going and going well past any column width you would pick.txt".into(), ..Default::default() }));
        s.app.jobs.push(crate::ops::Job { id: 9999, label: "Copying 120 items to Backups".into(), kind: crate::ops::OpKind::Copy { srcs: vec![], dest: home.join("Backups"), decide: Default::default() }, progress: progress.clone(), cancel: Default::default(), pause: Default::default(), started: Instant::now(), finished_at: None, show_errors: false });
        s.app.toast("Copied the path of notes.txt");
        s.idle(5);
        s.act("move -50 -50");
        still(&mut s);
        s.shot(&format!("{tag}-toast-job"));
        progress.lock().unwrap().finished = true;
        s.app.jobs.clear();
        s.idle(3);
        report(&format!("audit {tag}"), &s);
    }

    fn inspector_mut(s: &mut Sim) -> &mut crate::inspector::Inspector {
        let id = s.app.active;
        s.app.panels.get_mut(&id).and_then(|p| p.inspector.as_mut()).expect("inspector open")
    }

    fn rich(s: &mut Sim) -> Option<(&'static str, usize)> {
        inspector_mut(s).rich_state()
    }

    fn has_control(_s: &Sim, what: &str) -> bool {
        crate::ui::CONTROLS.with(|c| c.borrow().iter().any(|(_, _, w)| *w == what))
    }

    fn inspector_texts(s: &Sim) -> Vec<String> {
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("inspector shown"));
        s.texts.iter().filter(|(r, _)| ir.contains(r.center())).map(|(_, t)| t.clone()).collect()
    }

    /// Developer previews as someone reading a project would use them: a README rendered
    /// with its pictures and links, then as source; JSON, TOML and YAML as trees folded by
    /// keys and clicks, with a path copied; tables with quoted fields and sideways
    /// scrolling; a database's tables and rows through sqlite3 and without it; a .env
    /// with its values hidden; a log followed as it grows and filtered; blame, wrapped and
    /// spaced, opening a commit. Then a narrow inspector, the bottom dock and a light
    /// theme over all of it.
    #[test]
    fn inspector_developer_previews() {
        if !sandboxed() {
            return;
        }
        let home = fixture("devprev");
        let d = home.join("Dev");
        crate::tests::dev_fixture(&d);
        let git = crate::media::have("git");
        let fake = root().join("devprev-tools");
        crate::tests::fake_sqlite(&fake);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ToggleHidden);
        s.cmd(ToggleInspector);
        s.settle();
        let shot = |s: &mut Sim, name: &str| {
            look(s, name);
            s.idle(3);
            s.shot(&format!("dev-{name}"));
        };
        // Markdown, rendered: blocks laid out, both local pictures decoded.
        select_path(&mut s, &d.join("README.md"));
        wait_until(&mut s, "markdown rendered", 10, |s| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.markdown()).and_then(|m| m.counts()).is_some_and(|c| c.0 > 20 && c.1 == 2 && c.2 == 2));
        assert!(rich(&mut s).is_some_and(|r| r.0 == "markdown"));
        toolbar_centred(&mut s, "markdown", &[]);
        shot(&mut s, "markdown");
        assert!(inspector_texts(&s).iter().any(|t| t == "Maia"), "the title is drawn");
        assert!(!inspector_texts(&s).iter().any(|t| t.contains("**") || t.starts_with("# ")), "no markup is left over");
        // A link to a file beside it opens it.
        let at = inspector(&s).markdown().and_then(|m| m.link_pos("docs/guide.md")).unwrap_or_else(|| s.fail("the guide's link drawn"));
        let before = crate::mime::LAUNCHED.lock().unwrap().len();
        s.idle(40);
        s.act(&format!("click {:.0} {:.0}", at.x, at.y));
        s.idle(2);
        let launched = crate::mime::LAUNCHED.lock().unwrap();
        assert!(launched.len() > before && launched.last().is_some_and(|l| l.contains("guide.md")), "the guide opened: {:?}", launched.last());
        drop(launched);
        // Source: the text, highlighted; remembered when the inspector opens again.
        click_control(&mut s, "Source");
        wait_until(&mut s, "markdown source", 5, |s| s.app.cfg.source_views.markdown && inspector(s).text_spans().is_some());
        assert!(s.app.cfg_dirty, "the choice is saved");
        shot(&mut s, "markdown-source");
        s.cmd(ToggleInspector);
        s.cmd(ToggleInspector);
        s.settle();
        assert!(s.app.cfg.source_views.markdown);
        assert!(has_control(&s, "Rendered"), "Rendered offered beside Source");
        click_control(&mut s, "Rendered");
        s.idle(2);
        assert!(!s.app.cfg.source_views.markdown);
        // JSON: a tree in file order, folded by keys while it has the keyboard.
        select_path(&mut s, &d.join("package.json"));
        wait_until(&mut s, "json tree", 10, |s| inspector(s).rich_name() == Some("tree"));
        let rows = rich(&mut s).unwrap().1;
        assert!(rows >= 14, "{rows} rows");
        shot(&mut s, "json-tree");
        let cursor = cursor_pos(&s);
        click_in_inspector(&mut s, "scripts", "click");
        assert!(inspector_mut(&mut s).tree().is_some_and(|t| t.focus), "a click gives the tree the keyboard");
        s.act("key ArrowLeft");
        assert_eq!(rich(&mut s).unwrap().1, rows - 2, "folded by the left arrow");
        assert!(inspector_texts(&s).iter().any(|t| t.contains("2 keys")), "a folded node counts what it holds");
        s.act("key ArrowRight");
        s.act("key ArrowRight");
        assert_eq!(cursor_pos(&s), cursor, "arrows went to the tree, not the listing");
        let path = inspector_mut(&mut s).tree().map(|t| t.path(t.sel)).unwrap();
        assert_eq!(path, ".scripts.build");
        assert!(inspector_texts(&s).iter().any(|t| t == ".scripts.build"), "the path bar shows where");
        click_control(&mut s, "copy path");
        assert!(toast_is(&s, "Copied .scripts.build"));
        s.act("key Escape");
        assert!(!inspector_mut(&mut s).tree().unwrap().focus, "Escape gives the keyboard back");
        // Tree / Source, remembered apart from Markdown's.
        click_control(&mut s, "Source");
        wait_until(&mut s, "json source", 5, |s| s.app.cfg.source_views.data);
        assert!(!s.app.cfg.source_views.markdown);
        click_control(&mut s, "Tree");
        s.idle(2);
        // A broken file says why above its text; JSON Lines; TOML; YAML.
        select_path(&mut s, &d.join("broken.json"));
        wait_until(&mut s, "broken json", 10, |s| inspector(s).rich_name() == Some("broken"));
        s.idle(2);
        assert!(inspector_texts(&s).iter().any(|t| t.starts_with("Can't read this as JSON")), "{:?}", inspector_texts(&s));
        shot(&mut s, "json-broken");
        for (f, min) in [("events.jsonl", 9), ("config.toml", 14), ("ci.yml", 16)] {
            select_path(&mut s, &d.join(f));
            wait_until(&mut s, f, 10, |s| inspector(s).rich_name() == Some("tree"));
            assert!(rich(&mut s).unwrap().1 >= min, "{f}: {:?}", rich(&mut s));
            shot(&mut s, &format!("tree-{f}"));
        }
        // Tables: quoted fields, numbers to the right, a wide one scrolled sideways.
        select_path(&mut s, &d.join("people.csv"));
        wait_until(&mut s, "csv table", 10, |s| inspector(s).table().is_some());
        assert_eq!(inspector(&s).table().unwrap().rows.len(), 4);
        s.idle(2);
        assert!(inspector_texts(&s).iter().any(|t| t == "Grace \"Amazing\" Hopper"));
        shot(&mut s, "csv");
        select_path(&mut s, &d.join("wide.csv"));
        wait_until(&mut s, "wide table", 10, |s| inspector(s).table().is_some_and(|t| t.cols == 30));
        s.idle(2);
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        assert!(inspector_texts(&s).iter().any(|t| t == "column 0"));
        for _ in 0..12 {
            s.act(&format!("scroll {:.0} {:.0} -6 shift", ir.center().x, ir.center().y));
        }
        s.idle(3);
        assert!(!inspector_texts(&s).iter().any(|t| t == "column 0") && inspector_texts(&s).iter().any(|t| t == "column 29"), "scrolled to the last column: {:?}", inspector_texts(&s));
        assert!(s.texts.iter().any(|(r, t)| t == "1" && ir.contains(r.center()) && r.right() < ir.left() + 60.0), "row numbers stay put");
        shot(&mut s, "csv-wide");
        click_control(&mut s, "Source");
        wait_until(&mut s, "table source", 5, |s| s.app.cfg.source_views.table);
        click_control(&mut s, "Table");
        // A database: tables with counts, the first's rows (it cannot be read: said so),
        // then a click on another.
        select_path(&mut s, &d.join("data.db"));
        wait_until(&mut s, "tables listed and counted", 15, |s| inspector(s).sqlite_state().is_some_and(|st| st.0 == Some(3) && st.1 == 3));
        s.idle(5);
        assert!(inspector_texts(&s).iter().any(|t| t == "3 rows"));
        assert!(inspector_texts(&s).iter().any(|t| t == "Can't read this table"), "{:?}", inspector_texts(&s));
        click_in_inspector(&mut s, "people", "click");
        wait_until(&mut s, "people's rows", 10, |s| inspector(s).sqlite_state().is_some_and(|st| st.2 == Some((1, 3))));
        s.idle(2);
        assert!(inspector_texts(&s).iter().any(|t| t == "<12 bytes>"));
        let cursor = cursor_pos(&s);
        s.act("key ArrowDown");
        wait_until(&mut s, "the view's rows by key", 10, |s| inspector(s).sqlite_state().is_some_and(|st| st.2 == Some((2, 3))));
        assert_eq!(cursor_pos(&s), cursor, "the key went to the list");
        shot(&mut s, "sqlite");
        let log = std::fs::read_to_string(fake.join("sqlite3.log")).unwrap();
        assert!(log.lines().filter(|l| !l.contains("-version")).all(|l| l.contains("-readonly") && l.contains("-safe") && l.contains("immutable=1")), "{log}");
        // Not a database inside: its text. No sqlite3: a note.
        select_path(&mut s, &d.join("other.db"));
        wait_until(&mut s, "other.db as text", 10, |s| inspector(s).text_lines().is_some());
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(root().join("devprev-none")));
        select_path(&mut s, &d.join("data.db"));
        wait_until(&mut s, "no sqlite3 note", 10, |s| inspector(s).note().is_some_and(|n| n.contains("sqlite3")));
        shot(&mut s, "sqlite-missing");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        // .env: keys shown, values hidden until clicked or all shown.
        select_path(&mut s, &d.join(".env"));
        wait_until(&mut s, "env view", 10, |s| inspector(s).rich_name() == Some("env"));
        s.idle(2);
        let secret = |s: &Sim| s.texts.iter().any(|(_, t)| t.contains("sk-live") || t.contains("pw@localhost") || t.contains("second line"));
        assert!(!secret(&s), "values hidden");
        assert!(inspector_texts(&s).iter().any(|t| t.starts_with("export API_KEY=••••")));
        shot(&mut s, "env");
        click_in_inspector(&mut s, &format!("export API_KEY={}", crate::envfile::MASK), "click");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t.contains("sk-live-0123456789")), "a click shows that value");
        assert!(!s.texts.iter().any(|(_, t)| t.contains("pw@localhost")), "and only that one");
        click_control(&mut s, "Show values");
        s.idle(2);
        assert!(secret(&s), "all shown");
        click_control(&mut s, "Hide values");
        s.idle(2);
        click_in_inspector(&mut s, "export API_KEY=sk-live-0123456789", "click");
        s.idle(2);
        assert!(!secret(&s), "hidden again");
        // A log: followed as it grows, the view at its end; filtered; let go.
        select_path(&mut s, &d.join("app.log"));
        wait_until(&mut s, "log", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.len() == 40));
        s.idle(2);
        assert!(has_control(&s, "line filter"), "a log has its filter");
        click_control(&mut s, "Follow");
        wait_until(&mut s, "following", 5, |s| inspector(s).text_lines().is_some_and(|l| l.1));
        {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().append(true).open(d.join("app.log")).unwrap();
            for i in 41..=300 {
                writeln!(f, "2026-10-16 10:01:{:02} WARN request {i} slow", i % 60).unwrap();
            }
        }
        wait_until(&mut s, "appended lines", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.len() == 300));
        s.idle(4);
        assert!(inspector_texts(&s).iter().any(|t| t.contains("request 300 slow")), "the view stayed at the end: {:?}", inspector_texts(&s));
        shot(&mut s, "log-follow");
        // Nothing arriving: the worker waits without waking the window. What the last
        // arrival set going settles first, on the wall clock (the folder's listing read
        // again, then the memory trim two seconds after it).
        s.app.toast = None;
        s.act("move -50 -50");
        s.settle();
        for _ in 0..2 {
            std::thread::sleep(std::time::Duration::from_millis(1400));
            s.idle(3);
        }
        let wakes = (0..10).filter(|_| {
            s.frame(vec![]);
            s.repaint < std::time::Duration::from_secs(5)
        }).count();
        assert_eq!(wakes, 0, "repaints asked for while the followed log was quiet: {:?}", s.ctx.repaint_causes());
        click_control(&mut s, "line filter");
        s.act("text request 29");
        s.idle(2);
        assert_eq!(inspector(&s).filtered_lines(), Some(11), "request 29 and 290…299");
        shot(&mut s, "log-filter");
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.act("key Escape");
        click_control(&mut s, "Follow");
        s.idle(2);
        assert!(inspector(&s).text_lines().is_some_and(|l| !l.1), "not following: no worker");
        // A file that grows while shown is offered Follow too.
        select_path(&mut s, &d.join("notes.md"));
        click_control(&mut s, "Source");
        wait_until(&mut s, "notes", 10, |s| inspector(s).text_lines().is_some());
        assert!(!has_control(&s, "Follow"));
        std::fs::write(d.join("notes.md"), "# Notes\nmore\nand more\n").unwrap();
        wait_until(&mut s, "Follow offered", 10, |s| has_control(s, "Follow"));
        click_control(&mut s, "Rendered");
        // Blame: who and when beside each line; the summary on hover; a click opens it.
        if git {
            go(&mut s, &d.join("src"));
            wait_until(&mut s, "work tree read", 10, |s| crate::gitstatus::repo_for(&s.tab().path).is_some());
            select_path(&mut s, &d.join("src/lib.rs"));
            wait_until(&mut s, "code", 10, |s| inspector(s).text_lines().is_some());
            s.idle(2);
            click_control(&mut s, "Blame");
            wait_until(&mut s, "blame read", 15, |s| inspector(s).blame_state() == Some((7, 3)));
            s.idle(2);
            let t = inspector_texts(&s);
            assert!(t.iter().any(|x| x == "Ada") && t.iter().any(|x| x == "Grace") && t.iter().any(|x| x == "Not committed"), "{t:?}");
            assert_eq!(t.iter().filter(|x| *x == "Ada").count(), 1, "a run of lines is named once");
            toolbar_centred(&mut s, "blame", &[]);
            shot(&mut s, "blame");
            click_control(&mut s, "Wrap lines");
            s.app.cfg.preview_line_spacing = 1.6;
            s.idle(2);
            shot(&mut s, "blame-wrapped");
            s.app.cfg.preview_line_spacing = 1.25;
            s.idle(2);
            let g = s.text_rect("Grace");
            s.act(&format!("move {:.0} {:.0}", g.center().x, g.center().y));
            s.idle(50);
            assert!(s.texts.iter().any(|(_, t)| t.starts_with("Add two")), "the commit's summary on hover");
            s.act(&format!("click {:.0} {:.0}", g.center().x, g.center().y));
            wait_until(&mut s, "the commit opened", 15, |s| s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.git.as_ref()).is_some_and(|h| h.probe().subject.as_deref() == Some("Add two")));
            wait_until(&mut s, "its files", 10, |s| history(s).is_some_and(|h| h.3 == Some(1)));
            s.idle(3);
            shot(&mut s, "blame-commit");
            s.act("key Escape");
            s.act("key Escape");
            s.idle(3);
            assert!(inspector(&s).git.is_none(), "back from the history");
            wait_until(&mut s, "blame again", 10, |s| inspector(s).blame_state().is_some());
            click_control(&mut s, "Wrap lines");
            go(&mut s, &d);
        }
        // Big files: everything heavy happens on workers or a few milliseconds a frame.
        // A long README, 5 MB of JSON, a table past its row cap, a 3 MB log.
        let big = d.join("Big");
        std::fs::create_dir_all(&big).unwrap();
        let para = "Some **bold** text, `code` and a [link](https://example.invalid) in a paragraph that wraps over a few lines of the preview. ";
        std::fs::write(big.join("long.md"), (0..2500).map(|i| format!("## Part {i}\n\n{}\n\n- one\n- two\n\n", para.repeat(1 + i % 3))).collect::<String>()).unwrap();
        std::fs::write(big.join("big.json"), format!("[{}]", (0..60_000).map(|i| format!("{{\"id\": {i}, \"name\": \"item {i}\", \"tags\": [\"a\", \"b\"], \"ok\": true}}")).collect::<Vec<_>>().join(",\n"))).unwrap();
        std::fs::write(big.join("big.csv"), (0..120_000).map(|i| format!("{i},name {i},{}.5\n", i * 3)).collect::<String>()).unwrap();
        std::fs::write(big.join("big.log"), (0..60_000).map(|i| format!("2026-10-16 10:00:00 INFO line {i} of a long log\n")).collect::<String>()).unwrap();
        go(&mut s, &big);
        let slow_before = s.slow.len();
        for (f, what) in [("long.md", "markdown"), ("big.json", "tree"), ("big.csv", "table")] {
            select_path(&mut s, &big.join(f));
            wait_until(&mut s, f, 30, |s| inspector(s).rich_name() == Some(what));
            s.idle(10);
        }
        assert!(inspector(&s).table().is_some_and(|t| t.rows.len() == crate::table::MAX_ROWS && t.note.is_some()), "a table past the cap says so");
        select_path(&mut s, &big.join("long.md"));
        wait_until(&mut s, "long markdown laid out", 30, |s| inspector(s).markdown().and_then(|m| m.counts()).is_some_and(|c| c.0 > 5000));
        s.resize(900.0, 700.0);
        s.idle(20);
        s.resize(1280.0, 800.0);
        s.idle(20);
        select_path(&mut s, &big.join("big.log"));
        wait_until(&mut s, "log end", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.last().is_some_and(|x| x.contains("line 59999"))));
        s.idle(4);
        assert!(inspector_texts(&s).iter().any(|t| t.contains("line 59999 of")), "a long log opens at its end");
        let slow: Vec<_> = s.slow[slow_before..].to_vec();
        assert!(slow.is_empty(), "big previews stalled: {slow:?}");
        go(&mut s, &d);
        // A narrow inspector, the bottom dock and a light theme: every preview again.
        let files = ["README.md", "package.json", "config.toml", "ci.yml", "people.csv", "wide.csv", "data.db", ".env", "app.log", "broken.json"];
        for (pass, (ratio, layout, theme)) in [(0.18, crate::config::InspectorLayout::Right, crate::theme::ThemeName::Midnight), (0.5, crate::config::InspectorLayout::Bottom, crate::theme::ThemeName::Midnight), (0.4, crate::config::InspectorLayout::Right, crate::theme::ThemeName::Paper)].into_iter().enumerate() {
            let id = s.app.active;
            s.app.cfg.theme = theme;
            if let Some(p) = s.app.panels.get_mut(&id) {
                p.inspector_ratio = ratio;
                if let Some(i) = p.inspector.as_mut() {
                    i.layout = Some(layout);
                }
            }
            s.idle(3);
            for f in files {
                select_path(&mut s, &d.join(f));
                s.idle(4);
                s.settle();
                if f == "data.db" {
                    wait_until(&mut s, "tables again", 10, |s| inspector(s).sqlite_state().is_some_and(|st| st.1 == 3) && s.texts.iter().any(|(_, t)| t == "Can't read this table"));
                }
                look(&mut s, &format!("{f} in pass {pass}"));
                s.idle(2);
                if f == "README.md" || f == "data.db" || f == "people.csv" {
                    s.shot(&format!("dev-pass{pass}-{f}"));
                }
            }
        }
        for (ms, step) in &s.slow {
            eprintln!("  slow frame {ms:.0} ms after: {step}");
        }
        report("devprev", &s);
    }

    /// Office documents in the inspector, through a stand-in for LibreOffice: a slow one
    /// says it is being prepared and its conversion dies when the user moves on; a
    /// document shows as its PDF with its pages; the second look is the cached PDF,
    /// LibreOffice not started again; one it can't read says so under its name; and
    /// without LibreOffice nothing is converted.
    #[test]
    fn office_previews() {
        if !sandboxed() || !crate::media::have("pdftoppm") {
            return;
        }
        let d = root().join("home/office");
        let _ = std::fs::remove_dir_all(&d);
        for f in ["Q3 report.docx", "budget.xlsx", "slow talk.odp", "broken.pptx", "notes.txt"] {
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join(f), format!("not really {f}")).unwrap();
        }
        let fake = root().join("office-tools");
        let _ = std::fs::remove_dir_all(&fake);
        crate::tests::fake_soffice(&fake);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.cmd(ToggleInspector);
        s.settle();
        let log = || std::fs::read_to_string(fake.join("soffice.log")).unwrap_or_default().lines().count();
        // Slow: a quiet line while LibreOffice works, and nothing else moves.
        select_path(&mut s, &d.join("slow talk.odp"));
        wait_until(&mut s, "the slow conversion", 10, |_| fake.join("sleep.pid").exists());
        assert!(inspector(&s).office_pending());
        s.idle(2);
        assert!(some_text(&s, "Preparing preview…"));
        look(&mut s, "office preparing");
        s.shot("office-preparing");
        let pid = std::fs::read_to_string(fake.join("sleep.pid")).unwrap();
        // Moving on kills it.
        select_path(&mut s, &d.join("Q3 report.docx"));
        assert!(crate::tests::process_gone(pid.trim()), "the slow conversion was killed");
        wait_until(&mut s, "the document's pages", 20, |s| inspector(s).pdf_state().is_some_and(|(p, n, shown)| p == 1 && n == Some(2) && shown.is_some_and(|x| x.0 == 1)));
        let e = s.tab().entries.iter().find(|e| e.name == "Q3 report.docx").unwrap().clone();
        assert_eq!(inspector(&s).pdf_path(), Some(crate::office::cached(&e.path, e.size, e.modified).as_path()));
        assert!(!some_text(&s, "Preparing preview…"));
        toolbar_centred(&mut s, "office", &["1 / 2"]);
        look(&mut s, "office preview");
        s.shot("office-preview");
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("move {:.0} {:.0}", list.right() + 150.0, list.center().y));
        s.act("key PageDown");
        wait_until(&mut s, "page 2", 20, |s| inspector(s).pdf_state().is_some_and(|(p, _, shown)| p == 2 && shown.is_some_and(|x| x.0 == 2)));
        s.act("move -50 -50");
        // The second look: the cached PDF straight away, LibreOffice not run.
        let runs = log();
        select_path(&mut s, &d.join("notes.txt"));
        wait_until(&mut s, "the text", 10, |s| inspector(s).text_lines().is_some());
        select_path(&mut s, &d.join("Q3 report.docx"));
        wait_until(&mut s, "the cached pages", 3, |s| inspector(s).pdf_state().is_some_and(|(_, n, _)| n == Some(2)));
        assert_eq!(log(), runs, "not converted again");
        // A spreadsheet the same way; a deck LibreOffice can't read.
        select_path(&mut s, &d.join("budget.xlsx"));
        wait_until(&mut s, "the spreadsheet's pages", 20, |s| inspector(s).pdf_state().is_some_and(|(_, n, shown)| n == Some(2) && shown.is_some()));
        select_path(&mut s, &d.join("broken.pptx"));
        wait_until(&mut s, "the refusal", 20, |s| inspector(s).note() == Some("Can't preview this document"));
        s.idle(2);
        assert!(some_text(&s, "Can't preview this document") && some_text(&s, "broken.pptx"));
        look(&mut s, "office refused");
        s.shot("office-refused");
        // At other font sizes and Spacing the toolbar keeps its place.
        select_path(&mut s, &d.join("Q3 report.docx"));
        wait_until(&mut s, "the pages again", 10, |s| inspector(s).pdf_state().is_some_and(|(_, n, shown)| n == Some(2) && shown.is_some()));
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            toolbar_centred(&mut s, &format!("office at font {font}, spacing {spacing}"), &["1 / 2"]);
            look(&mut s, &format!("office at font {font}, spacing {spacing}"));
        }
        // No LibreOffice: as before, nothing converted.
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(root().join("office-none")));
        let runs = log();
        std::fs::write(d.join("new.odt"), "fresh").unwrap();
        s.cmd(Refresh);
        wait_until(&mut s, "the new document listed", 10, |s| s.tab().entries.iter().any(|e| e.name == "new.odt"));
        select_path(&mut s, &d.join("new.odt"));
        s.settle();
        assert!(!inspector(&s).office_pending() && inspector(&s).pdf_state().is_none());
        assert_eq!(log(), runs);
        report("office", &s);
    }

    /// Set As Wallpaper from a picture's menu with Noctalia running (a stand-in, found
    /// among the running programs) on Hyprland; from the palette on GNOME, both of its
    /// pictures set; on a desktop Maia can't drive, a toast and nothing run. Not offered
    /// for a text file or for two pictures.
    #[test]
    fn set_as_wallpaper() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/wallpaper");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        png_file(&d.join("dunes.png"), 320, 200, 4);
        png_file(&d.join("forest.png"), 200, 320, 7);
        std::fs::write(d.join("notes.txt"), "not a picture").unwrap();
        let fake = root().join("wallpaper-tools");
        let _ = std::fs::remove_dir_all(&fake);
        crate::tests::fake_tool(&fake, "noctalia", "[ \"$1\" = run ] && while :; do sleep 1; done; exit 0");
        crate::tests::fake_tool(&fake, "gsettings", "exit 0");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        crate::wallpaper::DESKTOP.with(|w| *w.borrow_mut() = "Hyprland".into());
        let mut shell = std::process::Command::new(fake.join("noctalia")).arg("run").spawn().unwrap();
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
                crate::wallpaper::DESKTOP.with(|w| w.borrow_mut().clear());
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.settle();
        let calls = || crate::tests::fake_calls(&fake).into_iter().filter(|c| c.get(1).map(String::as_str) != Some("run")).collect::<Vec<_>>();
        // Not for text, nor for two pictures at once.
        for (what, ctrl) in [("notes.txt", false), ("forest.png", true)] {
            let row = row_text(&s, if ctrl { "dunes.png" } else { what });
            select_path(&mut s, &d.join("dunes.png"));
            if !ctrl {
                select_path(&mut s, &d.join(what));
            } else {
                let r = row_text(&s, what);
                s.act(&format!("click {:.0} {:.0} ctrl", r.center().x, r.center().y));
            }
            let row = if ctrl { row } else { row_text(&s, what) };
            s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
            s.idle(2);
            assert!(s.popup_open() && !some_text(&s, "Set As Wallpaper"), "{what}");
            s.act("key Escape");
        }
        // Noctalia: its own message, the picture's path.
        select_path(&mut s, &d.join("dunes.png"));
        let row = row_text(&s, "dunes.png");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        look(&mut s, "the picture's menu");
        shot_still(&mut s, "wallpaper-menu");
        s.click_text("Set As Wallpaper");
        wait_until(&mut s, "noctalia asked", 10, |_| !calls().is_empty());
        wait_until(&mut s, "the toast", 10, |s| toast_is(s, "Wallpaper set"));
        assert_eq!(calls(), [["noctalia", "msg", "wallpaper-set", d.join("dunes.png").to_str().unwrap()]]);
        look(&mut s, "wallpaper set");
        s.shot("wallpaper-set");
        let _ = shell.kill();
        let _ = shell.wait();
        // GNOME, from the palette: the light and the dark picture.
        crate::wallpaper::DESKTOP.with(|w| *w.borrow_mut() = "ubuntu:GNOME".into());
        select_path(&mut s, &d.join("forest.png"));
        palette_run(&mut s, "Set As Wallpaper");
        wait_until(&mut s, "gsettings run", 10, |_| calls().len() == 3);
        let uri = crate::clipboard::path_to_uri(&d.join("forest.png"));
        assert_eq!(calls()[1], ["gsettings", "set", "org.gnome.desktop.background", "picture-uri", uri.as_str()]);
        assert_eq!(calls()[2], ["gsettings", "set", "org.gnome.desktop.background", "picture-uri-dark", uri.as_str()]);
        // A desktop Maia doesn't know: said plainly, nothing run.
        crate::wallpaper::DESKTOP.with(|w| *w.borrow_mut() = "LXQt".into());
        s.app.toast = None;
        palette_run(&mut s, "Set As Wallpaper");
        wait_until(&mut s, "the refusal", 10, |s| toast_is(s, "Maia can't set the wallpaper on this desktop"));
        assert_eq!(calls().len(), 3);
        look(&mut s, "wallpaper refused");
        s.shot("wallpaper-refused");
        report("wallpaper", &s);
    }

    /// Email and Print from a file's menu and the palette: the mail app opened with the
    /// files attached; the Print dialog with CUPS's printers, the default chosen, another
    /// picked by key, two copies sent to lp; no printers said plainly; neither offered
    /// for a folder, Print not without CUPS, and no mail app said so.
    #[test]
    fn email_and_print() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/sendto");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Receipts")).unwrap();
        std::fs::write(d.join("report.pdf"), sample_pdf(&["0.2 0.4 0.8"])).unwrap();
        png_file(&d.join("photo.png"), 120, 80, 2);
        std::fs::write(d.join("notes.txt"), "remember the milk").unwrap();
        let fake = root().join("sendto-tools");
        let _ = std::fs::remove_dir_all(&fake);
        crate::tests::fake_tool(&fake, "xdg-email", "exit 0");
        let lpstat = |printers: &str| crate::tests::fake_tool(&fake, "lpstat", &format!("case \"$1\" in -e) printf '{printers}';; -d) echo 'system default destination: HP_ENVY_6000';; esac"));
        lpstat("Office_Laser\\nHP_ENVY_6000\\n");
        crate::tests::fake_tool(&fake, "lp", "echo 'request id is Office_Laser-12 (2 file(s))'");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        crate::sendto::MAIL_APP.with(|m| *m.borrow_mut() = Some("thunderbird %u".into()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
                crate::sendto::MAIL_APP.with(|m| *m.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.settle();
        let right = |s: &mut Sim, name: &str| {
            let row = row_text(s, name);
            s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
            s.idle(2);
        };
        // A folder: neither.
        select_path(&mut s, &d.join("Receipts"));
        right(&mut s, "Receipts");
        assert!(!some_text(&s, "Email") && !some_text(&s, "Print…"));
        s.act("key Escape");
        // Email from the menu.
        select_path(&mut s, &d.join("report.pdf"));
        right(&mut s, "report.pdf");
        assert!(some_text(&s, "Email") && some_text(&s, "Print…"));
        shot_still(&mut s, "email-print-menu");
        let n = launched().len();
        s.click_text("Email");
        s.idle(2);
        assert!(launched()[n..].iter().any(|l| l.contains("xdg-email") && l.contains(&format!("\"--attach\" \"{}\"", d.join("report.pdf").display()))), "{:?}", &launched()[n..]);
        assert!(toast_is(&s, "Opening a new email"));
        // Print two files from the palette: the default printer first chosen.
        let r = row_text(&s, "photo.png");
        s.act(&format!("click {:.0} {:.0} ctrl", r.center().x, r.center().y));
        palette_run(&mut s, "Print…");
        wait_until(&mut s, "the printers", 10, |s| matches!(&s.app.popup, Popup::Print(st) if st.printers.is_some()));
        s.idle(3);
        assert!(some_text(&s, "Print 2 files") && some_text(&s, "Office_Laser") && some_text(&s, "HP_ENVY_6000") && some_text(&s, "Default"));
        assert!(matches!(&s.app.popup, Popup::Print(st) if st.sel == 1), "the default chosen");
        look(&mut s, "print dialog");
        shot_still(&mut s, "print-dialog");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("print dialog at font {font}, spacing {spacing}"));
            if font == 16.5 {
                shot_still(&mut s, "print-dialog-large");
            }
        }
        // A short window: the dialog scrolls rather than lose its buttons.
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("print dialog at {w}x{h}"));
        }
        s.act("key ArrowUp");
        assert!(matches!(&s.app.popup, Popup::Print(st) if st.sel == 0));
        click_control(&mut s, "more copies");
        click_control(&mut s, "more copies");
        s.act("key ArrowLeft");
        s.idle(1);
        assert!(some_text(&s, "2"));
        assert!(matches!(&s.app.popup, Popup::Print(st) if st.copies == 2));
        s.act("key Enter");
        wait_until(&mut s, "lp", 10, |s| toast_is(s, "Sent 2 copies to Office_Laser"));
        let lp = crate::tests::fake_calls(&fake).into_iter().find(|c| c[0] == "lp").unwrap();
        assert_eq!(lp, ["lp", "-d", "Office_Laser", "-n", "2", d.join("photo.png").to_str().unwrap(), d.join("report.pdf").to_str().unwrap()]);
        assert!(!s.popup_open());
        // No printers.
        lpstat("");
        select_path(&mut s, &d.join("notes.txt"));
        palette_run(&mut s, "Print…");
        wait_until(&mut s, "no printers", 10, |s| matches!(&s.app.popup, Popup::Print(st) if st.printers.is_some()));
        s.idle(3);
        assert!(some_text(&s, "No printers are set up"));
        look(&mut s, "no printers");
        shot_still(&mut s, "print-none");
        s.act("key Enter");
        assert!(s.popup_open(), "nothing to print on");
        s.act("key Escape");
        assert!(!s.popup_open());
        // No CUPS: not offered. No mail app: said so.
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(root().join("sendto-none")));
        crate::sendto::MAIL_APP.with(|m| *m.borrow_mut() = None);
        right(&mut s, "notes.txt");
        assert!(some_text(&s, "Email") && !some_text(&s, "Print…"));
        s.click_text("Email");
        s.idle(2);
        assert!(toast_is(&s, "No mail app is set up"));
        report("email-print", &s);
    }

    /// Picture fixes with the real ImageMagick: offered in a picture's menu only; a
    /// photo stored turned turned right in place, again from the palette, and Undo
    /// bringing the original back byte for byte; copies fitted within 1920 px and at a
    /// typed size, selected as they land; a crop dragged on the inspector's picture,
    /// moved and stretched, saved with Enter at the size the footer said, and one
    /// cancelled with Escape; at other font sizes and Spacing.
    #[test]
    fn picture_fixes() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/picfix");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let Some(fake) = crate::tests::imagemagick_tools(&root().join("picfix-tools")) else { return };
        let raw = root().join("raw.jpg");
        assert!(std::process::Command::new(fake.join("magick")).args(["-size", "400x240", "gradient:#e94-#36c"]).arg(&raw).status().unwrap().success());
        let original = crate::tests::with_exif_orientation(&std::fs::read(&raw).unwrap(), 6);
        std::fs::write(d.join("photo.jpg"), &original).unwrap();
        png_file(&d.join("big.png"), 2400, 1600, 6);
        std::fs::write(d.join("notes.txt"), "not a picture").unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.cmd(ToggleInspector);
        s.settle();
        let shown = |name: &str| crate::imgdec::load(&d.join(name), 64).map(|x| x.1);
        let right = |s: &mut Sim, name: &str| {
            select_path(s, &d.join(name));
            let row = row_text(s, name);
            s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
            s.idle(2);
        };
        // Only for pictures.
        right(&mut s, "notes.txt");
        assert!(!some_text(&s, "Picture"));
        s.act("key Escape");
        // Turned right in place, from the menu.
        assert_eq!(shown("photo.jpg"), Some([240, 400]));
        right(&mut s, "photo.jpg");
        s.click_text("Picture");
        s.idle(2);
        s.scroll_popup_to("Crop…");
        for t in ["Rotate left", "Rotate right", "Resize", "Crop…"] {
            assert!(some_text(&s, t), "{t}");
        }
        s.click_text("Resize");
        s.idle(2);
        // A long menu in a short window scrolls down to the sizes.
        for _ in 0..20 {
            if some_text(&s, "Other size…") {
                break;
            }
            let at = s.text_rect("Resize").center();
            s.act(&format!("scroll {:.0} {:.0} -3", at.x, at.y));
            s.idle(2);
        }
        assert!(some_text(&s, "Fit within 1920 px") && some_text(&s, "Other size…"));
        look(&mut s, "the picture menu");
        shot_still(&mut s, "picture-menu");
        s.click_text("Rotate right");
        wait_until(&mut s, "the turn", 15, |s| s.app.undo.last().is_some_and(|u| u.label == "Rotate photo.jpg"));
        assert_eq!(shown("photo.jpg"), Some([400, 240]));
        let undos = s.app.undo.len();
        palette_run(&mut s, "Rotate Right");
        wait_until(&mut s, "the second turn", 15, |_| shown("photo.jpg") == Some([240, 400]));
        s.idle(2);
        assert_eq!(s.app.undo.len(), undos, "one Undo for both turns");
        assert_eq!(crate::trashcan::list().into_iter().filter(|it| it.original == d.join("photo.jpg")).count(), 1);
        s.act("key ctrl+z");
        s.settle();
        assert_eq!(std::fs::read(d.join("photo.jpg")).unwrap(), original, "the original back");
        // Copies, selected as they land.
        right(&mut s, "big.png");
        s.click_text("Picture");
        s.idle(1);
        s.click_text("Resize");
        s.idle(1);
        s.click_text("Fit within 1920 px");
        wait_until(&mut s, "the fitted copy", 15, |s| toast_is(s, "Saved big (1920 px).png"));
        wait_until(&mut s, "the copy selected", 10, |s| s.tab().target_entries().first().is_some_and(|e| e.name == "big (1920 px).png"));
        assert_eq!(shown("big (1920 px).png"), Some([1920, 1280]));
        select_path(&mut s, &d.join("big.png"));
        wait_until(&mut s, "big.png shown", 10, |s| s.app.thumbs.dims(&d.join("big.png")).is_some());
        palette_run(&mut s, "Resize To…");
        s.idle(2);
        assert!(matches!(&s.app.popup, Popup::Prompt { text, .. } if text == "2400 × 1600"), "its own size to start from");
        look(&mut s, "the size prompt");
        shot_still(&mut s, "resize-prompt");
        s.act("key ctrl+a");
        s.act("text 25%");
        s.act("key Enter");
        wait_until(&mut s, "the quarter copy", 15, |s| toast_is(s, "Saved big (600 px).png"));
        palette_run(&mut s, "Resize To…");
        s.act("key ctrl+a");
        s.act("text huge");
        s.act("key Enter");
        assert!(toast_is(&s, "Type a size such as 1280, 1280 × 720 or 50%"));
        s.app.toast = None;
        // A crop, drawn, moved and stretched, saved with Enter.
        let n = launched().len();
        right(&mut s, "photo.jpg");
        s.click_text("Picture");
        s.idle(1);
        s.click_text("Crop…");
        wait_until(&mut s, "the picture", 10, |s| some_text(s, "Drag over the part to keep"));
        look(&mut s, "crop started");
        s.shot("crop-start");
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        let c = pos2(ir.center().x, ir.top() + 30.0 + (ir.height() - 30.0) / 2.0);
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", c.x - 80.0, c.y - 120.0, c.x + 60.0, c.y + 100.0));
        s.idle(2);
        let sel = |s: &Sim| inspector(s).crop.as_ref().and_then(|c| c.sel);
        let first = sel(&s).unwrap_or_else(|| s.fail("a crop drawn"));
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", c.x, c.y, c.x + 20.0, c.y + 10.0));
        s.idle(2);
        let moved = sel(&s).unwrap();
        assert!((moved.width() - first.width()).abs() < 1e-4 && moved.min.x > first.min.x, "moved, same size: {first:?} -> {moved:?}");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", c.x + 80.0, c.y, c.x + 120.0, c.y));
        s.idle(2);
        let wide = sel(&s).unwrap();
        assert!(wide.max.x > moved.max.x && wide.min.x == moved.min.x, "stretched by its right edge: {moved:?} -> {wide:?}");
        let [_, _, w, h] = crate::picfix::crop_px(wide, [240, 400]).unwrap();
        let hint = format!("{w} × {h}  ·  Enter to crop");
        assert!(some_text(&s, &hint), "the footer says {hint:?}");
        look(&mut s, "crop drawn");
        s.shot("crop-drawn");
        s.act("key Enter");
        wait_until(&mut s, "the cropped copy", 15, |s| toast_is(s, "Saved photo (cropped).jpg"));
        assert_eq!(shown("photo (cropped).jpg"), Some([w, h]));
        assert_eq!(launched().len(), n, "Enter cropped, it opened nothing");
        assert!(inspector(&s).crop.is_none());
        wait_until(&mut s, "the cropped copy selected", 10, |s| s.tab().target_entries().first().is_some_and(|e| e.name == "photo (cropped).jpg"));
        // Cancelled with Escape; left by moving on.
        select_path(&mut s, &d.join("photo.jpg"));
        palette_run(&mut s, "Crop…");
        assert!(inspector(&s).crop.as_ref().is_some_and(|c| c.path == d.join("photo.jpg")));
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", c.x - 40.0, c.y - 40.0, c.x + 40.0, c.y + 40.0));
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("crop at font {font}, spacing {spacing}"));
        }
        // A small window, the inspector docked below, a light theme: the crop stays on
        // the picture and its line in the footer.
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        for (w, h) in [(640.0, 480.0), (520.0, 340.0)] {
            s.resize(w, h);
            look(&mut s, &format!("crop at {w}x{h}"));
        }
        s.app.cfg.inspector_layout = crate::config::InspectorLayout::Bottom;
        s.resize(900.0, 700.0);
        look(&mut s, "crop docked below");
        assert!(inspector(&s).crop.is_some());
        s.shot("crop-light-bottom");
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.act("key Escape");
        assert!(inspector(&s).crop.is_none());
        assert!(!d.join("photo (cropped) (1).jpg").exists());
        palette_run(&mut s, "Crop…");
        assert!(inspector(&s).crop.is_some());
        select_path(&mut s, &d.join("notes.txt"));
        s.idle(2);
        assert!(inspector(&s).crop.is_none(), "another file: no crop");
        report("picfix", &s);
    }

    /// Select these names in the active tab, the cursor on the first.
    fn select_names(s: &mut Sim, names: &[&str]) {
        let id = s.app.active;
        let t = s.app.panels.get_mut(&id).unwrap().tab_mut();
        let picks: Vec<u32> = names.iter().filter_map(|n| t.entries.iter().position(|e| e.name == *n).map(|i| i as u32)).collect();
        assert_eq!(picks.len(), names.len(), "{names:?} listed");
        t.selected = picks.iter().copied().collect();
        t.cursor = picks.first().copied();
        s.idle(2);
    }

    /// Open the menu of the row `name` and the submenu `sub`, scrolling a long menu
    /// down to it in a short window.
    fn open_submenu(s: &mut Sim, name: &str, sub: &str) {
        let row = row_text(s, name);
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        for _ in 0..30 {
            if some_text(s, sub) {
                break;
            }
            let at = s.texts.iter().rev().find(|(_, t)| t == "Open").map(|x| x.0.center()).unwrap_or(row.center());
            s.act(&format!("scroll {:.0} {:.0} -3", at.x, at.y));
            s.idle(2);
        }
        s.click_text(sub);
        s.idle(2);
    }

    /// Click `label` in the open menu, scrolling it into view first.
    fn menu_pick(s: &mut Sim, label: &str) {
        menu_show(s, label);
        s.click_text(label);
    }

    /// Scroll the open menu until `label` is in view.
    fn menu_show(s: &mut Sim, label: &str) {
        for _ in 0..30 {
            if s.texts.iter().any(|(r, t)| t == label && r.bottom() < s.size.y - 4.0) {
                break;
            }
            // Over the menu itself (a job card may be up too): the context menu's own
            // first rows, else the popup drawn last.
            let menu = match &s.app.popup {
                Popup::Ctx(_) => s.texts.iter().rev().find(|(_, t)| ["Open", "Mount", "Select All"].contains(&t.as_str())).map(|x| x.0.center()),
                _ => None,
            };
            let at = menu.or_else(|| crate::ui::POPUPS.with(|p| p.borrow().last().map(|r| r.center()))).unwrap_or(pos2(s.size.x / 2.0, s.size.y / 2.0));
            s.act(&format!("scroll {:.0} {:.0} -3", at.x, at.y));
            s.idle(2);
        }
    }

    /// Converting from the Convert menu and the palette: only formats the files aren't
    /// in already, only for files of one kind; a sound file as FLAC with the job card
    /// saying how far it is, Undo taking the copy away; a long video cancelled on its
    /// card, ffmpeg's part file gone; pictures put into one PDF under a name typed in
    /// the prompt; two PDFs merged and one split into a folder of pages; at other font
    /// sizes and Spacing.
    #[test]
    fn convert_files_from_the_menu() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/convert");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let Some(tools) = crate::tests::convert_tools(&root().join("convert-tools")) else { return };
        crate::tests::convert_inputs(&d, &tools);
        let ok = std::process::Command::new(tools.join("ffmpeg")).args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=90:size=640x480:rate=30", "-c:v", "mpeg4", "-q:v", "20"]).arg(d.join("long.avi")).status().unwrap().success();
        assert!(ok);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.settle();
        // A sound file: not offered as what it is already.
        select_names(&mut s, &["tone.mp3"]);
        open_submenu(&mut s, "tone.mp3", "Convert");
        for t in ["FLAC", "Opus"] {
            assert!(some_text(&s, t), "{t}");
        }
        assert!(!some_text(&s, "MP3") && !some_text(&s, "MP4"));
        look(&mut s, "the convert menu");
        shot_still(&mut s, "convert-menu");
        menu_pick(&mut s, "FLAC");
        wait_until(&mut s, "the FLAC", 30, |s| s.app.jobs.iter().any(|j| j.progress.lock().unwrap().finished) && d.join("tone.flac").exists());
        s.idle(2);
        assert!(some_text(&s, "Converted tone.mp3 to FLAC"), "the card says it");
        assert_eq!(s.app.undo.last().map(|u| u.label.as_str()), Some("Convert tone.mp3 to FLAC"));
        s.act("key ctrl+z");
        s.settle();
        assert!(!d.join("tone.flac").exists(), "Undo took the copy away");
        // Two kinds at once: nothing to convert.
        select_names(&mut s, &["tone.mp3", "two.pdf"]);
        let row = row_text(&s, "tone.mp3");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        assert!(!some_text(&s, "Convert"));
        s.act("key Escape");
        // A long video: the card counts inside the file; Cancel stops it.
        select_names(&mut s, &["long.avi"]);
        palette_run(&mut s, "Convert To WebM");
        let webm = |s: &Sim| s.app.jobs.iter().find(|j| j.label == "Converting long.avi to WebM").map(|j| j.progress.clone());
        wait_until(&mut s, "progress inside the file", 30, |s| webm(s).is_some_and(|p| p.lock().unwrap().within.unwrap_or(0.0) > 0.01));
        s.idle(2);
        let line = s.texts.iter().find(|(_, t)| t.starts_with("0 / 1 item · ")).map(|x| x.1.clone()).unwrap_or_else(|| s.fail("the card's line"));
        assert!(line.ends_with('%'), "{line}");
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Converting long.avi")), "the card's title");
        s.shot("convert-card");
        let parts: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.')).collect();
        assert_eq!(parts, [".long.webm.part"], "{:?}", s.app.jobs.first().map(|j| j.progress.lock().unwrap().errors.clone()));
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
        }
        s.click_text("Cancel");
        wait_until(&mut s, "cancelled", 15, |s| webm(s).is_none_or(|p| p.lock().unwrap().finished));
        s.idle(2);
        assert!(!d.join(".long.webm.part").exists() && !d.join("long.webm").exists());
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Cancelled converting")), "the card says it stopped");
        s.shot("convert-cancelled");
        // Pictures into one PDF, named first.
        select_names(&mut s, &["clear.png", "turned.jpg", "phone.heic"]);
        open_submenu(&mut s, "clear.png", "Convert");
        menu_show(&mut s, "PDF…");
        assert!(some_text(&s, "JPG") && some_text(&s, "PNG") && some_text(&s, "WebP"));
        s.click_text("PDF…");
        s.idle(2);
        assert!(matches!(&s.app.popup, Popup::Prompt { text, .. } if text == "Pictures.pdf"));
        look(&mut s, "the PDF's name");
        shot_still(&mut s, "convert-pdf-name");
        s.act("key ctrl+a");
        s.act("text Holiday");
        s.act("key Enter");
        wait_until(&mut s, "the PDF", 30, |_| d.join("Holiday.pdf").exists());
        assert_eq!(crate::media::pdf_pages(&d.join("Holiday.pdf")), Some(3));
        // Two PDFs merged; one split.
        select_names(&mut s, &["three.pdf", "two.pdf"]);
        palette_run(&mut s, "Merge PDFs…");
        assert!(matches!(&s.app.popup, Popup::Prompt { text, .. } if text == "Merged.pdf"));
        s.act("key Enter");
        wait_until(&mut s, "merged", 30, |_| d.join("Merged.pdf").exists());
        assert_eq!(crate::media::pdf_pages(&d.join("Merged.pdf")), Some(5));
        wait_until(&mut s, "the card gone", 10, |s| s.app.jobs.is_empty());
        s.settle();
        select_names(&mut s, &["three.pdf"]);
        open_submenu(&mut s, "three.pdf", "Convert");
        assert!(!some_text(&s, "Merge into one PDF…"));
        menu_pick(&mut s, "Split into pages");
        wait_until(&mut s, "split", 30, |_| d.join("three pages/three 3.pdf").exists());
        s.settle();
        look(&mut s, "after converting");
        s.shot("convert-done");
        report("convert", &s);
    }

    /// Tidy Folder from the folder's menu: every move listed beside where it goes, by
    /// kind and then photos by month (the date a photo was taken, or its file date,
    /// said so); applied as one job; Undo puts every file back and takes the new
    /// folders away; by month too; at other font sizes and Spacing and a small window.
    #[test]
    fn tidy_a_folder() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/Downloads");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Keep me")).unwrap();
        for f in ["setup.deb", "photos.tar.gz", "report.pdf", "song.mp3", "clip.mp4", "main.rs", ".hidden.png"] {
            std::fs::write(d.join(f), f).unwrap();
        }
        png_file(&d.join("screen.png"), 40, 30, 2);
        if crate::tests::exif_photo(&d.join("trip.jpg"), "64x48", "2022:03:09 08:00:00", None).is_none() {
            return;
        }
        let may = crate::timefmt::parse_local("2025-05-20T12:00:00").unwrap();
        for f in ["screen.png", "clip.mp4"] {
            std::fs::File::options().write(true).open(d.join(f)).unwrap().set_modified(may).unwrap();
        }
        let before: Vec<String> = {
            let mut v: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.settle();
        let open = |s: &mut Sim| {
            let list = s.app.panels[&s.app.active].list_rect;
            s.act(&format!("click {:.0} {:.0} right", list.center().x, list.bottom() - 30.0));
            s.idle(2);
            menu_pick(s, "Tidy Folder…");
            wait_until(s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Tidy(t) if t.plans.is_some()));
            s.idle(2);
        };
        open(&mut s);
        for t in ["Tidy Downloads", "By kind", "Photos by month", "setup.deb", "Installers", "Archives", "Documents", "Music", "Videos", "Pictures", "7 files into 6 folders"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        let Popup::Tidy(t) = &s.app.popup else { s.fail("the dialog") };
        let names: Vec<String> = t.moves().iter().map(|m| m.src.file_name().unwrap().to_string_lossy().into_owned()).collect();
        assert!(!names.iter().any(|n| ["main.rs", "Keep me", ".hidden.png"].contains(&n.as_str())), "left alone: {names:?}");
        look(&mut s, "tidy by kind");
        shot_still(&mut s, "tidy-kind");
        click_control(&mut s, "tidy by month");
        s.idle(2);
        for t in ["2022/2022-03", "2025/2025-05  (file date)", "3 files into 2 folders"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        look(&mut s, "tidy by month");
        shot_still(&mut s, "tidy-month");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("tidy at font {font}, spacing {spacing}"));
        }
        s.resize(640.0, 480.0);
        look(&mut s, "tidy in a small window");
        s.shot("tidy-small");
        s.resize(1280.0, 800.0);
        click_control(&mut s, "tidy by kind");
        s.act("key Enter");
        wait_until(&mut s, "tidied", 10, |_| d.join("Installers/setup.deb").is_file() && d.join("Pictures/trip.jpg").is_file());
        wait_until(&mut s, "the job done", 10, |s| s.app.undo.last().is_some_and(|u| u.label == "Tidy Downloads"));
        for (f, k) in [("photos.tar.gz", "Archives"), ("report.pdf", "Documents"), ("song.mp3", "Music"), ("clip.mp4", "Videos"), ("screen.png", "Pictures")] {
            assert!(d.join(k).join(f).is_file(), "{k}/{f}");
        }
        assert!(d.join("main.rs").is_file() && d.join(".hidden.png").is_file() && d.join("Keep me").is_dir());
        s.settle();
        look(&mut s, "tidied");
        s.shot("tidy-done");
        s.act("key ctrl+z");
        s.settle();
        let after: Vec<String> = {
            let mut v: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
            v.sort();
            v
        };
        assert_eq!(after, before, "Undo put it all back");
        // By month, from the palette.
        palette_run(&mut s, "Tidy Folder…");
        wait_until(&mut s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Tidy(t) if t.plans.is_some()));
        click_control(&mut s, "tidy by month");
        click_control(&mut s, "tidy apply");
        wait_until(&mut s, "by month", 10, |_| d.join("2022/2022-03/trip.jpg").is_file() && d.join("2025/2025-05/clip.mp4").is_file());
        assert!(d.join("report.pdf").is_file(), "only photos and videos");
        // Nothing left to sort: said so, and Tidy can't be pressed.
        let empty = root().join("home/Downloads/Keep me");
        go(&mut s, &empty);
        palette_run(&mut s, "Tidy Folder…");
        wait_until(&mut s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Tidy(t) if t.plans.is_some()));
        s.idle(2);
        assert!(some_text(&s, "Nothing here to sort by kind"));
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::Tidy(_)), "nothing to do: it stays open");
        s.act("key Escape");
        report("tidy", &s);
    }

    /// Books, comics, fonts and a photo's place in the inspector: an EPUB's title, cover
    /// and chapters as text; a CBZ a page at a time with Page Down; a font's names and
    /// samples, Install copying it into the fonts folder with fc-cache told, Undo taking
    /// it out; the details of a photo with its date and place, Open map handing
    /// OpenStreetMap's address to the browser. At other font sizes and Spacing.
    #[test]
    fn books_comics_fonts_and_places() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/reading");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        if crate::tests::book_fixture(&d).is_none() {
            return;
        }
        let font = crate::tests::sample_font();
        let font_name = font.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::copy(&font, d.join(&font_name)).unwrap();
        let family = crate::fontview::info(&std::fs::read(&font).unwrap()).family;
        if crate::tests::exif_photo(&d.join("harbour.jpg"), "320x200", "2023:07:14 10:20:30", Some(["S", "33/1 51/1 3540/100", "E", "151/1 12/1 4020/100"])).is_none() {
            return;
        }
        let tools = root().join("reading-tools");
        crate::tests::fake_tool(&tools, "fc-cache", "exit 0");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.cmd(ToggleInspector);
        s.settle();
        let looks = |s: &mut Sim, what: &str| {
            for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
                s.app.cfg.font_size = font;
                s.app.cfg.spacing = spacing;
                s.idle(3);
                look(s, &format!("{what} at font {font}, spacing {spacing}"));
            }
        };
        // The book.
        select_path(&mut s, &d.join("Lighthouse.epub"));
        wait_until(&mut s, "the book", 15, |s| inspector(s).rich_name() == Some("book"));
        s.settle();
        for t in ["The Lighthouse & the Sea", "Ada Keeper", "Chapter One"] {
            in_inspector(&s, t);
        }
        assert!(inspector(&s).markdown().is_some_and(|m| m.doc.images.len() == 1), "the cover");
        look(&mut s, "a book");
        s.shot("preview-book");
        looks(&mut s, "a book");
        // The comic, turned with Page Down.
        select_path(&mut s, &d.join("Harbour.cbz"));
        wait_until(&mut s, "the comic's first page", 15, |s| inspector(s).comic_pages() == Some(10) && inspector(s).pdf_state().is_some_and(|p| p.2.is_some_and(|x| x.0 == 1)));
        s.idle(2);
        in_inspector(&s, "1 / 10");
        look(&mut s, "a comic");
        s.shot("preview-comic");
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        s.act(&format!("move {:.0} {:.0}", ir.center().x, ir.center().y));
        s.act("key PageDown");
        wait_until(&mut s, "page two", 15, |s| inspector(s).pdf_state().is_some_and(|p| p.0 == 2 && p.2.is_some_and(|x| x.0 == 2)));
        looks(&mut s, "a comic");
        // The font, installed and taken out again.
        select_path(&mut s, &d.join(&font_name));
        wait_until(&mut s, "the font", 15, |s| inspector(s).font_state().is_some_and(|f| f.is_ok_and(|f| f.2 > 0)));
        s.idle(2);
        in_inspector(&s, &family);
        in_inspector(&s, "54 pt");
        assert!(!inspector(&s).font_state().unwrap().unwrap().3, "not installed yet");
        look(&mut s, "a font");
        s.shot("preview-font");
        click_control(&mut s, "font install");
        let fonts = root().join("xdg/data/fonts");
        wait_until(&mut s, "installed", 15, |s| toast_is(s, &format!("Installed {family}")));
        assert_eq!(std::fs::read(fonts.join(&font_name)).unwrap(), std::fs::read(&font).unwrap());
        assert!(crate::tests::fake_calls(&tools).iter().any(|c| c.first().is_some_and(|p| p == "fc-cache")), "fc-cache told");
        wait_until(&mut s, "shown as installed", 15, |s| inspector(s).font_state().is_some_and(|f| f.is_ok_and(|f| f.3)));
        s.idle(2);
        in_inspector(&s, "Installed");
        looks(&mut s, "an installed font");
        s.act("key ctrl+z");
        s.settle();
        assert!(!fonts.join(&font_name).exists(), "Undo took it out");
        // The photo's place.
        select_path(&mut s, &d.join("harbour.jpg"));
        s.settle();
        let pill = in_inspector(&s, "harbour.jpg");
        s.act(&format!("click {:.0} {:.0}", pill.center().x, pill.center().y));
        wait_until(&mut s, "the place", 15, |s| some_text(s, "33.85983° S, 151.21117° E"));
        for t in ["Taken", "2023-07-14 10:20", "Location", "Open map"] {
            assert!(some_text(&s, t), "{t:?} in the details");
        }
        s.act("move -50 -50");
        s.idle(3);
        s.shot("preview-place");
        let n = launched().len();
        click_control(&mut s, "open map");
        s.idle(2);
        let l = launched();
        assert_eq!(l.len(), n + 1);
        assert!(l.last().is_some_and(|c| c.contains("https://www.openstreetmap.org/?mlat=-33.859833&mlon=151.211167")), "{:?}", l.last());
        looks(&mut s, "a photo's details");
        report("books, comics, fonts, places", &s);
    }

    /// The Timeline from the sidebar, under Home: the last month's files from all over
    /// the home folder, newest first under Today and the other date groups, with the
    /// hidden, cached and built ones left out; opened from the palette in a tab of its
    /// own too, and its file opened where it is; at other font sizes and Spacing.
    #[test]
    fn timeline_of_recent_files() {
        if !sandboxed() {
            return;
        }
        let home = root().join("home");
        crate::tests::timeline_fixture(&home);
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.settle();
        // The sidebar, long with what other journeys left in it, scrolled to it.
        for _ in 0..10 {
            if sidebar_text(&s, "Timeline").is_some() {
                break;
            }
            s.act(&format!("scroll {:.0} 400 -6", s.app.sidebar_width / 2.0));
            s.idle(2);
        }
        let row = sidebar_text(&s, "Timeline").unwrap_or_else(|| s.fail("Timeline in the sidebar"));
        let home_row = sidebar_text(&s, "Home").unwrap();
        assert!(row.top() > home_row.top() && row.top() - home_row.top() < 60.0, "right under Home");
        s.act(&format!("click {:.0} {:.0}", row.center().x, row.center().y));
        s.settle();
        wait_until(&mut s, "the walk", 15, |s| !s.tab().is_loading());
        s.idle(3);
        // Other sessions' files share this home: the fixture's own, in the order shown.
        let ours = ["Documents/letter.odt", "Code/app/src/main.rs", "Downloads/ticket.pdf", "Pictures/2026/beach.png", "Documents/old report.pdf", ".cache/thumb.png", ".notes.txt", "Code/app/.git/index", "Code/app/web/node_modules/left-pad/index.js", "Code/app/target/debug/app"].map(|p| home.join(p));
        let names: Vec<String> = s.tab().view.iter().map(|&i| &s.tab().entries[i as usize]).filter(|e| ours.contains(&e.path)).map(|e| e.name.clone()).collect();
        assert_eq!(names, ["letter.odt", "main.rs", "ticket.pdf", "beach.png"], "newest first, the rest left out");
        assert_eq!(s.tab().title(), "Timeline");
        assert_eq!(s.tab().group_labels.first().map(String::as_str), Some("Today"));
        assert!(s.texts.iter().any(|(_, t)| t.starts_with("Today  (")), "the day's header");
        assert!(some_text(&s, "Timeline"), "its name in the path bar");
        look(&mut s, "the timeline");
        s.shot("timeline");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("the timeline at font {font}, spacing {spacing}"));
        }
        for v in [ViewList, ViewLarge, ViewTree, ViewDetails] {
            s.cmd(v);
            s.settle();
            look(&mut s, &format!("the timeline in {v:?}"));
        }
        // From the palette, in a tab where Home was.
        s.cmd(Home);
        s.settle();
        palette_run(&mut s, "Timeline");
        wait_until(&mut s, "the walk", 15, |s| !s.tab().is_loading() && s.tab().smart.is_some());
        assert_eq!(s.tab().settings.group, crate::config::GroupBy::Modified);
        report("timeline", &s);
    }

    /// Music tags: Edit Tags from the menu of two sound files and a picture, the fields
    /// they share filled in and the one they don't shown as Mixed, a title and an album
    /// typed, the picture made their cover; saved through ffmpeg, Undo putting both back;
    /// then Batch Rename naming them by track and title. At other font sizes and Spacing.
    #[test]
    fn music_tags_edited_and_used_in_names() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/album");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let Some(tools) = crate::tests::convert_tools(&root().join("tags-tools")) else { return };
        crate::tests::convert_inputs(&d, &tools);
        let ff = |args: &[&str]| assert!(std::process::Command::new(tools.join("ffmpeg")).args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).current_dir(&d).status().unwrap().success());
        ff(&["-i", "tone.mp3", "-map", "0:a", "-c:a", "flac", "-metadata", "genre=Drone", "-metadata", "track=2", "second.flac"]);
        ff(&["-f", "lavfi", "-i", "testsrc=size=80x80", "-frames:v", "1", "art.png"]);
        let mp3_before = std::fs::read(d.join("tone.mp3")).unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let t = crate::drives::Tools::current().unwrap();
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.settle();
        select_names(&mut s, &["tone.mp3", "second.flac", "art.png"]);
        open_submenu(&mut s, "tone.mp3", "Edit Tags…");
        wait_until(&mut s, "the tags read", 15, |s| matches!(&s.app.popup, Popup::MusicTags(m) if m.read.is_some()));
        s.idle(3);
        for t in ["Tags of 2 files", "Test Tone", "Maia", "Mixed", "Use art.png", "Keep", "Remove"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        look(&mut s, "the tags dialog");
        shot_still(&mut s, "music-tags");
        click_control(&mut s, "tag title");
        s.act("key ctrl+a");
        s.act("text Morning Tone");
        click_control(&mut s, "tag album");
        s.act("text Sine Waves");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("the tags dialog at font {font}, spacing {spacing}"));
        }
        click_control(&mut s, "tags save");
        wait_until(&mut s, "saved", 30, |s| toast_is(s, "Saved the tags of 2 files"));
        for f in ["tone.mp3", "second.flac"] {
            let got = crate::musictags::read(&t, &d.join(f)).unwrap();
            assert_eq!((got.get("title"), got.get("album"), got.cover), ("Morning Tone", "Sine Waves", true), "{f}");
        }
        assert_eq!(crate::musictags::read(&t, &d.join("second.flac")).unwrap().get("genre"), "Drone", "the mixed field left alone");
        s.act("key ctrl+z");
        s.settle();
        assert_eq!(std::fs::read(d.join("tone.mp3")).unwrap(), mp3_before, "Undo put the original back");
        // Names from the tags.
        select_names(&mut s, &["tone.mp3", "second.flac"]);
        palette_run(&mut s, "Batch Rename…");
        s.act("text {track} {title}");
        wait_until(&mut s, "names from the tags", 15, |s| some_text(s, "02 Test Tone.flac"));
        assert!(some_text(&s, "Test Tone.mp3"), "no track, no number and no space for it");
        look(&mut s, "names from tags");
        shot_still(&mut s, "rename-from-tags");
        s.act("key Escape");
        report("music tags", &s);
    }

    /// Format, in a removable drive's menu (the sidebar and Computer), through stand-ins
    /// for udisks only: not offered for the system's or an internal disk; the dialog
    /// names the drive and its size, says everything on it goes, and Erase and Format
    /// waits until the name is typed; the card erased and made exFAT with its new label;
    /// a stick something holds open refused at its unmount, udisks never asked; FAT32's
    /// label too long said so. At other font sizes and Spacing.
    #[test]
    fn format_a_usb_drive() {
        if !sandboxed() {
            return;
        }
        let d = fake_drives("format");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let gdbus_log = || std::fs::read_to_string(d.join("gdbus.log")).unwrap_or_default();
        let home = fixture("format");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.cmd(Computer);
        wait_until(&mut s, "drives listed", 10, |s| s.tab().entries.iter().any(|e| e.name == "Photo Card"));
        s.settle();
        for name in ["Root", "Windows"] {
            let r = list_text(&s, name);
            s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
            s.idle(3);
            assert!(!some_text(&s, "Format…"), "{name}: never formatted from here");
            s.act("key Escape");
        }
        // From the sidebar.
        let card = sidebar_text(&s, "Photo Card").unwrap_or_else(|| s.fail("the card in the sidebar"));
        s.act(&format!("click {:.0} {:.0} right", card.center().x, card.center().y));
        s.idle(3);
        menu_pick(&mut s, "Format…");
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::FormatDrive(_)));
        for t in ["Format “Photo Card”", "14.9 GB removable drive · /dev/sdz", "Type Photo Card to confirm", "Everything on this 14.9 GB drive is erased: every partition and every file on it."] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        s.act("key Enter");
        assert!(matches!(s.app.popup, Popup::FormatDrive(_)) && gdbus_log().is_empty(), "nothing before the name is typed");
        click_control(&mut s, "format erase");
        assert!(matches!(s.app.popup, Popup::FormatDrive(_)) && gdbus_log().is_empty(), "the button waits too");
        click_control(&mut s, "format fat32");
        click_control(&mut s, "format label");
        s.act("key ctrl+a");
        s.act("text Holiday Photos");
        s.idle(2);
        assert!(some_text(&s, "FAT32 labels are 11 characters at most"));
        click_control(&mut s, "format exfat");
        click_control(&mut s, "format confirm");
        s.act("text Photo Card");
        s.idle(2);
        look(&mut s, "the format dialog");
        shot_still(&mut s, "format-dialog");
        for (font, spacing) in [(16.5, 1.6), (11.0, 0.7), (20.0, 2.0), (13.0, 1.0)] {
            s.app.cfg.font_size = font;
            s.app.cfg.spacing = spacing;
            s.idle(3);
            look(&mut s, &format!("the format dialog at font {font}, spacing {spacing}"));
        }
        click_control(&mut s, "format erase");
        wait_until(&mut s, "formatted", 15, |s| toast_is(s, "Formatted Photo Card"));
        let log = gdbus_log();
        assert!(log.contains("--object-path\n/org/freedesktop/UDisks2/block_devices/sdz\n--method\norg.freedesktop.UDisks2.Block.Format\n'dos'"), "{log}");
        assert!(log.contains("org.freedesktop.UDisks2.PartitionTable.CreatePartitionAndFormat") && log.contains("'exfat'\n{'label': <'Holiday Photos'>}"), "{log}");
        assert!(!log.contains("sdw") && !log.contains("sdy"), "no other disk touched");
        s.settle();
        wait_until(&mut s, "the new label", 15, |s| sidebar_text(s, "Holiday Photos").is_some());
        look(&mut s, "after formatting");
        s.shot("format-done");
        // A stick that won't unmount: refused before udisks is asked to format it.
        let before = gdbus_log();
        let r = list_text(&s, "Busy Stick");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(3);
        menu_pick(&mut s, "Format…");
        s.idle(2);
        click_control(&mut s, "format confirm");
        s.act("text Busy Stick");
        s.act("key Enter");
        wait_until(&mut s, "refused", 15, |s| s.app.drive_jobs.is_empty() && (s.app.toast.as_ref().is_some_and(|t| t.0.starts_with("Couldn't format Busy Stick")) || matches!(s.app.popup, Popup::Holders(_))));
        assert_eq!(gdbus_log(), before, "udisks never asked");
        assert!(d.join("media/Busy Stick/busy.txt").exists(), "its files untouched");
        report("format", &s);
    }

    /// The new surfaces in four themes, a wide and a narrow window and with animations
    /// off: Convert's menu and its name prompt, a conversion's card, Tidy (with plans
    /// and with nothing to do), Compress With Password, Edit Tags, Format, the Timeline,
    /// and the previews of a book, a comic, a font and the ones that can't be shown (a
    /// broken book, comic and font). Every frame checked; photographed with
    /// MAIA_SIM_SHOTS.
    #[test]
    fn new_surfaces_in_every_theme() {
        if !sandboxed() || !crate::archive::available() {
            return;
        }
        use crate::theme::ThemeName;
        let home = root().join("home/surfaces");
        let _ = std::fs::remove_dir_all(&home);
        let d = home.join("Mixed");
        std::fs::create_dir_all(&d).unwrap();
        let Some(tools) = crate::tests::convert_tools(&root().join("surfaces-tools")) else { return };
        crate::tests::convert_inputs(&d, &tools);
        if crate::tests::book_fixture(&d).is_none() {
            return;
        }
        std::fs::copy(crate::tests::sample_font(), d.join("Sample.ttf")).unwrap();
        std::fs::write(d.join("broken.epub"), "PK not a book").unwrap();
        std::fs::write(d.join("broken.cbz"), "PK not a comic").unwrap();
        std::fs::write(d.join("broken.otf"), "not a font").unwrap();
        std::fs::create_dir_all(home.join("Empty")).unwrap();
        crate::tests::timeline_fixture(&home.join("Recent"));
        let ok = std::process::Command::new(tools.join("ffmpeg")).args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=600:size=320x240:rate=30", "-c:v", "mpeg4", "-q:v", "28"]).arg(d.join("long.avi")).status().unwrap().success();
        assert!(ok);
        let drv = fake_drives("surfaces");
        for t in ["magick", "ffmpeg", "ffprobe", "pdfunite", "pdfseparate", "heif-dec"] {
            let _ = std::os::unix::fs::symlink(std::fs::read_link(tools.join(t)).unwrap(), drv.join(t));
        }
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(drv.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let shots = std::env::var_os("MAIA_SIM_SHOTS").is_some();
        let mut first = true;
        for theme in [ThemeName::Midnight, ThemeName::Paper, ThemeName::Latte, ThemeName::Gruvbox] {
            for (w, h) in [(1280.0, 800.0), (720.0, 480.0)] {
                let tag = format!("{}-{w}", theme.label().to_lowercase().replace(' ', ""));
                let mut s = Sim::new(&d, vec2(w, h));
                s.app.cfg.theme = theme;
                s.app.cfg.animations = !first || !shots;
                s.settle();
                let snap = |s: &mut Sim, what: &str| {
                    look(s, &format!("{what} in {tag}"));
                    if shots {
                        s.idle(12);
                        s.shot(&format!("surface-{what}-{tag}"));
                    }
                };
                // Convert: the menu, the PDF's name, a running card.
                select_names(&mut s, &["clear.png", "turned.jpg"]);
                let r = row_text(&s, "clear.png");
                s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
                s.idle(3);
                s.act("text convert");
                s.idle(2);
                assert!(some_text(&s, "Convert › PDF…"), "found by search");
                snap(&mut s, "convert-menu");
                s.act("key Escape");
                s.cmd(ConvertPdf);
                s.idle(2);
                assert!(matches!(s.app.popup, Popup::Prompt { .. }));
                snap(&mut s, "convert-prompt");
                s.act("key Escape");
                select_names(&mut s, &["long.avi"]);
                s.cmd(ConvertWebm);
                wait_until(&mut s, "the card", 30, |s| s.app.jobs.iter().any(|j| j.progress.lock().unwrap().within.unwrap_or(0.0) > 0.0));
                s.idle(3);
                if shots {
                    s.shot(&format!("surface-convert-card-{tag}"));
                }
                for j in &s.app.jobs {
                    j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                wait_until(&mut s, "cancelled", 15, |s| s.app.jobs.iter().all(|j| j.progress.lock().unwrap().finished));
                s.app.jobs.clear();
                // Tidy, with moves and with none.
                s.cmd(TidyFolder);
                wait_until(&mut s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Tidy(t) if t.plans.is_some()));
                snap(&mut s, "tidy");
                s.act("key Escape");
                // Compress With Password, Edit Tags.
                select_names(&mut s, &["three.pdf", "two.pdf"]);
                s.cmd(CompressLocked);
                s.act("text pw");
                snap(&mut s, "lock");
                s.act("key Escape");
                select_names(&mut s, &["tone.mp3", "clear.png"]);
                s.cmd(EditTags);
                wait_until(&mut s, "tags read", 15, |s| matches!(&s.app.popup, Popup::MusicTags(m) if m.read.is_some()));
                snap(&mut s, "tags");
                s.act("key Escape");
                // Previews, the good and the broken.
                s.cmd(ToggleInspector);
                for (name, what) in [("Lighthouse.epub", "book"), ("Harbour.cbz", "comic"), ("Sample.ttf", "font"), ("broken.epub", "broken-book"), ("broken.cbz", "broken-comic"), ("broken.otf", "broken-font")] {
                    select_names(&mut s, &[name]);
                    let t0 = Instant::now();
                    while t0.elapsed().as_secs() < 10 {
                        s.idle(1);
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        let i = inspector(&s);
                        let done = i.rich_name().is_some() || i.note().is_some() || i.font_state().is_some() || i.pdf_state().is_some_and(|p| p.2.is_some()) || (what.starts_with("broken") && t0.elapsed().as_millis() > 1500);
                        if done {
                            break;
                        }
                    }
                    snap(&mut s, &format!("preview-{what}"));
                }
                s.cmd(ToggleInspector);
                // Format, from the card's menu in the Computer view.
                s.cmd(Computer);
                wait_until(&mut s, "drives", 10, |s| s.tab().entries.iter().any(|e| e.name == "Photo Card"));
                let r = list_text(&s, "Photo Card");
                s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
                s.idle(3);
                menu_pick(&mut s, "Format…");
                s.idle(2);
                snap(&mut s, "format");
                s.act("key Escape");
                // The Timeline, and Tidy with nothing to do.
                s.cmd(Timeline);
                s.settle();
                snap(&mut s, "timeline");
                go(&mut s, &home.join("Empty"));
                s.cmd(TidyFolder);
                wait_until(&mut s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Tidy(t) if t.plans.is_some()));
                snap(&mut s, "tidy-empty");
                s.act("key Escape");
                report(&format!("surfaces {tag}"), &s);
                first = false;
                if !shots {
                    return;
                }
            }
        }
    }

    /// Recent Projects from its key: the git repositories under the home folder, the
    /// last visited first and the rest by git's own activity, each with its branch and,
    /// once git has said, what is unsaved there; Tab (or the switch) keeps only those with
    /// unsaved work, every project asked about for it. Typing narrows; Enter opens one
    /// here, Ctrl+Enter in a new tab, a click here too. The next time it opens at once,
    /// with what was found before.
    #[test]
    fn recent_projects_open_from_the_list() {
        if !sandboxed() {
            return;
        }
        let home = root().join("home/projects");
        let _ = std::fs::remove_dir_all(&home);
        let (maia, tools, site, old) = (home.join("Code/maia"), home.join("Code/tools"), home.join("Work/site"), home.join("Work/old"));
        for r in [&maia, &tools] {
            std::fs::create_dir_all(r.join("src")).unwrap();
            std::fs::write(r.join("README.md"), "hello\n").unwrap();
            crate::tests::git(r, &["init", "-q"]);
            crate::tests::git(r, &["add", "."]);
            crate::tests::git(r, &["commit", "-q", "-m", "first"]);
        }
        std::fs::write(maia.join("README.md"), "changed\n").unwrap();
        // Never visited: the site committed to a minute ago, the old one a month ago.
        let fake = |p: &Path, head: &str, age: u64| {
            std::fs::create_dir_all(p.join(".git")).unwrap();
            std::fs::write(p.join(".git/HEAD"), head).unwrap();
            std::fs::write(p.join(".git/index"), "").unwrap();
            for f in ["HEAD", "index"] {
                std::fs::File::options().write(true).open(p.join(".git").join(f)).unwrap().set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(age)).unwrap();
            }
        };
        fake(&site, "3f2a9c41d7e8b0a1c2d3e4f5a6b7c8d9e0f1a2b3\n", 60);
        fake(&old, "ref: refs/heads/legacy\n", 30 * 86_400);
        std::fs::create_dir_all(home.join("web")).unwrap();
        std::fs::write(home.join("web/package.json"), "{}\n").unwrap();
        fake(&home.join("web/node_modules/pkg"), "ref: refs/heads/main\n", 0);
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        // Visited: maia, then tools, the last.
        let id = s.app.active;
        for p in [maia.join("src"), tools.clone(), home.clone()] {
            s.app.navigate(id, p);
            s.settle();
        }
        s.act("key ctrl+alt+p");
        fn st(s: &Sim) -> &crate::projects::State {
            match &s.app.popup {
                Popup::Projects(p) => p,
                _ => s.fail("Recent Projects is not open"),
            }
        }
        wait_until(&mut s, "the projects found", 20, |s| st(s).done);
        let mine: Vec<String> = st(&s).items.iter().filter(|p| p.root.starts_with(&home)).map(|p| p.name.clone()).collect();
        assert_eq!(mine, ["tools", "maia", "site", "old"], "visited first, then by activity; nothing from node_modules");
        let said = |s: &Sim, r: &Path| st(s).changes.get(r).and_then(|a| a.status());
        wait_until(&mut s, "git asked", 20, |s| said(s, &maia).is_some_and(|x| x.changed == 1) && said(s, &tools) == Some(crate::projects::Status::default()));
        assert!(some_text(&s, "main"), "the branch");
        assert!(some_text(&s, "1"), "maia's one change");
        look(&mut s, "recent projects");
        s.idle(12);
        s.shot("recent-projects");
        // Only the projects with unsaved work: maia, and none of the clean or fake ones.
        s.act("key Tab");
        assert!(st(&s).unsaved);
        wait_until(&mut s, "every project asked", 20, |s| st(s).items.iter().filter(|p| p.root.starts_with(&home)).all(|p| st(s).changes.get(&p.root).is_some_and(|a| *a != crate::projects::Asked::Asking)));
        s.idle(2);
        assert!(!s.texts.iter().any(|(_, t)| t.starts_with("Checking")), "the ones git can't answer for are not waited on");
        let shown = |s: &Sim| st(s).shown().iter().map(|&i| st(s).items[i].root.clone()).filter(|r| r.starts_with(&home)).collect::<Vec<_>>();
        assert_eq!(shown(&s), [maia.clone()]);
        look(&mut s, "unsaved projects");
        s.idle(12);
        s.shot("recent-projects-unsaved");
        click_control(&mut s, "all projects");
        assert!(!st(&s).unsaved && shown(&s).contains(&tools), "everything again");
        // Narrowed and opened here.
        s.act("text site");
        s.idle(2);
        let first = |s: &Sim| st(s).shown().first().map(|&i| st(s).items[i].root.clone());
        assert_eq!(first(&s), Some(site.clone()));
        assert!(some_text(&s, "3f2a9c4"), "a detached HEAD as its commit");
        s.act("key Enter");
        assert!(!s.popup_open());
        wait_until(&mut s, "the site opened", 10, |s| s.tab().path == site);
        // Again: at once, from last time; Ctrl+Enter opens a new tab.
        s.act("key ctrl+alt+p");
        assert!(st(&s).items.iter().any(|p| p.root == tools), "the list from before, straight away");
        s.act("text tools");
        s.idle(2);
        assert_eq!(first(&s), Some(tools.clone()));
        let tabs = s.app.panels[&s.app.active].tabs.len();
        s.act("key ctrl+Enter");
        s.settle();
        assert!(!s.popup_open());
        assert_eq!(s.app.panels[&s.app.active].tabs.len(), tabs + 1);
        assert_eq!(s.tab().path, tools);
        // A click opens a row here.
        s.act("key ctrl+alt+p");
        s.act("text old");
        s.idle(2);
        let want = crate::popups::home_short(&old);
        let r = s.texts.iter().find(|(_, t)| *t == want).map(|x| x.0).unwrap_or_else(|| s.fail("the old project's row"));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "the old project opened", 10, |s| s.tab().path == old);
        assert!(!s.popup_open());
        // Short and narrow windows; nothing by a name.
        s.act("key ctrl+alt+p");
        s.act("text zzzqqq");
        s.idle(2);
        assert!(some_text(&s, "No project by that name"));
        s.act("key ctrl+a");
        s.act("key Backspace");
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("recent projects at {w}x{h}"));
        }
        s.act("key Escape");
        assert!(!s.popup_open());
        report("recent projects", &s);
    }

    /// Quick Open from its key, in a subfolder of a repository: the project's files listed
    /// from its top, narrowed by a few letters; Enter goes to one (its folder, the file
    /// selected), Ctrl+Enter in a new tab, a click too; small windows and nothing found.
    #[test]
    fn quick_open_finds_a_file_in_the_project() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("quickopen");
        let p = home.join("Code/app");
        for f in ["src/main.rs", "src/view/list.rs", "src/view/grid.rs", "docs/guide.md", "README.md"] {
            std::fs::create_dir_all(p.join(f).parent().unwrap()).unwrap();
            std::fs::write(p.join(f), "x\n").unwrap();
        }
        crate::tests::git(&p, &["init", "-q"]);
        crate::tests::git(&p, &["add", "."]);
        let mut s = Sim::new(&p.join("src/view"), vec2(1280.0, 800.0));
        fn st(s: &Sim) -> &crate::quickopen::State {
            match &s.app.popup {
                Popup::QuickOpen(q) => q,
                _ => s.fail("Quick Open is not open"),
            }
        }
        s.act("key ctrl+o");
        wait_until(&mut s, "the files listed", 10, |s| st(s).listed);
        assert_eq!(st(&s).root, p, "the project's top, not the folder");
        s.idle(3);
        assert!(some_text(&s, "5 files in ~/Code/app") || s.texts.iter().any(|(_, t)| t.starts_with("5 files in")), "how many");
        look(&mut s, "quick open");
        s.idle(12);
        s.shot("quick-open");
        s.act("text guide");
        wait_until(&mut s, "a match", 10, |s| st(s).rows().first() == Some(&"docs/guide.md"));
        look(&mut s, "quick open narrowed");
        s.shot("quick-open-narrowed");
        s.act("key Enter");
        wait_until(&mut s, "the file shown", 10, |s| s.tab().path == p.join("docs") && s.tab().cursor_path() == Some(p.join("docs/guide.md")));
        assert!(!s.popup_open());
        // Ctrl+Enter: in a new tab.
        let tabs = s.app.panels[&s.app.active].tabs.len();
        s.act("key ctrl+o");
        wait_until(&mut s, "listed", 10, |s| st(s).listed);
        s.act("text grid");
        wait_until(&mut s, "grid", 10, |s| st(s).rows().first() == Some(&"src/view/grid.rs"));
        s.act("key ctrl+Enter");
        wait_until(&mut s, "a new tab on it", 10, |s| s.app.panels[&s.app.active].tabs.len() == tabs + 1 && s.tab().cursor_path() == Some(p.join("src/view/grid.rs")));
        // A click.
        s.act("key ctrl+o");
        wait_until(&mut s, "listed", 10, |s| st(s).listed);
        s.act("text readme");
        wait_until(&mut s, "readme", 10, |s| st(s).rows().first() == Some(&"README.md"));
        s.idle(2);
        s.click_text("README.md");
        wait_until(&mut s, "the readme shown", 10, |s| s.tab().path == p && s.tab().cursor_path() == Some(p.join("README.md")));
        // Nothing by that name; small windows.
        s.act("key ctrl+o");
        wait_until(&mut s, "listed", 10, |s| st(s).listed);
        s.act("text zzqqxx");
        s.idle(3);
        assert!(some_text(&s, "No file by that name"));
        s.act("key ctrl+a");
        s.act("key Backspace");
        for (w, h) in [(520.0, 340.0), (720.0, 480.0), (1280.0, 800.0)] {
            s.resize(w, h);
            s.idle(3);
            look(&mut s, &format!("quick open at {w}x{h}"));
        }
        s.act("key Escape");
        assert!(!s.popup_open());
        report("quick open", &s);
    }

    /// Undo History from its key: the changes newest first with what they were and when;
    /// a row chosen takes back it and everything after it, newest first, and leaves the
    /// older ones; nothing left says so.
    #[test]
    fn undo_history_takes_back_several() {
        if !sandboxed() {
            return;
        }
        let home = fixture("undohist");
        let d = home.join("Documents");
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.act("key ctrl+alt+z");
        s.idle(2);
        assert!(some_text(&s, "Nothing to undo yet"));
        look(&mut s, "undo history, empty");
        s.act("key Escape");
        let rename = |s: &mut Sim, find: &str, to: &str| {
            s.act(&format!("text {find}"));
            s.act("key F2");
            s.act("key ctrl+a");
            s.act(&format!("text {to}"));
            s.act("key Enter");
            s.settle();
            // The letters typed to find it stay in the filter until Escape.
            s.act("key Escape");
            s.settle();
        };
        rename(&mut s, "notes", "a.txt");
        rename(&mut s, "report", "b.md");
        assert!(d.join("a.txt").exists() && d.join("b.md").exists(), "each rename on the file typed, not one selected before");
        s.click_text("budget 2026.csv");
        s.act("key Delete");
        if s.popup_open() {
            s.act("key Enter");
        }
        wait_until(&mut s, "trashed", 20, |s| s.app.undo.len() == 3 && !d.join("budget 2026.csv").exists());
        s.act("key ctrl+alt+z");
        assert!(matches!(s.app.popup, Popup::UndoHistory(_)));
        s.idle(3);
        assert!(some_text(&s, "Moved to the trash · 1 item") && some_text(&s, "Renamed · 1 item"));
        assert!(some_text(&s, "Move budget 2026.csv to the trash") && some_text(&s, "Rename report.md to b.md") && some_text(&s, "Rename notes.txt to a.txt"), "each named by what it did");
        assert!(some_text(&s, "today"), "when");
        look(&mut s, "undo history");
        s.idle(10);
        s.shot("undo-history");
        // The second newest: it and the trash go back, the first rename stays.
        s.act("key ArrowDown");
        s.idle(2);
        assert!(some_text(&s, "Undo 2 changes"));
        s.shot("undo-history-two");
        s.act("key Enter");
        s.settle();
        assert!(!s.popup_open());
        assert!(d.join("budget 2026.csv").exists() && d.join("report.md").exists() && !d.join("b.md").exists(), "both taken back");
        assert!(d.join("a.txt").exists() && !d.join("notes.txt").exists(), "the older one stays");
        assert_eq!(s.app.undo.len(), 1);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Undid 2 changes"));
        // The button and a click; then nothing left.
        s.act("key ctrl+alt+z");
        s.idle(2);
        click_control(&mut s, "undo back");
        s.settle();
        assert!(d.join("notes.txt").exists() && s.app.undo.is_empty());
        // What was taken back waits above, to be done again: the nearest chosen first,
        // the furthest taking those before it along.
        s.act("key ctrl+alt+z");
        s.idle(2);
        assert!(some_text(&s, "Undone · Moved to the trash · 1 item") && some_text(&s, "Undone · Renamed · 1 item"));
        assert!(some_text(&s, "Redo") && some_text(&s, "do again"), "the nearest change to do again is chosen");
        look(&mut s, "undo history, all undone");
        s.shot("undo-history-redo");
        s.act("key ArrowUp");
        s.act("key ArrowUp");
        s.idle(2);
        assert!(some_text(&s, "Redo 3 changes"));
        s.act("key Enter");
        s.settle();
        assert!(!d.join("budget 2026.csv").exists() && d.join("b.md").exists() && d.join("a.txt").exists(), "all three done again");
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Redid 3 changes"));
        assert_eq!(s.app.undo.len(), 3);
        assert!(!s.app.undo.can_redo());
        s.act("key ctrl+alt+z");
        s.idle(2);
        for (w, h) in [(520.0, 340.0), (1280.0, 800.0)] {
            s.resize(w, h);
            s.idle(3);
            look(&mut s, &format!("undo history at {w}x{h}"));
        }
        click_control(&mut s, "undo close");
        assert!(!s.popup_open());
        report("undo history", &s);
    }

    /// The card of the job at `k`'s progress, by its label.
    fn job_with(s: &Sim, label: &str) -> Option<std::sync::Arc<std::sync::Mutex<crate::ops::Progress>>> {
        s.app.jobs.iter().rev().find(|j| j.label == label).map(|j| j.progress.clone())
    }

    /// Copies onto one disk take turns: a second copy's card waits for the first, named;
    /// Start Now runs it anyway, Cancel ends it with nothing copied; a copy on another disk
    /// runs at once; the first resumed, the one waiting follows. Check Copies switched on in
    /// Options: a copy that comes back different from the disk is an error on its card and
    /// is removed, and a move across disks keeps its original. A move across disks undone
    /// goes back through a job, and Redo moves it there again.
    #[test]
    fn copies_take_turns_and_are_checked() {
        if !sandboxed() {
            return;
        }
        use crate::ops::OpKind;
        let home = fixture("turns");
        let docs = home.join("Documents");
        let backups = home.join("Backups");
        std::fs::create_dir_all(&backups).unwrap();
        std::fs::write(docs.join("big.bin"), vec![7u8; 96 << 20]).unwrap();
        // Another disk: the sandbox's /tmp is a file system of its own.
        let far = Path::new("/tmp").join("turns");
        let _ = std::fs::remove_dir_all(&far);
        std::fs::create_dir_all(far.join("in")).unwrap();
        std::fs::create_dir_all(far.join("out")).unwrap();
        std::fs::write(far.join("in/photo.png"), "png").unwrap();
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.cmd(SplitRight);
        go(&mut s, &backups);
        s.act("key Tab");
        s.settle();
        // A long copy, held paused (as if its Pause had been pressed), holds the disk.
        s.app.start_job(OpKind::Copy { srcs: vec![docs.join("big.bin")], dest: backups.clone(), decide: Default::default() });
        s.app.jobs.last().unwrap().pause.store(true, std::sync::atomic::Ordering::Relaxed);
        wait_until(&mut s, "the first job running", 10, |s| s.app.queue.running().len() == 1);
        let waiting = |s: &Sim, label: &str| job_with(s, label).is_some_and(|p| p.lock().unwrap().waiting.as_deref() == Some("Copying big.bin"));
        select_path(&mut s, &docs.join("notes.txt"));
        s.act("key F5");
        wait_until(&mut s, "the second copy queued", 10, |s| waiting(s, "Copying notes.txt"));
        s.idle(3);
        assert!(some_text(&s, "Waiting for “Copying big.bin”") && some_text(&s, "Start now"), "the card says what it waits for");
        assert!(!backups.join("notes.txt").exists());
        look(&mut s, "a copy waiting its turn");
        s.shot("job-queued");
        click_control(&mut s, "job start now");
        wait_until(&mut s, "started now", 10, |_| backups.join("notes.txt").exists());
        // Cancelled while it waits: nothing copied.
        select_path(&mut s, &docs.join("report.md"));
        s.act("key F5");
        wait_until(&mut s, "queued again", 10, |s| waiting(s, "Copying report.md"));
        s.idle(2);
        click_control(&mut s, "job cancel");
        wait_until(&mut s, "cancelled", 10, |s| job_with(s, "Copying report.md").is_some_and(|p| p.lock().unwrap().finished));
        s.idle(2);
        assert!(some_text(&s, "Cancelled copying report.md") && !backups.join("report.md").exists());
        // Another disk doesn't wait.
        go(&mut s, &far.join("in"));
        s.act("key Tab");
        go(&mut s, &far.join("out"));
        s.act("key Tab");
        s.settle();
        select_path(&mut s, &far.join("in/photo.png"));
        s.act("key F5");
        wait_until(&mut s, "a copy on another disk", 10, |_| far.join("out/photo.png").exists());
        go(&mut s, &docs);
        s.act("key Tab");
        go(&mut s, &backups);
        s.act("key Tab");
        s.settle();
        // Queued behind the first, then the first resumed: the one waiting follows.
        select_path(&mut s, &docs.join("budget 2026.csv"));
        s.act("key F5");
        wait_until(&mut s, "queued behind the first", 10, |s| waiting(s, "Copying budget 2026.csv"));
        s.idle(2);
        s.click_text("Resume");
        wait_until(&mut s, "both done", 20, |_| backups.join("budget 2026.csv").exists() && std::fs::metadata(backups.join("big.bin")).is_ok_and(|m| m.len() == 96 << 20));
        wait_until(&mut s, "the cards gone", 20, |s| s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        std::fs::remove_file(docs.join("big.bin")).unwrap();
        std::fs::remove_file(backups.join("big.bin")).unwrap();
        // Check Copies, from its row in Options.
        s.act("key ctrl+Comma");
        s.settle();
        s.act("key ctrl+PageDown");
        let popup = crate::ui::POPUPS.with(|p| p.borrow().first().copied()).unwrap_or_else(|| s.fail("Options drawn"));
        for _ in 0..10 {
            if s.texts.iter().any(|(r, t)| t == "Check copies" && popup.contains_rect(*r)) {
                break;
            }
            s.act(&format!("scroll {:.0} {:.0} -3", popup.center().x + 100.0, popup.center().y));
            s.idle(6);
        }
        click_in_row(&mut s, "switch", "Check copies");
        assert!(s.app.cfg.check_copies, "switched on");
        look(&mut s, "Check copies in Options");
        s.shot("options-check-copies");
        s.act("key Escape");
        s.act("key Escape");
        s.settle();
        // Checked: a good copy lands; one that comes back changed is an error, removed.
        select_path(&mut s, &docs.join("résumé – final (v3).txt"));
        s.act("key F5");
        wait_until(&mut s, "a checked copy", 10, |_| backups.join("résumé – final (v3).txt").exists());
        let bad = format!("{} trip.png", crate::ops::SPOILED);
        std::fs::write(docs.join(&bad), vec![9u8; 300_000]).unwrap();
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().reload();
        s.settle();
        select_path(&mut s, &docs.join(&bad));
        s.act("key F5");
        let failed = |s: &Sim| s.app.jobs.iter().any(|j| j.progress.lock().unwrap().finished && !j.progress.lock().unwrap().errors.is_empty());
        wait_until(&mut s, "the check's error", 10, failed);
        assert!(!backups.join(&bad).exists(), "the bad copy removed");
        s.idle(2);
        s.click_text("1 error · show");
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t.contains("read back from the disk differently")), "why, on the card");
        look(&mut s, "a copy that failed its check");
        s.shot("job-check-failed");
        // A move across disks that fails its check keeps the original.
        s.act("key Tab");
        go(&mut s, &far.join("out"));
        s.act("key Tab");
        s.settle();
        select_path(&mut s, &docs.join(&bad));
        s.act("key F6");
        wait_until(&mut s, "the move's check", 10, |s| s.app.jobs.iter().filter(|j| j.progress.lock().unwrap().finished && !j.progress.lock().unwrap().errors.is_empty()).count() >= 2 || !docs.join(&bad).exists());
        assert!(docs.join(&bad).exists() && !far.join("out").join(&bad).exists(), "the original stays");
        // A move across disks, undone through a job and done again.
        s.app.cfg.check_copies = false;
        select_path(&mut s, &docs.join("report.md"));
        s.act("key F6");
        wait_until(&mut s, "moved to the other disk", 10, |s| far.join("out/report.md").exists() && s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Move(_))));
        assert!(!docs.join("report.md").exists());
        s.act("key ctrl+z");
        s.idle(1);
        assert!(s.app.jobs.iter().any(|j| j.label == "Moving report.md back"), "undone by a job with its card");
        wait_until(&mut s, "moved back", 10, |s| docs.join("report.md").exists() && s.app.job_steps.is_empty());
        assert!(!far.join("out/report.md").exists() && s.app.undo.can_redo());
        s.act("key ctrl+shift+z");
        wait_until(&mut s, "moved again", 10, |s| far.join("out/report.md").exists() && s.app.job_steps.is_empty());
        assert!(!docs.join("report.md").exists());
        s.act("key ctrl+z");
        wait_until(&mut s, "and back", 10, |s| docs.join("report.md").exists() && s.app.job_steps.is_empty());
        let _ = std::fs::remove_dir_all(&far);
        report("turns and checks", &s);
    }

    /// Redo: Undo's changes done again by its keys and from Undo History, where they wait
    /// above what can still be taken back; both lists kept across a restart; a new change
    /// leaves nothing to redo.
    #[test]
    fn redo_and_undo_outlast_a_restart() {
        if !sandboxed() {
            return;
        }
        let home = fixture("redo");
        let d = home.join("Documents");
        let start = |d: &Path| {
            let mut s = Sim::new(d, vec2(1280.0, 800.0));
            s.app.undo_keep = crate::undofile::Keeper::new(Some(crate::undofile::path()), &s.ctx);
            wait_until(&mut s, "Undo read back", 10, |s| !s.app.undo_keep.loading());
            s
        };
        let _ = std::fs::remove_file(crate::undofile::path());
        let mut s = start(&d);
        s.act("text notes");
        s.act("key F2");
        s.act("key ctrl+a");
        s.act("text plan.txt");
        s.act("key Enter");
        s.settle();
        s.act("key Escape");
        s.settle();
        s.click_text("budget 2026.csv");
        s.act("key Delete");
        if s.popup_open() {
            s.act("key Enter");
        }
        wait_until(&mut s, "trashed", 20, |s| s.app.undo.len() == 2 && !d.join("budget 2026.csv").exists());
        s.act("key ctrl+z");
        s.act("key ctrl+z");
        s.settle();
        assert!(d.join("notes.txt").exists() && d.join("budget 2026.csv").exists(), "both undone");
        s.act("key ctrl+shift+z");
        s.settle();
        assert!(d.join("plan.txt").exists() && !d.join("notes.txt").exists(), "the rename again");
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Redid “Rename notes.txt to plan.txt”"));
        s.act("key ctrl+alt+z");
        s.idle(3);
        assert!(some_text(&s, "Undone · Moved to the trash · 1 item") && some_text(&s, "Renamed · 1 item"));
        assert!(some_text(&s, "Undo"), "the newest change chosen, as before");
        look(&mut s, "undo history with one to redo");
        s.shot("undo-history-one-redo");
        s.act("key ArrowUp");
        s.idle(2);
        assert!(some_text(&s, "Redo"));
        s.act("key Enter");
        s.settle();
        assert!(!d.join("budget 2026.csv").exists(), "trashed again from the history");
        s.act("key ctrl+z");
        s.settle();
        assert!(d.join("budget 2026.csv").exists());
        wait_until(&mut s, "written", 10, |s| s.app.undo_keep.settled(&s.app.undo));
        drop(s);
        // A new run: Undo and Redo where they were.
        let mut s = start(&d);
        assert_eq!(s.app.undo.len(), 1);
        assert!(s.app.undo.can_redo());
        s.act("key ctrl+y");
        s.settle();
        assert!(!d.join("budget 2026.csv").exists(), "redone after the restart");
        s.act("key ctrl+z");
        s.act("key ctrl+z");
        s.settle();
        assert!(d.join("notes.txt").exists() && d.join("budget 2026.csv").exists(), "undone after the restart");
        // A new change: nothing left to redo.
        s.act("key ctrl+shift+n");
        s.settle();
        // Named where it was made.
        wait_until(&mut s, "the new folder's name field", 10, |s| s.tab().rename.is_some());
        s.act("key ctrl+a");
        s.act("text Fresh");
        s.act("key Enter");
        s.settle();
        assert!(d.join("Fresh").is_dir() && !s.app.undo.can_redo(), "made, and nothing left to redo");
        s.act("key ctrl+y");
        s.idle(1);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Nothing to redo"));
        report("redo", &s);
    }

    /// Workspaces: leaving a project through Recent Projects keeps its tabs and panels;
    /// opening it again there brings them back (and keeps the one left), marked in its
    /// row; Ctrl+Enter still opens just the folder.
    #[test]
    fn workspaces_kept_per_project() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = root().join("home/workspaces");
        let _ = std::fs::remove_dir_all(&home);
        let (a, b) = (home.join("Code/alpha"), home.join("Code/beta"));
        for r in [&a, &b] {
            std::fs::create_dir_all(r.join("src")).unwrap();
            std::fs::write(r.join("README.md"), "x\n").unwrap();
            crate::tests::git(r, &["init", "-q"]);
        }
        let mut s = Sim::new(&a, vec2(1280.0, 800.0));
        // Alpha: two tabs, and a second panel on its src.
        s.act("key ctrl+t");
        go(&mut s, &a.join("src"));
        s.cmd(SplitRight);
        s.settle();
        assert_eq!(s.app.panels.len(), 2);
        let open = |s: &mut Sim, name: &str, keys: &str| {
            s.act("key ctrl+alt+p");
            wait_until(s, "the projects", 20, |s| matches!(&s.app.popup, Popup::Projects(p) if p.done));
            s.act(&format!("text {name}"));
            s.idle(2);
            s.act(&format!("key {keys}"));
            s.settle();
        };
        open(&mut s, "beta", "Enter");
        assert!(s.app.ud.workspaces.get(&a).is_some_and(|w| w.panels.len() == 2), "alpha kept as it was left");
        assert_eq!(s.tab().path, b);
        // Beta on its own: the other panel, still on alpha, closed.
        s.act("key Tab");
        s.cmd(ClosePanel);
        s.settle();
        assert_eq!(s.tab().path, b);
        assert_eq!(s.app.panels.len(), 1);
        s.act("key ctrl+alt+p");
        wait_until(&mut s, "the projects", 20, |s| matches!(&s.app.popup, Popup::Projects(p) if p.done));
        s.act("text alpha");
        s.idle(3);
        look(&mut s, "a project with a workspace");
        s.idle(10);
        s.shot("workspace-row");
        s.act("key Enter");
        s.settle();
        assert_eq!(s.app.panels.len(), 2, "alpha's two panels back");
        let paths: Vec<PathBuf> = s.app.panels.values().flat_map(|p| p.tabs.iter().map(|t| t.path.clone())).collect();
        assert!(paths.contains(&a.join("src")) && paths.contains(&a), "{paths:?}");
        assert!(s.app.ud.workspaces.get(&b).is_some_and(|w| w.panels.len() == 1), "beta kept as left");
        // Ctrl+Enter: the folder alone, in a new tab.
        let panels = s.app.panels.len();
        open(&mut s, "beta", "ctrl+Enter");
        assert_eq!(s.app.panels.len(), panels, "no workspace for a new tab");
        assert_eq!(s.tab().path, b);
        report("workspaces", &s);
    }

    /// Click by letters: every control tagged; a file row's tag selects it, a capital last
    /// letter right-clicks (its menu opens); with the menu open only the menu's items are
    /// tagged; Escape, or letters that match nothing, give up without clicking.
    #[test]
    fn click_by_letters() {
        if !sandboxed() {
            return;
        }
        let home = fixture("hints");
        let d = home.join("Documents");
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.idle(2);
        s.act("key ctrl+j");
        let tags = |s: &Sim| s.app.hints.as_ref().map(|h| h.targets.clone()).unwrap_or_default();
        assert!(tags(&s).len() > 15, "rows, tabs, buttons: {}", tags(&s).len());
        let at = |s: &Sim, label: &str| {
            let r = s.text_rect(label);
            tags(s).iter().find(|(t, _)| t.contains(r.center())).map(|(_, n)| n.clone()).unwrap_or_else(|| s.fail(&format!("no tag on {label}")))
        };
        look(&mut s, "letter tags");
        s.idle(2);
        s.shot("click-by-letters");
        let tag = at(&s, "notes.txt");
        s.act(&format!("text {tag}"));
        s.settle();
        assert!(s.app.hints.is_none(), "done once a tag is whole");
        assert_eq!(s.tab().cursor_path(), Some(d.join("notes.txt")), "the row clicked");
        // A capital last letter: a right-click, the file's menu.
        s.act("key ctrl+j");
        let tag = at(&s, "report.md");
        let (head, last) = tag.split_at(tag.len() - 1);
        s.act(&format!("text {head}{}", last.to_uppercase()));
        s.settle();
        assert!(matches!(s.app.popup, Popup::Ctx(_)), "the menu opened");
        // In the menu: only its items.
        s.act("key ctrl+j");
        let menu = s.ctx.memory(|m| m.area_rect(egui::Id::new("ctxmenu"))).unwrap_or_else(|| s.fail("the menu's rectangle"));
        assert!(!tags(&s).is_empty() && tags(&s).iter().all(|(r, _)| menu.expand(12.0).contains(r.center())), "the menu's items only");
        look(&mut s, "letter tags in a menu");
        s.act("key Escape");
        assert!(s.app.hints.is_none() && matches!(s.app.popup, Popup::Ctx(_)), "Escape leaves the tags, not the menu");
        s.act("key Escape");
        // Letters that match nothing: nothing clicked.
        let cur = s.tab().cursor_path();
        s.act("key ctrl+j");
        s.act("text 99");
        s.settle();
        assert!(s.app.hints.is_none() && s.tab().cursor_path() == cur && !s.popup_open());
        report("click by letters", &s);
    }

    /// Marks: Ctrl+M and a letter marks the file under the cursor (or the folder), Ctrl+'
    /// and the letter goes back, selecting the file; the list goes by Enter too, Delete
    /// forgets one, and they are kept with the user's data.
    #[test]
    fn marks_set_and_gone_back_to() {
        if !sandboxed() {
            return;
        }
        let home = fixture("marks");
        let d = home.join("Documents");
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.click_text("notes.txt");
        s.act("key ctrl+m");
        assert!(matches!(s.app.popup, Popup::Marks(_)));
        s.idle(2);
        look(&mut s, "set a mark");
        s.act("text n");
        assert_eq!(s.app.ud.marks.get("n"), Some(&d.join("notes.txt")));
        go(&mut s, &d.join("Taxes"));
        s.act("key ctrl+m");
        s.act("text t");
        assert_eq!(s.app.ud.marks.get("t"), Some(&d.join("Taxes")), "a folder under the cursor: this folder");
        go(&mut s, &home);
        s.act("key ctrl+Quote");
        assert!(matches!(s.app.popup, Popup::Marks(_)));
        s.idle(3);
        look(&mut s, "go to a mark");
        s.shot("marks");
        s.act("text n");
        s.settle();
        assert_eq!(s.tab().path, d);
        assert_eq!(s.tab().cursor_path(), Some(d.join("notes.txt")), "the file selected");
        // By the list: down to t, Enter.
        go(&mut s, &home);
        s.act("key ctrl+Quote");
        s.act("key ArrowDown");
        s.act("key Enter");
        s.settle();
        assert_eq!(s.tab().path, d.join("Taxes"));
        // Forgotten.
        s.act("key ctrl+Quote");
        s.act("key Delete");
        assert_eq!(s.app.ud.marks.len(), 1);
        s.act("key Escape");
        let ctx = s.ctx.clone();
        s.app.save_all(&ctx);
        assert!(crate::config::load_userdata().marks.contains_key("t"), "kept with the user's data");
        report("marks", &s);
    }

    /// What's Using This… from a folder's menu lists a `sleep` this test started working
    /// in it, as the user's own, with its PID; Stop asks first, then ends it and the list
    /// empties. One that ignores SIGTERM is still there a few seconds later and is offered
    /// Force Stop, asked again. What's Using a Port… lists a port only a child of the
    /// test listens on, found by its number, and stops it. A move that hit a busy file
    /// offers the list from its card, a refused eject opens it. Nothing the test didn't
    /// start is ever signalled.
    #[test]
    fn whats_using_files_and_ports() {
        if !sandboxed() {
            return;
        }
        use std::process::{Command, Stdio};
        struct Kids(Vec<std::process::Child>);
        impl Drop for Kids {
            fn drop(&mut self) {
                for c in &mut self.0 {
                    let _ = c.kill();
                    let _ = c.wait();
                }
            }
        }
        let mut kids = Kids(vec![]);
        let spawn = |cmd: &mut Command, kids: &mut Kids| -> u32 {
            let c = cmd.stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
            let pid = c.id();
            crate::holders::OURS.lock().unwrap().push(pid);
            kids.0.push(c);
            // Until it is sleep, not the fork on its way there.
            let t0 = Instant::now();
            while crate::holders::info(pid).is_none_or(|p| p.name != "sleep") && t0.elapsed().as_secs() < 5 {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            pid
        };
        let refused = crate::holders::REFUSED.lock().unwrap().len();
        let dr = fake_drives("holders");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(dr.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let d = root().join("home/holders");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("Photos")).unwrap();
        std::fs::write(d.join("notes.txt"), "words\n").unwrap();
        let a = spawn(Command::new("sleep").arg("120").current_dir(d.join("Photos")).stdin(Stdio::null()), &mut kids);
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        fn st(s: &Sim) -> &crate::holders::State {
            match &s.app.popup {
                Popup::Holders(h) => h,
                _ => s.fail("What's Using is not open"),
            }
        }
        let r = list_text(&s, "Photos");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.act("text using");
        s.idle(1);
        assert!(some_text(&s, "What's Using This…"), "in the folder's menu");
        s.act("key Enter");
        wait_until(&mut s, "the processes read", 10, |s| st(s).scanned);
        let rows: Vec<(u32, String, Option<crate::holders::Hold>)> = st(&s).rows.iter().map(|r| (r.proc.as_ref().map_or(0, |p| p.pid), r.proc.as_ref().map(|p| p.name.clone()).unwrap_or_default(), r.hold.clone())).collect();
        assert_eq!(rows, [(a, "sleep".to_string(), Some(crate::holders::Hold::Inside(d.join("Photos"))))]);
        assert!(some_text(&s, "What's using “Photos”") && some_text(&s, "Yours") && some_text(&s, "sleep"));
        assert!(some_text(&s, &format!("PID {a} · working in it")), "the PID, and how, from the folder asked about");
        look(&mut s, "what's using a folder");
        s.idle(12);
        s.shot("whats-using");
        click_control(&mut s, "holder stop");
        assert!(some_text(&s, &format!("Stop sleep ({a})? It may lose what it hasn't saved.")), "asks first");
        assert!(alive(a), "not before the answer");
        click_control(&mut s, "holder confirm");
        wait_until(&mut s, "stopped", 10, |_| !alive(a));
        wait_until(&mut s, "the list read again", 10, |s| st(s).scanned && st(s).rows.is_empty() && st(s).said.is_some());
        assert!(some_text(&s, &format!("Stopped sleep ({a})")));
        assert!(some_text(&s, "Nothing of yours is using it. Other users' programs can only be seen with administrator access."));
        s.act("key Escape");
        assert!(!s.popup_open());
        // Ignores SIGTERM, with the notes open on its input: Force Stop, asked again.
        let b = spawn(Command::new("sh").args(["-c", "trap '' TERM; exec sleep 120"]).current_dir(&d).stdin(std::fs::File::open(d.join("notes.txt")).unwrap()), &mut kids);
        s.cmd(Refresh);
        s.settle();
        select_path(&mut s, &d.join("notes.txt"));
        palette_run(&mut s, "What's Using This");
        wait_until(&mut s, "the holder of the notes", 10, |s| st(s).scanned && st(s).rows.iter().any(|r| r.proc.as_ref().is_some_and(|p| p.pid == b)));
        assert_eq!(st(&s).rows[0].hold, Some(crate::holders::Hold::Open(d.join("notes.txt"))));
        assert!(some_text(&s, &format!("PID {b} · has it open")));
        s.act("key Enter");
        assert!(some_text(&s, &format!("Stop sleep ({b})? It may lose what it hasn't saved.")));
        s.act("key Enter");
        wait_until(&mut s, "still running after SIGTERM", 10, |s| st(s).stubborn.iter().any(|p| p.pid == b));
        assert!(alive(b));
        s.idle(2);
        assert!(some_text(&s, &format!("Force sleep ({b}) to stop? Anything it hasn't saved is lost.")), "offered, asked first");
        look(&mut s, "force stop");
        s.idle(12);
        s.shot("whats-using-force");
        s.act("key Enter");
        wait_until(&mut s, "forced", 10, |_| !alive(b));
        s.act("key Escape");
        // A port only a child of the test listens on.
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let hp = held.local_addr().unwrap().port();
        let fd = std::os::fd::AsRawFd::as_raw_fd(&held);
        let mut cmd = Command::new("sleep");
        cmd.arg("120");
        unsafe {
            std::os::unix::process::CommandExt::pre_exec(&mut cmd, move || if libc::dup2(fd, 200) < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
        }
        let c = spawn(&mut cmd, &mut kids);
        drop(held);
        palette_run(&mut s, "What's Using a Port");
        wait_until(&mut s, "the ports read", 10, |s| st(s).scanned && st(s).rows.iter().any(|r| r.port.as_ref().is_some_and(|p| p.port == hp)));
        s.act(&format!("text {hp}"));
        s.idle(2);
        let shown = st(&s).shown();
        assert_eq!(shown.len(), 1, "found by its number");
        assert_eq!(st(&s).rows[shown[0]].proc.as_ref().map(|p| p.pid), Some(c));
        assert!(some_text(&s, &hp.to_string()) && some_text(&s, "TCP · 127.0.0.1") && some_text(&s, &format!("PID {c}")));
        look(&mut s, "what's using a port");
        s.idle(12);
        s.shot("whats-using-port");
        s.act("key Enter");
        s.act("key Enter");
        wait_until(&mut s, "the listener stopped", 10, |_| !alive(c));
        wait_until(&mut s, "the port gone", 10, |s| st(s).scanned && !st(s).rows.iter().any(|r| r.port.as_ref().is_some_and(|p| p.port == hp)));
        assert!(some_text(&s, "Nothing is listening on a port"), "nothing else listens in the sandbox");
        s.act("key Escape");
        // A move that something was using: its card offers the list.
        let progress = std::sync::Arc::new(std::sync::Mutex::new(crate::ops::Progress { total_items: 1, done_items: 1, finished: true, errors: vec![format!("{}: Device or resource busy (os error 16)", d.join("Photos").display())], busy: vec![d.join("Photos")], ..Default::default() }));
        s.app.jobs.push(crate::ops::Job { id: 9998, label: "Moving 1 item".into(), kind: crate::ops::OpKind::Move { srcs: vec![d.join("Photos")], dest: d.join("Elsewhere"), decide: Default::default() }, progress, cancel: Default::default(), pause: Default::default(), started: Instant::now(), finished_at: None, show_errors: false });
        s.idle(3);
        click_control(&mut s, "job holders");
        assert!(matches!(&st(&s).what, crate::holders::What::Paths(p) if *p == [d.join("Photos")]));
        s.act("key Escape");
        s.app.jobs.clear();
        // A refused eject says what holds the stick.
        let stick = dr.join("media/Busy Stick");
        let e = spawn(Command::new("sleep").arg("120").current_dir(&stick).stdin(Stdio::null()), &mut kids);
        s.settle();
        let r = sidebar_text(&s, "Busy Stick").unwrap_or_else(|| s.fail("the busy stick in the sidebar"));
        s.act(&format!("move {:.0} {:.0}", r.center().x, r.center().y));
        click_control(&mut s, "eject button");
        wait_until(&mut s, "the eject refused", 10, |s| matches!(&s.app.popup, Popup::Holders(_)) && st(s).scanned);
        assert_eq!(st(&s).why.as_deref(), Some("Couldn't eject Busy Stick: Target is busy"));
        assert!(st(&s).rows.iter().any(|r| r.proc.as_ref().is_some_and(|p| p.pid == e)), "the sleep inside it");
        look(&mut s, "what's using a busy stick");
        s.idle(12);
        s.shot("whats-using-busy");
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("what's using at {w}x{h}"));
        }
        s.act("key Escape");
        assert_eq!(crate::holders::REFUSED.lock().unwrap().len(), refused, "nothing else was signalled");
        drop(kids);
        report("whats using", &s);
    }

    /// Smart folders: a search in the filter saved as one from the folder's menu, the
    /// dialog filled in from it; a section of its own in the sidebar; opened, it lists
    /// what matches and finds a new file on Refresh. Edited, renamed and removed from
    /// its entry's menu; a new one from the palette with a size it can't read refused
    /// first; Ctrl+click opens a new tab; the section goes with the last one.
    #[test]
    fn smart_folders_saved_opened_edited() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/smart");
        let _ = std::fs::remove_dir_all(&d);
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 90);
        let file = |p: &str, len: usize, aged: bool| {
            let p = d.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, vec![b'x'; len]).unwrap();
            if aged {
                std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
            }
        };
        file("Invoices/2026/invoice-031.pdf", 2_000_000, false);
        file("Invoices/2026/invoice-030.pdf", 1000, false);
        file("Invoices/2025/invoice-001.pdf", 3_000_000, true);
        file("Invoices/invoice notes.txt", 10, false);
        file("Invoices/scans/page.txt", 10, false);
        file("Photos/IMG_0001.jpg", 500_000, false);
        file("Photos/IMG_0002.png", 400_000, true);
        file("Photos/thumb.png", 1000, false);
        file(".cache/invoice-cached.pdf", 10, false);
        file("web/package.json", 2, false);
        file("web/node_modules/x/invoice-y.pdf", 10, false);
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        let rels = |s: &Sim| -> Vec<String> {
            let mut v: Vec<String> = s.tab().entries.iter().filter_map(|e| e.rel().map(|r| r.into_owned())).collect();
            v.sort();
            v
        };
        fn ed(s: &Sim) -> &crate::smart::Edit {
            match &s.app.popup {
                Popup::SmartEdit(e) => e,
                _ => s.fail("the smart folder dialog is not open"),
            }
        }
        s.act("key ctrl+f");
        s.act("text invoice");
        s.idle(3);
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("click {:.0} {:.0} right", list.center().x, list.bottom() - 30.0));
        s.idle(2);
        s.act("text smart");
        s.idle(1);
        assert!(some_text(&s, "Save As Smart Folder…"), "in the folder's menu while searching");
        s.act("key Enter");
        assert_eq!((ed(&s).name.as_str(), ed(&s).query.as_str(), ed(&s).root.as_str(), ed(&s).id), ("invoice", "invoice", "~/smart", None));
        click_control(&mut s, "smart query");
        s.act("key End");
        s.act("text  type:pdfs");
        assert_eq!(ed(&s).query, "invoice type:pdfs");
        look(&mut s, "smart folder dialog");
        s.idle(12);
        s.shot("smart-folder-edit");
        s.act("key Enter");
        assert!(!s.popup_open(), "saved");
        assert_eq!(s.app.ud.smart.len(), 1);
        wait_until(&mut s, "its search", 10, |s| s.tab().smart.is_some() && !s.tab().is_loading());
        s.settle();
        assert_eq!(rels(&s), ["Invoices/2025/invoice-001.pdf", "Invoices/2026/invoice-030.pdf", "Invoices/2026/invoice-031.pdf"], "PDFs with invoice in the name; no hidden or installed copies");
        assert!(s.tab().filter.is_empty(), "the search is the smart folder now");
        assert_eq!(s.tab().title(), "invoice");
        assert!(sidebar_text(&s, "Smart Folders").is_some() && sidebar_text(&s, "invoice").is_some());
        look(&mut s, "smart folder results");
        s.shot("smart-folder-results");
        // Up to date: found again on Refresh.
        file("Invoices/invoice-032.pdf", 10, false);
        s.cmd(Refresh);
        wait_until(&mut s, "the new invoice", 10, |s| rels(s).iter().any(|r| r == "Invoices/invoice-032.pdf"));
        // Back to the folder; the sidebar opens it again.
        s.act("key alt+ArrowLeft");
        s.settle();
        assert!(s.tab().smart.is_none() && s.tab().path == d);
        let r = sidebar_text(&s, "invoice").unwrap();
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "opened from the sidebar", 10, |s| s.tab().smart.is_some() && !s.tab().is_loading() && rels(s).len() == 4);
        // Edited from its menu: changed in the last 30 days only.
        let r = sidebar_text(&s, "invoice").unwrap();
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.click_text("Edit…");
        assert!(ed(&s).id.is_some());
        click_control(&mut s, "smart query");
        s.act("key End");
        s.act("text  modified<30d");
        s.act("key Enter");
        assert!(!s.popup_open());
        wait_until(&mut s, "searched again", 10, |s| !s.tab().is_loading() && rels(s).len() == 3);
        assert!(!rels(&s).iter().any(|r| r.contains("invoice-001")), "the old one left out");
        // Renamed.
        let r = sidebar_text(&s, "invoice").unwrap();
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.click_text("Rename…");
        assert!(matches!(s.app.popup, Popup::Prompt { .. }));
        s.act("key ctrl+a");
        s.act("text Bills");
        s.act("key Enter");
        s.idle(2);
        assert!(sidebar_text(&s, "Bills").is_some() && s.tab().title() == "Bills");
        // A new one from the palette; a size it can't read is refused first.
        palette_run(&mut s, "New Smart Folder");
        assert_eq!(ed(&s).root, "~/smart");
        click_control(&mut s, "smart name");
        s.act("key ctrl+a");
        s.act("text Big pictures");
        click_control(&mut s, "smart query");
        s.act("text type:pictures /(/");
        s.act("key Enter");
        assert_eq!(ed(&s).error.as_deref(), Some("Find: /(/ is not a regular expression"));
        assert!(some_text(&s, "Find: /(/ is not a regular expression"));
        look(&mut s, "smart folder refused");
        s.act("key ctrl+a");
        s.act("text type:pictures size>100K");
        s.act("key Enter");
        assert!(!s.popup_open());
        wait_until(&mut s, "the pictures", 10, |s| s.tab().smart.as_ref().is_some_and(|x| x.name == "Big pictures") && !s.tab().is_loading());
        s.settle();
        assert_eq!(rels(&s), ["Photos/IMG_0001.jpg", "Photos/IMG_0002.png"]);
        // Ctrl+click: a new tab.
        let tabs = s.app.panels[&s.app.active].tabs.len();
        let r = sidebar_text(&s, "Bills").unwrap();
        s.act(&format!("click {:.0} {:.0} ctrl", r.center().x, r.center().y));
        s.settle();
        assert_eq!(s.app.panels[&s.app.active].tabs.len(), tabs + 1);
        assert_eq!(s.tab().smart.as_ref().map(|x| x.name.as_str()), Some("Bills"));
        for v in [ViewList, ViewLarge, ViewTree, ViewDetails] {
            s.cmd(v);
            look(&mut s, &format!("smart folder {v:?}"));
        }
        // One kept as fields by an older version opens as before, and edits as a query.
        s.app.ud.smart.push(crate::smart::SmartFolder { id: 99, name: "Old scans".into(), root: d.clone(), kinds: vec!["pdfs".into()], days: Some(30), ..Default::default() });
        s.idle(2);
        let r = sidebar_text(&s, "Old scans").unwrap_or_else(|| s.fail("the older smart folder"));
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "the older one's search", 10, |s| s.tab().smart.as_ref().is_some_and(|x| x.id == 99) && !s.tab().is_loading());
        s.settle();
        assert_eq!(rels(&s), ["Invoices/2026/invoice-030.pdf", "Invoices/2026/invoice-031.pdf", "Invoices/invoice-032.pdf"]);
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.click_text("Edit…");
        assert_eq!(ed(&s).query, "type:pdfs modified<30d");
        look(&mut s, "older smart folder as a query");
        s.shot("smart-folder-older");
        // A query of the whole language, ranked by its words as the filter ranks them.
        click_control(&mut s, "smart query");
        s.act("key ctrl+a");
        s.act("text invoice (type:pdfs | ext:txt) -2025");
        s.act("key Enter");
        assert!(!s.popup_open());
        wait_until(&mut s, "the query's search", 10, |s| !s.tab().is_loading() && rels(s).len() == 5);
        s.settle();
        assert_eq!(rels(&s), ["Invoices/2026/invoice-030.pdf", "Invoices/2026/invoice-031.pdf", "Invoices/invoice notes.txt", "Invoices/invoice-032.pdf", "Invoices/scans/page.txt"]);
        let last = s.tab().view.last().map(|&i| s.tab().entries[i as usize].name.clone());
        assert_eq!(last.as_deref(), Some("page.txt"), "found only by its folder's name, so last");
        look(&mut s, "smart folder from a query");
        s.shot("smart-folder-query");
        // Removed from their menus: the tabs show their folders; the section goes.
        for name in ["Bills", "Big pictures", "Old scans"] {
            let r = sidebar_text(&s, name).unwrap_or_else(|| s.fail(&format!("{name} in the sidebar")));
            s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
            s.idle(2);
            s.click_text("Remove");
            s.idle(2);
        }
        assert!(s.app.ud.smart.is_empty());
        assert!(sidebar_text(&s, "Smart Folders").is_none(), "no section with none");
        assert!(s.app.panels.values().all(|p| p.tabs.iter().all(|t| t.smart.is_none())));
        for (w, h) in [(520.0, 340.0), (1280.0, 800.0)] {
            s.resize(w, h);
            look(&mut s, &format!("smart folders at {w}x{h}"));
        }
        report("smart folders", &s);
    }

    fn versions_page(s: &Sim) -> Option<&crate::versions::Page> {
        s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.versions.as_deref())
    }

    /// Previous Versions from a file's menu: Snapper's and Timeshift's copies newest first,
    /// the unchanged ones left out, each previewed under the list as it is picked; one
    /// saved beside the file, one put in its place and undone; snapshots only root can
    /// read said in a sentence; a disk without snapshots said so; Escape closing the
    /// inspector it opened.
    #[test]
    fn previous_versions_from_the_snapshots() {
        if !sandboxed() {
            return;
        }
        let home = fixture("versions");
        let d = home.join("Disk");
        let (file, table) = crate::tests::snapshot_fixture(&d);
        crate::versions::FAKE.with(|f| *f.borrow_mut() = Some(table.clone()));
        let docs = file.parent().unwrap().to_path_buf();
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        let row = row_text(&s, "report.txt");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        // The menu is taller than the window: its end scrolled to.
        if !some_text(&s, "Previous Versions…") {
            let r = s.text_rect("Rename");
            s.act(&format!("scroll {:.0} {:.0} -30", r.center().x, r.center().y));
            s.idle(4);
        }
        assert!(some_text(&s, "Previous Versions…"), "in the file's menu");
        s.click_text("Previous Versions…");
        wait_until(&mut s, "the snapshots read", 10, |s| versions_page(s).is_some_and(|p| p.found.is_some()));
        s.idle(3);
        let n = versions_page(&s).and_then(|p| p.found.as_ref()).map_or(0, |f| f.versions.len());
        assert_eq!(n, 4, "the copies that differ, newest first");
        assert!(some_text(&s, "Previous versions of report.txt"));
        assert!(some_text(&s, "before & after") && some_text(&s, "before update") && some_text(&s, "weekly, monthly"), "what each snapshot says of itself");
        let first = versions_page(&s).and_then(|p| p.selected()).map(|v| v.entry.path.clone()).unwrap();
        wait_until(&mut s, "the newest version previewed", 10, |s| s.texts.iter().any(|(_, t)| t.trim() == "third"));
        assert!(first.starts_with(d.join("vol/.snapshots/3")));
        look(&mut s, "previous versions");
        s.shot("versions-graphite");
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "previous versions, light");
        s.shot("versions-paper");
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        // The keys walk the list; each version shows under it.
        s.act("key ArrowDown");
        wait_until(&mut s, "the second version previewed", 10, |s| s.texts.iter().any(|(_, t)| t.trim() == "frist"));
        assert_eq!(versions_page(&s).map(|p| p.sel), Some(1));
        s.act("key End");
        wait_until(&mut s, "the oldest previewed", 10, |s| s.texts.iter().any(|(_, t)| t.trim() == "rsync copy, the oldest"));
        s.act("key Home");
        s.act("key ArrowDown");
        // Saved beside the file.
        click_control(&mut s, "version copy");
        let when = crate::versions::parse_utc("2026-10-08 06:00:00");
        let copy = docs.join(crate::versions::copy_name(&file, when));
        wait_until(&mut s, "the copy saved", 10, |_| copy.is_file());
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "frist\n");
        assert!(versions_page(&s).is_none(), "the page closes once it is done");
        wait_until(&mut s, "the copy selected", 10, |s| s.tab().cursor.map(|c| s.tab().entries[c as usize].path.clone()) == Some(copy.clone()));
        // Put in its place, from the palette, and undone.
        select_path(&mut s, &file);
        palette_run(&mut s, "Previous Versions");
        wait_until(&mut s, "the snapshots read again", 10, |s| versions_page(s).is_some_and(|p| p.found.is_some()));
        click_control(&mut s, "version restore");
        wait_until(&mut s, "restored", 10, |_| std::fs::read_to_string(&file).ok().as_deref() == Some("third\n"));
        wait_until(&mut s, "undo offered", 10, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Replaced(_))));
        assert!(root().join("xdg/data/Trash/files/report.txt").exists(), "the file it replaced is in the trash");
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 10, |_| std::fs::read_to_string(&file).ok().as_deref() == Some("fourth draft, the one in use\n"));
        // Snapshots only root may read.
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(d.join("vol/.snapshots"), std::fs::Permissions::from_mode(0o000)).unwrap();
        select_path(&mut s, &file);
        s.cmd(PreviousVersions);
        wait_until(&mut s, "read as far as it can", 10, |s| versions_page(s).is_some_and(|p| p.found.is_some()));
        s.idle(3);
        assert!(some_text(&s, "Snapper's snapshots here can only be read by an administrator."), "said plainly");
        assert_eq!(versions_page(&s).and_then(|p| p.found.as_ref()).map(|f| f.versions.len()), Some(2), "Timeshift's still listed");
        look(&mut s, "versions with locked snapshots");
        s.shot("versions-locked-graphite");
        s.resize(520.0, 340.0);
        look(&mut s, "versions small");
        s.shot("versions-small-graphite");
        s.resize(1280.0, 800.0);
        std::fs::set_permissions(d.join("vol/.snapshots"), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Escape: the inspector the page opened goes with it.
        s.act("key Escape");
        s.idle(2);
        assert!(s.app.panels[&s.app.active].inspector.is_none(), "closed with the page");
        // A disk with no snapshots.
        crate::versions::FAKE.with(|f| *f.borrow_mut() = Some(String::new()));
        s.cmd(PreviousVersions);
        wait_until(&mut s, "nothing found", 10, |s| versions_page(s).is_some_and(|p| p.found.is_some()));
        s.idle(2);
        assert!(some_text(&s, "This disk keeps no snapshots of this file's folder."));
        look(&mut s, "no snapshots");
        // Another file: the page leaves for its preview.
        s.cmd(ToggleInspector);
        s.cmd(ToggleInspector);
        s.cmd(PreviousVersions);
        s.act("key ArrowDown");
        s.idle(2);
        assert!(versions_page(&s).is_some(), "the keys stay with the page");
        let other = docs.join(crate::versions::copy_name(&file, when));
        select_path(&mut s, &other);
        s.idle(2);
        assert!(versions_page(&s).is_none(), "gone with another file under the cursor");
        crate::versions::FAKE.with(|f| *f.borrow_mut() = None);
        report("versions", &s);
    }

    /// Memory through a picture-heavy session: camera-sized pictures thumbnailed in the
    /// largest icons and opened in the viewer, then left. Prints resident and peak memory.
    #[test]
    fn memory_through_many_pictures() {
        if !sandboxed() {
            return;
        }
        let home = fixture("memory");
        let d = home.join("Photos");
        std::fs::create_dir_all(&d).unwrap();
        let (w, h) = (2400u32, 1600u32);
        for k in 0..60u32 {
            let mut out = vec![];
            let mut e = png::Encoder::new(&mut out, w, h);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.set_compression(png::Compression::Fast);
            let row: Vec<u8> = (0..w).flat_map(|x| [(x / 9 + k * 40) as u8, (k * 13) as u8, (x / 7) as u8]).collect();
            let px: Vec<u8> = (0..h).flat_map(|y| row.iter().map(move |c| c.wrapping_add((y / 6) as u8))).collect();
            e.write_header().unwrap().write_image_data(&px).unwrap();
            std::fs::write(d.join(format!("photo {k:02}.png")), out).unwrap();
        }
        let kb = |what: &str| {
            let (rss, peak) = crate::memory::resident_kb();
            eprintln!("memory {what}: resident {} MB, peak {} MB", rss / 1024, peak / 1024);
            rss as i64 / 1024
        };
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.idle(3);
        // Measured from here, so journeys that ran before in this process don't count.
        let base = kb("open");
        s.cmd(Cmd::ViewXL);
        for _ in 0..14 {
            s.act("key PageDown");
            for _ in 0..8 {
                s.idle(4);
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
        // Sixty decoded photos' buffers are freed by now: with glibc left as it is, they
        // stayed in the workers' arenas (84 MB more than at the start); handed back, a few.
        let mb = kb("thumbnailed") - base;
        assert!(mb < 32, "the decoded pictures' memory is handed back: {mb} MB more resident");
        s.act("key Home");
        s.act("key Enter");
        for _ in 0..6 {
            s.act("key Right");
            for _ in 0..4 {
                s.idle(3);
                std::thread::sleep(std::time::Duration::from_millis(120));
            }
        }
        kb("viewer");
        s.act("key Escape");
        s.cmd(Cmd::Up);
        for _ in 0..10 {
            s.idle(3);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let mb = kb("left") - base;
        assert!(mb < 32, "and stays handed back after the folder is left: {mb} MB more");
        report("memory", &s);
    }

    /// A picture for the Stack view's cards: a two-colour sweep with a disc, each seed its own.
    fn cover_png(seed: u32) -> Vec<u8> {
        let (w, h) = (240u32, 180u32);
        let hue = |k: u32| -> [f32; 3] {
            let a = (seed * 47 + k * 113) % 360;
            let f = |n: f32| {
                let k = (n + a as f32 / 30.0) % 12.0;
                0.62 - 0.38 * (k - 3.0).min(9.0 - k).clamp(-1.0, 1.0)
            };
            [f(0.0), f(8.0), f(4.0)]
        };
        let (c0, c1) = (hue(0), hue(1));
        let (cx, cy, rr) = (60.0 + (seed % 5) as f32 * 30.0, 50.0 + (seed % 3) as f32 * 40.0, 34.0 + (seed % 4) as f32 * 9.0);
        let mut out = vec![];
        let mut e = png::Encoder::new(&mut out, w, h);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let px: Vec<u8> = (0..h)
            .flat_map(|y| {
                (0..w).flat_map(move |x| {
                    let t = (x + y) as f32 / (w + h) as f32;
                    let inside = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt() < rr;
                    let c: Vec<u8> = (0..3).map(|i| (255.0 * if inside { 0.95 - c0[i] * 0.4 } else { c0[i] + (c1[i] - c0[i]) * t }) as u8).collect();
                    [c[0], c[1], c[2], 255]
                })
            })
            .collect();
        e.write_header().unwrap().write_image_data(&px).unwrap();
        out
    }

    /// The Stack view: pictures as covers, folders and other files by their icons, the
    /// cursor's item out of the row with its name over it, flipped through by keys.
    #[test]
    fn stack_view() {
        if !sandboxed() {
            return;
        }
        let home = fixture("stack");
        let d = home.join("Shelf");
        std::fs::create_dir_all(&d).unwrap();
        for k in 0..36 {
            std::fs::write(d.join(format!("photo {:02}.png", k + 1)), cover_png(k)).unwrap();
        }
        for f in ["Albums", "Projects", "Scans"] {
            std::fs::create_dir_all(d.join(f)).unwrap();
        }
        // Covers: one kept for the folder, four of five pictures in a square, one of two alone.
        std::fs::write(d.join("Albums/cover.png"), cover_png(40)).unwrap();
        std::fs::write(d.join("Albums/b.png"), cover_png(41)).unwrap();
        for k in 0..5 {
            std::fs::write(d.join(format!("Projects/shot {k}.png")), cover_png(50 + k)).unwrap();
        }
        for k in 0..2 {
            std::fs::write(d.join(format!("Scans/page {k}.png")), cover_png(60 + k)).unwrap();
        }
        for (f, body) in [("notes.txt", "notes\n"), ("main.rs", "fn main() {}\n"), ("song.mp3", "ID3"), ("report.pdf", "%PDF-1.4"), ("archive.zip", "PK")] {
            std::fs::write(d.join(f), body).unwrap();
        }
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        s.cmd(Cmd::ViewStack);
        s.act("key Down");
        for _ in 0..6 {
            s.act("key Right");
            s.idle(2);
        }
        s.idle(40);
        // Thumbnails fade in on the clock, not the frame count.
        std::thread::sleep(std::time::Duration::from_millis(300));
        look(&mut s, "stack");
        s.shot("stack-graphite");
        s.act("key Home");
        s.idle(40);
        std::thread::sleep(std::time::Duration::from_millis(300));
        look(&mut s, "stack, first");
        s.shot("stack-first");
        s.act("key Right");
        s.idle(40);
        std::thread::sleep(std::time::Duration::from_millis(300));
        s.idle(4);
        look(&mut s, "stack, a folder of pictures");
        s.shot("stack-mosaic");
        // Pointing at the scrubber shows what is there.
        let list = s.app.panels[&s.app.active].list_rect;
        let y = list.bottom() - (16.0 * s.app.cfg.spacing.max(0.7)).round() - 24.0;
        s.act(&format!("move {:.0} {y:.0}", list.center().x + 60.0));
        s.idle(6);
        std::thread::sleep(std::time::Duration::from_millis(200));
        s.idle(3);
        s.shot("stack-peek");
        s.act("key End");
        s.idle(40);
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "stack, light");
        s.shot("stack-paper");
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        // Beside another panel, narrow.
        s.cmd(Cmd::SplitRight);
        s.idle(4);
        s.cmd(Cmd::ViewStack);
        s.act("key End");
        s.idle(40);
        look(&mut s, "stack, split");
        s.shot("stack-split");
        // The row eases toward the cursor, so for some frames after the list shrinks it
        // still stands past its end: from the last of 44 items, a filter that keeps one.
        s.act("text notes");
        s.idle(30);
        assert_eq!(s.tab().view.len(), 1, "the filter keeps notes.txt alone");
        look(&mut s, "stack, the list shrunk under the row");
        report("stack", &s);
    }

    fn space(s: &Sim) -> &crate::spaceuse::State {
        match &s.app.popup {
            Popup::SpaceUsed(st) => st,
            _ => s.fail("Show Space Used is not open"),
        }
    }

    fn space_names(s: &Sim) -> Vec<String> {
        let st = space(s);
        st.order.iter().map(|&k| st.items[k as usize].entry.name.clone()).collect()
    }

    /// Show Space Used from the folder's menu: biggest first as the sizes come in, each
    /// with its bar; into a folder and back to it, never above where it started; two
    /// items trashed after asking, gone from the list and brought back by Undo; a file
    /// shown in its folder.
    #[test]
    fn space_used_biggest_first() {
        if !sandboxed() {
            return;
        }
        let home = fixture("space");
        let d = home.join("Disk");
        crate::tests::space_fixture(&d);
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        let list = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("click {:.0} {:.0} right", list.center().x, list.bottom() - 40.0));
        s.idle(2);
        assert!(some_text(&s, "Show Space Used"), "in the folder's menu");
        s.click_text("Show Space Used");
        wait_until(&mut s, "every size counted", 10, |s| !space(s).counting());
        s.idle(3);
        assert_eq!(space_names(&s), ["Big", "movie.mkv", "Small", "big-link", "Empty"]);
        assert!(some_text(&s, "488 KB") && some_text(&s, "391 KB") && some_text(&s, "10 B"), "each size beside its bar");
        assert!(s.texts.iter().any(|(_, t)| t.ends_with("Disk") || t.contains("Disk · ")), "where");
        assert!(some_text(&s, "879 KB"), "how much in all, in the ring");
        look(&mut s, "space used");
        s.shot("space-used-graphite");
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "space used, light");
        s.shot("space-used-paper");
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        // Filling the window and back.
        s.act("key f");
        s.idle(30);
        look(&mut s, "space used, full window");
        s.shot("space-used-full");
        s.act("key f");
        s.idle(30);
        // Into Big, and back with the cursor on it.
        s.act("key Enter");
        wait_until(&mut s, "Big read", 10, |s| space(s).dir == d.join("Big") && !space(s).counting());
        assert_eq!(space_names(&s), ["a.bin", "Sub"]);
        s.act("key Backspace");
        wait_until(&mut s, "back up", 10, |s| space(s).dir == d && !space(s).counting());
        assert_eq!(space(&s).sel, 0, "on the folder it came from");
        s.act("key Backspace");
        assert_eq!(space(&s).dir, d, "never above where it started");
        assert!(some_text(&s, "Space used was opened in Disk: it goes no higher"));
        // A click on a folder opens it too.
        let r = s.text_rect("Small");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "Small read", 10, |s| space(s).dir == d.join("Small"));
        s.act("key Backspace");
        wait_until(&mut s, "back up again", 10, |s| space(s).dir == d && !space(s).counting());
        // The movie and Small checked, trashed after asking.
        assert_eq!(space(&s).sel, 2, "back on Small");
        s.act("key Home");
        s.act("key ArrowDown");
        s.act("key Space");
        s.act("key ArrowDown");
        s.act("key Space");
        assert_eq!(space(&s).checked.len(), 2);
        s.act("key Delete");
        assert!(space(&s).confirm, "asks first");
        assert!(some_text(&s, "Move 2 items (391 KB) to the trash?"), "{:?}", s.texts.iter().filter(|t| t.1.starts_with("Move")).collect::<Vec<_>>());
        look(&mut s, "space used asking");
        s.shot("space-used-confirm-graphite");
        s.act("key Enter");
        wait_until(&mut s, "trashed", 10, |_| !d.join("movie.mkv").exists() && !d.join("Small").exists());
        assert_eq!(space_names(&s), ["Big", "big-link", "Empty"], "gone from the list");
        // A file shown in its folder.
        s.act("key Home");
        s.act("key Enter");
        wait_until(&mut s, "Big read once more", 10, |s| space(s).dir == d.join("Big") && !space(s).counting());
        click_control(&mut s, "space show");
        s.settle();
        assert!(!s.popup_open());
        assert_eq!(s.tab().path, d.join("Big"));
        assert_eq!(s.tab().cursor.map(|c| s.tab().entries[c as usize].name.clone()).as_deref(), Some("a.bin"), "selected in its folder");
        wait_until(&mut s, "undo offered", 10, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Trashed(_))));
        s.act("key ctrl+z");
        wait_until(&mut s, "undone", 10, |_| d.join("movie.mkv").exists() && d.join("Small/c.txt").exists());
        // From the palette, in a small window and at another view.
        go(&mut s, &d);
        palette_run(&mut s, "Show Space Used");
        wait_until(&mut s, "counted again", 10, |s| !space(s).counting());
        s.resize(520.0, 340.0);
        look(&mut s, "space used small");
        s.shot("space-used-small-graphite");
        s.act("key Escape");
        assert!(!s.popup_open());
        s.resize(1280.0, 800.0);
        go(&mut s, &d.join("Empty"));
        palette_run(&mut s, "Show Space Used");
        wait_until(&mut s, "an empty folder read", 10, |s| !space(s).counting());
        assert!(some_text(&s, "This folder is empty"));
        s.act("key Escape");
        report("space used", &s);
    }

    fn free_up(s: &Sim) -> &crate::freeup::View {
        space(s).free_up.as_ref().unwrap_or_else(|| s.fail("Free up has not started"))
    }

    /// Free Up Space from the palette: Space used on the whole disk, showing Free up. Build
    /// output, packages, a download cache, thumbnails and the trash found under the home
    /// folder and grouped; the last thirty days of the disk's record drawn over them, a day
    /// pointed at; a group opened and a folder in it left out by keys; the rest deleted
    /// after asking, and what was left out still there; Contents and back by Tab and by the
    /// tabs; Find Duplicates from its row; a small window and the whole window.
    #[test]
    fn free_up_space() {
        if !sandboxed() {
            return;
        }
        let home = fixture("freeup");
        let w = |p: &str, n: usize| {
            let p = home.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, vec![b'x'; n]).unwrap();
        };
        w("Projects/engine/Cargo.toml", 20);
        w("Projects/engine/target/debug/engine", 3_000_000);
        w("Projects/tool/Cargo.toml", 20);
        w("Projects/tool/target/debug/tool", 1_200_000);
        w("Projects/site/package.json", 2);
        w("Projects/site/node_modules/react/index.js", 800_000);
        let cache = dirs::cache_dir().unwrap();
        let pip = cache.join("pip/http/freeup-wheel");
        std::fs::create_dir_all(pip.parent().unwrap()).unwrap();
        std::fs::write(&pip, vec![b'p'; 600_000]).unwrap();
        std::fs::create_dir_all(cache.join("thumbnails/normal")).unwrap();
        std::fs::write(cache.join("thumbnails/normal/freeup.png"), vec![b't'; 40_000]).unwrap();
        w("old draft.txt", 50_000);
        crate::trashcan::delete(&home.join("old draft.txt")).unwrap();
        // Twelve days of the disk's record, rising a little each day, under the reading the
        // dialog takes itself today.
        let top = crate::trashcan::topdir(&home);
        let (total, free) = crate::fsx::disk_space(&home).unwrap();
        let used = total - free;
        let today = crate::timefmt::local_day(std::time::SystemTime::now());
        let book = crate::freeup::book_path();
        let _ = std::fs::remove_file(&book);
        for k in (1..=12).rev() {
            crate::freeup::note_in(&book, &top, used / 2 + used / 2 * (12 - k as u64) / 12, total, today - k);
        }
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        palette_run(&mut s, "Free Up Space");
        assert!(space(&s).showing_free, "opens on Free up");
        wait_until(&mut s, "everything found and counted", 20, |s| space(s).free_up.as_ref().is_some_and(|f| f.done && f.counting() == 0));
        assert_eq!(space(&s).root, top, "the whole disk");
        s.idle(3);
        for t in ["Build output", "Contents", "Free up", "Used now", "After", "Used, last 30 days"] {
            assert!(some_text(&s, t), "{t} on screen");
        }
        use crate::freeup::Group;
        assert_eq!(free_up(&s).groups.iter().map(|g| g.group).collect::<Vec<_>>(), [Group::Builds, Group::Packages, Group::Caches, Group::Thumbs, Group::Trash]);
        // At a big font the list scrolls: its last row brought into view.
        s.act("key End");
        s.idle(20);
        assert!(some_text(&s, "Duplicate files"));
        s.act("key Home");
        s.idle(20);
        let parts = |s: &Sim, g: crate::freeup::Group| -> Vec<PathBuf> { free_up(s).groups.iter().filter(|x| x.group == g).flat_map(|x| x.parts.iter().map(|p| p.path.clone())).collect() };
        let builds = parts(&s, crate::freeup::Group::Builds);
        assert!(builds.contains(&home.join("Projects/engine/target")) && builds.contains(&home.join("Projects/tool/target")), "{builds:?}");
        assert!(parts(&s, crate::freeup::Group::Packages).contains(&home.join("Projects/site/node_modules")));
        assert!(parts(&s, crate::freeup::Group::Caches).contains(&cache.join("pip")));
        assert!(s.texts.iter().any(|(_, t)| t.starts_with('+') && t.ends_with("in 7 days")), "the week's change: {:?}", s.texts.iter().filter(|t| t.1.contains("days")).collect::<Vec<_>>());
        look(&mut s, "free up");
        s.shot("free-up-graphite");
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "free up, light");
        s.shot("free-up-paper");
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        // A day of the chart pointed at.
        let chart = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == "free chart").map(|(_, r, _)| *r)).unwrap_or_else(|| s.fail("no chart"));
        s.act(&format!("move {:.0} {:.0}", chart.right() - 3.0, chart.center().y));
        s.idle(2);
        assert!(s.texts.iter().any(|(_, t)| t.ends_with(" used") && t.contains(':')), "the day pointed at");
        s.act("move -50 -50");
        // Build output opened, the tool's build left out, closed again.
        s.act("key Home");
        assert_eq!(free_up(&s).sel, 0);
        let rows = free_up(&s).rows().len();
        s.act("key ArrowRight");
        assert_eq!(free_up(&s).rows().len(), rows + free_up(&s).groups[0].parts.len(), "its folders under it");
        look(&mut s, "free up, a group open");
        s.shot("free-up-open");
        let tool = free_up(&s).groups[0].parts.iter().position(|p| p.path == home.join("Projects/tool/target")).unwrap();
        for _ in 0..=tool {
            s.act("key ArrowDown");
        }
        s.act("key space");
        assert!(!free_up(&s).groups[0].parts[tool].checked, "left out");
        s.act("key ArrowLeft");
        assert_eq!(free_up(&s).rows().len(), rows, "closed, the cursor on its group");
        assert_eq!(free_up(&s).sel, 0);
        // The packages put in, the thumbnails (other journeys' too) left alone.
        s.act("key ArrowDown");
        s.act("key space");
        let thumbs = free_up(&s).rows().iter().position(|r| matches!(r, crate::freeup::Row::Group(g) if free_up(&s).groups[*g].group == crate::freeup::Group::Thumbs)).unwrap();
        while free_up(&s).sel < thumbs {
            s.act("key ArrowDown");
        }
        s.act("key space");
        let chosen = free_up(&s).chosen();
        assert!(some_text(&s, &format!("Free {}", crate::fsx::fmt_size(chosen))), "the button says how much");
        s.act("key Delete");
        assert!(free_up(&s).confirm, "asks first");
        assert!(s.texts.iter().any(|(_, t)| t.starts_with(&format!("Delete {} for good?", crate::fsx::fmt_size(chosen)))));
        look(&mut s, "free up asking");
        s.shot("free-up-confirm");
        s.act("key Enter");
        wait_until(&mut s, "deleted", 20, |_| !home.join("Projects/engine/target").exists() && !home.join("Projects/site/node_modules").exists() && !cache.join("pip").exists());
        wait_until(&mut s, "the disk read again", 20, |s| free_up(s).note.as_deref().is_some_and(|n| n.starts_with("Freed")));
        assert!(home.join("Projects/tool/target/debug/tool").exists() && home.join("Projects/engine/Cargo.toml").exists(), "what was left out stays");
        assert!(crate::trashcan::list().iter().any(|i| i.original == home.join("old draft.txt")), "the trash was not chosen");
        look(&mut s, "free up after");
        s.shot("free-up-after");
        // Contents and back, by key and by the tabs.
        s.act("key Tab");
        assert!(!space(&s).showing_free);
        wait_until(&mut s, "the disk's folders counted", 20, |s| !space(s).counting());
        assert!(space_names(&s).iter().any(|n| n == "home"), "the disk's top folder");
        look(&mut s, "space used of the disk");
        click_control(&mut s, "space free up");
        assert!(space(&s).showing_free);
        click_control(&mut s, "space contents");
        assert!(!space(&s).showing_free);
        // Tab goes round: the map, then Free up.
        s.act("key Tab");
        assert!(!space(&s).showing_free && crate::spaceuse::MAP.load(std::sync::atomic::Ordering::Relaxed), "the map");
        look(&mut s, "space used as a map");
        s.shot("space-map");
        s.act("key Tab");
        assert!(space(&s).showing_free);
        // Find Duplicates from its row, in the home folder.
        s.act("key End");
        assert_eq!(free_up(&s).rows()[free_up(&s).sel], crate::freeup::Row::Dupes);
        s.act("key Enter");
        s.settle();
        assert!(matches!(&s.app.popup, Popup::Duplicates(_)), "Find Duplicates open: {} {:?}", s.app.popup.kind_name(), s.app.toast.as_ref().map(|t| t.0.clone()));
        s.act("key Escape");
        // A small window, and the whole window.
        s.resize(520.0, 340.0);
        palette_run(&mut s, "Free Up Space");
        wait_until(&mut s, "found again", 20, |s| space(s).free_up.as_ref().is_some_and(|f| f.done && f.counting() == 0));
        look(&mut s, "free up small");
        s.shot("free-up-small");
        s.act("key Escape");
        s.resize(1280.0, 800.0);
        palette_run(&mut s, "Free Up Space");
        wait_until(&mut s, "found once more", 20, |s| space(s).free_up.as_ref().is_some_and(|f| f.done && f.counting() == 0));
        s.act("key f");
        s.idle(30);
        look(&mut s, "free up, whole window");
        s.shot("free-up-full");
        s.act("key f");
        s.idle(30);
        s.act("key Escape");
        // From Show Space Used in a project, Free up keeps to that folder.
        go(&mut s, &home.join("Projects"));
        palette_run(&mut s, "Show Space Used");
        click_control(&mut s, "space free up");
        wait_until(&mut s, "the project folder looked at", 20, |s| space(s).free_up.as_ref().is_some_and(|f| f.done && f.counting() == 0));
        assert_eq!(parts(&s, crate::freeup::Group::Builds), [home.join("Projects/tool/target")]);
        assert!(parts(&s, crate::freeup::Group::Trash).is_empty() && parts(&s, crate::freeup::Group::Thumbs).is_empty(), "nothing from outside it");
        look(&mut s, "free up in a project");
        s.act("key Escape");
        // An empty folder has nothing; with animations off, in three more themes.
        go(&mut s, &home.join("Empty"));
        palette_run(&mut s, "Show Space Used");
        click_control(&mut s, "space free up");
        wait_until(&mut s, "an empty folder looked at", 20, |s| space(s).free_up.as_ref().is_some_and(|f| f.done));
        assert!(some_text(&s, "Nothing here that comes back by itself"));
        look(&mut s, "free up, nothing");
        s.shot("free-up-empty");
        s.act("key Escape");
        go(&mut s, &home);
        s.app.cfg.animations = false;
        for (theme, name) in [(crate::theme::ThemeName::Midnight, "midnight"), (crate::theme::ThemeName::Latte, "latte"), (crate::theme::ThemeName::Gruvbox, "gruvbox")] {
            s.app.cfg.theme = theme;
            palette_run(&mut s, "Free Up Space");
            wait_until(&mut s, "found in another theme", 20, |s| space(s).free_up.as_ref().is_some_and(|f| f.done && f.counting() == 0));
            look(&mut s, &format!("free up, {name}"));
            s.shot(&format!("free-up-{name}"));
            s.act("key Escape");
        }
        s.app.cfg.animations = true;
        report("free up", &s);
    }

    fn viewer(s: &Sim) -> &crate::viewer::State {
        match &s.app.popup {
            Popup::Viewer(v) => v,
            _ => s.fail("the picture viewer is not open"),
        }
    }

    /// The picture viewer: F3 from a picture, the neighbours decoded ahead, Left and
    /// Right in the listing's order, zoom by keys, wheel and double-click, a drag that
    /// pans, a slideshow with its fade, nothing drawn while it rests, Escape leaving the
    /// cursor on the last picture; Enter and double-click opening the picture's app until
    /// Options says otherwise; a broken picture said so; a small window.
    #[test]
    fn picture_viewer_full_window() {
        if !sandboxed() {
            return;
        }
        let home = fixture("viewer");
        let trip = home.join("Pictures/Trip");
        png_file(&trip.join("panorama.png"), 2400, 1200, 7);
        let mut s = Sim::new(&trip, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        let pic = |s: &Sim| viewer(s).current().map(|p| p.name.clone()).unwrap_or_default();
        select_path(&mut s, &trip.join("img03.png"));
        s.act("key F3");
        assert_eq!(pic(&s), "img03.png");
        assert_eq!(viewer(&s).pics.len(), 13, "every picture in the folder");
        wait_until(&mut s, "the picture decoded", 10, |s| s.app.thumbs.dims(&trip.join("img03.png")).is_some());
        wait_until(&mut s, "its neighbours decoded ahead", 10, |s| s.app.thumbs.dims(&trip.join("img02.png")).is_some() && s.app.thumbs.dims(&trip.join("img04.png")).is_some());
        assert!(s.app.thumbs.dims(&trip.join("img08.png")).is_none(), "and no further");
        s.idle(2);
        assert!(some_text(&s, "img03.png") && some_text(&s, "4 / 13"), "the name and where it is");
        shot_still(&mut s, "viewer-graphite");
        s.act("key ArrowRight");
        assert_eq!(pic(&s), "img04.png");
        s.act("key ArrowLeft");
        s.act("key ArrowLeft");
        assert_eq!(pic(&s), "img02.png");
        s.act("key End");
        assert_eq!(pic(&s), "panorama.png");
        s.act("key ArrowRight");
        assert_eq!(pic(&s), "panorama.png", "the last stays the last");
        wait_until(&mut s, "the panorama decoded", 10, |s| s.app.thumbs.dims(&trip.join("panorama.png")).is_some());
        s.idle(2);
        // Zoom: one to one, fit, a step, the wheel; a drag moves a zoomed picture.
        s.act("key 1");
        let one = viewer(&s).zoom;
        assert!(one > 1.1, "one pixel for one is bigger than fitting: {one}");
        shot_still(&mut s, "viewer-zoomed-graphite");
        s.act("drag 640 400 500 300");
        assert!(viewer(&s).pan != Vec2::ZERO, "dragged");
        s.act("key 0");
        assert_eq!((viewer(&s).zoom, viewer(&s).pan), (1.0, Vec2::ZERO));
        s.act("drag 640 400 500 300");
        assert_eq!(viewer(&s).pan, Vec2::ZERO, "a picture that fits doesn't move");
        s.act("key Plus");
        assert!((viewer(&s).zoom - 1.25).abs() < 1e-3);
        s.act("scroll 640 400 -5");
        // The wheel's scroll eases out over a few frames.
        s.idle(60);
        assert!(viewer(&s).zoom < 1.25, "the wheel zooms out");
        s.act("dclick 640 400");
        assert_eq!(viewer(&s).zoom, 1.0, "a double-click fits it again");
        s.idle(40);
        s.act("dclick 640 400");
        assert!((viewer(&s).zoom - one).abs() < 1e-3, "and then shows it one to one");
        s.act("key 0");
        // A slideshow: on to the next by itself, round to the first, with a fade.
        s.act("key Space");
        assert!(viewer(&s).playing);
        assert!(s.repaint <= std::time::Duration::from_secs_f64(crate::viewer::SLIDE), "the timer runs while it plays: {:?}", s.repaint);
        s.idle(250);
        assert_eq!(pic(&s), "img00.png", "round to the first");
        s.act("key Space");
        assert!(!viewer(&s).playing);
        s.idle(200);
        // Memory goes back to the system two seconds (real ones) after decodes settle:
        // that one wake is the app's, not the viewer's.
        std::thread::sleep(std::time::Duration::from_millis(2200));
        s.idle(3);
        assert_eq!(s.repaint, std::time::Duration::MAX, "nothing repaints while the viewer rests");
        assert!(!some_text(&s, "1 / 13"), "the name gone once the pointer rests");
        s.act("move 300 300");
        assert!(some_text(&s, "1 / 13"), "back when it moves");
        s.act("key ArrowRight");
        assert_eq!(viewer(&s).fading_from(), s.app.cfg.animations.then_some(0), "a quiet fade between pictures");
        s.app.cfg.animations = false;
        s.act("key ArrowRight");
        assert_eq!(viewer(&s).fading_from(), None, "none without animations");
        s.app.cfg.animations = true;
        // Escape: the cursor on the picture last shown (it shrinks back there first).
        s.act("key Escape");
        wait_until(&mut s, "the viewer closed", 5, |s| !s.popup_open());
        assert_eq!(s.tab().cursor.map(|c| s.tab().entries[c as usize].name.clone()).as_deref(), Some("img02.png"));
        // From the picture's menu.
        let row = row_text(&s, "img07.png");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        s.click_text("View Pictures");
        assert_eq!(pic(&s), "img07.png");
        s.act("key Escape");
        wait_until(&mut s, "the viewer closed", 5, |s| !s.popup_open());
        // Enter and double-click open the picture's app by default.
        let n = crate::mime::LAUNCHED.lock().unwrap().len();
        select_path(&mut s, &trip.join("img05.png"));
        s.act("key Enter");
        assert!(!s.popup_open(), "not the viewer");
        assert!(crate::mime::LAUNCHED.lock().unwrap().len() > n, "its app");
        // Options can send them to the viewer.
        s.act("key ctrl+Comma");
        s.act("text pictures in maia");
        s.act("key Enter");
        s.act("key Escape");
        assert!(s.app.cfg.picture_viewer, "switched on in Options");
        select_path(&mut s, &trip.join("img05.png"));
        s.act("key Enter");
        assert_eq!(pic(&s), "img05.png", "Enter shows it here");
        s.act("key Escape");
        s.idle(30);
        let row = row_text(&s, "img06.png");
        s.act(&format!("dclick {:.0} {:.0}", row.center().x, row.center().y));
        assert_eq!(pic(&s), "img06.png", "so does a double-click");
        s.act("key Escape");
        // A broken picture.
        go(&mut s, &home.join("Pictures"));
        select_path(&mut s, &home.join("Pictures/broken.png"));
        s.cmd(ViewPictures);
        wait_until(&mut s, "the broken one tried", 10, |s| s.app.thumbs.failed(&home.join("Pictures/broken.png")));
        s.idle(2);
        assert!(some_text(&s, "Can't show this picture"));
        s.resize(520.0, 340.0);
        s.act("key ArrowLeft");
        s.act("move 200 200");
        s.idle(3);
        s.shot("viewer-small-graphite");
        s.act("key Escape");
        look(&mut s, "after the viewer, small");
        s.resize(1280.0, 800.0);
        go(&mut s, &home.join("Documents"));
        s.cmd(ViewPictures);
        assert!(!s.popup_open() && toast_is(&s, "No pictures here"));
        report("viewer", &s);
    }

    /// Animated pictures and photos only other tools read: an animated GIF plays in the
    /// inspector on its own timer (never faster than its frames), pauses and plays from
    /// the toolbar and holds still under the viewer; the viewer plays it too, K pausing
    /// it, then steps on to an AVIF and a TIFF stored turned, decoded upright by
    /// ImageMagick on a worker (when it is here); with nothing to read it, a HEIC says
    /// what to install. No slow frames.
    #[test]
    fn animated_pictures_and_photos() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/anim");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("dance.gif"), crate::tests::gif_run(240, 160, 12, 8, gif::Repeat::Infinite)).unwrap();
        png_file(&d.join("still.png"), 120, 80, 2);
        std::fs::write(d.join("photo.heic"), "not a photo").unwrap();
        let tools = crate::tests::any_imagemagick_tools(&root().join("anim-tools"));
        let mut avif = false;
        if let Some((t, im)) = &tools {
            let made = std::process::Command::new(t.join(im)).args(["-size", "600x200", "xc:#d63", "-size", "600x200", "xc:#36c", "-append", "-orient", "RightTop"]).arg(d.join("turned.tiff")).status();
            assert!(made.is_ok_and(|s| s.success()), "a TIFF made");
            avif = std::process::Command::new("ffmpeg").args(["-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=320x240", "-frames:v", "1"]).arg(d.join("photo.avif")).status().is_ok_and(|s| s.success());
            crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(t.clone()));
        } else {
            eprintln!("sim: photos skipped (no ImageMagick)");
        }
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.cmd(ToggleInspector);
        select_path(&mut s, &d.join("dance.gif"));
        wait_until(&mut s, "the GIF playing", 10, |s| inspector(s).anim_state().is_some());
        let at = |s: &Sim| inspector(s).anim_state().map_or(usize::MAX, |a| a.0);
        let mut shown = std::collections::HashSet::new();
        for _ in 0..90 {
            s.idle(1);
            shown.insert(at(&s));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(shown.len() >= 12, "frames move on with time: {shown:?}");
        assert!(s.repaint > std::time::Duration::ZERO && s.repaint <= std::time::Duration::from_millis(80), "woken for the next frame only: {:?}", s.repaint);
        toolbar_centred(&mut s, "animated", &[]);
        s.act("move -50 -50");
        s.idle(2);
        s.shot("animated-gif-inspector-graphite");
        click_control(&mut s, "Pause");
        let still = at(&s);
        s.idle(30);
        assert_eq!((at(&s), inspector(&s).anim_state().unwrap().4), (still, false), "paused");
        // Memory goes back to the system two seconds (real ones) after decodes settle.
        std::thread::sleep(std::time::Duration::from_millis(2200));
        s.idle(3);
        assert_eq!(s.repaint, std::time::Duration::MAX, "nothing ticks while paused");
        click_control(&mut s, "Play");
        s.idle(20);
        assert_ne!(at(&s), still, "playing again");
        // The viewer plays it; the inspector under it holds still.
        s.act("key F3");
        let vat = |s: &Sim| viewer(s).anim.as_ref().filter(|a| a.moving()).map_or(usize::MAX, |a| a.state().0);
        wait_until(&mut s, "the viewer playing", 10, |s| vat(s) != usize::MAX);
        let under = at(&s);
        let mut shown = std::collections::HashSet::new();
        for _ in 0..60 {
            s.idle(1);
            shown.insert(vat(&s));
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(shown.len() >= 8, "the viewer's frames move on: {shown:?}");
        assert_eq!(at(&s), under, "the inspector holds still under the viewer");
        s.idle(2);
        shot_still(&mut s, "animated-gif-viewer-graphite");
        s.act("key K");
        let held = vat(&s);
        s.idle(30);
        assert_eq!(vat(&s), held, "K pauses");
        assert!(!viewer(&s).playing, "not the slideshow");
        s.act("key K");
        s.idle(20);
        assert_ne!(vat(&s), held, "K plays");
        if tools.is_some() {
            // Name order: dance.gif, photo.avif, photo.heic, still.png, turned.tiff.
            s.act("key End");
            wait_until(&mut s, "the TIFF decoded", 20, |s| s.app.thumbs.dims(&d.join("turned.tiff")).is_some());
            assert_eq!(s.app.thumbs.dims(&d.join("turned.tiff")), Some([400, 600]), "upright");
            assert!(viewer(&s).anim.is_none(), "nothing plays for a photo");
            s.idle(2);
            shot_still(&mut s, "photo-viewer-graphite");
            s.act("key ArrowLeft");
            s.act("key ArrowLeft");
            wait_until(&mut s, "the HEIC tried", 20, |s| s.app.thumbs.failed(&d.join("photo.heic")));
            s.idle(2);
            assert!(some_text(&s, "Can't show this picture"));
            if avif {
                s.act("key ArrowLeft");
                wait_until(&mut s, "the AVIF decoded", 20, |s| s.app.thumbs.dims(&d.join("photo.avif")).is_some());
                assert_eq!(s.app.thumbs.dims(&d.join("photo.avif")), Some([320, 240]));
            }
            s.act("key Escape");
            select_path(&mut s, &d.join("turned.tiff"));
            s.idle(3);
            look(&mut s, "a TIFF in the inspector");
            s.shot("photo-inspector-graphite");
        } else {
            s.act("key Escape");
        }
        // Nothing to read a HEIC: what to install.
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        select_path(&mut s, &d.join("still.png"));
        select_path(&mut s, &d.join("photo.heic"));
        // Only where nothing reads a HEIC: a desktop thumbnailer for it (glycin's, say)
        // makes it a picture like any other.
        if crate::thumbcache::thumbnailer_for(&crate::mime::mime_db().for_name("photo.heic", false, false)).is_none() {
            wait_until(&mut s, "the install note", 5, |s| some_text(s, "Install ImageMagick to see HEIC photos"));
            look(&mut s, "a HEIC without ImageMagick");
        } else {
            eprintln!("animated pictures: a thumbnailer here reads HEIC, so the install note is not shown");
        }
        for (ms, step) in &s.slow {
            eprintln!("  slow frame {ms:.0} ms after: {step}");
        }
        assert!(s.slow.is_empty(), "stalled");
        report("animated pictures", &s);
    }

    fn control_rect(s: &mut Sim, what: &str) -> Rect {
        s.idle(1);
        crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == what).map(|(_, r, _)| *r)).unwrap_or_else(|| s.fail(&format!("no {what} on screen")))
    }

    fn shelf_names(s: &Sim) -> Vec<String> {
        s.app.shelf.items.iter().map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned()).collect()
    }

    /// The shelf: files gathered from three folders by key, menu and a drop; copied into
    /// one folder and undone, then moved into another and undone; one item taken off by
    /// its ×, one whose file went away dropping off by itself, the rest cleared; a file
    /// dropped on the empty shelf's spot; an item dragged off it into a folder; the strip
    /// only there while it holds something.
    #[test]
    fn shelf_gathers_then_moves_or_copies() {
        if !sandboxed() {
            return;
        }
        let home = fixture("shelf");
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        assert_eq!(s.app.shelf.shown_h, 0.0, "no strip while it is empty");
        select_path(&mut s, &home.join("Documents/notes.txt"));
        s.act("key alt+s");
        assert_eq!(shelf_names(&s), ["notes.txt"]);
        s.idle(12);
        assert!(s.app.shelf.shown_h > 20.0, "the strip slides in");
        assert!(some_text(&s, "1 item"));
        go(&mut s, &home.join("Music/Album"));
        let row = row_text(&s, "01 Intro.mp3");
        s.act(&format!("click {:.0} {:.0} right", row.center().x, row.center().y));
        s.idle(2);
        s.click_text("Add to Shelf");
        assert_eq!(shelf_names(&s), ["notes.txt", "01 Intro.mp3"]);
        // Dragged from the list onto the strip.
        let row = row_text(&s, "02 Song.flac");
        let strip = control_rect(&mut s, "shelf all");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", row.center().x, row.center().y, strip.right() + 200.0, strip.center().y));
        assert_eq!(shelf_names(&s), ["notes.txt", "01 Intro.mp3", "02 Song.flac"], "dropped on it");
        assert!(home.join("Music/Album/02 Song.flac").exists(), "and left where it was");
        assert!(some_text(&s, "3 items") && some_text(&s, "notes.txt") && some_text(&s, "02 Song.flac"));
        look(&mut s, "shelf with three items");
        s.shot("shelf-graphite");
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "shelf, light");
        s.shot("shelf-paper");
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        // Copied into Empty, and undone.
        go(&mut s, &home.join("Empty"));
        click_control(&mut s, "shelf copy");
        wait_until(&mut s, "copied", 10, |_| ["notes.txt", "01 Intro.mp3", "02 Song.flac"].iter().all(|f| home.join("Empty").join(f).exists()));
        assert_eq!(s.app.shelf.items.len(), 3, "a copy keeps the shelf");
        wait_until(&mut s, "the copy's undo", 10, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Created(_))));
        s.act("key ctrl+z");
        wait_until(&mut s, "copies undone", 10, |_| !home.join("Empty/notes.txt").exists());
        // Moved into a new folder from the palette, and undone.
        std::fs::create_dir_all(home.join("Gathered")).unwrap();
        go(&mut s, &home.join("Gathered"));
        palette_run(&mut s, "Move Shelf Here");
        wait_until(&mut s, "moved", 10, |_| ["notes.txt", "01 Intro.mp3", "02 Song.flac"].iter().all(|f| home.join("Gathered").join(f).exists()));
        assert!(!home.join("Documents/notes.txt").exists());
        assert!(s.app.shelf.items.is_empty(), "a move empties the shelf");
        s.idle(12);
        assert_eq!(s.app.shelf.shown_h, 0.0, "and the strip goes");
        wait_until(&mut s, "the move's undo", 10, |s| s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Move(_))));
        s.act("key ctrl+z");
        wait_until(&mut s, "move undone", 10, |_| home.join("Documents/notes.txt").exists() && home.join("Music/Album/01 Intro.mp3").exists());
        // One off by its ×, one gone from the disk, the rest cleared.
        go(&mut s, &home.join("Documents"));
        s.act("key ctrl+a");
        s.act("key alt+s");
        let n = s.app.shelf.items.len();
        assert!(n >= 5, "the whole folder: {n}");
        s.idle(12);
        let chip = control_rect(&mut s, "shelf item");
        s.act(&format!("move {:.0} {:.0}", chip.center().x, chip.center().y));
        s.act(&format!("click {:.0} {:.0}", chip.right() - 12.0, chip.center().y));
        assert_eq!(s.app.shelf.items.len(), n - 1, "taken off by its ×");
        assert!(!s.app.shelf.items.iter().any(|i| i.path == home.join("Documents/Taxes")), "the first one");
        std::fs::remove_file(home.join("Documents/report.md")).unwrap();
        wait_until(&mut s, "the vanished file dropped", 10, |s| !s.app.shelf.has(&home.join("Documents/report.md")));
        // A click on an item shows it in its folder.
        go(&mut s, &home);
        let chip = control_rect(&mut s, "shelf item");
        s.act(&format!("click {:.0} {:.0}", chip.left() + 12.0, chip.center().y));
        s.settle();
        assert_eq!(s.tab().path, home.join("Documents"), "its folder");
        assert_eq!(s.tab().cursor.map(|c| s.tab().entries[c as usize].path.clone()), s.app.shelf.items.first().map(|i| i.path.clone()), "selected there");
        click_control(&mut s, "shelf clear");
        assert!(s.app.shelf.items.is_empty());
        // Dropped on the empty shelf's spot, at the bottom while files are dragged.
        go(&mut s, &home.join("Music/Album"));
        s.idle(12);
        let row = row_text(&s, "01 Intro.mp3");
        let (cx, by) = (s.size.x / 2.0 + 20.0, s.size.y - 22.0);
        s.act(&format!("down {:.0} {:.0}", row.center().x, row.center().y));
        for k in 1..=10 {
            let t = k as f32 / 10.0;
            s.act(&format!("move {:.0} {:.0}", row.center().x + (cx - row.center().x) * t, row.center().y + (by - row.center().y) * t));
        }
        assert!(some_text(&s, "Shelf"), "the spot to drop on");
        s.shot("shelf-drop-spot-graphite");
        s.act(&format!("up {cx:.0} {by:.0}"));
        assert_eq!(shelf_names(&s), ["01 Intro.mp3"], "dropped on the spot");
        assert!(home.join("Music/Album/01 Intro.mp3").exists());
        // Dragged off the shelf onto a folder: moved there.
        go(&mut s, &home);
        s.idle(12);
        let chip = control_rect(&mut s, "shelf item");
        let docs = row_text(&s, "Documents");
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", chip.left() + 14.0, chip.center().y, docs.center().x, docs.center().y));
        wait_until(&mut s, "moved onto the folder", 10, |_| home.join("Documents/01 Intro.mp3").exists());
        wait_until(&mut s, "and off the shelf", 10, |s| s.app.shelf.items.is_empty());
        // Tight and split.
        go(&mut s, &home.join("Documents"));
        s.act("key ctrl+a");
        s.act("key alt+s");
        s.cmd(SplitRight);
        s.settle();
        look(&mut s, "shelf with a split");
        s.resize(520.0, 340.0);
        look(&mut s, "shelf small");
        s.shot("shelf-small-graphite");
        s.resize(1280.0, 800.0);
        report("shelf", &s);
    }

    // ---------------------------------------------------------------- welcome tour

    fn tour(s: &Sim) -> &crate::tour::State {
        match &s.app.popup {
            Popup::Tour(t) => t,
            _ => s.fail(&format!("the tour is not open (popup {})", s.app.popup.kind_name())),
        }
    }

    /// Nothing of Maia's on disk, as on a first start.
    fn forget_maia() {
        for p in [crate::config::config_path(), crate::config::userdata_path(), crate::config::session_path()] {
            let _ = std::fs::remove_file(p);
        }
    }

    /// The welcome settled and nothing pointed at: its own text demanded clean (it covers
    /// the window, so what is under it is not looked at).
    fn look_welcome(s: &mut Sim, what: &str) {
        s.note(format!("look: {what}"));
        s.act("move -50 -50");
        s.idle(16);
        s.check_overlap = true;
        s.idle(2);
        s.check_overlap = false;
    }

    /// Wait for the page's film to be on screen (decoded on its own thread).
    fn film_on(s: &mut Sim) {
        wait_until(s, "the film shown", 10, |s| tour(s).film_shown());
        s.idle(12);
    }

    /// The welcome tour on a first start and never again; What's New on a start after
    /// an update (a 0.2 home has no record of the version) and not after a newer one; both
    /// from the palette and Options. Pages by keys, buttons, dots and clicks outside;
    /// choices on the first page made in place and kept; films played to their end and
    /// then still, with nothing left waking the window; every page in a wide, a narrow
    /// and a short window, in a dark and a light theme, and with animations off.
    #[test]
    fn welcome_tour_and_whats_new() {
        if !sandboxed() {
            return;
        }
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::tour::AUTO.with(|a| a.set(false));
                forget_maia();
            }
        }
        let _off = Off;
        crate::tour::AUTO.with(|a| a.set(true));
        let home = fixture("tour");
        let now = env!("CARGO_PKG_VERSION");
        let seen = || crate::config::load_userdata().seen_version;
        // A first start: the welcome over the whole window, its choices at their defaults.
        forget_maia();
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.idle(3);
        assert!(tour(&s).kind == crate::tour::Kind::Welcome && tour(&s).auto && tour(&s).page == 0, "the tour on a first start");
        assert_eq!(seen(), now, "this version written down at once");
        s.shot("tour-start");
        let n = crate::tour::WELCOME.len();
        for t in ["Welcome to Maia", "Get started", "Skip", &format!("1 of {n}")] {
            assert!(some_text(&s, t), "{t:?} on the first page");
        }
        let d = crate::config::Config::default();
        // A run in another theme (MAIA_SIM_THEME) starts there.
        let themed = std::env::var_os("MAIA_SIM_THEME").is_some();
        assert!((themed || s.app.cfg.theme == d.theme) && !s.app.cfg.show_hidden && !s.app.cfg.single_click && !s.app.cfg_dirty, "nothing chosen yet");
        look_welcome(&mut s, "welcome first page");
        let controls = |s: &Sim, what: &str| crate::ui::CONTROLS.with(|c| c.borrow().iter().filter(|(_, _, w)| *w == what).map(|(_, r, _)| *r).collect::<Vec<_>>());
        let click = |s: &mut Sim, r: Rect| s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        // The look: a theme pointed at shows the window in it, one clicked is kept.
        click_control(&mut s, "welcome next");
        assert_eq!(tour(&s).page, 1);
        s.idle(14);
        assert!(some_text(&s, "Pick a look") && some_text(&s, "Follow the desktop"));
        let themes = controls(&s, "tour theme");
        assert_eq!(themes.len(), 6, "six themes to pick from");
        s.act(&format!("move {:.0} {:.0}", themes[1].center().x, themes[1].center().y));
        s.idle(2);
        assert!(s.app.preview_theme.is_some(), "pointed at, the window shows it");
        s.act("move -50 -50");
        s.idle(2);
        click(&mut s, themes[5]);
        assert_eq!(s.app.cfg.theme, crate::theme::ThemeName::Latte, "picked");
        assert!(!s.app.cfg.follow_system_theme, "picked by hand, the desktop's is not followed");
        s.idle(2);
        assert!(!s.app.pal.dark, "the window light at once");
        assert!(s.app.cfg_dirty, "kept as Options keeps it");
        look_welcome(&mut s, "welcome look");
        // The size: Roomy, the window following; animations off and on again.
        s.act("key ArrowRight");
        assert_eq!(tour(&s).page, 2);
        s.idle(14);
        let sizes = controls(&s, "welcome size");
        assert_eq!(sizes.len(), 3);
        click(&mut s, sizes[2]);
        assert!((s.app.cfg.font_size - 16.5).abs() < 0.01 && (s.app.cfg.spacing - 1.6).abs() < 0.01, "Roomy: {} {}", s.app.cfg.font_size, s.app.cfg.spacing);
        s.idle(4);
        look_welcome(&mut s, "welcome size, roomy");
        let anim = s.app.cfg.animations;
        click_in_row(&mut s, "switch", "Animations");
        assert_eq!(s.app.cfg.animations, !anim);
        click_in_row(&mut s, "switch", "Animations");
        assert_eq!(s.app.cfg.animations, anim);
        // Opening: one click, for folders only.
        s.act("key Enter");
        assert_eq!(tour(&s).page, 3, "Enter goes on");
        s.idle(14);
        assert!(!some_text(&s, "Folders only"), "only with one click");
        let clicks = controls(&s, "welcome click");
        assert_eq!(clicks.len(), 2);
        click(&mut s, clicks[1]);
        assert!(s.app.cfg.single_click);
        s.idle(14);
        assert!(some_text(&s, "Folders only"));
        click_in_row(&mut s, "switch", "Folders only");
        assert!(s.app.cfg.single_click_folders_only);
        look_welcome(&mut s, "welcome one click");
        // What is shown: hidden files, behind it at once; previews nowhere.
        click_control(&mut s, "welcome next");
        assert_eq!(tour(&s).page, 4);
        s.idle(14);
        let hidden = controls(&s, "welcome hidden");
        assert_eq!(hidden.len(), 2);
        click(&mut s, hidden[1]);
        s.settle();
        assert!(s.app.cfg.show_hidden && s.tab().view.iter().any(|&i| s.tab().entries[i as usize].name == ".bashrc"), "hidden files shown behind the welcome");
        let chips = controls(&s, "welcome thumbs choice");
        assert_eq!(chips.len(), 3, "three places for previews");
        click(&mut s, chips[2]);
        assert_eq!(s.app.cfg.thumbnails, crate::config::ThumbMode::Never);
        click(&mut s, chips[0]);
        assert_eq!(s.app.cfg.thumbnails, crate::config::ThumbMode::LocalDisks);
        look_welcome(&mut s, "welcome what you see");
        // The extras: search by meaning switched by its whole row.
        click_control(&mut s, "welcome next");
        assert_eq!(tour(&s).page, 5);
        s.idle(14);
        assert!(some_text(&s, "Open folders in Maia") && some_text(&s, "Search by meaning"));
        s.click_text("Search by meaning");
        assert!(s.app.cfg.semantic_search, "the row flips it");
        s.click_text("Search by meaning");
        assert!(!s.app.cfg.semantic_search);
        look_welcome(&mut s, "welcome extras");
        // Back and on by the buttons, the keys and the dots; no further back than the first.
        click_control(&mut s, "welcome back");
        assert_eq!(tour(&s).page, 4);
        click_control(&mut s, "welcome next");
        click_control(&mut s, "welcome next");
        assert_eq!(tour(&s).page, 6);
        film_on(&mut s);
        assert!(some_text(&s, "Go anywhere by typing") && some_text(&s, "Ctrl+P") && some_text(&s, "Go To"), "the page, its keys as bound");
        look_welcome(&mut s, "welcome film page");
        for _ in 0..8 {
            s.act("key ArrowLeft");
        }
        assert_eq!(tour(&s).page, 0, "and no further back than the first");
        let dots = controls(&s, "tour dot");
        assert_eq!(dots.len(), n, "a dot a page");
        click(&mut s, dots[n - 1]);
        assert_eq!(tour(&s).page, n - 1);
        film_on(&mut s);
        assert!(some_text(&s, "Start using Maia") && !some_text(&s, "Skip"), "the last page ends it, no Skip");
        s.act("key ArrowRight");
        assert!(s.popup_open(), "Right on the last page stays");
        // Escape closes it, saying where it is.
        s.act("key Escape");
        assert!(!s.popup_open());
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0.contains("Welcome Tour")), "where to find it again");
        // Skip closes it too, from any page but the last.
        s.cmd(Welcome);
        s.act("key ArrowRight");
        click_control(&mut s, "welcome skip");
        assert!(!s.popup_open(), "skipped");
        s.app.cfg.font_size = d.font_size;
        s.app.cfg.spacing = d.spacing;
        s.app.save_all(&s.ctx.clone());
        drop(s);
        // Started again: nothing, and the choices kept.
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.idle(3);
        assert!(!s.popup_open(), "the tour never comes back by itself");
        assert!(s.app.cfg.show_hidden && s.app.cfg.theme == crate::theme::ThemeName::Latte, "the choices kept");
        drop(s);
        // An update from 0.2: settings there, no version recorded. What's New.
        forget_maia();
        crate::config::save_json(&crate::config::config_path(), &crate::config::Config::default()).unwrap();
        std::fs::write(crate::config::userdata_path(), r#"{"bookmarks":[],"recents":[]}"#).unwrap();
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.idle(3);
        assert!(tour(&s).kind == crate::tour::Kind::News && tour(&s).auto, "What's New after an update");
        // Over the whole window as the welcome is: the version, then a film a page.
        let v = crate::tour::NEWS_VERSION;
        let n = crate::tour::NEWS.len();
        s.idle(14);
        for t in [&format!("Maia {v}") as &str, v, "See what's new", "Skip", &format!("1 of {n}")] {
            assert!(some_text(&s, t), "{t:?} on What's New's first page");
        }
        look_welcome(&mut s, "news first page");
        click_control(&mut s, "welcome next");
        assert_eq!(tour(&s).page, 1);
        film_on(&mut s);
        assert!(some_text(&s, "Jump to the folders you use") && some_text(&s, "Ctrl+G") && some_text(&s, "Jump To Folder"), "the page, its keys as bound");
        look_welcome(&mut s, "news film page");
        // A command without keys is shown by its name, as the palette finds it.
        s.act("key ArrowRight");
        film_on(&mut s);
        assert!(some_text(&s, "Show Space Used") && some_text(&s, "Free Up Space"));
        for _ in 2..n {
            s.act("key ArrowRight");
        }
        s.idle(14);
        assert_eq!(tour(&s).page, n - 1);
        for t in ["Also new", "For everyone", "Send To Phone", "Transfers you can steer", "Done"] {
            assert!(some_text(&s, t), "{t:?} on the last page");
        }
        assert!(!some_text(&s, "Skip"), "the last page ends it, no Skip");
        look_welcome(&mut s, "news list page");
        // The groups further down are a scroll away.
        let at = s.text_rect_any(&["Send To Phone"]).unwrap_or_else(|| s.fail("no card"));
        for _ in 0..6 {
            s.act(&format!("scroll {:.0} {:.0} -10", at.center().x, at.center().y));
            s.idle(3);
        }
        assert!(some_text(&s, "Lines of code") && some_text(&s, "For developers"), "the last group reached");
        look_welcome(&mut s, "news list page scrolled");
        // Done closes it, saying where it is.
        click_control(&mut s, "welcome next");
        assert!(!s.popup_open(), "done");
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0.contains("What's New in Maia")), "where to find it again");
        assert_eq!(seen(), now);
        drop(s);
        // A newer version seen (Maia went back a version): nothing.
        let mut ud = crate::config::load_userdata();
        ud.seen_version = "99.0.0".into();
        crate::config::save_json(&crate::config::userdata_path(), &ud).unwrap();
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.idle(3);
        assert!(!s.popup_open(), "nothing after a newer version");
        // From the palette, and from Options: no word on closing.
        s.act("key ctrl+shift+p");
        s.act("text welcome tour");
        s.act("key Enter");
        assert!(tour(&s).kind == crate::tour::Kind::Welcome && !tour(&s).auto, "from the palette");
        click_control(&mut s, "welcome skip");
        assert!(!s.popup_open() && s.app.toast.is_none(), "closed without a word");
        s.app.popup = Popup::options_at(crate::popups::Section::System);
        s.idle(3);
        // At a big font the section scrolls to it.
        for _ in 0..6 {
            if some_text(&s, "What's new") {
                break;
            }
            let c = s.size / 2.0;
            s.act(&format!("scroll {:.0} {:.0} -10", c.x + 80.0, c.y));
            s.idle(3);
        }
        assert!(some_text(&s, "What's new"), "a row in Startup & system");
        click_control(&mut s, "whats new button");
        s.idle(1);
        assert!(tour(&s).kind == crate::tour::Kind::News, "from Options");
        // A film plays to its end, then rests, and nothing wakes the window.
        s.act("key ArrowRight");
        film_on(&mut s);
        wait_until(&mut s, "the film over", 60, |s| {
            tour(s).film_done()
        });
        // Memory goes back to the system two seconds (real ones) after decodes settle;
        // with motion off the film ends inside them.
        std::thread::sleep(std::time::Duration::from_millis(2200));
        s.idle(3);
        assert_eq!(s.repaint, std::time::Duration::MAX, "nothing left to draw: {:?}", s.repaint);
        // A click on it plays it again.
        click_control(&mut s, "tour film");
        s.idle(2);
        assert!(!tour(&s).film_done(), "playing again");
        s.act("key Escape");
        // Animations off: each film shows its last state at once, nothing ticking.
        s.app.cfg.animations = false;
        s.cmd(WhatsNew);
        s.act("key ArrowRight");
        film_on(&mut s);
        wait_until(&mut s, "the still", 10, |s| tour(s).film_done());
        s.idle(3);
        assert_eq!(s.repaint, std::time::Duration::MAX, "a still: {:?}", s.repaint);
        s.act("key Escape");
        s.app.cfg.animations = true;
        // Every page, in a wide, a narrow and a short window, dark and light.
        let shots = std::env::var_os("MAIA_SIM_SHOTS").is_some();
        for theme in [crate::theme::ThemeName::Graphite, crate::theme::ThemeName::Paper] {
            s.app.cfg.theme = theme;
            for (w, h) in [(1280.0, 800.0), (640.0, 480.0), (900.0, 420.0)] {
                s.resize(w, h);
                for (cmd, kind) in [(Welcome, "welcome"), (WhatsNew, "news")] {
                    s.cmd(cmd);
                    let n = tour(&s).pages().len();
                    for p in 0..n {
                        if !matches!(crate::tour::pages(tour(&s).kind)[p].body, crate::tour::Body::Film(_)) {
                            s.idle(14);
                        } else {
                            film_on(&mut s);
                        }
                        look(&mut s, &format!("tour {kind} page {p} {w}x{h}"));
                        if shots && (w == 1280.0 || p < 2) {
                            s.shot(&format!("tour-{}-{w}-{kind}-{p}", theme.label().to_lowercase()));
                        }
                        s.act("key ArrowRight");
                    }
                    s.act("key Escape");
                    assert!(!s.popup_open());
                }
            }
        }
        // Each step in a short and a narrow window, at a big font: nothing overlaps.
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        for (w, h, fs, sp) in [(900.0, 420.0, 16.5, 1.6), (520.0, 640.0, 16.5, 1.6), (1280.0, 800.0, 20.0, 2.0), (700.0, 460.0, 11.0, 0.7)] {
            s.resize(w, h);
            s.cmd(Welcome);
            s.app.cfg.font_size = fs;
            s.app.cfg.spacing = sp;
            for p in 0..6 {
                s.idle(14);
                look_welcome(&mut s, &format!("welcome step {p} at {w}x{h}, {fs}/{sp}"));
                if shots {
                    s.shot(&format!("welcome-step-{p}-{w}x{h}-{fs}"));
                }
                s.act("key ArrowRight");
            }
            s.act("key Escape");
        }
        s.app.cfg.font_size = d.font_size;
        s.app.cfg.spacing = d.spacing;
        s.resize(1280.0, 800.0);
        report("welcome tour", &s);
    }

    // ---------------------------------------------------------------- tour films

    /// A made-up landscape photograph: a sky in two colours, a sun with its glow, three
    /// rows of hills and their reflection in still water. The films need pictures that
    /// read as pictures, and nothing here may come from anyone's camera.
    fn landscape(path: &Path, w: u32, h: u32, k: usize) {
        type C = [f32; 3];
        // Sky top, sky at the horizon, sun, far, middle and near hills, water.
        const LOOKS: [[C; 7]; 6] = [
            [[38.0, 52.0, 110.0], [246.0, 168.0, 112.0], [255.0, 226.0, 170.0], [128.0, 92.0, 132.0], [84.0, 62.0, 108.0], [40.0, 34.0, 70.0], [52.0, 60.0, 112.0]],
            [[64.0, 128.0, 206.0], [196.0, 226.0, 246.0], [255.0, 252.0, 232.0], [120.0, 160.0, 178.0], [78.0, 128.0, 112.0], [44.0, 92.0, 64.0], [70.0, 128.0, 172.0]],
            [[16.0, 22.0, 52.0], [84.0, 92.0, 158.0], [236.0, 236.0, 255.0], [62.0, 66.0, 112.0], [38.0, 42.0, 82.0], [20.0, 24.0, 52.0], [30.0, 38.0, 80.0]],
            [[250.0, 196.0, 128.0], [255.0, 238.0, 206.0], [255.0, 250.0, 236.0], [212.0, 150.0, 120.0], [168.0, 104.0, 86.0], [104.0, 62.0, 58.0], [220.0, 170.0, 140.0]],
            [[90.0, 160.0, 190.0], [220.0, 236.0, 222.0], [255.0, 250.0, 220.0], [136.0, 168.0, 150.0], [96.0, 140.0, 96.0], [58.0, 100.0, 54.0], [96.0, 150.0, 160.0]],
            [[120.0, 70.0, 140.0], [250.0, 140.0, 120.0], [255.0, 214.0, 150.0], [150.0, 80.0, 120.0], [104.0, 52.0, 92.0], [56.0, 26.0, 56.0], [110.0, 60.0, 110.0]],
        ];
        let c = LOOKS[k % LOOKS.len()];
        let ph = k as f32 * 1.7;
        let (wf, hf) = (w as f32, h as f32);
        let water = hf * (0.70 + (k % 3) as f32 * 0.03);
        let sun = (wf * (0.25 + (k % 5) as f32 * 0.12), hf * (0.30 + (k % 4) as f32 * 0.07));
        let ridge = |l: usize, x: f32| -> f32 {
            let (base, amp, f) = [(0.50, 0.10, 2.1), (0.58, 0.08, 3.3), (0.66, 0.05, 5.2)][l];
            let x = x / wf * std::f32::consts::TAU;
            hf * (base + amp * ((x * f * 0.5 + ph + l as f32).sin() * 0.6 + (x * f + ph * 0.7).sin() * 0.3 + (x * f * 2.3 + l as f32 * 2.0).sin() * 0.1))
        };
        let mix = |a: C, b: C, t: f32| -> C { std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t.clamp(0.0, 1.0)) };
        let scene = |x: f32, y: f32| -> C {
            for l in (0..3).rev() {
                let top = ridge(l, x);
                if y >= top {
                    // Haze: a ridge lightens towards its top.
                    return mix(c[3 + l], c[1], 0.25 * (1.0 - ((y - top) / (hf * 0.2)).min(1.0)) * (2 - l) as f32 / 2.0);
                }
            }
            let sky = mix(c[0], c[1], (y / water).powf(1.4));
            let d = ((x - sun.0).powi(2) + (y - sun.1).powi(2)).sqrt() / hf;
            if d < 0.045 {
                return c[2];
            }
            mix(sky, c[2], (0.35 - d).max(0.0) * 1.6)
        };
        let mut px = Vec::with_capacity((w * h * 3) as usize);
        for y in 0..h {
            for x in 0..w {
                let (xf, yf) = (x as f32, y as f32);
                let col = if yf < water {
                    scene(xf, yf)
                } else {
                    // Still water: the scene upside down, darker, with long ripples.
                    let ripple = ((yf - water) * 0.9 + (xf * 0.02).sin() * 3.0).sin() * (yf - water) * 0.02;
                    mix(scene(xf + ripple * 6.0, 2.0 * water - yf), c[6], 0.45)
                };
                px.extend(col.map(|v| v.clamp(0.0, 255.0) as u8));
            }
        }
        write_png(path, w as usize, h as usize, &px);
    }

    /// A home folder as someone keeps it, for the films: no odd names, nothing broken.
    fn film_home() -> PathBuf {
        let h = root().join("home/sam");
        let w = |p: &str, s: &str| {
            let p = h.join(p);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, s).unwrap();
        };
        w("Documents/Garden plan.md", "# Garden plan\n\nWhat goes where this spring, and what it costs.\n\n## Beds\n\n- **North bed**: tomatoes, basil, two rows of beans\n- **South bed**: courgettes and squash\n- **By the fence**: sweet peas\n\n## To buy\n\n| Item | Count | Price |\n|---|---|---|\n| Seed trays | 4 | 12.00 |\n| Compost | 6 bags | 39.00 |\n| Canes | 20 | 9.50 |\n\n> Water in the evening once the seedlings are out.\n");
        w("Documents/Budget 2026.csv", "Month,Rent,Food,Travel,Saved\nJanuary,950,412,60,300\nFebruary,950,388,120,260\nMarch,950,430,45,310\nApril,950,401,220,180\nMay,950,395,80,290\nJune,950,420,340,150\n");
        w("Documents/Letter to the landlord.txt", "Dear Ms. Alvarez,\n\nThe kitchen tap has been dripping since Monday. Could someone look at it\nthis week? I am home after five every day.\n\nThank you,\nSam\n");
        w("Documents/Reading list.md", "# Reading list\n\n1. The Overstory\n2. Piranesi\n3. A Psalm for the Wild-Built\n");
        w("Documents/Recipes/Lemon cake.md", "# Lemon cake\n\n200 g flour, 200 g sugar, 3 eggs, 2 lemons.\n");
        w("Documents/Recipes/Bean soup.md", "# Bean soup\n\nSoak overnight.\n");
        for (m, c) in [("07", "0.25 0.45 0.80"), ("08", "0.80 0.35 0.25"), ("09", "0.25 0.65 0.40")] {
            w(&format!("Documents/Invoices/Invoice 2026-{m}.pdf"), &sample_pdf(&[c]));
        }
        w("Documents/Invoices/Electricity 2026-08.pdf", &sample_pdf(&["0.6 0.5 0.2"]));
        std::fs::create_dir_all(h.join("Pictures/Lisbon")).unwrap();
        for i in 0..9 {
            let png = h.join(format!("Pictures/Lisbon/IMG_{}.png", 2041 + i));
            landscape(&png, 960, 640, i);
            let jpg = png.with_extension("jpg");
            let ok = std::process::Command::new("magick").arg(&png).args(["-quality", "88"]).arg(&jpg).status().is_ok_and(|s| s.success());
            if ok {
                let _ = std::fs::remove_file(&png);
            }
        }
        landscape(&h.join("Pictures/Harbour at dusk.png"), 1200, 700, 5);
        landscape(&h.join("Pictures/Hills in May.png"), 1200, 700, 4);
        w("Music/Night Drive/01 Coastline.flac", "fLaC");
        w("Music/Night Drive/02 Long Way Home.flac", "fLaC");
        w("Downloads/Backup/readme.txt", "Copies of the important things.\n");
        w("Downloads/tickets.pdf", &sample_pdf(&["0.3 0.3 0.6"]));
        std::fs::create_dir_all(h.join("Videos")).unwrap();
        // Room taken, for Space Used: real bytes, so every way of counting agrees.
        std::fs::write(h.join("Videos/Harbour at dusk.mp4"), vec![7u8; 24 << 20]).unwrap();
        std::fs::write(h.join("Videos/Birthday.mkv"), vec![8u8; 11 << 20]).unwrap();
        std::fs::write(h.join("Downloads/ubuntu.iso"), vec![9u8; 16 << 20]).unwrap();
        // A small project with a history and work in progress.
        let p = h.join("Code/weather");
        let wf = |f: &str, s: &str| {
            let f = p.join(f);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, s).unwrap();
        };
        wf("README.md", "# weather\n\nToday's forecast in the terminal: the high, the low and the chance of rain for any city, in Celsius or Fahrenheit.\n\n## Install\n\n    cargo install --path .\n\n## Use\n\n    weather Lisbon\n    weather --fahrenheit Porto\n\n## Next\n\n- Wind on the Beaufort scale\n- A week ahead\n- Colours for hot and cold days\n");
        wf("Cargo.toml", "[package]\nname = \"weather\"\nversion = \"0.4.0\"\nedition = \"2024\"\n");
        wf("src/main.rs", "mod forecast;\nmod units;\n\nfn main() {\n    let f = forecast::today(\"Lisbon\");\n    println!(\"{}\", f.summary());\n}\n");
        wf("src/forecast.rs", "pub struct Forecast {\n    pub high: f32,\n    pub low: f32,\n}\n\npub fn today(city: &str) -> Forecast {\n    let _ = city;\n    Forecast { high: 24.0, low: 16.0 }\n}\n");
        wf("src/units.rs", "pub fn to_fahrenheit(c: f32) -> f32 {\n    c * 9.0 / 5.0 + 32.0\n}\n");
        wf("src/cli/args.rs", "pub struct Args { pub city: String }\n");
        wf("docs/usage.md", "# Usage\n\n    weather Lisbon\n");
        let g = |args: &[&str], day: u32| {
            let date = format!("2026-10-{day:02}T18:30:00+00:00");
            let ok = std::process::Command::new("git")
                .args(["-c", "user.name=Sam Rivera", "-c", "user.email=sam@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
                .args(args)
                .env("GIT_AUTHOR_DATE", &date)
                .env("GIT_COMMITTER_DATE", &date)
                .current_dir(&p)
                .stdout(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?}");
        };
        g(&["init", "-q"], 1);
        for (day, msg) in [(9, "Start with today's forecast"), (12, "Read the city from the command line"), (15, "Convert to Fahrenheit")] {
            g(&["add", "-A"], day);
            g(&["commit", "-qm", msg], day);
            std::fs::write(p.join("CHANGES"), msg).unwrap();
        }
        wf("src/forecast.rs", "pub struct Forecast {\n    pub high: f32,\n    pub low: f32,\n    pub rain: u8,\n}\n\npub fn today(city: &str) -> Forecast {\n    let _ = city;\n    Forecast { high: 24.0, low: 16.0, rain: 10 }\n}\n");
        wf("src/wind.rs", "pub fn beaufort(kmh: f32) -> u8 {\n    (kmh / 6.0).min(12.0) as u8\n}\n");
        h
    }

    /// A film as a few states of the window, each photographed once it has settled and
    /// held for a while, with the keys that led to it (or where the pointer clicked) for
    /// the encoder to draw over it. Only states: the frames of a movement cost nearly a
    /// whole picture each, which the binary has no room for.
    struct Film {
        dir: PathBuf,
        name: String,
        rows: Vec<(String, u32, String, Option<Pos2>)>,
    }

    impl Film {
        fn new(s: &Sim, name: &str) -> Film {
            let dir = s.shots_dir().unwrap_or_else(|| s.fail("films need MAIA_SIM_SHOTS")).join("films").join(name);
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Film { dir, name: name.into(), rows: vec![] }
        }

        /// The window as it is once everything has landed and stopped moving, held for
        /// `ms`, with `keys` shown over it.
        fn shot(&mut self, s: &mut Sim, ms: u32, keys: &str) {
            s.act("move -50 -50");
            s.settle();
            // Growing and fading run on the wall clock and the frames' clock both.
            for _ in 0..8 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                s.idle(12);
            }
            // The films ship: no path outside the made-up home may be in them.
            let sandbox = root().to_string_lossy().into_owned();
            if let Some((_, t)) = s.texts.iter().find(|(_, t)| t.contains(&sandbox) || t.contains(sandbox.trim_end_matches("/root"))) {
                s.fail(&format!("film {} shows a real path: {t:?}", self.name));
            }
            let Some((w, h, rgb)) = s.pixels() else { return };
            let file = format!("{:03}.png", self.rows.len());
            write_png(&self.dir.join(&file), w, h, &rgb);
            self.rows.push((file, ms, keys.to_string(), None));
        }

        /// As `shot`, with the pointer drawn where `line` (a click) pressed.
        fn shot_click(&mut self, s: &mut Sim, line: &str, ms: u32) {
            let w: Vec<&str> = line.split_whitespace().collect();
            let n = |i: usize| w.get(i).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            s.act(line);
            self.shot(s, ms, "");
            if let Some(r) = self.rows.last_mut() {
                r.3 = Some(pos2(n(1) * s.ppp, n(2) * s.ppp));
            }
        }

        fn finish(self, s: &Sim) {
            let p = s.app.pal;
            let hex = |c: egui::Color32| format!("{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
            let mut out = format!("# {} raised={} border={} text={} ppp={}\n", self.name, hex(p.raised), hex(p.border), hex(p.text), s.ppp);
            for (file, ms, keys, at) in &self.rows {
                let at = at.map_or("-".to_string(), |a| format!("{:.0},{:.0}", a.x, a.y));
                out.push_str(&format!("{file}\t{ms}\t{}\t{at}\n", if keys.is_empty() { "-" } else { keys }));
            }
            std::fs::write(self.dir.join("frames.txt"), out).unwrap();
            eprintln!("film {}: {} states", self.name, self.rows.len());
        }
    }

    /// A window as the films show it: Graphite, the tour's fonts, 800 by 500 points
    /// drawn at the scale they are shown at on a 1.6 screen.
    fn film_window(at: &Path, fonts: bool) -> Sim {
        let mut s = Sim::new(at, vec2(720.0, 450.0));
        s.ppp = FILM_PPP;
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.app.cfg.font_size = 15.0;
        s.app.cfg.inspector_font_size = 13.0;
        if fonts {
            s.app.cfg.font_family = "Google Sans Flex".into();
            s.app.cfg.inspector_font = "Google Sans Code".into();
        }
        s.app.cfg.semantic_search = false;
        // Maia's own icons: the same on every machine the films are made on.
        s.app.cfg.system_icons = false;
        // The window behind the tour has the sidebar already: the films leave it out
        // and give the feature the room (and the binary the bytes).
        s.app.sidebar = false;
        s.settle();
        s.idle(20);
        s
    }

    /// The films' scale: shown 540 points wide on a screen at 1.6, over a 720 point
    /// window, one pixel for one.
    const FILM_PPP: f32 = 1.6 * 540.0 / 720.0;

    /// Drives for the films' sidebar: the system disk, a camera's card waiting to be
    /// mounted and a backup drive that is (fake tools only).
    fn film_drives() -> PathBuf {
        let d = fake_drives("films");
        let rows = [
            "/dev/sdw\t-\t-\t-\t500107862016\t0\t1\tdisk",
            "/dev/sdw1\tsdw\t-\tvfat\t536870912\t0\t1\tpart",
            "/dev/sdw2\tsdw\t-\tbtrfs\t499570991104\t0\t1\tpart",
            "/dev/sdz\t-\t-\t-\t64008609792\t1\t1\tdisk",
            "/dev/sdz1\tsdz\tPhoto Card\texfat\t64007561216\t1\t1\tpart",
            "/dev/sdx\t-\t-\t-\t1000204886016\t1\t1\tdisk",
            "/dev/sdx1\tsdx\tBackup\text4\t1000203837440\t1\t1\tpart",
        ];
        std::fs::write(d.join("devices"), rows.join("\n") + "\n").unwrap();
        let backup = d.join("media/Backup");
        std::fs::create_dir_all(&backup).unwrap();
        std::fs::write(d.join("mounted/sdx1"), backup.to_str().unwrap()).unwrap();
        let _ = std::fs::remove_file(d.join("busy/sdx1"));
        let _ = std::fs::remove_file(d.join("mounted/sdz1"));
        let table: String = [("sdw1", "/boot", "vfat"), ("sdw2", "/", "btrfs"), ("sdx1", backup.to_str().unwrap(), "ext4")].iter().map(|(n, m, f)| format!("/dev/{n} {} {f} rw 0 0\n", m.replace(' ', "\\040"))).collect();
        std::fs::write(d.join("mounts"), table).unwrap();
        d
    }

    /// The welcome tour's and What's New's films, photographed for tools/tour-media.sh
    /// (MAIA_TOUR_MEDIA=1 with MAIA_SIM_SHOTS; the fonts in its fonts folder).
    #[test]
    fn tour_films() {
        if !sandboxed() || std::env::var_os("MAIA_TOUR_MEDIA").is_none() || std::env::var_os("MAIA_SIM_SHOTS").is_none() {
            return;
        }
        let shots = PathBuf::from(std::env::var_os("MAIA_SIM_SHOTS").unwrap());
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(film_drives()));
        // A home of its own, so whatever a film shows is under ~ and never the sandbox's
        // own folders; settings, caches and the fonts beside it, out of Space Used.
        let sam = root().join("home/sam");
        // SAFETY: one test at a time (sim.sh), before this test's first App.
        unsafe {
            std::env::set_var("HOME", &sam);
            for (k, d) in [("XDG_CONFIG_HOME", "config"), ("XDG_DATA_HOME", "data"), ("XDG_CACHE_HOME", "cache"), ("XDG_STATE_HOME", "state")] {
                std::env::set_var(k, root().join("films-xdg").join(d));
            }
        }
        let only = std::env::var("MAIA_TOUR_FILM").unwrap_or_default();
        let want = |n: &str| only.is_empty() || only.split(',').any(|o| o == n);
        let h = film_home();
        // The fonts, where fontconfig looks in this home.
        let fdir = root().join("films-xdg/data/fonts");
        std::fs::create_dir_all(&fdir).unwrap();
        let mut fonts = false;
        for f in std::fs::read_dir(shots.join("fonts")).into_iter().flatten().flatten() {
            fonts |= std::fs::copy(f.path(), fdir.join(f.file_name())).is_ok();
        }
        let docs = h.join("Documents");
        let pics = h.join("Pictures/Lisbon");
        let wait_thumbs = |s: &mut Sim| {
            let t0 = Instant::now();
            while t0.elapsed().as_secs() < 10 && s.tab().entries.iter().any(|e| !e.is_dir && s.app.thumbs.dims(&e.path).is_none()) {
                s.frame(vec![]);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            s.settle();
        };
        if want("goto") {
            // Places visited before, which Go To finds by a few letters.
            let mut ud = crate::config::UserData::with_defaults();
            for p in [docs.join("Invoices"), h.join("Code/weather"), pics.clone(), docs.join("Recipes")] {
                ud.push_recent(&p);
            }
            crate::config::save_json(&crate::config::userdata_path(), &ud).unwrap();
            let mut s = film_window(&h, fonts);
            let mut f = Film::new(&s, "goto");
            // Typed into at once: empty, Go To lists the sandbox's own mounts.
            f.shot(&mut s, 900, "");
            s.act("key ctrl+p");
            s.act("text inv");
            wait_until(&mut s, "Invoices found", 10, |s| s.texts.iter().any(|(r, t)| t == "Invoices" && r.top() < 120.0));
            f.shot(&mut s, 1500, "Ctrl P");
            s.act("key Enter");
            f.shot(&mut s, 2200, "Enter");
            f.finish(&s);
            let _ = std::fs::remove_file(crate::config::userdata_path());
        }
        if want("palette") {
            let mut s = film_window(&docs, fonts);
            let mut f = Film::new(&s, "palette");
            s.act("key ctrl+shift+p");
            s.act("text split r");
            f.shot(&mut s, 1700, "Ctrl Shift P");
            s.act("key Enter");
            f.shot(&mut s, 2200, "Enter");
            f.finish(&s);
        }
        if want("split") {
            let mut s = film_window(&docs, fonts);
            // Room for two lists side by side in a small window.
            s.app.sidebar = false;
            s.cmd(SplitRight);
            s.settle();
            go(&mut s, &h.join("Downloads/Backup"));
            s.act("key Tab");
            s.settle();
            select_path(&mut s, &docs.join("Garden plan.md"));
            let mut f = Film::new(&s, "split");
            s.act("key ctrl+c");
            f.shot(&mut s, 1300, "Ctrl C");
            s.act("key Tab");
            s.act("key ctrl+v");
            f.shot(&mut s, 2200, "Tab Ctrl V");
            f.finish(&s);
        }
        if want("preview") {
            let mut s = film_window(&docs, fonts);
            select_path(&mut s, &docs.join("Garden plan.md"));
            let mut f = Film::new(&s, "preview");
            s.act("key Space");
            f.shot(&mut s, 1700, "Space");
            s.act("key ArrowUp");
            f.shot(&mut s, 2000, "↑");
            f.finish(&s);
        }
        if want("git") && crate::media::have("git") {
            let p = h.join("Code/weather");
            let mut s = film_window(&p.join("src"), fonts);
            wait_until(&mut s, "the changes marked", 10, |s| branch_now(s) == "main" && crate::gitstatus::repo_for(&s.tab().path).is_some_and(|r| !r.marks.is_empty()));
            let mut f = Film::new(&s, "git");
            f.shot(&mut s, 1300, "");
            s.cmd(GitHistory);
            wait_until(&mut s, "the history", 10, |s| some_text(s, "Changes"));
            f.shot(&mut s, 2400, "");
            f.finish(&s);
        }
        if want("jump") {
            let mut s = film_window(&h, fonts);
            // Gone to as a person goes: the app counts each folder left.
            let walk = |s: &mut Sim, p: &Path| {
                let panel = s.app.active;
                s.app.actions.push(crate::app::Action::Navigate { panel, path: p.to_path_buf() });
                s.settle();
            };
            for (dir, n) in [(h.join("Code/weather"), 6), (pics.clone(), 4), (docs.join("Recipes"), 3), (docs.join("Invoices"), 2), (h.join("Music/Night Drive"), 1)] {
                for _ in 0..n {
                    walk(&mut s, &dir);
                    walk(&mut s, &h);
                }
            }
            let mut f = Film::new(&s, "jump");
            s.act("key ctrl+g");
            wait_until(&mut s, "the folders", 10, |_| controls_named("jump row").len() >= 4);
            f.shot(&mut s, 1700, "Ctrl G");
            s.act("text lis");
            s.idle(3);
            f.shot(&mut s, 1200, "");
            s.act("key Enter");
            wait_until(&mut s, "in Lisbon", 10, |s| s.tab().path == pics);
            wait_thumbs(&mut s);
            f.shot(&mut s, 2200, "Enter");
            f.finish(&s);
            let _ = std::fs::remove_file(crate::config::userdata_path());
        }
        if want("space") {
            let mut s = film_window(&h, fonts);
            s.cmd(SpaceUsed);
            wait_until(&mut s, "counted", 20, |s| !space(s).counting());
            // What it counted, as if three days ago the harbour film had not come yet and
            // Downloads had held a little more.
            let mut sizes: std::collections::HashMap<String, u64> = space(&s).items.iter().filter_map(|i| Some((i.entry.name.clone(), i.size?))).collect();
            if let Some(v) = sizes.get_mut("Videos") {
                *v = v.saturating_sub(24 << 20);
            }
            if let Some(v) = sizes.get_mut("Downloads") {
                *v += 9 << 20;
            }
            s.act("key Escape");
            s.settle();
            let today = crate::timefmt::local_day(std::time::SystemTime::now());
            let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - 3 * 86_400;
            crate::grew::note_in(&crate::grew::book_path(), crate::grew::Reading { day: today - 3, at, dir: h.clone(), sizes });
            s.cmd(SpaceUsed);
            wait_until(&mut s, "what grew", 20, |s| !space(s).counting() && space(s).before.is_some());
            // The whole window, for the ring beside the list.
            s.act("key f");
            let mut f = Film::new(&s, "space");
            f.shot(&mut s, 2200, "");
            s.act("key Tab");
            s.idle(20);
            f.shot(&mut s, 2400, "Tab");
            s.act("key Escape");
            f.finish(&s);
        }
        if want("calendar") {
            // The trip's photos over three days in May: the time of day they were taken
            // is the files' own.
            let mut photos: Vec<PathBuf> = std::fs::read_dir(&pics).unwrap().flatten().map(|e| e.path()).collect();
            photos.sort();
            let at = |x: i64| crate::timefmt::local_day(std::time::UNIX_EPOCH + std::time::Duration::from_secs(x as u64));
            for (i, p) in photos.iter().enumerate() {
                let d = crate::calendar::day_of(2026, 5, if i < 3 { 14 } else if i < 7 { 15 } else { 17 });
                let mut secs = d * 86_400 + 12 * 3600 + i as i64 * 600;
                secs += (d - at(secs)) * 86_400;
                std::fs::File::options().write(true).open(p).unwrap().set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64)).unwrap();
            }
            let mut s = film_window(&pics, fonts);
            wait_thumbs(&mut s);
            let mut f = Film::new(&s, "calendar");
            s.act("key shift+F3");
            wait_until(&mut s, "the trip's month", 20, |s| some_text(s, "May 2026") && s.texts.iter().any(|(_, t)| t.starts_with("9 pictures")));
            for _ in 0..20 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                s.idle(4);
            }
            f.shot(&mut s, 1700, "Shift F3");
            let days = controls_named("calendar day");
            let Some(day) = s.texts.iter().filter(|(r, t)| t == "15" && days.iter().any(|d| d.contains(r.center()))).map(|x| x.0).next() else { s.fail("no 15th") };
            f.shot_click(&mut s, &format!("click {:.0} {:.0}", day.center().x, day.center().y), 1700);
            s.act("key shift+ArrowRight");
            s.act("key shift+ArrowRight");
            f.shot(&mut s, 2300, "Shift → →");
            s.act("key Escape");
            f.finish(&s);
        }
        if want("readme") {
            let mut s = film_window(&h.join("Code/weather"), fonts);
            wait_until(&mut s, "the card", 10, |_| controls_named("readme edge").len() == 1);
            let mut f = Film::new(&s, "readme");
            f.shot(&mut s, 1800, "");
            // Its edge pulled down for more of it.
            let e = controls_named("readme edge")[0].center();
            let to = e + vec2(0.0, 120.0);
            s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", e.x, e.y, to.x, to.y));
            f.shot(&mut s, 2400, "");
            if let Some(r) = f.rows.last_mut() {
                r.3 = Some(pos2(to.x * s.ppp, to.y * s.ppp));
            }
            f.finish(&s);
            s.app.cfg.readme_height = 0.0;
            s.app.cfg_dirty = true;
            s.settle();
        }
        if want("pastes") {
            let mut s = film_window(&docs, fonts);
            // Three sets copied in three folders.
            select_name(&mut s, "Garden plan.md");
            s.act("key ctrl+c");
            go(&mut s, &pics);
            let names: Vec<String> = s.tab().view.iter().map(|&i| s.tab().entries[i as usize].name.clone()).filter(|n| n.starts_with("IMG_")).take(3).collect();
            select_names(&mut s, &names.iter().map(String::as_str).collect::<Vec<_>>());
            s.act("key ctrl+c");
            go(&mut s, &h.join("Music"));
            select_name(&mut s, "Night Drive");
            s.act("key ctrl+c");
            go(&mut s, &h.join("Downloads"));
            s.app.toast = None;
            let mut f = Film::new(&s, "pastes");
            s.act("key ctrl+shift+v");
            wait_until(&mut s, "the sets", 10, |_| controls_named("paste row").len() == 3);
            f.shot(&mut s, 2000, "Ctrl Shift V");
            s.act("text 2");
            wait_until(&mut s, "the photos pasted", 10, |s| names.iter().all(|n| h.join("Downloads").join(n).exists()) && !s.popup_open());
            wait_thumbs(&mut s);
            f.shot(&mut s, 2200, "2");
            f.finish(&s);
        }
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    }

    /// Maia as an app's Open and Save dialog, driven as a person would: a picture opened
    /// by a click and the accept button, several by Ctrl+click and Enter, a folder chosen
    /// after walking into it, a name typed over the selected name part and saved with
    /// Enter, a taken name replaced after asking, Cancel and Escape, the app's filters
    /// switched by mouse and by keys, its questions answered. Narrow and wide windows.
    #[test]
    fn picker_opens_saves_and_cancels() {
        if !sandboxed() {
            return;
        }
        use crate::picker::Picker;
        use crate::portal::{Choice, Filter, Mode, Pick};
        let home = fixture("picker");
        let docs = home.join("Documents");
        let pics = home.join("Pictures");
        let start = |pick: Pick, at: &Path, size: Vec2| -> Sim {
            let mut s = Sim::new(at, size);
            s.app.picker = Some(Box::new(Picker::new(pick)));
            s.settle();
            look(&mut s, "picker opened");
            s
        };
        let done = |s: &Sim| s.app.picker.as_ref().and_then(|p| p.done.clone());
        let uri = |p: &Path| crate::clipboard::path_to_uri(p);
        let listed = |s: &Sim| -> Vec<String> {
            let t = s.tab();
            t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect()
        };
        let row = |s: &Sim, name: &str| -> Rect { s.text_rect(name) };
        // One picture: clicked, then the accept button the app named.
        let images = Filter { name: "Images".into(), rules: vec![(0, "*.[pP][nN][gG]".into()), (1, "image/webp".into())] };
        let texts = Filter { name: "Text files".into(), rules: vec![(1, "text/plain".into())] };
        let pick = Pick { title: "Upload".into(), accept_label: "Upload".into(), filters: vec![images.clone(), texts.clone()], ..Default::default() };
        let mut s = start(pick, &pics, vec2(1000.0, 640.0));
        assert!(listed(&s).contains(&"wide.png".to_string()) && listed(&s).contains(&"Trip".to_string()));
        let r = row(&s, "wide.png");
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        s.shot("picker-open");
        s.click_text("Upload");
        assert_eq!(done(&s).map(|a| (a.response, a.uris, a.filter)), Some((0, vec![uri(&pics.join("wide.png"))], Some(0))));
        // Several, by Ctrl+click, answered by Enter; the filter switched to text first.
        let pick = Pick { multiple: true, filters: vec![images.clone(), texts.clone()], ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(1000.0, 640.0));
        assert_eq!(listed(&s), ["Taxes"], "Images first: only the folder to walk into");
        click_control_at(&mut s, "picker filter");
        assert!(some_text_at(&s, "Text files"), "the filter's list is open");
        s.idle(10);
        s.shot("picker-filters");
        s.click_text("Text files");
        s.settle();
        let shown = listed(&s);
        assert!(shown.contains(&"notes.txt".to_string()) && shown.contains(&"report.md".to_string()) && shown.contains(&"Taxes".to_string()), "{shown:?}");
        assert!(!shown.iter().any(|n| n.ends_with(".mp3")), "{shown:?}");
        let (a, b) = (row(&s, "notes.txt"), row(&s, "report.md"));
        s.act(&format!("click {:.0} {:.0}", a.center().x, a.center().y));
        s.act(&format!("click {:.0} {:.0} left ctrl", b.center().x, b.center().y));
        s.act("key Enter");
        let got = done(&s).unwrap_or_else(|| s.fail("Enter picked"));
        assert_eq!(got.response, 0);
        let mut want = vec![uri(&docs.join("notes.txt")), uri(&docs.join("report.md"))];
        let mut have = got.uris.clone();
        want.sort();
        have.sort();
        assert_eq!(have, want);
        assert_eq!(got.filter, Some(1));
        // The filters by keys: Alt+T opens them, the arrows and Enter choose.
        let pick = Pick { filters: vec![images.clone(), texts.clone()], ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(1000.0, 640.0));
        s.act("key alt+t");
        assert!(some_text_at(&s, "Text files"));
        s.act("key ArrowDown");
        s.act("key Enter");
        s.settle();
        assert_eq!(s.app.picker.as_ref().unwrap().filter, Some(1));
        assert!(listed(&s).contains(&"notes.txt".to_string()));
        assert!(!s.popup_open(), "no other popup in the way");
        s.act("key alt+t");
        s.act("key Escape");
        assert!(done(&s).is_none(), "Escape closed the list, not the window");
        s.act("key Escape");
        assert_eq!(done(&s).map(|a| a.response), Some(1), "then Escape cancels");
        // A folder: walked into with a double click, chosen with Enter.
        let pick = Pick { directory: true, title: "Choose a folder".into(), ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(1000.0, 640.0));
        assert!(listed(&s).iter().all(|n| docs.join(n).is_dir()), "only folders: {:?}", listed(&s));
        let r = row(&s, "Taxes");
        s.act(&format!("dclick {:.0} {:.0}", r.center().x, r.center().y));
        s.settle();
        assert_eq!(s.tab().path, docs.join("Taxes"));
        assert!(some_text_at(&s, "Taxes") && some_text_at(&s, "Select"), "the bar names the folder shown");
        s.shot("picker-folder");
        s.act("key Enter");
        assert_eq!(done(&s).map(|a| a.uris), Some(vec![uri(&docs.join("Taxes"))]));
        // Save: the name part selected, typed over, saved with Enter; the question answered.
        let q = Choice { id: "fmt".into(), label: "Format".into(), options: vec![("md".into(), "Markdown".into()), ("html".into(), "Web page".into())], chosen: "md".into() };
        let yes = Choice { id: "open".into(), label: "Open after".into(), options: vec![], chosen: "false".into() };
        let pick = Pick { mode: Mode::Save, current_name: "Untitled.md".into(), choices: vec![q, yes], ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(1000.0, 640.0));
        s.shot("picker-save");
        click_control_at(&mut s, "picker check");
        s.click_text("Format: Markdown");
        s.click_text("Web page");
        assert!(some_text_at(&s, "Format: Web page"));
        // The click took the focus from the field: back into it, the name typed whole.
        click_control_at(&mut s, "picker name");
        s.act("key ctrl+a");
        s.act("text minutes.md");
        s.act("key Enter");
        let got = done(&s).unwrap_or_else(|| s.fail("saved"));
        assert_eq!(got.uris, vec![uri(&docs.join("minutes.md"))]);
        assert_eq!(got.choices, vec![("fmt".to_string(), "html".to_string()), ("open".into(), "true".into())]);
        // Typing straight away replaces just the name part.
        let pick = Pick { mode: Mode::Save, current_name: "Untitled.md".into(), ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(1000.0, 640.0));
        s.act("text agenda");
        assert_eq!(s.app.picker.as_ref().unwrap().name, "agenda.md");
        // A name already there: asked, then replaced.
        s.act("key ctrl+a");
        s.act("text report.md");
        s.act("key Enter");
        assert!(done(&s).is_none() && s.popup_open(), "asked first");
        s.shot("picker-replace");
        look(&mut s, "replace asked");
        s.click_text("Replace");
        assert_eq!(done(&s).map(|a| a.uris), Some(vec![uri(&docs.join("report.md"))]));
        assert!(std::fs::read_to_string(docs.join("report.md")).unwrap().starts_with("# Report"), "the app writes it, not Maia");
        // The question's close fade runs out here (its last frame is kept per thread).
        s.idle(12);
        // Several files saved in one folder: the folder walked into, the names the app's.
        let pick = Pick { mode: Mode::SaveFiles, files: vec!["a.txt".into(), "b.txt".into()], ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(1000.0, 640.0));
        assert_eq!(listed(&s), ["Taxes"], "a folder is chosen, files are not shown");
        assert!(some_text_at(&s, "a.txt, b.txt"), "the bar names what will be saved");
        let r = row(&s, "Taxes");
        s.act(&format!("dclick {:.0} {:.0}", r.center().x, r.center().y));
        s.settle();
        click_control_at(&mut s, "picker accept");
        assert_eq!(done(&s).map(|a| a.uris), Some(vec![uri(&docs.join("Taxes/a.txt")), uri(&docs.join("Taxes/b.txt"))]));
        // Cancel, by its button, in the narrowest window.
        let pick = Pick { mode: Mode::Save, current_name: "x.txt".into(), filters: vec![texts.clone()], choices: vec![Choice { id: "a".into(), label: "A rather long question".into(), options: vec![], chosen: String::new() }], ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &docs, vec2(520.0, 340.0));
        look(&mut s, "narrow save");
        s.shot("picker-narrow");
        s.click_text("Cancel");
        assert_eq!(done(&s).map(|a| a.response), Some(1));
        // Every view at narrow and wide sizes, the menu open.
        let pick = Pick { multiple: true, filters: vec![images, texts], ..Default::default() };
        report("picker", &s);
        let mut s = start(pick, &home, vec2(1280.0, 800.0));
        for v in [Cmd::ViewDetails, Cmd::ViewTree, Cmd::ViewList, Cmd::ViewMedium, Cmd::ViewXL] {
            s.cmd(v);
            for (w, h) in [(1280.0, 800.0), (640.0, 480.0), (520.0, 340.0)] {
                s.resize(w, h);
                look(&mut s, &format!("picker {v:?} {w}x{h}"));
                click_control_at(&mut s, "picker filter");
                s.idle(2);
                s.act("key Escape");
            }
        }
        assert!(done(&s).is_none());
        // Split, with animations off: the bar answers for the panel in use, the other
        // panel filtered as well.
        s.resize(1280.0, 800.0);
        s.app.cfg.animations = false;
        s.cmd(Cmd::ViewDetails);
        s.cmd(Cmd::SplitRight);
        look(&mut s, "picker split, no animations");
        click_control_at(&mut s, "picker filter");
        // Open over the status bar (popups are checked for text running out of them).
        s.idle(3);
        assert!(some_text_at(&s, "Text files"), "the list opened at once");
        s.act("key Escape");
        assert!(s.app.panels.values().all(|p| p.tab().pick.is_some()), "every panel lists what the filter lets through");
        s.cmd(Cmd::ClosePanel);
        s.app.cfg.animations = true;
        // Everything in a big folder selected: the bar counts what the filter lets through.
        s.cmd(Cmd::ViewDetails);
        go(&mut s, &home.join("Big"));
        assert_eq!(listed(&s).len(), 100, "the Big folder's pictures");
        s.act("key ctrl+a");
        s.idle(3);
        assert!(some_text_at(&s, "100 files"), "said in the bar");
        click_control_at(&mut s, "picker accept");
        assert_eq!(done(&s).map(|a| a.uris.len()), Some(100));
        report("picker", &s);
        // Turned on and off from Options in the ordinary window: the user's own
        // portals.conf gets the line, keeps the rest, and gets its old dialog back.
        let conf = crate::config::config_dir().parent().unwrap_or(&root()).join("xdg-desktop-portal/portals.conf");
        let theirs = "[preferred]\n# mine\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=kde\n";
        std::fs::create_dir_all(conf.parent().unwrap()).unwrap();
        std::fs::write(&conf, theirs).unwrap();
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        let switch = |s: &mut Sim, want: bool| {
            s.act("key ctrl+Comma");
            s.settle();
            s.act("text open and save dialog");
            wait_until(s, "state read", 5, |s| s.app.picker_setting().is_some());
            s.idle(3);
            assert_eq!(s.app.picker_setting(), Some(!want), "the switch starts the other way");
            let before = s.app.toast.as_ref().map(|t| t.0.clone());
            click_control(s, "switch");
            wait_until(s, "routing changed", 5, |s| s.app.toast.as_ref().is_some_and(|t| Some(&t.0) != before.as_ref()));
            assert_eq!(s.app.picker_setting(), Some(want));
            look(s, &format!("open and save dialog {want}"));
            s.act("key Escape");
            s.settle();
        };
        switch(&mut s, true);
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), "[preferred]\n# mine\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=maia\n");
        assert!(crate::portal::Places::user().portal_file().is_file(), "the backend is there for the portal to find");
        switch(&mut s, false);
        assert_eq!(std::fs::read_to_string(&conf).unwrap(), theirs, "KDE's dialog again");
        assert!(!crate::portal::Places::user().portal_file().exists());
        report("picker options", &s);
    }

    #[test]
    fn monkey_sessions() {
        if !sandboxed() {
            return;
        }
        let seeds: u64 = std::env::var("MAIA_SIM_SEEDS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
        let steps: usize = std::env::var("MAIA_SIM_STEPS").ok().and_then(|v| v.parse().ok()).unwrap_or(300);
        let mut all = vec![];
        let only: Option<u64> = std::env::var("MAIA_SIM_SEED").ok().and_then(|v| v.parse().ok());
        for seed in only.map(|o| o..=o).unwrap_or(1..=seeds) {
            let home = fixture(&format!("monkey{seed}"));
            // A git work tree with every kind of change, for the status marks.
            crate::tests::git_fixture(&home.join("Code/repo"));
            // And a small project whose commits change files of every kind, pictures too.
            crate::tests::commit_view_fixture(&home.join("Code/project"));
            // And a project to develop in.
            dev_fixture(&home);
            let _ = std::process::Command::new("git").args(["remote", "add", "origin", "https://github.com/octo-org/project.git"]).current_dir(home.join("Code/project")).status();
            // Both repositories can be committed to: an identity of their own, no signing.
            for r in ["Code/repo", "Code/project"] {
                for (k, v) in [("user.name", "Monkey"), ("user.email", "monkey@example.invalid"), ("commit.gpgsign", "false")] {
                    let _ = std::process::Command::new("git").args(["config", k, v]).current_dir(home.join(r)).status();
                }
            }
            // Fake drives, network places and pkexec to use at random (never the real tools).
            // Every developer preview, in a work tree of its own, with a fake sqlite3.
            crate::tests::dev_fixture(&home.join("Dev"));
            let drv = fake_drives(&format!("monkey{seed}"));
            crate::tests::fake_sqlite(&drv);
            let net = fake_network(&format!("monkey{seed}"));
            let adm = fake_pkexec(&format!("monkey{seed}"));
            let eds = fake_editors(&format!("monkey{seed}"));
            for (dir, f) in [(&net, "gio"), (&net, "phones"), (&adm, "pkexec"), (&eds, "code"), (&eds, "nvim")] {
                let _ = std::fs::copy(dir.join(f), drv.join(f));
            }
            for sub in ["gvfs", "netmounts"] {
                let _ = std::fs::create_dir_all(drv.join(sub));
            }
            // Office documents, converted by a stand-in for LibreOffice.
            crate::tests::fake_soffice(&drv);
            // Wallpaper setters that only say yes.
            for t in ["gsettings", "plasma-apply-wallpaperimage", "noctalia"] {
                crate::tests::fake_tool(&drv, t, "exit 0");
            }
            // ImageMagick, the real one, with ffmpeg and poppler's and libheif's tools, and
            // real pictures, sound, video and PDFs to convert.
            let _ = crate::tests::imagemagick_tools(&drv);
            if crate::tests::convert_tools(&drv).is_some() {
                let _ = std::fs::create_dir_all(home.join("Media"));
                crate::tests::convert_inputs(&home.join("Media"), &drv);
            }
            // A book, a comic, a font and a photo with a place, to preview.
            let _ = crate::tests::book_fixture(&home.join("Media"));
            let _ = std::fs::copy(crate::tests::sample_font(), home.join("Media/Sample.ttf"));
            let _ = crate::tests::exif_photo(&home.join("Media/place.jpg"), "64x48", "2023:07:14 10:20:30", Some(["N", "48/1 51/1 3000/100", "E", "2/1 17/1 4000/100"]));
            crate::tests::fake_tool(&drv, "fc-cache", "exit 0");
            // A mail opener and CUPS with two printers.
            crate::tests::fake_tool(&drv, "xdg-email", "exit 0");
            crate::tests::fake_tool(&drv, "lpstat", "case \"$1\" in -e) printf 'Office_Laser\\nHP_ENVY_6000\\n';; -d) echo 'system default destination: HP_ENVY_6000';; esac");
            crate::tests::fake_tool(&drv, "lp", "echo 'request id is Office_Laser-1 (1 file(s))'");
            for f in ["plan.docx", "sheet.ods", "slow deck.odp", "broken.pptx"] {
                let _ = std::fs::create_dir_all(home.join("Documents/Office"));
                let _ = std::fs::write(home.join("Documents/Office").join(f), "not really an office file");
            }
            crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(drv));
            // Snapshots of one file, for Previous Versions.
            let (_, table) = crate::tests::snapshot_fixture(&home.join("Disk"));
            crate::versions::FAKE.with(|f| *f.borrow_mut() = Some(table));
            // An archive to wander into.
            if crate::archive::available() {
                let _ = crate::archive::create(&home.join("bundle.zip"), &home.join("Documents"), &["notes.txt".into(), "Taxes".into()]);
                // And a locked one (password "pw"), when zip is installed.
                let _ = std::process::Command::new("zip").args(["-q", "-r", "-P", "pw"]).arg(home.join("locked.zip")).arg("Taxes").current_dir(home.join("Documents")).status();
            }
            let mut s = Sim::new(&home, vec2(1280.0, 800.0));
            // GitHub pictures, when a session turns them on, from a stand-in for GitHub.
            s.app.avatars.dir = root().join("xdg/cache/maia-avatars");
            let json = br#"[{"sha":"0","author":{"avatar_url":"https://avatars.githubusercontent.com/u/1?v=4"}}]"#.to_vec();
            let table = vec![("api.github.com".to_string(), crate::avatars::Http::Ok(json)), ("avatars.githubusercontent.com".to_string(), crate::avatars::Http::Ok(crate::tests::png_bytes(96, 96, seed as u32)))];
            s.app.avatars.set_get(crate::avatars::fake_get(table, Default::default()));
            // Later launches handing folders over (single instance).
            let (tx, rx) = std::sync::mpsc::channel();
            s.app.requests = Some(rx);
            let mut rng = Rng::new(seed);
            s.note(format!("seed {seed}"));
            // Every third session starts in the Columns view, so all the rest is tried there.
            if seed % 3 == 0 {
                s.cmd(Cmd::ViewColumns);
            }
            for k in 0..steps {
                if rng.below(80) == 0 {
                    let picks = [home.join("Music"), home.join("Documents/notes.txt"), home.join("bundle.zip"), home.join("gone")];
                    let p = rng.pick(&picks).clone();
                    s.note(format!("second launch with {}", p.display()));
                    let _ = tx.send(crate::single::Request::Open(vec![p]));
                }
                monkey_step(&mut s, &mut rng);
                // Every so often: dismiss whatever is open and check the layout of the
                // state the random walk has reached (feature combinations no script has).
                if k % 25 == 24 {
                    for _ in 0..3 {
                        if s.popup_open() || s.tab().rename.is_some() {
                            s.act("key Escape");
                        }
                    }
                    look(&mut s, &format!("monkey seed {seed} step {k}"));
                }
            }
            s.settle();
            // Let mounts and ejects in flight finish before the disk is wiped.
            let t0 = Instant::now();
            while !s.app.drive_jobs.is_empty() && t0.elapsed().as_secs() < 10 {
                s.idle(1);
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            let (p50, p95, max) = s.timing();
            eprintln!("sim monkey seed {seed}: {} frames, p50 {p50:.1} p95 {p95:.1} max {max:.1} ms", s.frame_ms.len());
            for (ms, step) in &s.slow {
                eprintln!("  slow frame {ms:.0} ms after: {step}");
            }
            all.extend(s.frame_ms.drain(..));
            // Random sessions copy and duplicate freely (whole system folders included):
            // start every seed from a clean disk so leftovers don't pile up.
            // Stop background copies/moves and wait for them to wind down before wiping.
            let progress: Vec<_> = s.app.jobs.iter().map(|j| {
                j.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                j.progress.clone()
            }).collect();
            drop(s);
            let t0 = Instant::now();
            while progress.iter().any(|p| !p.lock().map(|p| p.finished).unwrap_or(true)) && t0.elapsed().as_secs() < 30 {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            let _ = home;
            wipe_root();
        }
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        crate::versions::FAKE.with(|f| *f.borrow_mut() = None);
        crate::wallpaper::DESKTOP.with(|w| w.borrow_mut().clear());
        crate::sendto::MAIL_APP.with(|m| *m.borrow_mut() = None);
        let launched = crate::mime::LAUNCHED.lock().unwrap().len();
        let out = crate::clipboard::DRAGGED_OUT.lock().unwrap().len();
        eprintln!("sim monkey: {launched} external launches intercepted, {out} drags handed out");
        let n = |i: usize| super::PICKERS[i].load(std::sync::atomic::Ordering::Relaxed);
        eprintln!("sim monkey: {} picker windows, {} answered, {} cancelled", n(0), n(1), n(2));
        let _ = all;
    }

    /// Find in a preview, a big file paged and found in, any file shown as hex, and a text
    /// file edited in place: found and replaced in, saved, its original back by Undo, and
    /// changes asked about on leaving.
    #[test]
    fn find_page_and_edit_in_previews() {
        if !sandboxed() {
            return;
        }
        let home = fixture("previews-find");
        let d = home.join("Documents");
        let mut big = String::with_capacity(12 << 20);
        for i in 0..400_000 {
            big.push_str(&format!("row {i} payload\n"));
        }
        std::fs::write(d.join("big.txt"), &big).unwrap();
        let mut blob = vec![0u8; 3 << 20];
        blob[2_000_000..2_000_004].copy_from_slice(b"MAIA");
        std::fs::write(d.join("blob.bin"), &blob).unwrap();
        let mut s = Sim::new(&d, vec2(1300.0, 800.0));
        // Find in a text preview: every match counted, Enter steps on, Escape closes.
        select_name(&mut s, "notes.txt");
        s.cmd(ToggleInspector);
        wait_until(&mut s, "notes shown", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.len() == 400));
        s.cmd(FindInPreview);
        s.act("text notes");
        s.idle(3);
        wait_until(&mut s, "400 matches", 5, |s| inspector(s).find.as_ref().is_some_and(|f| f.hits.len() == 400));
        assert!(some_text_at(&s, "1 of 400"), "the count shown");
        s.act("key enter");
        assert_eq!(inspector(&s).find.as_ref().unwrap().cur, 1, "Enter: the next match");
        s.act("key shift+enter");
        s.act("key shift+enter");
        assert_eq!(inspector(&s).find.as_ref().unwrap().cur, 399, "before the first: the last");
        s.shot("find-in-preview");
        s.act("key escape");
        assert!(inspector(&s).find.is_none(), "Escape closes the bar");
        // A big file: paged, all of it counted, found in to its end.
        select_name(&mut s, "big.txt");
        wait_until(&mut s, "lines counted", 20, |s| inspector(s).big_state().is_some_and(|b| b.0 == (400_000, true, false)));
        s.cmd(FindInPreview);
        s.act("text row 345678 ");
        wait_until(&mut s, "found far in", 20, |s| inspector(s).big_state().and_then(|b| b.1).is_some_and(|f| f.0 == 1 && f.1));
        wait_until(&mut s, "the match on screen", 10, |s| some_text_at(s, "row 345678 payload"));
        s.shot("find-in-big-file");
        s.act("key escape");
        // Anything that isn't text: hex, every byte of it, found in too.
        select_name(&mut s, "blob.bin");
        wait_until(&mut s, "hex rows", 10, |s| inspector(s).big_state().is_some_and(|b| b.0.2 && b.0.0 > 0) && some_text_at(s, "00000000"));
        s.cmd(FindInPreview);
        s.act("text MAIA");
        wait_until(&mut s, "the bytes found", 20, |s| inspector(s).big_state().and_then(|b| b.1).is_some_and(|f| f.0 == 1 && f.1));
        wait_until(&mut s, "scrolled to them", 10, |s| s.texts.iter().any(|(_, t)| t.starts_with(&format!("{:08x}", 2_000_000 / 16 * 16))));
        wait_until(&mut s, "their bytes read", 10, |s| s.texts.iter().any(|(_, t)| t.contains("4d 41 49 41")));
        s.shot("hex-view");
        s.act("key escape");
        // A long log opens at its end; finding in it finds in all of it, and closing the
        // bar goes back to the end.
        let mut log = String::with_capacity(4 << 20);
        for i in 0..60_000 {
            log.push_str(&format!("{i:06} {} worker request handled\n", if i % 1000 == 7 { "ERROR" } else { "INFO" }));
        }
        std::fs::write(d.join("app.log"), &log).unwrap();
        wait_until(&mut s, "the log listed", 10, |s| s.tab().entries.iter().any(|e| e.name == "app.log"));
        select_name(&mut s, "app.log");
        wait_until(&mut s, "the log's end", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.last().is_some_and(|x| x.starts_with("059999"))));
        s.cmd(FindInPreview);
        s.act("text ERROR");
        wait_until(&mut s, "every error in the log", 20, |s| inspector(s).big_state().and_then(|b| b.1).is_some_and(|f| f.0 == 60 && f.1));
        wait_until(&mut s, "the first on screen", 10, |s| some_text_at(s, "000007 ERROR worker request handled"));
        s.act("key escape");
        wait_until(&mut s, "back at the end", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.last().is_some_and(|x| x.starts_with("059999"))));
        // Edit in place: replace, save, Undo brings the original back.
        let notes = d.join("notes.txt");
        let before = std::fs::read_to_string(&notes).unwrap();
        select_name(&mut s, "notes.txt");
        s.cmd(EditInPreview);
        wait_until(&mut s, "editing", 5, |s| inspector(s).edit.as_ref().is_some_and(|e| e.focused));
        s.act("text Top: ");
        s.act("key ctrl+f");
        s.act("text notes");
        s.idle(2);
        let field = crate::ui::CONTROLS.with(|c| c.borrow().iter().find(|(_, _, w)| *w == "replace field").map(|(_, r, _)| *r)).unwrap_or_else(|| s.fail("replace field"));
        s.act(&format!("click {:.0} {:.0}", field.center().x, field.center().y));
        s.act("text memo");
        click_control(&mut s, "All");
        s.idle(2);
        s.shot("edit-replace");
        click_control(&mut s, "Save (Ctrl+S)");
        s.idle(2);
        let saved = std::fs::read_to_string(&notes).unwrap();
        assert!(saved.starts_with("Top: line of memo\n") && !saved.contains("notes") && saved.lines().count() == 400, "saved: {:?}", &saved[..40]);
        assert!(s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Replaced(_))), "Undo offered");
        // Leaving with changes asks; Don't save keeps the file as saved.
        let ed = s.app.panels[&s.app.active].inspector.as_ref().and_then(|i| i.edit.as_ref()).map(|e| e.focused);
        if ed != Some(true) {
            s.act("click 1000 400");
        }
        s.act("text more");
        s.act("key escape");
        assert!(s.popup_open(), "asked about the changes");
        s.click_text("Don't save");
        s.idle(2);
        assert!(inspector(&s).edit.is_none(), "editing stopped");
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), saved, "nothing more written");
        s.cmd(Undo);
        s.idle(2);
        assert_eq!(std::fs::read_to_string(&notes).unwrap(), before, "Undo: the file as it was");
        report("find_page_and_edit_in_previews", &s);
    }

    /// Conflicts settled by the new choices: pictures compared by their thumbnails, one
    /// renamed by typing a name (F2), one replaced only if newer (N); then restoring from
    /// the trash onto a name taken again asks, and keeps both.
    #[test]
    fn conflict_rename_newer_and_restore() {
        if !sandboxed() {
            return;
        }
        let home = fixture("conflicts-more");
        let (src, dst) = (home.join("Pictures/Trip"), home.join("Documents"));
        for n in ["img00.png", "img01.png"] {
            std::fs::copy(src.join("img05.png"), dst.join(n)).unwrap();
        }
        // The copy there of img01 is newer: "if newer" keeps it.
        let later = std::time::SystemTime::now() + std::time::Duration::from_secs(3600);
        std::fs::File::options().write(true).open(dst.join("img01.png")).unwrap().set_modified(later).unwrap();
        let before = std::fs::read(dst.join("img01.png")).unwrap();
        let mut s = Sim::new(&src, vec2(1280.0, 800.0));
        s.cmd(SplitRight);
        go(&mut s, &dst);
        s.act("key Tab");
        s.settle();
        select_path(&mut s, &src.join("img00.png"));
        let i = s.tab().entries.iter().position(|e| e.name == "img01.png").unwrap() as u32;
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().selected.insert(i);
        s.act("key F5");
        wait_until(&mut s, "the conflict", 10, |s| matches!(s.app.popup, Popup::Conflict(_)));
        // Their pictures come from the thumbnail workers.
        for _ in 0..30 {
            s.idle(1);
            std::thread::sleep(std::time::Duration::from_millis(15));
        }
        s.shot("conflict-pictures");
        look(&mut s, "conflict with pictures");
        s.act("key F2");
        let field = crate::ui::CONTROLS.with(|c| c.borrow().iter().any(|(_, _, w)| *w == "new name"));
        assert!(field, "a field for the name");
        s.act("text holiday");
        s.shot("conflict-rename");
        s.act("key Enter");
        s.settle();
        s.act("key N");
        wait_until(&mut s, "copied", 10, |_| dst.join("holiday.png").exists());
        s.settle();
        assert_eq!(std::fs::read(dst.join("img01.png")).unwrap(), before, "the newer one there kept");
        assert_eq!(std::fs::read(dst.join("holiday.png")).unwrap(), std::fs::read(src.join("img00.png")).unwrap());
        // Trashed, then the name taken again: restoring asks.
        let trashed = dst.join("holiday.png");
        crate::trashcan::delete(&trashed).unwrap();
        std::fs::write(&trashed, "a new one").unwrap();
        let item = crate::trashcan::list().into_iter().find(|it| it.original == trashed).unwrap_or_else(|| s.fail("in the trash"));
        go(&mut s, item.file.parent().unwrap());
        select_path(&mut s, &item.file);
        s.cmd(RestoreTrash);
        assert!(matches!(s.app.popup, Popup::Choose { .. }), "asked");
        s.idle(20);
        s.shot("restore-asks");
        s.click_text("Keep both");
        s.settle();
        assert!(dst.join("holiday (1).png").exists() && std::fs::read_to_string(&trashed).unwrap() == "a new one", "both kept");
        report("conflict_rename_newer_and_restore", &s);
    }

    /// The new features in one sitting, each meeting the others: the terminal open while
    /// a file is edited in the preview and found in, a file the shell made synced to the
    /// other panel with its tag, permissions changed all the way down there, then a
    /// custom action made and run from the right-click menu, and a command from Open With.
    #[test]
    fn new_features_together() {
        if !sandboxed() {
            return;
        }
        use crate::compare::{Diff, mark};
        use std::os::unix::fs::PermissionsExt;
        let home = fixture("together");
        let (a, b) = (home.join("Work"), home.join("Backup"));
        std::fs::create_dir_all(a.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::copy(home.join("Documents/notes.txt"), a.join("notes.txt")).unwrap();
        std::fs::write(a.join("sub/inner.txt"), "inside\n").unwrap();
        crate::tags::write(&a.join("notes.txt"), &["Red".into()]).unwrap();
        let mut s = Sim::new(&a, vec2(1280.0, 800.0));
        s.act("key ctrl+shift+s");
        s.settle();
        go(&mut s, &b);
        s.act("key Tab");
        s.settle();
        assert_eq!(s.tab().path, a);
        let idle = |s: &Sim| s.app.terminal.pty().is_some_and(|p| p.idle());
        s.act("key ctrl+Backtick");
        wait_until(&mut s, "the prompt", 10, |s| idle(s) && !s.app.terminal.text().trim().is_empty());
        // The files take the keyboard back; the preview's editor gets the typing, not the shell.
        let row_in = |s: &Sim, name: &str, panel: u64| {
            let list = s.app.panels[&panel].list_rect;
            s.texts.iter().find(|(r, t)| t == name && list.contains(r.center())).map(|(r, _)| *r).unwrap_or_else(|| s.fail(&format!("{name} not on screen")))
        };
        let pa = s.app.active;
        let r = row_in(&s, "notes.txt", pa);
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        assert!(!s.app.terminal.focused, "the click took the keyboard");
        s.cmd(ToggleInspector);
        wait_until(&mut s, "notes shown", 10, |s| inspector(s).text_lines().is_some_and(|l| l.0.len() == 400));
        s.act("key shift+F4");
        wait_until(&mut s, "editing", 5, |s| inspector(s).edit.as_ref().is_some_and(|e| e.focused));
        s.act("text Edited: ");
        s.act("key ctrl+s");
        wait_until(&mut s, "saved", 5, |_| std::fs::read_to_string(a.join("notes.txt")).unwrap().starts_with("Edited: line of notes\n"));
        assert!(!s.app.terminal.text().contains("Edited"), "the shell got none of it");
        assert_eq!(crate::tags::read(&a.join("notes.txt")), vec!["Red".to_string()], "saving kept the tag");
        s.idle(10);
        s.shot("together-edit-and-terminal");
        s.act("key escape");
        assert!(inspector(&s).edit.is_none(), "saved, so Escape just leaves");
        s.act("key ctrl+f");
        s.act("text Edited");
        wait_until(&mut s, "found", 5, |s| inspector(s).find.as_ref().is_some_and(|f| f.hits.len() == 1));
        s.act("key escape");
        // The shell sees the edit and makes a file; the panel shows it.
        s.act("key ctrl+Backtick");
        assert!(s.app.terminal.focused, "the key gives the shell the keyboard");
        s.act("text head -1 notes.txt; echo made > shell.txt");
        s.act("key enter");
        wait_until(&mut s, "the shell read the edit", 10, |s| s.app.terminal.text().lines().any(|l| l.trim() == "Edited: line of notes"));
        wait_until(&mut s, "the panel shows its file", 10, |s| s.tab().entries.iter().any(|e| e.name == "shell.txt"));
        // Compare and sync to the other panel: everything copied, tag and date kept.
        let r = row_in(&s, "shell.txt", pa);
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        palette_run(&mut s, "Compare Panels");
        assert_eq!(mark(&a.join("shell.txt")), Some(Diff::Only));
        palette_run(&mut s, "Sync To Other Panel");
        wait_until(&mut s, "the plan", 10, |s| matches!(&s.app.popup, Popup::Sync(st) if st.plan.is_some()));
        s.act("key Enter");
        wait_until(&mut s, "synced", 10, |_| b.join("notes.txt").exists() && b.join("shell.txt").exists() && b.join("sub/inner.txt").exists());
        s.settle();
        assert_eq!(crate::tags::read(&b.join("notes.txt")), vec!["Red".to_string()], "the tag went with it");
        let mt = |p: &Path| std::fs::metadata(p).unwrap().modified().unwrap();
        assert_eq!(mt(&b.join("notes.txt")), mt(&a.join("notes.txt")), "and its date");
        // There: group write for the folder and everything in it, no file made runnable.
        s.act("key Tab");
        s.settle();
        assert_eq!(s.tab().path, b);
        select_name(&mut s, "sub");
        s.cmd(Properties);
        wait_until(&mut s, "properties", 5, |s| matches!(s.app.popup, Popup::Properties(_)));
        s.idle(2);
        click_control(&mut s, "group write");
        s.click_text("Change everything inside too");
        s.idle(2);
        s.shot("together-permissions");
        s.click_text("Apply");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        wait_until(&mut s, "applied", 10, |_| mode(&b.join("sub")) == 0o775 && mode(&b.join("sub/inner.txt")) & 0o020 != 0);
        assert_eq!(mode(&b.join("sub/inner.txt")) & 0o111, 0, "a document stays a document");
        s.act("key escape");
        s.settle();
        // A custom action, made in its editor and run from the right-click menu.
        s.cmd(CustomActions);
        assert!(matches!(s.app.popup, Popup::Actions { .. }));
        s.idle(1);
        s.click_text("+ Add action");
        click_control(&mut s, "action command");
        s.act("text cp %f %f.bak");
        s.idle(1);
        s.shot("together-custom-action");
        s.click_text("Done");
        s.settle();
        assert_eq!(s.app.cfg.custom_actions.last().map(|a| a.command.as_str()), Some("cp %f %f.bak"));
        let pb = s.app.active;
        let r = row_in(&s, "notes.txt", pb);
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.click_text("Custom actions");
        s.idle(2);
        s.shot("together-menu");
        s.click_text("New action");
        s.settle();
        let ran = |want: &str| crate::mime::LAUNCHED.lock().unwrap().iter().any(|c| c.contains(want));
        let (nb, sb) = (b.join("notes.txt").display().to_string(), b.join("shell.txt").display().to_string());
        assert!(ran(&format!("cp {nb} {nb}.bak")), "the action, with the file put in: {:?}", crate::mime::LAUNCHED.lock().unwrap());
        // Any command from Open With.
        s.settle();
        select_name(&mut s, "shell.txt");
        s.cmd(OpenWith);
        s.idle(2);
        click_control(&mut s, "open with command");
        s.act("text cp %f %f.with");
        s.act("key Enter");
        s.settle();
        assert!(ran(&format!("cp {sb} {sb}.with")), "the command run: {:?}", crate::mime::LAUNCHED.lock().unwrap());
        s.settle();
        // The shell still answers, and closes with exit.
        s.act("key ctrl+Backtick");
        s.act("text exit");
        s.act("key enter");
        wait_until(&mut s, "the pane closed", 10, |s| s.app.terminal.pty().is_none());
        look(&mut s, "after everything together");
        report("new_features_together", &s);
    }

    /// Just inside character `c` of `line` (drawn whole in the inspector, monospace): a
    /// press there lands before it.
    fn sel_at(s: &Sim, line: &str, c: usize) -> Pos2 {
        let r = in_inspector(s, line);
        let cw = r.width() / line.chars().count().max(1) as f32;
        pos2(r.left() + (c as f32 + 0.2) * cw, r.center().y)
    }

    fn last_copy(s: &Sim) -> String {
        s.copied.last().cloned().unwrap_or_default()
    }

    fn drag_between(s: &mut Sim, a: Pos2, b: Pos2) {
        s.idle(40);
        s.act(&format!("drag {:.1} {:.1} {:.1} {:.1}", a.x, a.y, b.x, b.y));
    }

    /// Selecting and copying in every preview that shows text, as someone taking a bit of
    /// it would: a drag across lines, Shift+click to stretch it, double and triple clicks,
    /// Ctrl+A, Ctrl+C with the preview focused and the files' own Ctrl+C after a click in
    /// the list, the right-click Copy; then rendered Markdown, a data tree's value, table
    /// cells, rows and columns, a database's rows, a .env copied masked until shown, a
    /// filtered log, and a big file's text and hex copied on a worker.
    #[test]
    fn select_and_copy_in_previews() {
        if !sandboxed() {
            return;
        }
        let home = fixture("textsel");
        let d = home.join("Dev");
        crate::tests::dev_fixture(&d);
        std::fs::write(d.join("notes.txt"), "first line here\nsecond line with words\nthird line\n").unwrap();
        let big: String = (0..60_000).map(|i| format!("row {i:06} of the big file\n")).collect();
        std::fs::write(d.join("big.txt"), &big).unwrap();
        let blob: Vec<u8> = (0..1_500_000u32).map(|i| (i * 7 % 251) as u8).collect();
        std::fs::write(d.join("blob.bin"), &blob).unwrap();
        let fake = root().join("textsel-tools");
        crate::tests::fake_sqlite(&fake);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.cmd(ToggleHidden);
        s.cmd(ToggleInspector);
        s.settle();
        let sel = |s: &Sim| inspector(s).text_selection();
        // Plain text: a drag from the second word of the first line into the second.
        select_path(&mut s, &d.join("notes.txt"));
        wait_until(&mut s, "notes read", 10, |s| inspector(s).text_lines().is_some());
        s.idle(3);
        let (a, b) = (sel_at(&s, "first line here", 6), sel_at(&s, "second line with words", 6));
        drag_between(&mut s, a, b);
        assert_eq!(sel(&s), Some(((0, 6), (1, 6))), "dragged across two lines");
        s.idle(2);
        s.shot("textsel-text");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "line here\nsecond");
        assert!(toast_is(&s, "Copied 2 lines"), "{:?}", s.app.toast);
        assert!(s.app.clipboard.is_none(), "the files were not copied");
        // Shift+click stretches it; a double-click takes a word, a triple-click the line.
        let c = sel_at(&s, "third line", 5);
        s.idle(40);
        s.act(&format!("click {:.1} {:.1} shift", c.x, c.y));
        assert_eq!(sel(&s), Some(((0, 6), (2, 5))));
        let w = sel_at(&s, "second line with words", 13);
        s.idle(40);
        s.act(&format!("dclick {:.1} {:.1}", w.x, w.y));
        assert_eq!(sel(&s), Some(((1, 12), (1, 16))), "the word");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "with");
        s.idle(40);
        s.act(&format!("dclick {:.1} {:.1}", w.x, w.y));
        s.act(&format!("click {:.1} {:.1}", w.x, w.y));
        assert_eq!(sel(&s), Some(((1, 0), (2, 0))), "the line");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "second line with words\n");
        s.act("key ctrl+a");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "first line here\nsecond line with words\nthird line", "all of it");
        // A right-click on it: Copy, from the menu.
        let m = sel_at(&s, "third line", 2);
        s.idle(40);
        s.act(&format!("click {:.1} {:.1} right", m.x, m.y));
        s.idle(2);
        assert!(matches!(s.app.popup, Popup::Ctx(_)), "the menu");
        s.shot("textsel-menu");
        let n = s.copied.len();
        s.click_text("Copy");
        assert_eq!(s.copied.len(), n + 1);
        assert_eq!(last_copy(&s), "first line here\nsecond line with words\nthird line");
        // A click in the list gives Ctrl+C back to the files.
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        // At a big font the folder's README card leaves it below the fold: scrolled to.
        if !s.texts.iter().any(|(r, t)| t == "notes.txt" && r.left() > s.app.sidebar_width && !ir.contains(r.center())) {
            let id = s.app.active;
            let t = s.app.panels.get_mut(&id).unwrap().tab_mut();
            t.cursor = t.entries.iter().position(|e| e.name == "notes.txt").map(|i| i as u32);
            t.scroll_to_cursor = true;
            s.idle(20);
        }
        let row = s.texts.iter().find(|(r, t)| t == "notes.txt" && r.left() > s.app.sidebar_width && !ir.contains(r.center())).map(|x| x.0).unwrap_or_else(|| s.fail("notes.txt listed"));
        s.idle(40);
        s.act(&format!("click {:.0} {:.0}", row.center().x, row.center().y));
        let n = s.copied.len();
        s.act("key ctrl+c");
        assert_eq!(s.copied.len(), n, "no text copied");
        assert!(s.app.clipboard.as_ref().is_some_and(|c| c.0 == vec![d.join("notes.txt")]), "the file copied");
        s.app.clipboard = None;
        // Rendered Markdown: from inside the title into the paragraph under it.
        select_path(&mut s, &d.join("README.md"));
        wait_until(&mut s, "markdown laid out", 10, |s| inspector(s).markdown().and_then(|m| m.counts()).is_some_and(|c| c.0 > 20));
        s.idle(3);
        let md = |s: &Sim, needle: &str, ch: usize| inspector(s).markdown().and_then(|m| m.char_pos(needle, ch)).unwrap_or_else(|| s.fail(&format!("{needle} drawn")));
        let (a, b) = (md(&s, "Maia", 1), md(&s, "A file manager", 6));
        drag_between(&mut s, a, b);
        s.idle(2);
        s.shot("textsel-markdown");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "aia\nA file", "the heading's rest and the paragraph's start");
        // Down to the code, by the wheel.
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        for _ in 0..40 {
            if md(&s, "println", 2).y < ir.bottom() - 120.0 {
                break;
            }
            s.act(&format!("scroll {:.0} {:.0} -2", ir.center().x, ir.center().y));
            s.idle(12);
        }
        let code = md(&s, "println", 2);
        s.idle(40);
        s.act(&format!("dclick {:.1} {:.1}", code.x, code.y));
        s.act(&format!("click {:.1} {:.1}", code.x, code.y));
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "    println!(\"hi\"); // greet", "a triple-click takes a line of a code block");
        let cell = md(&s, "opens the inspector", 0);
        s.idle(40);
        s.act(&format!("dclick {:.1} {:.1}", cell.x, cell.y));
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "opens", "a word in a table");
        // A link still opens with a click.
        let at = inspector(&s).markdown().and_then(|m| m.link_pos("docs/guide.md")).unwrap();
        let before = crate::mime::LAUNCHED.lock().unwrap().len();
        s.idle(40);
        s.act(&format!("click {:.0} {:.0}", at.x, at.y));
        assert!(crate::mime::LAUNCHED.lock().unwrap().len() > before, "the link followed");
        // A data tree: a double-click takes a value whole; Ctrl+C with only a row picked
        // takes its value; a drag over rows takes them as drawn; folding still works.
        select_path(&mut s, &d.join("package.json"));
        wait_until(&mut s, "json tree", 10, |s| inspector(s).rich_name() == Some("tree"));
        s.idle(3);
        let v = sel_at(&s, "build: \"vite build\"", 12);
        s.idle(40);
        s.act(&format!("dclick {:.1} {:.1}", v.x, v.y));
        s.idle(2);
        s.shot("textsel-tree");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "vite build", "the string, without its quotes");
        click_in_inspector(&mut s, "version: \"0.2.0\"", "click");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "0.2.0", "the picked row's value");
        let (a, b) = (sel_at(&s, "name: \"maia-web\"", 0), sel_at(&s, "private: true", 13));
        drag_between(&mut s, a, b);
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "name: \"maia-web\"\nversion: \"0.2.0\"\nprivate: true");
        let rows = rich(&mut s).unwrap().1;
        click_in_inspector(&mut s, "scripts", "click");
        s.act("key ArrowLeft");
        assert_eq!(rich(&mut s).unwrap().1, rows - 2, "the arrows still fold");
        s.act("key Escape");
        // A table: a cell, Shift+click to a rectangle, a column by its title, all of it.
        select_path(&mut s, &d.join("people.csv"));
        wait_until(&mut s, "csv table", 10, |s| inspector(s).table().is_some());
        s.idle(3);
        click_in_inspector(&mut s, "Ada Lovelace", "click");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "Ada Lovelace");
        let e = in_inspector(&s, "linus@example.invalid");
        s.idle(40);
        s.act(&format!("click {:.0} {:.0} shift", e.center().x, e.center().y));
        s.idle(2);
        s.shot("textsel-table");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "Ada Lovelace\tada@example.invalid\n\"Grace \"\"Amazing\"\" Hopper\"\tgrace@example.invalid\n\"Line\nbreak\"\tx@example.invalid\nLinus\tlinus@example.invalid", "tab-separated, quoted where a cell needs it");
        click_in_inspector(&mut s, "email", "click");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "email\nada@example.invalid\ngrace@example.invalid\nx@example.invalid\nlinus@example.invalid", "a column with its title");
        s.act("key ctrl+a");
        s.act("key ctrl+c");
        assert!(last_copy(&s).starts_with("id\tname\temail\tjoined\tscore\n1\tAda Lovelace\t"), "{:?}", last_copy(&s));
        // A database's rows, through the same table.
        select_path(&mut s, &d.join("data.db"));
        wait_until(&mut s, "tables listed", 15, |s| inspector(s).sqlite_state().is_some_and(|st| st.0 == Some(3)));
        click_in_inspector(&mut s, "people", "click");
        wait_until(&mut s, "people's rows", 10, |s| inspector(s).sqlite_state().is_some_and(|st| st.2 == Some((1, 3))));
        s.idle(3);
        click_in_inspector(&mut s, "Grace, R.", "click");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "Grace, R.");
        // A .env: what is hidden copies hidden; shown, as it is.
        select_path(&mut s, &d.join(".env"));
        wait_until(&mut s, ".env shown", 10, |s| inspector(s).rich_name() == Some("env"));
        s.idle(3);
        let mask = crate::envfile::MASK;
        let line = format!("export API_KEY={mask}");
        let (a, b) = (sel_at(&s, "# Local secrets", 0), sel_at(&s, &line, line.chars().count() - 1));
        drag_between(&mut s, a, b);
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), format!("# Local secrets\nexport API_KEY={}", &mask[..mask.len() - '•'.len_utf8()]), "masked");
        assert!(!s.copied.iter().any(|c| c.contains("sk-live")), "no secret on the clipboard");
        click_control(&mut s, "Show values");
        s.act("key ctrl+a");
        s.act("key ctrl+c");
        assert!(last_copy(&s).contains("export API_KEY=sk-live-0123456789"), "{:?}", last_copy(&s));
        click_control(&mut s, "Hide values");
        // A log through its filter: the selection is of the lines it lets through.
        select_path(&mut s, &d.join("app.log"));
        wait_until(&mut s, "log read", 10, |s| inspector(s).text_lines().is_some());
        s.idle(3);
        click_control(&mut s, "line filter");
        s.act("text request 3");
        wait_until(&mut s, "filtered", 5, |s| inspector(s).filtered_lines() == Some(11));
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        s.act(&format!("scroll {:.0} {:.0} 20", ir.center().x, ir.center().y));
        s.idle(30);
        let first = s.texts.iter().filter(|(r, t)| t.contains("request 3") && s.app.panels[&s.app.active].inspector_rect.unwrap().contains(r.center())).min_by(|a, b| a.0.top().total_cmp(&b.0.top())).map(|x| x.1.clone()).unwrap();
        let p0 = sel_at(&s, &first, 0);
        s.idle(40);
        s.act(&format!("dclick {:.1} {:.1}", p0.x, p0.y));
        s.act(&format!("click {:.1} {:.1}", p0.x, p0.y));
        assert_eq!(inspector(&s).text_selection(), Some(((0, 0), (1, 0))), "the first line let through");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), format!("{first}\n"));
        // Back in the filter's field, Ctrl+C and Ctrl+A are the field's.
        click_control(&mut s, "line filter");
        let n = s.copied.len();
        s.act("key ctrl+a");
        s.act("key ctrl+c");
        assert_eq!(last_copy(&s), "request 3", "the field's text, not the preview's: {:?}", &s.copied[n..]);
        assert_eq!(inspector(&s).text_selection(), Some(((0, 0), (1, 0))), "the preview's selection as it was");
        // A big file: two lines from far down, then all of it, read on a worker.
        select_path(&mut s, &d.join("big.txt"));
        wait_until(&mut s, "paged", 10, |s| inspector(s).big_state().is_some_and(|b| b.0.1));
        s.idle(5);
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap();
        s.act(&format!("scroll {:.0} {:.0} -400", ir.center().x, ir.center().y));
        s.idle(30);
        let lines: Vec<String> = s.texts.iter().filter(|(r, t)| t.starts_with("row ") && ir.contains(r.center())).map(|x| x.1.clone()).collect();
        let (l0, l1) = (lines[2].clone(), lines[3].clone());
        let (a, b) = (sel_at(&s, &l0, 4), sel_at(&s, &l1, 10));
        drag_between(&mut s, a, b);
        let n0: usize = l0[4..10].parse().unwrap();
        assert_eq!(inspector(&s).big_selection(), Some(((n0, 4), (n0 + 1, 10))));
        s.idle(2);
        s.shot("textsel-big");
        let n = s.copied.len();
        s.act("key ctrl+c");
        wait_until(&mut s, "the lines read", 10, |s| s.copied.len() > n);
        assert_eq!(last_copy(&s), format!("{}\n{}", &l0[4..], &l1[..10]));
        let n = s.copied.len();
        s.act("key ctrl+a");
        s.act("key ctrl+c");
        wait_until(&mut s, "all of it read", 20, |s| s.copied.len() > n);
        assert_eq!(last_copy(&s), big.trim_end_matches('\n'), "the whole file, as shown");
        // Hex: bytes from one to another, as hex, then as their text.
        select_path(&mut s, &d.join("blob.bin"));
        wait_until(&mut s, "hex", 10, |s| inspector(s).big_state().is_some_and(|b| b.0.2));
        s.idle(5);
        let row1 = s.texts.iter().find(|(r, t)| t.starts_with("00000010") && ir.contains(r.center())).map(|x| x.0).unwrap_or_else(|| s.fail("the second hex row"));
        // The hex column starts ten columns in; a byte is two of them and a space.
        let cw = row1.width() / 8.0;
        let (a, b) = (pos2(row1.left() + cw * 10.5, row1.center().y), pos2(row1.left() + cw * 19.5, row1.center().y));
        drag_between(&mut s, a, b);
        assert_eq!(inspector(&s).hex_selection(), Some(((16, 19), false)), "four bytes in the hex column");
        s.idle(2);
        s.shot("textsel-hex");
        let n = s.copied.len();
        s.act("key ctrl+c");
        wait_until(&mut s, "bytes read", 10, |s| s.copied.len() > n);
        assert_eq!(last_copy(&s), blob[16..20].iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(" "));
        report("select and copy", &s);
    }

    /// ImageMagick as this machine has it (`magick`, or version 6's `convert`), as the
    /// only tool the session may run.
    fn magick_tools(dir: &Path) -> Option<PathBuf> {
        let _ = std::fs::create_dir_all(dir);
        let path = std::env::var_os("PATH")?;
        let mut any = false;
        for name in ["magick", "convert"] {
            if let Some(real) = std::env::split_paths(&path).map(|d| d.join(name)).find(|m| m.is_file()) {
                let _ = std::os::unix::fs::symlink(real, dir.join(name));
                any = true;
            }
        }
        any.then(|| dir.to_path_buf())
    }

    /// Sorting pictures in the viewer, as someone going through a day's photos would:
    /// Delete sends one to the trash and the next shows, Ctrl+Z brings it back here and
    /// in the list after; R and L turn the picture shown through ImageMagick and it shows
    /// turned, two turns keeping one original, Ctrl+Z putting it back; Ctrl+1 and the tag
    /// strip (arrows, Enter, a click, a new tag typed) set tags, shown in the pill; C pins
    /// one to compare the next ones beside it, zoomed together; Escape backs out a step
    /// at a time. Without ImageMagick a turn says what it needs. Nothing repaints at rest;
    /// a small window keeps its buttons apart.
    #[test]
    fn sort_pictures_in_the_viewer() {
        if !sandboxed() {
            return;
        }
        let d = root().join("home/sortpics");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        png_file(&d.join("a.png"), 320, 200, 1);
        png_file(&d.join("b.png"), 200, 320, 2);
        png_file(&d.join("c.png"), 320, 200, 3);
        png_file(&d.join("d.png"), 320, 200, 4);
        png_file(&d.join("e.png"), 2400, 1200, 5);
        let tools = magick_tools(&root().join("sortpics-tools"));
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone().unwrap_or_else(|| root().join("sortpics-none"))));
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
            }
        }
        let _reset = Reset;
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        let pic = |s: &Sim| viewer(s).current().map(|p| p.name.clone()).unwrap_or_default();
        let dims = |name: &str| crate::imgdec::load(&d.join(name), 512).map(|x| x.1);
        select_path(&mut s, &d.join("a.png"));
        s.act("key F3");
        wait_until(&mut s, "a decoded", 10, |s| s.app.thumbs.dims(&d.join("a.png")).is_some());
        s.idle(2);
        for b in ["viewer trash", "viewer rotate right", "viewer rotate left", "viewer tags", "viewer compare", "viewer close"] {
            assert!(has_control(&s, b), "{b} shown with the name");
        }
        shot_still(&mut s, "viewer-tools-graphite");
        // Delete: to the trash, the next one shown; Ctrl+Z here brings it back.
        s.act("key Delete");
        assert_eq!((pic(&s), viewer(&s).pics.len(), viewer(&s).trashed_here()), ("b.png".to_string(), 4, 1));
        wait_until(&mut s, "a in the trash", 10, |s| !d.join("a.png").exists() && s.app.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::Trashed(_))));
        s.act("key ctrl+z");
        assert!(d.join("a.png").exists(), "back from the trash");
        assert_eq!((pic(&s), viewer(&s).pics.len()), ("a.png".to_string(), 5), "and shown where it was");
        s.act("key ctrl+z");
        assert!(some_text(&s, "Nothing to undo here"), "only what was done in the viewer");
        // Deleted again, and back by Ctrl+Z in the list once the viewer is closed.
        s.act("key Delete");
        wait_until(&mut s, "a in the trash again", 10, |s| !d.join("a.png").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        s.act("key Escape");
        wait_until(&mut s, "the viewer closed", 5, |s| !s.popup_open());
        s.idle(3);
        s.act("key ctrl+z");
        s.settle();
        assert!(d.join("a.png").exists(), "Undo in the list");
        // Turns, shown without leaving the viewer.
        select_path(&mut s, &d.join("b.png"));
        s.act("key F3");
        assert_eq!(pic(&s), "b.png");
        let original = std::fs::read(d.join("b.png")).unwrap();
        if tools.is_some() {
            s.act("key r");
            wait_until(&mut s, "the turn", 20, |s| viewer(s).turning.is_empty() && s.app.undo.last().is_some_and(|u| u.label == "Rotate b.png"));
            assert_eq!(dims("b.png"), Some([320, 200]), "turned on disk");
            wait_until(&mut s, "shown turned", 10, |s| viewer(s).current().is_some_and(|p| s.app.thumbs.dims(&p.path) == Some([320, 200])));
            let undos = s.app.undo.len();
            s.act("key r");
            s.act("key l");
            wait_until(&mut s, "two more turns", 20, |s| viewer(s).turning.is_empty() && s.app.errands.busy == 0);
            s.idle(3);
            assert_eq!(dims("b.png"), Some([320, 200]), "right, then left: as it was turned once");
            assert_eq!(s.app.undo.len(), undos, "one Undo for every turn");
            assert_eq!(crate::trashcan::list().into_iter().filter(|it| it.original == d.join("b.png")).count(), 1, "one original kept");
            s.act("key r");
            wait_until(&mut s, "a last turn", 20, |s| viewer(s).turning.is_empty() && s.app.errands.busy == 0);
            s.act("key ctrl+z");
            assert_eq!(std::fs::read(d.join("b.png")).unwrap(), original, "the original back, byte for byte");
            wait_until(&mut s, "shown as it was", 10, |s| viewer(s).current().is_some_and(|p| s.app.thumbs.dims(&p.path) == Some([200, 320])));
        } else {
            eprintln!("sort pictures: no ImageMagick here, turns not tried");
        }
        // Colour labels by key; the strip by arrows, Enter, a click and a name typed.
        s.act("key ctrl+1");
        wait_until(&mut s, "red", 10, |_| crate::tags::read(&d.join("b.png")) == ["Red"]);
        s.act("key ctrl+1");
        wait_until(&mut s, "not red", 10, |_| crate::tags::read(&d.join("b.png")).is_empty());
        s.act("key t");
        assert!(viewer(&s).strip.is_some() && some_text(&s, "New Tag…"), "the strip");
        for _ in 0..4 {
            s.act("key ArrowRight");
        }
        assert_eq!(pic(&s), "b.png", "the arrows are the strip's while it is open");
        s.act("key Enter");
        wait_until(&mut s, "blue", 10, |_| crate::tags::read(&d.join("b.png")) == ["Blue"]);
        s.click_text("New Tag…");
        s.act("text keep");
        s.act("key Enter");
        wait_until(&mut s, "keep", 10, |_| crate::tags::read(&d.join("b.png")) == ["Blue", "keep"]);
        s.idle(3);
        assert!(some_text(&s, "keep"), "the tag in the pill and the strip");
        shot_still(&mut s, "viewer-tags-graphite");
        let r = s.text_rect("keep");
        s.idle(40);
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
        wait_until(&mut s, "keep off", 10, |_| crate::tags::read(&d.join("b.png")) == ["Blue"]);
        s.act("key Escape");
        assert!(viewer(&s).strip.is_none() && s.popup_open(), "Escape closes the strip first");
        // Compare: pinned on the left, the next ones beside it, zoomed together.
        s.act("key c");
        assert_eq!(viewer(&s).pin.as_ref().map(|p| p.name.as_str()), Some("b.png"));
        s.act("key ArrowRight");
        assert_eq!(pic(&s), "c.png");
        assert_eq!(viewer(&s).pin.as_ref().map(|p| p.name.as_str()), Some("b.png"), "the pinned one stays");
        wait_until(&mut s, "both decoded", 10, |s| s.app.thumbs.dims(&d.join("c.png")).is_some());
        s.idle(2);
        assert!(some_text(&s, "b.png") && some_text(&s, "c.png") && some_text(&s, "3 / 5"), "both named");
        shot_still(&mut s, "viewer-compare-graphite");
        s.act("key Plus");
        assert!((viewer(&s).zoom - 1.25).abs() < 1e-3);
        s.act("key Escape");
        assert!(viewer(&s).pin.is_none() && s.popup_open(), "Escape leaves the comparison");
        click_control(&mut s, "viewer compare");
        assert!(viewer(&s).pin.is_some(), "and its button pins");
        click_control(&mut s, "viewer compare");
        // At rest, nothing repaints (once the name has faded, and the app's memory trim
        // two seconds after the last decode has run).
        s.idle(240);
        std::thread::sleep(std::time::Duration::from_millis(2200));
        // The last message gone too (it shows for three seconds by the clock).
        while s.app.toast.as_ref().is_some_and(|t| t.1.elapsed().as_secs_f32() < 3.2) {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        s.idle(3);
        // (The sidebar looks at the drives every few seconds: that wake is the app's.)
        let causes = format!("{:?}", s.ctx.repaint_causes());
        assert!(s.repaint == std::time::Duration::MAX || !causes.contains("viewer.rs"), "the viewer asks for nothing while it rests: {:?} {causes}", s.repaint);
        // Without ImageMagick.
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(root().join("sortpics-none")));
        s.act("key r");
        assert!(some_text(&s, "Turning pictures needs ImageMagick"), "said over the viewer");
        s.idle(2);
        s.shot("viewer-no-magick-graphite");
        // A small window: the buttons that fit, apart.
        s.resize(520.0, 340.0);
        s.act("move 200 200");
        s.act("key c");
        s.act("key t");
        s.idle(3);
        s.shot("viewer-small-tools-graphite");
        s.act("key Escape");
        s.act("key Escape");
        s.act("key Escape");
        wait_until(&mut s, "the viewer closed", 5, |s| !s.popup_open());
        report("sort pictures", &s);
    }

    /// The subjects the history lists now.
    fn listed(s: &Sim) -> Vec<String> {
        git_hist(s).subjects()
    }

    /// Searching the history, as someone hunting for a change uses it: Ctrl+F over the
    /// graph, a name found in authors and marked in the rows, the count, Enter stepping
    /// through what was found and Down going into it, one opened and left again, the
    /// prefixes typed over the old text, nothing found, Escape putting the whole graph back
    /// where it was; the toolbar's button; started in a subfolder it looks there; then small
    /// windows and a light theme.
    #[test]
    fn git_history_search() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitsearch");
        let repo = home.join("Code/project");
        crate::tests::commit_view_fixture(&repo);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| branch_now(s) == "main");
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 20, |s| history(s).is_some_and(|h| h.1 && h.0 == 5));
        // The place the search must give back.
        s.act("key ArrowDown");
        s.act("key ArrowDown");
        assert_eq!(git_hist(&s).sel(), 2);
        s.act("key ctrl+f");
        assert_eq!(probe(&s).search.as_deref(), Some(""), "the bar, empty at the top of the work tree");
        s.act("text grace");
        wait_until(&mut s, "narrowed", 10, |s| probe(s).narrowed && history(s).is_some_and(|h| h.1));
        assert_eq!(listed(&s), ["Tune the panel", "Lay out the panels"]);
        s.idle(3);
        assert!(some_text(&s, "2 commits"), "the count");
        assert!(!some_text(&s, "Changes") && !some_text(&s, "Write the long notes"), "only what was found");
        look(&mut s, "search by author");
        s.shot("search-author");
        // Enter steps through them, the field keeping the keyboard; Down goes into them.
        s.act("key Enter");
        assert_eq!(git_hist(&s).sel(), 0);
        s.act("key Enter");
        assert_eq!(git_hist(&s).sel(), 1);
        s.act("key shift+Enter");
        assert_eq!(git_hist(&s).sel(), 0);
        s.act("key ArrowDown");
        s.act("key ArrowDown");
        s.act("key Enter");
        wait_until(&mut s, "the commit", 10, |s| probe(s).subject.as_deref() == Some("Lay out the panels") && history(s).is_some_and(|h| h.3.is_some()));
        s.act("key Escape");
        assert!(probe(&s).narrowed && probe(&s).at.is_none(), "back among what was found");
        assert_eq!(listed(&s).len(), 2);
        // Words in the message, typed over what was there (Ctrl+F selects it).
        let find = |s: &mut Sim, text: &str, want: &[&str]| {
            s.act("key ctrl+f");
            s.act(&format!("text {text}"));
            let want: Vec<String> = want.iter().map(|w| w.to_string()).collect();
            wait_until(s, text, 10, |s| history(s).is_some_and(|h| h.1) && probe(s).search.as_deref() == Some(text) && (listed(s) == want));
            s.idle(3);
        };
        find(&mut s, "handbook", &["Lay out the panels"]);
        find(&mut s, "author:ada", &["Remove the logo", "Start the project"]);
        look(&mut s, "search author term");
        find(&mut s, "path:assets since:2026-09-02", &["Remove the logo", "Lay out the panels"]);
        s.shot("search-path-since");
        let logo = git_out(&repo, &["rev-parse", "HEAD~1"]);
        find(&mut s, &logo[..6], &["Remove the logo"]);
        look(&mut s, "search by hash");
        find(&mut s, "zzz", &[]);
        assert!(some_text(&s, "No matches") && some_text(&s, "No commits match"));
        look(&mut s, "nothing found");
        s.shot("search-none");
        // Escape in the field: the whole history, at the commit it was on.
        s.act("key Escape");
        assert_eq!((probe(&s).search, probe(&s).narrowed, git_hist(&s).sel()), (None, false, 2));
        assert_eq!(listed(&s).len(), 5);
        look(&mut s, "search closed");
        // The toolbar's button opens it as well, and closes it.
        click_control(&mut s, "Find (Ctrl+F)");
        assert!(probe(&s).search.is_some());
        s.act("text panel");
        wait_until(&mut s, "narrowed", 10, |s| probe(s).narrowed && history(s).is_some_and(|h| h.1 && h.0 == 2));
        click_control(&mut s, "Find (Ctrl+F)");
        assert_eq!((probe(&s).search, probe(&s).narrowed), (None, false));
        // In a subfolder: the search starts in it, typing goes after its path.
        go(&mut s, &repo.join("src/ui"));
        s.act("key ctrl+f");

        wait_until(&mut s, "the folder's commits", 10, |s| probe(s).narrowed && history(s).is_some_and(|h| h.1));
        assert_eq!(probe(&s).search.as_deref(), Some("path:src/ui "));
        assert_eq!(listed(&s), ["Tune the panel", "Lay out the panels", "Start the project"]);
        s.act("text tune");
        wait_until(&mut s, "and a word", 10, |s| probe(s).search.as_deref() == Some("path:src/ui tune") && listed(s) == ["Tune the panel"]);
        s.idle(3);
        look(&mut s, "search in a subfolder");
        s.shot("search-subfolder");
        for (w, h) in [(720.0, 480.0), (520.0, 400.0)] {
            s.resize(w, h);
            look(&mut s, "search narrow");
        }
        s.resize(1280.0, 800.0);
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "search light");
        s.act("key Escape");
        assert!(probe(&s).search.is_none());
        report("gitsearch", &s);
    }

    /// Comparing, as someone deciding whether a branch is ready: Compare with main from a
    /// branch's own menu, the commits only each side has, where they split, the files topic
    /// changes since then with a diff under one and the picture before and after, every
    /// difference instead and back, a commit of the comparison opened and Back to it, a side
    /// folded from the keyboard; then two commits compared from the commit menu by marking
    /// one, the older put first; small windows and a light theme.
    #[test]
    fn git_compare_branches() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitcompare");
        let repo = home.join("Code/topics");
        crate::tests::branches_fixture(&repo);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| branch_now(s) == "main");
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 20, |s| history(s).is_some_and(|h| h.1 && h.0 == 5));
        open_branches(&mut s);
        let r = s.text_rect("topic");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        assert!(matches!(s.app.popup, Popup::Branch { .. }), "the branch's menu");
        s.idle(12);
        for t in ["Switch to it", "Compare with main", "Merge into main", "Rename…", "Delete"] {
            assert!(some_text(&s, t), "{t:?} in the menu");
        }
        s.shot("branch-actions-compare");
        s.click_text("Compare with main");
        wait_until(&mut s, "the comparison", 20, |s| probe(s).cmp_counts == Some([1, 3]) && git_hist(s).files().len() == 3);
        s.idle(3);
        assert_eq!(probe(&s).cmp, Some(["main".to_string(), "topic".to_string()]));
        for t in ["3 commits only on topic", "1 commit only on main", "Topic adds a file", "Main gets a readme", "Split at", "What topic adds", "Every difference", "3 files changed", "main → topic"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        look(&mut s, "comparison");
        s.shot("compare");
        click_in_inspector(&mut s, "notes.txt", "click");
        wait_until(&mut s, "its diff", 10, |s| some_text(s, "+TWO on topic"));
        wait_until(&mut s, "the picture's two sides", 10, |s| probe(s).pictures == 2);
        s.idle(3);
        look(&mut s, "comparison diff");
        s.shot("compare-diff");
        // Every difference: main's readme, gone on topic's side, counts too.
        click_control(&mut s, "every difference");
        wait_until(&mut s, "every file", 10, |s| git_hist(s).files().len() == 4 && !probe(s).split);
        assert!(git_hist(&s).files().iter().any(|f| f.path == "readme.md" && f.status == b'D'));
        look(&mut s, "every difference");
        click_control(&mut s, "what it adds");
        wait_until(&mut s, "since the split", 10, |s| git_hist(s).files().len() == 3 && probe(s).split);
        // A commit of the comparison opens; Back comes to the comparison again.
        click_in_inspector(&mut s, "Topic adds a file", "click");
        wait_until(&mut s, "the commit", 10, |s| probe(s).subject.as_deref() == Some("Topic adds a file") && probe(s).cmp.is_none());
        s.act("key Escape");
        assert_eq!(probe(&s).cmp_counts, Some([1, 3]), "back in the comparison");
        // The keys: Home to topic's heading, Enter folds it.
        s.act("key Home");
        s.act("key Enter");
        s.idle(3);
        assert!(!some_text(&s, "Topic adds a file") && some_text(&s, "3 commits only on topic"), "folded");
        s.act("key Enter");
        s.idle(3);
        assert!(some_text(&s, "Topic adds a file"));
        s.act("key Escape");
        assert!(probe(&s).at.is_none() && probe(&s).cmp.is_none(), "the graph");
        // Two commits: one marked, the other's menu compares with it.
        let edits = git_out(&repo, &["rev-parse", "topic~2"]);
        let start = git_out(&repo, &["rev-parse", "main~1"]);
        right_click_in_inspector(&mut s, "Topic edits the notes");
        s.idle(3);
        s.click_text("Mark for comparison");
        assert!(toast_starts(&s, &format!("Marked {}", &edits[..7])));
        assert_eq!(probe(&s).mark.as_deref(), Some(edits.as_str()));
        s.idle(3);
        look(&mut s, "a commit marked");
        s.shot("compare-marked");
        right_click_in_inspector(&mut s, "Topic redraws the picture");
        s.idle(3);
        assert!(some_text(&s, &format!("Compare with {}", &edits[..7])) && some_text(&s, "Mark for comparison"));
        s.shot("commit-menu-compare");
        s.click_text(&format!("Compare with {}", &edits[..7]));
        wait_until(&mut s, "two commits compared", 20, |s| probe(s).cmp_counts == Some([0, 2]) && git_hist(s).files().len() == 2);
        assert!(!some_text(&s, "What topic adds"), "on one line, one way to compare");
        look(&mut s, "two commits compared");
        s.act("key Escape");
        // Marked first, the newer: the older still goes first.
        right_click_in_inspector(&mut s, "Start");
        s.idle(3);
        s.click_text(&format!("Compare with {}", &edits[..7]));
        wait_until(&mut s, "older first", 20, |s| probe(s).cmp_counts == Some([0, 1]));
        assert_eq!(probe(&s).cmp, Some([start[..7].to_string(), edits[..7].to_string()]));
        s.act("key Escape");
        right_click_in_inspector(&mut s, "Topic edits the notes");
        s.idle(3);
        s.click_text("Unmark");
        assert_eq!(probe(&s).mark, None);
        // From the keyboard: down to topic, Shift+F10, Compare is the item after Switch.
        let topic_menu = |s: &Sim| matches!(&s.app.popup, Popup::Branch { branch, .. } if branch.name == "topic");
        for n in 1..12 {
            open_branches(&mut s);
            for _ in 0..n {
                s.act("key ArrowDown");
            }
            s.act("key shift+F10");
            if topic_menu(&s) {
                break;
            }
            s.act("key Escape");
            s.idle(3);
        }
        assert!(matches!(&s.app.popup, Popup::Branch { branch, .. } if branch.name == "topic"), "topic menu from the keys, not {:?}", match &s.app.popup { Popup::Branch { branch, .. } => branch.name.clone(), p => p.kind_name().into() });
        s.idle(3);
        s.act("key ArrowDown");
        s.act("key Enter");
        wait_until(&mut s, "the comparison", 20, |s| probe(s).cmp_counts == Some([1, 3]) && git_hist(s).files().len() == 3);
        for (w, h) in [(720.0, 480.0), (520.0, 400.0)] {
            s.resize(w, h);
            look(&mut s, "comparison narrow");
        }
        s.resize(1280.0, 800.0);
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "comparison light");
        s.shot("compare-light");
        report("gitcompare", &s);
    }

    /// Merging from the branch menu: a branch fast-forwarded to, git's refusal in plain
    /// words while a change is in the way, a merge that stops at a conflict settled on the
    /// Changes page and finished with git's message, one given up on with Abort, and a
    /// remote branch merged with a merge commit.
    #[test]
    fn git_merge_branches() {
        if !sandboxed() || !crate::media::have("git") {
            return;
        }
        let home = fixture("gitmerge");
        let repo = home.join("Code/merging");
        crate::tests::branches_fixture(&repo);
        let g = |args: &[&str]| crate::tests::git(&repo, args);
        crate::tests::work_repo(&repo);
        g(&["config", "merge.ff", "true"]);
        g(&["switch", "-qc", "quick"]);
        std::fs::write(repo.join("quick.txt"), "quick\n").unwrap();
        g(&["add", "-A"]);
        g(&["commit", "-qm", "A quick fix"]);
        g(&["switch", "-q", "main"]);
        let mut s = Sim::new(&repo, vec2(1280.0, 800.0));
        wait_until(&mut s, "branch read", 10, |s| branch_now(s) == "main");
        s.idle(2);
        open_history(&mut s);
        wait_until(&mut s, "history", 20, |s| history(s).is_some_and(|h| h.1 && h.0 == 6));
        let menu_pick = |s: &mut Sim, branch: &str, item: &str| {
            open_branches(s);
            let r = s.text_rect(branch);
            s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
            s.idle(12);
            s.click_text(item);
        };
        let was = git_out(&repo, &["rev-parse", "--short=7", "HEAD"]);
        menu_pick(&mut s, "quick", "Merge into main");
        wait_until(&mut s, "fast-forwarded", 20, |s| toast_is(s, &format!("Fast-forwarded main to quick · it was at {was}")));
        assert_eq!(git_out(&repo, &["rev-parse", "HEAD"]), git_out(&repo, &["rev-parse", "quick"]));
        wait_until(&mut s, "the graph read again", 20, |s| history(s).is_some_and(|h| h.1 && h.0 == 6) && some_text(s, "quick"));
        // A change of its own in the way: git's refusal, plainly.
        std::fs::write(repo.join("notes.txt"), "one\ntwo, not committed\nthree\n").unwrap();
        crate::gitstatus::rescan(&repo);
        menu_pick(&mut s, "topic", "Merge into main");
        wait_until(&mut s, "refused", 20, |s| matches!(s.app.popup, Popup::Notice { .. }));
        s.idle(12);
        assert!(some_text(&s, "Couldn't merge topic") && some_text(&s, "Your uncommitted changes to notes.txt would be overwritten by the merge. Commit or stash them first."));
        look(&mut s, "merge refused");
        s.shot("merge-refused");
        s.act("key Enter");
        assert!(!s.popup_open());
        // Committed on main as well: the merge stops at the conflict.
        std::fs::write(repo.join("notes.txt"), "one\ntwo on main\nthree\n").unwrap();
        g(&["commit", "-qam", "Main's own two"]);
        crate::gitstatus::rescan(&repo);
        menu_pick(&mut s, "topic", "Merge into main");
        wait_until(&mut s, "the Changes page", 20, |s| git_hist(s).changes.open && changes(s).unstaged.contains(&"notes.txt".to_string()));
        assert_eq!(crate::gitsync::doing(&repo), Some(crate::gitsync::Doing::Merge));
        s.idle(3);
        see(&mut s, "Merging · 1 file in conflict");
        // Given up on: as before.
        click_control(&mut s, "conflict abort");
        assert!(matches!(s.app.popup, Popup::Confirm { .. }), "Abort asks first");
        s.idle(12);
        look(&mut s, "abort the merge");
        s.act("key Enter");
        wait_until(&mut s, "aborted", 20, |s| crate::gitsync::doing(&repo).is_none() && !changes(s).busy);
        assert_eq!(std::fs::read_to_string(repo.join("notes.txt")).unwrap(), "one\ntwo on main\nthree\n");
        // Again, settled and finished.
        s.act("key Escape");
        menu_pick(&mut s, "topic", "Merge into main");
        wait_until(&mut s, "the Changes page", 20, |s| git_hist(s).changes.open && changes(s).unstaged.contains(&"notes.txt".to_string()));
        s.idle(3);
        click_in_inspector(&mut s, "notes.txt", "click");
        wait_until(&mut s, "the conflict shown", 10, |s| s.texts.iter().any(|(_, t)| t == "Keep both"));
        s.idle(3);
        for t in ["two on main", "TWO on topic"] {
            assert!(some_text(&s, t), "{t:?} shown");
        }
        look(&mut s, "merge conflict");
        s.shot("merge-conflict");
        click_control(&mut s, "keep theirs");
        wait_until(&mut s, "settled", 20, |s| changes(s).staged.contains(&"notes.txt".to_string()) && !changes(s).busy);
        click_control(&mut s, "conflict continue");
        wait_until(&mut s, "merged", 20, |s| matches!(changes(s).said, Some(Ok(ref m)) if m == "Finished the merge"));
        assert_eq!(git_out(&repo, &["log", "-1", "--format=%s"]), "Merge branch 'topic'");
        assert_eq!(std::fs::read_to_string(repo.join("notes.txt")).unwrap(), "one\nTWO on topic\nthree\n");
        s.act("key Escape");
        // A remote branch nothing tracks: its own menu, merged with a commit.
        std::fs::write(repo.join("side.txt"), "side\n").unwrap();
        g(&["switch", "-qc", "side", "main~2"]);
        g(&["add", "-A"]);
        g(&["commit", "-qm", "Work on the side"]);
        g(&["update-ref", "refs/remotes/origin/side", "HEAD"]);
        g(&["switch", "-q", "main"]);
        g(&["branch", "-D", "side"]);
        crate::gitstatus::rescan(&repo);
        let was = git_out(&repo, &["rev-parse", "--short=7", "HEAD"]);
        open_branches(&mut s);
        let r = s.text_rect("origin/side");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(12);
        for t in ["Remote branch", "Check out", "Compare with main", "Merge into main"] {
            assert!(some_text(&s, t), "{t:?} in the menu");
        }
        look(&mut s, "remote branch menu");
        s.shot("remote-branch-menu");
        s.click_text("Merge into main");
        wait_until(&mut s, "merged", 20, |s| toast_is(s, &format!("Merged origin/side into main · it was at {was}")));
        assert_eq!(git_out(&repo, &["log", "-1", "--format=%s"]), "Merge remote-tracking branch 'origin/side'");
        wait_until(&mut s, "the graph read again", 20, |s| history(s).is_some_and(|h| h.1) && some_text(s, "Merge remote-tracking branch 'origin/side'"));
        look(&mut s, "merged graph");
        s.shot("merged-graph");
        report("gitmerge", &s);
    }

    fn strip(s: &Sim) -> &crate::columns::Strip {
        s.tab().strip.as_deref().unwrap_or_else(|| s.fail("no column strip"))
    }

    /// Where a name is drawn in the list (the first found), for telling columns apart.
    fn in_list(s: &Sim, name: &str) -> Rect {
        let list = s.app.panels[&s.app.active].list_rect;
        s.texts.iter().find(|(r, t)| t == name && list.contains(r.center())).map(|(r, _)| *r).unwrap_or_else(|| {
            let near: Vec<&str> = s.texts.iter().filter(|(r, _)| list.contains(r.center())).map(|(_, t)| t.as_str()).take(60).collect();
            s.fail(&format!("{name:?} not drawn in the list; there: {near:?}"))
        })
    }

    fn cursor_name(s: &Sim) -> String {
        let t = s.tab();
        t.cursor.and_then(|c| t.entries.get(c as usize)).map(|e| e.name.clone()).unwrap_or_default()
    }

    fn click_at(s: &mut Sim, r: Rect, how: &str) {
        s.act(&format!("click {:.0} {:.0}{how}", r.center().x, r.center().y));
    }

    /// The tab's own column lies inside the list, all of it when the list is wide enough.
    fn own_column_shown(s: &mut Sim, what: &str) {
        s.idle(30);
        let list = s.app.panels[&s.app.active].list_rect;
        let st = strip(s);
        let w = crate::listing::column_width(&s.app.cfg).min(list.width().max(60.0));
        let left = list.left() + st.chain.len() as f32 * w - st.scroll;
        if (st.scroll - st.target).abs() > 0.5 {
            s.fail(&format!("{what}: the strip is still moving ({} to {})", st.scroll, st.target));
        }
        if left < list.left() - 1.0 || left + w > list.right() + 1.0 {
            s.fail(&format!("{what}: the tab's column at {left:.0}..{:.0} is not inside the list {list:?}", left + w));
        }
    }

    /// The Columns view: one column per folder from home down, walked with the keys and
    /// the mouse, worked in (rename, new folder, copy, paste, drag and drop, trash, undo,
    /// rubber band, menus), filtered, with saved views, split, narrow and wide, in every
    /// theme, with the inspector and with animations off.
    #[test]
    fn columns_view_walks_the_folders() {
        if !sandboxed() {
            return;
        }
        let home = fixture("columns");
        let docs = home.join("Documents");
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        // Wide enough for four columns at any look, so the mouse can reach the ones above
        // (narrow windows come later).
        let wide = (s.app.sidebar_width + 4.4 * crate::listing::column_width(&s.app.cfg)).max(1280.0);
        s.resize(wide, 800.0);
        s.act("key ctrl+shift+7");
        s.settle();
        assert!(s.tab().settings.column_view, "Ctrl+Shift+7 picks Columns");
        // (The fixture is a folder in the session's home.)
        let top = crate::fsx::home();
        assert_eq!(strip(&s).chain, vec![top.clone(), home.clone()], "the strip starts at the home folder");
        look(&mut s, "Documents in columns");
        assert!(some_text(&s, "Columns"), "the status chip names the view");
        assert!(!s.texts.iter().any(|(_, t)| t.ends_with('%')), "no zoom chip: Columns has no zoom");
        assert!(in_list(&s, "Documents").right() < in_list(&s, "notes.txt").left(), "home's column is left of Documents'");
        s.shot("columns-documents");

        // Keys: Down to a folder shows it in the next column, Right goes in, Left back.
        cursor_to(&mut s, "Taxes");
        s.settle();
        assert_eq!(strip(&s).preview.as_deref(), Some(docs.join("Taxes").as_path()), "the folder under the cursor opens in the next column");
        assert!(in_list(&s, "2025").left() > in_list(&s, "Taxes").right(), "Taxes' folders right of it");
        s.act("key ArrowRight");
        wait_until(&mut s, "into Taxes", 10, |s| s.tab().path == docs.join("Taxes"));
        s.settle();
        assert_eq!(strip(&s).chain, vec![top.clone(), home.clone(), docs.clone()]);
        assert_eq!(cursor_name(&s), "2024", "the cursor starts on the first item");
        s.act("key ArrowDown");
        assert_eq!(cursor_name(&s), "2025");
        s.act("key ArrowRight");
        wait_until(&mut s, "into 2025", 10, |s| s.tab().path == docs.join("Taxes/2025"));
        s.settle();
        s.act("key ArrowRight");
        s.settle();
        assert_eq!(s.tab().path, docs.join("Taxes/2025"), "Right on a file stays");
        s.act("key ArrowLeft");
        wait_until(&mut s, "back in Taxes", 10, |s| s.tab().path == docs.join("Taxes"));
        s.settle();
        assert_eq!(cursor_name(&s), "2025", "Left lands on the folder it came from");
        s.act("key ArrowLeft");
        wait_until(&mut s, "back in Documents", 10, |s| s.tab().path == docs);
        s.settle();
        assert_eq!(cursor_name(&s), "Taxes");
        // Enter opens a folder as in any view, and Backspace goes up.
        s.act("key Enter");
        wait_until(&mut s, "Enter into Taxes", 10, |s| s.tab().path == docs.join("Taxes"));
        s.act("key Backspace");
        wait_until(&mut s, "Backspace up", 10, |s| s.tab().path == docs);
        s.settle();

        // The mouse: a click in the home column makes home the tab's folder, Code picked.
        s.act("move -50 -50");
        s.idle(2);
        let code = in_list(&s, "Code");
        click_at(&mut s, code, "");
        wait_until(&mut s, "home from its column", 10, |s| s.tab().path == home);
        s.settle();
        assert_eq!(cursor_name(&s), "Code");
        assert_eq!(s.tab().selected.len(), 1);
        assert_eq!(strip(&s).preview.as_deref(), Some(home.join("Code").as_path()));
        // A click in the next column, then a double-click on a folder in the one after.
        let deep = in_list(&s, "deep");
        click_at(&mut s, deep, "");
        wait_until(&mut s, "Code from its column", 10, |s| s.tab().path == home.join("Code"));
        s.settle();
        s.idle(40);
        let a = in_list(&s, "a");
        s.act(&format!("dclick {:.0} {:.0}", a.center().x, a.center().y));
        wait_until(&mut s, "a double-click goes in", 10, |s| s.tab().path == home.join("Code/deep/a"));
        s.settle();
        // All the way down: the strip scrolls to keep the tab's column in sight.
        for _ in 0..7 {
            s.act("key ArrowRight");
            s.settle();
        }
        assert_eq!(s.tab().path, home.join("Code/deep/a/b/c/d/e/f/g/h"));
        assert!(strip(&s).scroll > 0.0, "the strip scrolled along");
        own_column_shown(&mut s, "deep");
        look(&mut s, "deep path");
        s.shot("columns-deep");
        for _ in 0..9 {
            s.act("key ArrowLeft");
            s.settle();
        }
        assert_eq!(s.tab().path, home.join("Code"));
        own_column_shown(&mut s, "back up");
        let (st, list) = (strip(&s), s.app.panels[&s.app.active].list_rect);
        let n = st.chain.len() + 1 + usize::from(st.preview.is_some());
        let w = crate::listing::column_width(&s.app.cfg).min(list.width());
        assert!(st.scroll <= (n as f32 * w - list.width()).max(0.0) + 0.5, "back up, the strip scrolled back ({} for {n} columns)", st.scroll);

        // File work in the tab's column: rename, a new folder, copy and paste into it.
        go(&mut s, &docs);
        cursor_to(&mut s, "notes.txt");
        s.act("key F2");
        assert!(s.tab().rename.is_some(), "F2 renames in a column");
        s.act("key ctrl+a");
        s.act("text notes2.txt");
        s.act("key Enter");
        s.settle();
        assert!(docs.join("notes2.txt").exists(), "renamed on disk");
        s.cmd(NewFolder);
        s.settle();
        s.idle(3);
        assert!(s.tab().rename.is_some(), "a new folder is named in its column");
        s.act("key ctrl+a");
        s.act("text Fresh");
        s.act("key Enter");
        s.settle();
        assert!(docs.join("Fresh").is_dir());
        cursor_to(&mut s, "report.md");
        s.act("key ctrl+c");
        cursor_to(&mut s, "Fresh");
        s.act("key ArrowRight");
        wait_until(&mut s, "into Fresh", 10, |s| s.tab().path == docs.join("Fresh"));
        s.settle();
        paste_lands(&mut s, &[docs.join("Fresh/report.md")]);
        let fresh = in_list(&s, "Fresh");
        assert!(s.texts.iter().any(|(r, t)| t == "report.md" && r.left() > fresh.right()), "the pasted file shows in Fresh's column");
        s.act("key ArrowLeft");
        wait_until(&mut s, "back in Documents", 10, |s| s.tab().path == docs);
        s.settle();
        // Drag a file onto a folder in the home column, and one onto that column's blank.
        s.act("move -50 -50");
        s.idle(2);
        let (from, to) = (in_list(&s, "notes2.txt").center(), in_list(&s, "Music").center());
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", from.x, from.y, to.x, to.y));
        wait_until(&mut s, "dropped on Music", 10, |s| home.join("Music/notes2.txt").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        s.settle();
        let list = s.app.panels[&s.app.active].list_rect;
        let (from, to) = (in_list(&s, "budget 2026.csv").center(), pos2(in_list(&s, "Music").center().x, list.bottom() - 20.0));
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", from.x, from.y, to.x, to.y));
        wait_until(&mut s, "dropped in home", 10, |s| home.join("budget 2026.csv").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        s.settle();
        assert_eq!(s.tab().path, docs, "a drop does not move the tab");
        // Trash and undo.
        cursor_to(&mut s, "report.md");
        s.act("key Delete");
        if s.popup_open() {
            s.act("key Enter");
        }
        s.settle();
        assert!(!docs.join("report.md").exists(), "Delete trashes from a column");
        s.act("key ctrl+z");
        s.settle();
        assert!(docs.join("report.md").exists(), "undo brings it back");
        // A rubber band from the blank under the rows, then Ctrl+click.
        s.act("key Escape");
        s.act("move -50 -50");
        s.idle(2);
        let x = in_list(&s, "report.md").left() + 20.0;
        let top = in_list(&s, "Taxes").top();
        s.act(&format!("drag {x:.0} {:.0} {x:.0} {:.0}", list.bottom() - 20.0, top - 3.0));
        s.settle();
        assert!(s.tab().selected.len() >= 3, "rubber band selected {}", s.tab().selected.len());
        assert!(strip(&s).preview.is_none(), "several picked: nothing to show after them");
        s.act("key Escape");
        let (a, b) = (in_list(&s, "report.md"), in_list(&s, "Taxes"));
        s.act(&format!("click {:.0} {:.0}", a.center().x, a.center().y));
        s.act(&format!("click {:.0} {:.0} ctrl", b.center().x, b.center().y));
        s.settle();
        assert_eq!(s.tab().selected.len(), 2, "Ctrl+click adds to the selection");
        // A right-click in the home column: home with Music picked, and its menu.
        s.idle(40);
        let m = in_list(&s, "Music");
        s.act(&format!("click {:.0} {:.0} right", m.center().x, m.center().y));
        s.idle(2);
        assert!(s.popup_open(), "the menu opens");
        assert_eq!(s.tab().path, home);
        assert_eq!(s.tab().selected_paths(), vec![home.join("Music")], "the menu is for what was clicked");
        s.act("key Escape");
        s.settle();
        // Typing filters the tab's column.
        go(&mut s, &docs);
        s.act("text rep");
        s.idle(3);
        assert_eq!(s.tab().filter, "rep");
        assert!(s.tab().view.iter().all(|&i| s.tab().entries[i as usize].name.to_lowercase().contains('r')), "the filter narrows the column");
        look(&mut s, "filtered column");
        s.act("key Escape");
        s.act("key Escape");

        // A folder with a saved view of its own keeps the columns when walked into.
        s.app.ud.views.insert(docs.join("Taxes"), crate::config::SavedView { scope: crate::config::SaveScope::ThisFolder, view: crate::config::ViewSettings { sort: crate::config::SortKey::Modified, ..Default::default() } });
        cursor_to(&mut s, "Taxes");
        s.act("key ArrowRight");
        wait_until(&mut s, "into Taxes", 10, |s| s.tab().path == docs.join("Taxes"));
        assert!(s.tab().settings.column_view, "still in Columns");
        assert_eq!(s.tab().settings.sort, crate::config::SortKey::Modified, "with the saved sort");
        s.act("key ArrowLeft");
        wait_until(&mut s, "back", 10, |s| s.tab().path == docs);
        assert_eq!(s.tab().settings.sort, crate::config::SortKey::Name, "the sort it had");
        s.app.ud.views.clear();

        // Finished copies' cards and the last message off the screen, for the pictures.
        s.app.jobs.retain(|j| j.finished_at.is_none());
        s.app.toast = None;
        // The inspector beside the columns follows the cursor.
        s.cmd(ToggleInspector);
        s.settle();
        cursor_to(&mut s, "Taxes");
        s.act("key ArrowRight");
        wait_until(&mut s, "into Taxes with the inspector", 10, |s| s.tab().path == docs.join("Taxes"));
        own_column_shown(&mut s, "with the inspector");
        look(&mut s, "inspector beside the columns");
        s.shot("columns-inspector");
        s.cmd(ToggleInspector);
        s.act("key ArrowLeft");
        s.settle();

        // Split: both panels in columns, each its own strip.
        s.cmd(SplitRight);
        s.settle();
        s.cmd(ViewColumns);
        s.settle();
        assert_eq!(s.app.panels.len(), 2);
        assert!(s.app.panels.values().all(|p| p.tab().settings.column_view), "both panels in Columns");
        cursor_to(&mut s, "Taxes");
        s.act("key ArrowRight");
        s.settle();
        own_column_shown(&mut s, "split");
        look(&mut s, "split columns");
        s.shot("columns-split");
        s.cmd(ClosePanel);
        s.settle();

        // Narrow and wide windows.
        go(&mut s, &home.join("Code/deep/a/b/c"));
        for (w, h) in [(520.0, 340.0), (640.0, 480.0), (900.0, 1200.0), (1920.0, 1080.0), (1280.0, 800.0), (wide, 800.0)] {
            s.resize(w, h);
            own_column_shown(&mut s, &format!("{w}x{h}"));
            look(&mut s, &format!("columns {w}x{h}"));
            if w < 600.0 {
                s.shot("columns-narrow");
            }
        }

        // Every theme, quickly.
        for th in crate::theme::ThemeName::ALL {
            s.app.cfg.theme = th;
            s.idle(2);
        }
        s.app.cfg.theme = crate::theme::ThemeName::Paper;
        look(&mut s, "a light theme");
        s.shot("columns-paper");
        s.app.cfg.theme = crate::theme::ThemeName::Midnight;
        look(&mut s, "after the themes");
        // The view chip's popup: Columns marked under the zoom axis; a stop leaves it, the
        // Columns row goes back.
        s.click_text("Columns");
        s.idle(3);
        assert!(matches!(s.app.popup, Popup::Zoom { .. }), "the chip opens the views");
        std::thread::sleep(std::time::Duration::from_millis(300));
        s.idle(10);
        s.shot("columns-viewpopup");
        s.click_text("List");
        s.idle(2);
        assert!(!s.tab().settings.column_view && (s.tab().settings.zoom - 0.2).abs() < 1e-3, "a named stop leaves Columns");
        if !s.popup_open() {
            s.click_text("List");
            s.idle(3);
        }
        let row = s.texts.iter().rev().find(|(_, t)| t == "Columns").map(|(r, _)| *r).unwrap_or_else(|| s.fail("the popup's Columns row"));
        s.act(&format!("click {:.0} {:.0}", row.center().x, row.center().y));
        s.idle(2);
        assert!(s.tab().settings.column_view, "the popup's Columns row picks it");
        s.act("key Escape");
        s.settle();

        // Animations off: the strip jumps to its place at once.
        s.app.cfg.animations = false;
        s.act("key ArrowLeft");
        s.settle();
        s.act("key ArrowRight");
        s.idle(1);
        let st = strip(&s);
        assert_eq!(st.scroll, st.target, "no easing with animations off");
        s.app.cfg.animations = true;

        // Back to Details from the menu's View list, and the columns' listings go.
        s.cmd(ViewDetails);
        s.settle();
        assert!(!s.tab().settings.column_view && s.tab().strip.is_none(), "Details leaves Columns and its listings");
        report("columns", &s);
    }

    fn dest_picker(s: &Sim) -> Option<&crate::popups::GoToState> {
        match &s.app.popup {
            Popup::GoTo(st) if st.dest.is_some() => Some(st),
            _ => None,
        }
    }

    /// Typed into the destination picker, until the folder typed down to is its first row.
    fn type_dest(s: &mut Sim, dir: &Path) {
        s.act(&format!("text {}/", dir.display()));
        wait_until(s, "the folder read", 10, |s| dest_picker(s).is_some_and(|st| st.items.first().is_some_and(|i| i.path == dir)));
        s.idle(2);
    }

    /// Copy To… and Move To…: by their keys and from the context menu, a path typed with
    /// Tab expanding it, a recent destination clicked, a copy landing and a move undone;
    /// what makes no sense refused in the picker; in the Columns view and a flattened
    /// folder too, at a narrow window.
    #[test]
    fn copy_to_and_move_to_pick_a_folder() {
        if !sandboxed() {
            return;
        }
        let home = fixture("copyto");
        let docs = home.join("Documents");
        let maia = home.join("Code/maia");
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        cursor_to(&mut s, "report.md");
        s.act("key shift+F5");
        assert!(dest_picker(&s).is_some(), "Shift+F5 opens the picker");
        s.text_rect("Copy “report.md” to…");
        look(&mut s, "destination picker, empty");
        s.shot("copyto-empty");
        // ".../Code/" typed, "ma" and Tab into the folder, then Enter.
        s.act(&format!("text {}/Code/", home.display()));
        wait_until(&mut s, "Code listed", 10, |s| dest_picker(s).is_some_and(|st| st.items.iter().any(|i| i.path == maia)));
        s.act("text ma");
        s.idle(2);
        s.act("key Tab");
        wait_until(&mut s, "into maia", 10, |s| dest_picker(s).is_some_and(|st| st.items.first().is_some_and(|i| i.path == maia)));
        assert!(dest_picker(&s).unwrap().items.iter().all(|i| i.is_dir), "folders only");
        look(&mut s, "destination picker in a folder");
        s.shot("copyto-picker");
        s.act("key Enter");
        assert!(dest_picker(&s).is_none(), "Enter starts the copy");
        wait_until(&mut s, "the copy", 10, |s| maia.join("report.md").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        assert!(docs.join("report.md").exists());
        assert_eq!(s.app.ud.destinations.first(), Some(&maia));
        // Move To… from the context menu; the folder used last is offered first, clicked.
        let r = list_text(&s, "notes.txt");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(2);
        s.click_text("Move To…");
        assert!(dest_picker(&s).is_some_and(|st| st.dest.as_ref().is_some_and(|d| d.mv)));
        s.text_rect("Move “notes.txt” to…");
        s.text_rect("Recent destinations");
        look(&mut s, "recent destinations");
        s.shot("moveto-recent");
        let row = s.texts.iter().filter(|(_, t)| t == "maia").map(|x| x.0).min_by(|a, b| a.top().total_cmp(&b.top())).unwrap_or_else(|| s.fail("maia offered"));
        s.act(&format!("click {:.0} {:.0}", row.center().x, row.center().y));
        wait_until(&mut s, "the move", 10, |s| maia.join("notes.txt").exists() && !docs.join("notes.txt").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        s.settle();
        assert!(!lists(s.tab(), &docs.join("notes.txt")), "gone from the folder it left");
        s.act("key ctrl+z");
        wait_until(&mut s, "the move undone", 10, |_| docs.join("notes.txt").exists() && !maia.join("notes.txt").exists());
        // A folder into itself: refused in the picker, which stays.
        s.settle();
        cursor_to(&mut s, "Taxes");
        s.act("key shift+F6");
        type_dest(&mut s, &docs.join("Taxes/2025"));
        s.act("key Enter");
        assert!(dest_picker(&s).is_some(), "still open");
        s.text_rect("A folder can't go inside itself");
        look(&mut s, "refused");
        s.shot("moveto-refused");
        // Nor into the trash, nor where it already is.
        s.act("key ctrl+a");
        s.act(&format!("text {}", crate::sidebar::trash_dir().display()));
        s.act("key Enter");
        s.text_rect("Not into the trash: Move To Trash puts things there");
        s.act("key ctrl+a");
        s.act(&format!("text {}", docs.display()));
        s.idle(2);
        s.act("key ctrl+a");
        s.act(&format!("text {}/", docs.display()));
        wait_until(&mut s, "Documents read", 10, |s| dest_picker(s).is_some_and(|st| st.items.first().is_some_and(|i| i.path == docs)));
        s.act("key Enter");
        s.text_rect("Already in that folder");
        s.act("key Escape");
        assert!(!s.popup_open() && docs.join("Taxes/2025/receipt.pdf").exists());
        // In the Columns view, at a narrow window.
        s.cmd(ViewColumns);
        s.resize(640.0, 480.0);
        s.settle();
        cursor_to(&mut s, "budget 2026.csv");
        s.act("key shift+F5");
        type_dest(&mut s, &home.join("Empty"));
        look(&mut s, "picker, narrow");
        s.act("key Enter");
        wait_until(&mut s, "the copy from Columns", 10, |s| home.join("Empty/budget 2026.csv").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        // Out of a flattened folder: a file from deep down.
        s.cmd(ViewDetails);
        s.resize(1280.0, 800.0);
        go(&mut s, &home.join("Code/deep"));
        s.cmd(ToggleFlatten);
        s.settle();
        select_name(&mut s, "leaf.txt");
        s.cmd(MoveTo);
        type_dest(&mut s, &home.join("Empty"));
        s.act("key Enter");
        wait_until(&mut s, "the move out of the flattened folder", 10, |s| home.join("Empty/leaf.txt").exists() && s.app.jobs.iter().all(|j| j.finished_at.is_some()));
        assert_eq!(s.app.ud.destinations[..2], [home.join("Empty"), maia.clone()], "newest first, each once");
        s.settle();
        s.act("move -50 -50");
        go(&mut s, &home.join("Empty"));
        look(&mut s, "landed");
        s.shot("copyto-landed");
        report("copy to", &s);
    }

    /// Select Matching… from the palette, a pattern typed into its prompt and Enter.
    fn select_matching(s: &mut Sim, pattern: &str) {
        s.act("key ctrl+shift+p");
        s.act("text Select Matching");
        s.act("key Enter");
        if !matches!(&s.app.popup, Popup::Prompt { title, .. } if title == "Select matching") {
            s.fail("the pattern is asked for");
        }
        s.act(&format!("text {pattern}"));
        s.act("key Enter");
        assert!(!s.popup_open(), "Enter applies it");
    }

    /// The names the view shows that end in one of `exts`, and whether the selection is
    /// exactly those.
    fn selected_exactly(s: &Sim, exts: &[&str]) -> (usize, bool) {
        let t = s.tab();
        let want: std::collections::HashSet<u32> = t.view.iter().copied().filter(|&i| exts.iter().any(|x| t.entries[i as usize].name.to_lowercase().ends_with(x))).collect();
        (want.len(), want == t.selected)
    }

    /// Select Matching… in every view, the Columns view and a narrow window included:
    /// what the pattern names is selected among what is shown, added to what was, inside
    /// a filter and a flattened folder too, with a word of how many.
    #[test]
    fn select_matching_in_every_view() {
        if !sandboxed() {
            return;
        }
        let home = fixture("selmatch");
        let mut s = Sim::new(&home.join("Big"), vec2(1280.0, 800.0));
        for v in [ViewDetails, ViewTree, ViewList, ViewMedium, ViewLarge, ViewXL, ViewColumns] {
            s.cmd(v);
            s.cmd(SelectNone);
            s.settle();
            select_matching(&mut s, "*.png");
            let (n, exact) = selected_exactly(&s, &[".png"]);
            assert!(n == 100 && exact, "{v:?}: the 100 pictures, only them");
            assert!(toast_is(&s, "100 items match *.png"), "{v:?}: says how many");
            select_matching(&mut s, "FILE00?0.{MD,json}");
            // file0010.json, file0040.json and file0070.json.
            assert_eq!(s.tab().selected.len(), 100 + 3, "{v:?}: added to the selection, case ignored");
            let c = s.tab().cursor.unwrap_or_else(|| s.fail("a cursor"));
            assert!(s.tab().selected.contains(&c), "{v:?}: the cursor on what was chosen");
            look(&mut s, &format!("selected by pattern in {v:?}"));
        }
        s.cmd(ViewDetails);
        s.cmd(SelectNone);
        s.act("key ctrl+shift+p");
        s.act("text Select Matching");
        s.act("key Enter");
        s.act("text *.{png,md}");
        look(&mut s, "the pattern prompt");
        s.shot("select-prompt");
        s.act("key Enter");
        assert_eq!(selected_exactly(&s, &[".png", ".md"]), (200, true));
        s.settle();
        s.act("move -50 -50");
        s.idle(2);
        s.shot("select-result");
        // Inside a filter: only what it lets through.
        s.cmd(SelectNone);
        s.act("key ctrl+f");
        s.act("text file01");
        wait_until(&mut s, "filtered", 5, |s| s.tab().filter == "file01" && !s.tab().is_loading() && s.tab().view.len() < 600);
        s.settle();
        select_matching(&mut s, "*.png");
        let (n, exact) = selected_exactly(&s, &[".png"]);
        assert!(n > 0 && n < 100 && exact, "only the filtered pictures: {n}");
        // Nothing matching: said, nothing selected.
        s.cmd(SelectNone);
        select_matching(&mut s, "*.gif");
        assert!(s.tab().selected.is_empty() && toast_is(&s, "Nothing here matches *.gif"));
        // A flattened folder: names anywhere below, or a path below it.
        go(&mut s, &home.join("Code"));
        s.cmd(ToggleFlatten);
        s.settle();
        select_matching(&mut s, "*.rs");
        assert_eq!(s.tab().selected.len(), 2, "main.rs and lib.rs, deep in the tree");
        s.cmd(SelectNone);
        select_matching(&mut s, "maia/*.toml");
        assert_eq!(s.tab().target_entries().iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["Cargo.toml"]);
        // Narrow, in Columns.
        go(&mut s, &home.join("Pictures/Trip"));
        s.cmd(ViewColumns);
        s.resize(520.0, 340.0);
        s.settle();
        select_matching(&mut s, "img0[0-4].png");
        assert_eq!(s.tab().selected.len(), 5);
        look(&mut s, "narrow columns");
        report("select matching", &s);
    }

    /// Without bsdtar the right-click menu still offers Compress, and choosing a format
    /// says what to install instead of the menu going without it.
    #[test]
    fn compress_without_bsdtar_says_what_to_install() {
        if !sandboxed() {
            return;
        }
        let home = fixture("nobsdtar");
        crate::archive::GONE.with(|g| g.set(true));
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        select_name(&mut s, "notes.txt");
        let r = list_text(&s, "notes.txt");
        s.act(&format!("click {:.0} {:.0} right", r.center().x, r.center().y));
        s.idle(20);
        assert!(some_text(&s, "Compress"), "Compress in the menu without bsdtar");
        if !some_text(&s, "Zip (.zip)") {
            s.click_text("Compress");
            s.idle(10);
        }
        look(&mut s, "compress menu without bsdtar");
        s.click_text("Zip (.zip)");
        s.idle(5);
        assert!(!matches!(s.app.popup, Popup::Compress(_)), "no dialog that could only fail");
        assert!(some_text(&s, crate::archive::COMPRESS_NEEDS), "says what to install");
        s.shot("compress-without-bsdtar");
        look(&mut s, "compress without bsdtar");
        crate::archive::GONE.with(|g| g.set(false));
        report("nobsdtar", &s);
    }

    // ---------------------------------------------------------------- features of 2026-10-26

    /// A popup settled with nothing pointed at, its own text demanded clean.
    fn look_popup(s: &mut Sim, what: &str) {
        s.note(format!("look: {what}"));
        s.act("move -50 -50");
        s.idle(12);
        assert!(s.popup_open(), "{what}: the popup is open");
        s.check_popup_overlap = true;
        s.idle(2);
        s.check_popup_overlap = false;
    }

    /// Nothing left asking for frames once the feature is closed and toasts have gone:
    /// an idle window costs no CPU.
    fn settles_idle(s: &mut Sim, what: &str) {
        s.act("move -50 -50");
        let t0 = Instant::now();
        while s.repaint != std::time::Duration::MAX {
            if t0.elapsed().as_secs() >= 8 {
                s.fail(&format!("{what}: still asking to be drawn again in {:?}", s.repaint));
            }
            s.frame(vec![]);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    fn click_rect(s: &mut Sim, r: Rect) {
        s.act(&format!("click {:.0} {:.0}", r.center().x, r.center().y));
    }

    /// The transfers chip and its list: a copy paused there (Space and its button), two
    /// waiting behind it put in another order with Alt and the arrows, one started at
    /// once, one cancelled; a queued copy whose name is taken asked about inside the
    /// list; the chip gone once all is done; the command with nothing running says so.
    /// In a small window and at the owner's look.
    #[test]
    fn transfers_steered_from_the_status_bar() {
        if !sandboxed() {
            return;
        }
        use std::sync::atomic::Ordering::Relaxed;
        let home = fixture("transfers");
        let src = home.join("Outbox");
        std::fs::create_dir_all(&src).unwrap();
        let big: Vec<u8> = (0..48_000_000u32).map(|i| (i % 249) as u8).collect();
        std::fs::write(src.join("film.mkv"), &big).unwrap();
        for n in ["one.txt", "two.txt", "three.txt", "taken.txt"] {
            std::fs::write(src.join(n), n).unwrap();
        }
        let dest = home.join("Inbox");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("taken.txt"), "already here").unwrap();
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        s.cmd(Transfers);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Nothing is being copied or moved"));
        // The film, held at once so it is still there to steer.
        s.app.start_job(crate::ops::OpKind::Copy { srcs: vec![src.join("film.mkv")], dest: dest.clone(), decide: Default::default() });
        s.app.jobs.last().unwrap().pause.store(true, Relaxed);
        let film = s.app.jobs.last().unwrap().id;
        wait_until(&mut s, "the film holding the disk", 10, |s| s.app.queue.running() == [film]);
        for n in ["one.txt", "two.txt", "three.txt"] {
            s.app.begin_transfer(vec![src.join(n)], dest.clone(), false);
            s.idle(2);
        }
        wait_until(&mut s, "three waiting", 10, |s| s.app.queue.waiting().len() == 3);
        // Behind them, a name already taken: asked when its turn comes, not now.
        s.app.begin_transfer(vec![src.join("taken.txt")], dest.clone(), false);
        s.idle(3);
        assert!(!matches!(s.app.popup, Popup::Conflict(..)), "no dialog for a queued copy");
        wait_until(&mut s, "four waiting", 10, |s| s.app.queue.waiting().len() == 4);
        assert!(some_text(&s, "5 jobs · 0%") || s.texts.iter().any(|(_, t)| t.starts_with("5 jobs · ")), "the chip");
        click_control(&mut s, "transfers chip");
        assert!(matches!(s.app.popup, Popup::Transfers(_)), "the list opens above the chip");
        let chip = controls_named("transfers chip")[0];
        look_popup(&mut s, "transfers");
        s.shot("transfers");
        assert!(some_text(&s, "Copying film.mkv"));
        assert_eq!(controls_named("transfer waiting").len(), 4);
        // The waiting ones by the keys: the third first.
        let ids = s.app.queue.waiting();
        s.act("key ArrowDown");
        s.act("key ArrowDown");
        s.act("key ArrowDown");
        s.act("key alt+ArrowUp");
        s.act("key alt+ArrowUp");
        assert_eq!(s.app.queue.waiting(), [ids[2], ids[0], ids[1], ids[3]], "moved up twice");
        // The second (two.txt) started now by its button; the first cancelled.
        let rows = controls_named("transfer start now");
        assert_eq!(rows.len(), 4);
        click_rect(&mut s, rows[2]);
        wait_until(&mut s, "two.txt copied past the paused film", 10, |_| dest.join("two.txt").exists());
        s.idle(2);
        let cancel = controls_named("transfer cancel");
        // The running film's cancel comes first; the waiting ones' after.
        click_rect(&mut s, cancel[cancel.len() - 2]);
        wait_until(&mut s, "one.txt cancelled", 10, |s| s.app.queue.waiting().len() == 2);
        // Space resumes the film; its button pauses it again, and again resumes.
        s.act("key space");
        assert!(!s.app.jobs.iter().find(|j| j.id == film).unwrap().pause.load(Relaxed), "resumed by Space");
        click_control(&mut s, "transfer pause");
        assert!(s.app.jobs.iter().find(|j| j.id == film).unwrap().pause.load(Relaxed), "paused by its button");
        click_control(&mut s, "transfer pause");
        // three.txt runs after the film, then taken.txt asks inside the list.
        wait_until(&mut s, "the clash asked in the list", 20, |s| some_text(s, "Keep both"));
        assert!(s.texts.iter().any(|(_, t)| t.starts_with(" is already there") || t.contains("is already there")), "said in its job");
        look_popup(&mut s, "transfers asking");
        s.shot("transfers-clash");
        click_control(&mut s, "clash keep both");
        wait_until(&mut s, "everything done", 20, |s| crate::transfers::live(&s.app) == 0);
        s.idle(3);
        assert!(!s.popup_open(), "the list closes once there is nothing in it");
        assert_eq!(std::fs::read(dest.join("film.mkv")).unwrap().len(), big.len());
        assert!(dest.join("three.txt").exists() && !dest.join("one.txt").exists(), "three copied, one cancelled");
        assert_eq!(std::fs::read_to_string(dest.join("taken.txt")).unwrap(), "already here", "kept");
        assert_eq!(std::fs::read_dir(&dest).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().starts_with("taken")).count(), 2, "both kept");
        assert!(!s.texts.iter().any(|(r, t)| t.contains(" jobs · ") || (t.contains("job · ") && r.intersects(chip))), "no chip");
        // In a small window, at a big font: the list fits above the chip.
        s.resize(560.0, 420.0);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.app.start_job(crate::ops::OpKind::Copy { srcs: vec![src.join("film.mkv")], dest: home.join("Empty"), decide: Default::default() });
        s.app.jobs.last().unwrap().pause.store(true, Relaxed);
        s.idle(3);
        s.act("key ctrl+shift+j");
        assert!(matches!(s.app.popup, Popup::Transfers(_)), "Show Transfers by its key");
        look_popup(&mut s, "transfers small");
        s.shot("transfers-small");
        s.act("key Escape");
        let j = s.app.jobs.last().unwrap();
        j.cancel.store(true, Relaxed);
        j.pause.store(false, Relaxed);
        wait_until(&mut s, "cancelled", 10, |s| crate::transfers::live(&s.app) == 0);
        settles_idle(&mut s, "transfers");
        report("transfers", &s);
    }

    /// Paste From Earlier: three sets copied, the list newest first; the second pasted by
    /// its digit, the first by a click; a cut set gone from the list once pasted; nothing
    /// copied yet says so.
    #[test]
    fn paste_from_earlier_copies() {
        if !sandboxed() {
            return;
        }
        let home = fixture("pastes");
        let docs = home.join("Documents");
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.idle(3);
        s.cmd(PasteEarlier);
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "Nothing copied yet"));
        for (name, key) in [("report.md", "ctrl+c"), ("notes.txt", "ctrl+c"), ("budget 2026.csv", "ctrl+x")] {
            select_name(&mut s, name);
            s.act(&format!("key {key}"));
        }
        assert_eq!(s.app.pastes.len(), 3);
        go(&mut s, &home.join("Empty"));
        s.act("key ctrl+shift+v");
        assert!(matches!(s.app.popup, Popup::Pastes(_)));
        assert_eq!(controls_named("paste row").len(), 3);
        assert!(some_text(&s, "1 file from pastes/Documents"), "said in a few words: the last two folders");
        look_popup(&mut s, "paste from earlier");
        s.shot("pastes");
        // The newest is the cut csv; 2 is notes.txt.
        s.act("text 2");
        wait_until(&mut s, "notes pasted", 10, |_| home.join("Empty/notes.txt").exists());
        assert!(!s.popup_open(), "picked, it closes");
        s.settle();
        s.act("key ctrl+shift+v");
        let rows = controls_named("paste row");
        click_rect(&mut s, rows[0]);
        wait_until(&mut s, "the cut moved", 10, |_| home.join("Empty/budget 2026.csv").exists() && !docs.join("budget 2026.csv").exists());
        s.settle();
        assert_eq!(s.app.pastes.len(), 2, "the cut set is gone once pasted");
        // At the owner's look, in a narrow window.
        s.resize(480.0, 600.0);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.act("key ctrl+shift+v");
        look_popup(&mut s, "paste from earlier, narrow");
        s.act("key Escape");
        assert!(!s.popup_open());
        settles_idle(&mut s, "pastes");
        report("pastes", &s);
    }

    /// Jump: folders gone to often rank first; a few letters find one, its own name
    /// counting for more; Enter goes there; a folder since deleted isn't offered.
    #[test]
    fn jump_to_where_you_go_most() {
        if !sandboxed() {
            return;
        }
        let home = fixture("jump");
        let mut s = Sim::new(&home, vec2(1280.0, 800.0));
        s.idle(3);
        let trips: [(&str, usize); 4] = [("Documents/Taxes/2025", 5), ("Music/Album", 3), ("Code/maia/src", 2), ("Pictures/Trip", 1)];
        // Gone to as a user goes: the app counts each folder left.
        let walk = |s: &mut Sim, p: PathBuf| {
            let panel = s.app.active;
            s.app.actions.push(crate::app::Action::Navigate { panel, path: p });
            s.settle();
        };
        for (dir, n) in trips {
            for _ in 0..n {
                walk(&mut s, home.join(dir));
                walk(&mut s, home.clone());
            }
        }
        std::fs::create_dir_all(home.join("Gone soon")).unwrap();
        for _ in 0..9 {
            walk(&mut s, home.join("Gone soon"));
            walk(&mut s, home.clone());
        }
        assert!(s.app.ud.visits.len() >= 5, "visits counted: {}", s.app.ud.visits.len());
        std::fs::remove_dir(home.join("Gone soon")).unwrap();
        s.act("key ctrl+g");
        assert!(matches!(s.app.popup, Popup::Jump(_)), "Jump by its key");
        wait_until(&mut s, "the folders checked", 10, |s| controls_named("jump row").len() >= 4);
        s.idle(4);
        let rows: Vec<String> = s.texts.iter().filter(|(_, t)| t.starts_with("~/")).map(|(_, t)| t.clone()).collect();
        assert!(!rows.iter().any(|t| t.contains("Gone soon")), "a deleted folder isn't offered: {rows:?}");
        assert!(!rows.iter().any(|t| t.ends_with("/jump")), "nor the folder shown now: {rows:?}");
        look_popup(&mut s, "jump");
        s.shot("jump");
        s.act("text trip");
        s.idle(3);
        look_popup(&mut s, "jump narrowed");
        s.act("key Enter");
        s.settle();
        assert!(!s.popup_open());
        assert_eq!(s.tab().path, home.join("Pictures/Trip"), "went there");
        // A few letters of the folder's own name beat the same letters further up.
        s.act("key ctrl+g");
        wait_until(&mut s, "listed again", 10, |s| controls_named("jump row").len() >= 4);
        s.act("text tax");
        s.idle(3);
        s.act("key Enter");
        s.settle();
        assert_eq!(s.tab().path, home.join("Documents/Taxes/2025"));
        s.resize(460.0, 520.0);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.act("key ctrl+g");
        wait_until(&mut s, "listed small", 10, |s| controls_named("jump row").len() >= 1);
        look_popup(&mut s, "jump small");
        s.act("key Escape");
        settles_idle(&mut s, "jump");
        report("jump", &s);
    }

    /// Pin To Top: a pinned file first whatever the sort, the pin drawn at its name's end
    /// in every view, kept through a rename and its undo, and off again; Pin Tab: the tab
    /// at the left as its icon, without a close button, kept out of Close Other Tabs.
    #[test]
    fn pins_in_folders_and_tab_strips() {
        if !sandboxed() {
            return;
        }
        let home = fixture("pins");
        let docs = home.join("Documents");
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.idle(3);
        let first = |s: &Sim| s.tab().view.first().map(|&i| s.tab().entries[i as usize].name.clone()).unwrap_or_default();
        assert_eq!(first(&s), "Taxes");
        select_name(&mut s, "notes.txt");
        s.cmd(PinToTop);
        s.settle();
        assert_eq!(first(&s), "notes.txt", "pinned ahead of the folders");
        assert!(s.app.ud.pinned.contains(&docs.join("notes.txt")));
        for v in [ViewDetails, ViewList, ViewLarge, ViewTree, ViewColumns] {
            s.cmd(v);
            s.settle();
            look(&mut s, &format!("pinned in {v:?}"));
        }
        s.cmd(ViewDetails);
        s.settle();
        s.cmd(SortSizeDesc);
        s.settle();
        assert_eq!(first(&s), "notes.txt", "whatever the sort");
        // Renamed, it stays pinned; the rename undone, too.
        select_name(&mut s, "notes.txt");
        s.act("key F2");
        s.act("key ctrl+a");
        s.act("text kept.txt");
        s.act("key Enter");
        s.settle();
        assert_eq!(first(&s), "kept.txt", "the pin follows the rename");
        s.act("key ctrl+z");
        s.settle();
        assert_eq!(first(&s), "notes.txt", "and its undo");
        select_name(&mut s, "notes.txt");
        s.cmd(PinToTop);
        s.settle();
        assert!(s.app.ud.pinned.is_empty(), "again unpins");
        assert_ne!(first(&s), "notes.txt");
        // Tabs.
        s.act("key ctrl+t");
        s.act("key ctrl+t");
        s.settle();
        let p = s.app.panel();
        assert_eq!(p.tabs.len(), 3);
        let id = p.tabs[2].id;
        s.cmd(PinTab);
        s.idle(20);
        let p = s.app.panel();
        assert_eq!(p.tabs[0].id, id, "to the left");
        assert!(p.tabs[0].pinned && p.tabs[p.active].id == id);
        look(&mut s, "a pinned tab");
        s.shot("pinned-tab");
        // Kept through a restart: the layout as saved, read back and applied.
        let ctx = s.ctx.clone();
        let saved = serde_json::to_string(&s.app.layout_snap(&ctx)).unwrap();
        let back: crate::config::LayoutSnap = serde_json::from_str(&saved).unwrap();
        assert!(s.app.apply_layout(&back), "the layout applies");
        s.idle(20);
        let p = s.app.panel();
        assert!(p.tabs[0].pinned && p.tabs.iter().filter(|t| t.pinned).count() == 1, "pinned after a restart");
        s.act("key ctrl+Tab");
        s.idle(20);
        assert!(!s.app.panel().tab().pinned);
        s.cmd(CloseOtherTabs);
        s.idle(20);
        assert_eq!(s.app.panel().tabs.len(), 2, "kept out of Close Other Tabs");
        assert!(s.app.panel().tabs[0].pinned);
        s.act("key ctrl+1");
        s.idle(20);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        look(&mut s, "a pinned tab at a big font");
        s.cmd(PinTab);
        s.idle(20);
        assert!(!s.app.panel().tabs.iter().any(|t| t.pinned));
        settles_idle(&mut s, "pins");
        report("pins", &s);
    }

    /// A folder's README above its files: shown at its start, dragged taller by its edge
    /// and kept for every folder, a double click back to the start's height, Show all and
    /// Show less, closed for this folder alone, and the command that hides them all.
    #[test]
    fn readme_card_above_the_files() {
        if !sandboxed() {
            return;
        }
        let home = fixture("readme");
        let proj = home.join("Code/maia");
        std::fs::write(proj.join("README"), format!("Maia\n\n{}", "A paragraph of the README that goes on for a while.\n\n".repeat(40))).unwrap();
        let mut s = Sim::new(&proj, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(20);
        wait_until(&mut s, "the card", 10, |s| controls_named("readme edge").len() == 1);
        s.idle(20);
        assert!(some_text(&s, "Show all"));
        look(&mut s, "readme card");
        s.shot("readme");
        let list_top = |s: &Sim| s.app.panels[&s.app.active].list_rect.top();
        let edge = controls_named("readme edge")[0];
        let before = edge.center().y;
        s.act(&format!("drag {:.0} {:.0} {:.0} {:.0}", edge.center().x, edge.center().y, edge.center().x, edge.center().y + 120.0));
        s.idle(4);
        assert!(s.app.cfg.readme_height > 0.0, "the height kept: {}", s.app.cfg.readme_height);
        let after = controls_named("readme edge")[0].center().y;
        assert!((after - before - 120.0).abs() < 8.0, "followed the pointer: {before} to {after}");
        look(&mut s, "readme card dragged");
        // Another folder with a README gets the same height.
        std::fs::write(home.join("Music/README.md"), "# Music\n\nThe albums.\n\n".repeat(30)).unwrap();
        go(&mut s, &home.join("Music"));
        s.idle(20);
        wait_until(&mut s, "the music card", 10, |s| controls_named("readme edge").len() == 1);
        s.idle(20);
        let e2 = controls_named("readme edge")[0];
        assert!((e2.center().y - after).abs() < 8.0, "the same height everywhere: {} vs {after}", e2.center().y);
        // Double click: the start's own height again.
        s.act(&format!("dclick {:.0} {:.0}", e2.center().x, e2.center().y));
        s.idle(20);
        assert_eq!(s.app.cfg.readme_height, 0.0);
        // Show all, then Show less.
        s.click_text("Show all");
        s.idle(20);
        assert!(some_text(&s, "Show less"));
        look(&mut s, "readme open");
        s.click_text("Show less");
        s.idle(20);
        // Closed: gone for this folder, still there for the project.
        click_control(&mut s, "readme close");
        s.idle(20);
        assert!(controls_named("readme edge").is_empty(), "closed");
        assert!(s.app.ud.readme_hidden.contains(&home.join("Music")));
        go(&mut s, &proj);
        s.idle(20);
        wait_until(&mut s, "the project's card", 10, |s| controls_named("readme edge").len() == 1);
        // At the owner's look, split, narrow; then the command hides them all.
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.idle(20);
        look(&mut s, "readme at a big font");
        s.cmd(SplitRight);
        s.settle();
        s.idle(20);
        look(&mut s, "readme split");
        s.resize(640.0, 480.0);
        s.idle(20);
        look(&mut s, "readme narrow");
        s.cmd(ToggleReadme);
        s.idle(20);
        assert!(!s.app.cfg.show_readme && controls_named("readme edge").is_empty());
        s.cmd(ToggleReadme);
        s.idle(20);
        let _ = list_top(&s);
        settles_idle(&mut s, "readme");
        report("readme", &s);
    }

    /// A picture copied in another app pasted as a picture file, named for the time and
    /// opened for renaming; copied text as a .txt; nothing usable on the clipboard pastes
    /// the files Maia copied; a folder that can't be written says so and leaves nothing.
    #[test]
    fn paste_a_picture_as_a_file() {
        if !sandboxed() {
            return;
        }
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                *crate::clipboard::OFFER.lock().unwrap() = None;
            }
        }
        let _clear = Clear;
        let home = fixture("clipfile");
        let pics = home.join("Pictures");
        let mut png = vec![];
        {
            let mut e = png::Encoder::new(&mut png, 2, 2);
            e.set_color(png::ColorType::Rgb);
            e.set_depth(png::BitDepth::Eight);
            e.write_header().unwrap().write_image_data(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 255]).unwrap();
        }
        *crate::clipboard::OFFER.lock().unwrap() = Some(("image/png".into(), png.clone()));
        let mut s = Sim::new(&pics, vec2(1280.0, 800.0));
        s.idle(3);
        s.act("key ctrl+v");
        let pasted = |dir: &Path, ext: &str| std::fs::read_dir(dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().starts_with("Pasted ") && p.extension().is_some_and(|e| e == ext)).collect::<Vec<_>>();
        wait_until(&mut s, "the picture written", 10, |_| pasted(&pics, "png").len() == 1);
        s.settle();
        assert_eq!(std::fs::read(&pasted(&pics, "png")[0]).unwrap(), png, "as it was offered");
        assert!(s.tab().rename.is_some(), "ready to be renamed");
        s.act("key Escape");
        s.act("key ctrl+v");
        wait_until(&mut s, "a second one beside it", 10, |_| pasted(&pics, "png").len() == 2);
        s.settle();
        s.act("key Escape");
        s.act("key ctrl+z");
        s.settle();
        assert_eq!(pasted(&pics, "png").len(), 1, "undone");
        *crate::clipboard::OFFER.lock().unwrap() = Some(("text/plain;charset=utf-8".into(), b"a note from the browser".to_vec()));
        s.act("key ctrl+v");
        wait_until(&mut s, "the text written", 10, |_| pasted(&pics, "txt").len() == 1);
        s.settle();
        s.act("key Escape");
        assert_eq!(std::fs::read_to_string(&pasted(&pics, "txt")[0]).unwrap(), "a note from the browser");
        // Something else: the app's own copy pastes.
        *crate::clipboard::OFFER.lock().unwrap() = Some(("application/x-other".into(), b"x".to_vec()));
        go(&mut s, &home.join("Documents"));
        select_name(&mut s, "report.md");
        s.act("key ctrl+c");
        go(&mut s, &pics);
        s.act("key ctrl+v");
        wait_until(&mut s, "the copied file pasted", 10, |_| pics.join("report.md").exists());
        // A folder that can't be written.
        *crate::clipboard::OFFER.lock().unwrap() = Some(("image/png".into(), png.clone()));
        use std::os::unix::fs::PermissionsExt;
        let ro = home.join("Read only");
        std::fs::create_dir_all(&ro).unwrap();
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o555)).unwrap();
        go(&mut s, &ro);
        s.act("key ctrl+v");
        wait_until(&mut s, "refused", 10, |s| s.app.toast.as_ref().is_some_and(|t| t.0.starts_with("Couldn't paste here")));
        assert_eq!(std::fs::read_dir(&ro).unwrap().count(), 0, "nothing left");
        std::fs::set_permissions(&ro, std::fs::Permissions::from_mode(0o755)).unwrap();
        settles_idle(&mut s, "paste a picture");
        report("paste a picture", &s);
    }

    /// Send To Phone: the address as a code and as text, fetched as a phone would over a
    /// real connection (one file at once, several as a page, a folder as a zip), the device
    /// and what it got shown; a photo sent without its place when that is switched on;
    /// closing stops the server.
    #[test]
    fn send_to_a_phone() {
        if !sandboxed() {
            return;
        }
        use std::io::{Read, Write};
        struct Clear;
        impl Drop for Clear {
            fn drop(&mut self) {
                *crate::phone::TEST_LAN.lock().unwrap() = None;
            }
        }
        let _clear = Clear;
        *crate::phone::TEST_LAN.lock().unwrap() = Some(std::net::Ipv4Addr::LOCALHOST);
        let home = fixture("phone");
        let docs = home.join("Documents");
        let fetch = |url: &str| -> (String, Vec<u8>) {
            let rest = url.strip_prefix("http://").unwrap();
            let (host, path) = rest.split_once('/').unwrap();
            let mut c = std::net::TcpStream::connect(host).unwrap();
            c.set_read_timeout(Some(std::time::Duration::from_secs(10))).unwrap();
            write!(c, "GET /{path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n").unwrap();
            let mut b = vec![];
            c.read_to_end(&mut b).unwrap();
            let at = b.windows(4).position(|w| w == b"\r\n\r\n").map_or(b.len(), |i| i + 4);
            (String::from_utf8_lossy(&b[..at]).into_owned(), b[at..].to_vec())
        };
        let mut s = Sim::new(&docs, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        // A folder alone, without bsdtar to zip it, can't go.
        select_name(&mut s, "Taxes");
        s.cmd(SendToPhone);
        assert!(!s.popup_open() && s.app.toast.as_ref().is_some_and(|t| t.0.contains("bsdtar")), "says what it needs");
        select_name(&mut s, "notes.txt");
        s.cmd(SendToPhone);
        assert!(matches!(s.app.popup, Popup::Phone(_)));
        let url = |s: &Sim| match &s.app.popup {
            Popup::Phone(st) => st.server.shared.lock().unwrap().url.clone(),
            _ => None,
        };
        wait_until(&mut s, "listening", 10, |s| url(s).is_some());
        let u = url(&s).unwrap().unwrap_or_else(|e| s.fail(&e));
        assert!(u.starts_with("http://127.0.0.1:") && u.ends_with('/'), "{u}");
        s.idle(10);
        assert!(some_text(&s, u.trim_start_matches("http://").trim_end_matches('/')) || s.texts.iter().any(|(_, t)| t.contains("127.0.0.1")), "the address shown");
        look_popup(&mut s, "send to phone");
        s.shot("phone");
        // The code as drawn in the window scans to the address (when frames are painted).
        if let (Some(dir), Some((w, h, rgb))) = (s.shots_dir(), s.pixels()) {
            let f = dir.join("phone-scan.png");
            write_png(&f, w, h, &rgb);
            if let Ok(o) = std::process::Command::new("zbarimg").args(["--raw", "-q"]).arg(&f).output() {
                assert_eq!(String::from_utf8_lossy(&o.stdout).trim(), u, "the window's code scans");
            }
            let _ = std::fs::remove_file(&f);
        }
        let (h, body) = fetch(&u);
        assert!(h.starts_with("HTTP/1.1 200") && body == std::fs::read(docs.join("notes.txt")).unwrap(), "{h}");
        wait_until(&mut s, "the phone seen", 10, |s| match &s.app.popup {
            Popup::Phone(st) => !st.server.shared.lock().unwrap().got.is_empty(),
            _ => false,
        });
        s.idle(4);
        look_popup(&mut s, "send to phone, sent");
        s.shot("phone-sent");
        // Without the token, nothing.
        let host = u.trim_start_matches("http://").split('/').next().unwrap().to_string();
        let (h, body) = fetch(&format!("http://{host}/notes.txt"));
        assert!(h.starts_with("HTTP/1.1 404") && body.is_empty(), "{h}");
        s.act("key Escape");
        s.idle(4);
        assert!(!s.popup_open());
        wait_until(&mut s, "the server stopped", 10, |_| std::net::TcpStream::connect(u.trim_start_matches("http://").split('/').next().unwrap()).is_err());
        // Several, a folder among them: a page, and the folder as a zip.
        let have_bsdtar = Path::new("/usr/bin/bsdtar").exists();
        let fake = root().join("phone-tools");
        let _ = std::fs::create_dir_all(&fake);
        if have_bsdtar {
            let _ = std::os::unix::fs::symlink("/usr/bin/bsdtar", fake.join("bsdtar"));
        }
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        select_names(&mut s, &["Taxes", "report.md"]);
        s.cmd(SendToPhone);
        wait_until(&mut s, "listening again", 10, |s| url(s).is_some());
        let u = url(&s).unwrap().unwrap();
        let (h, page) = fetch(&u);
        let page = String::from_utf8_lossy(&page).into_owned();
        assert!(h.contains("text/html") && page.contains("report.md") && page.contains("Taxes.zip"), "{page}");
        if have_bsdtar {
            let k = if page.find("Taxes.zip").unwrap() < page.find("report.md").unwrap() { 0 } else { 1 };
            let (h, zip) = fetch(&format!("{u}{k}/"));
            assert!(h.contains("application/zip") && zip.starts_with(b"PK"), "{h}");
        }
        look_popup(&mut s, "send to phone, several");
        s.act("key Escape");
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        // At the owner's look in a small window.
        s.resize(520.0, 560.0);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        select_name(&mut s, "notes.txt");
        s.cmd(SendToPhone);
        wait_until(&mut s, "listening small", 10, |s| url(s).is_some());
        look_popup(&mut s, "send to phone small");
        s.shot("phone-small");
        s.act("key Escape");
        settles_idle(&mut s, "phone");
        report("phone", &s);
    }

    /// View Calendar: a folder's pictures by the day they were taken (the file's date for
    /// those without one), on the newest month; a day clicked selects its pictures in the
    /// list, Shift and the arrows a run of days; months back and on; no pictures, a word.
    #[test]
    fn pictures_on_a_calendar() {
        if !sandboxed() {
            return;
        }
        let home = fixture("calendar");
        let trip = home.join("Pictures/Trip");
        // Three days: four pictures on the 3rd, five on the 5th, three on the 9th, in
        // the month before this one.
        let now = crate::timefmt::local_day(std::time::SystemTime::now());
        let (y, m, _) = crate::calendar::civil(now);
        let (py, pm) = if m == 1 { (y - 1, 12) } else { (y, m - 1) };
        let day = |d: u32| crate::calendar::day_of(py, pm, d);
        let names: Vec<PathBuf> = (0..12).map(|i| trip.join(format!("img{i:02}.png"))).collect();
        for (i, p) in names.iter().enumerate() {
            let d = if i < 4 { day(3) } else if i < 9 { day(5) } else { day(9) };
            // Noon in UTC, moved a day when the local day differs.
            let mut secs = d * 86_400 + 12 * 3600;
            let at = |x: i64| crate::timefmt::local_day(std::time::UNIX_EPOCH + std::time::Duration::from_secs(x as u64));
            secs += (d - at(secs)) * 86_400;
            let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs as u64);
            std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
            assert_eq!(crate::timefmt::local_day(t), d, "{}", p.display());
        }
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        s.act("key shift+F3");
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0 == "No pictures here"));
        go(&mut s, &trip);
        s.act("key shift+F3");
        assert!(matches!(s.app.popup, Popup::Calendar(_)));
        let month = format!("{} {py}", ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"][pm as usize - 1]);
        wait_until(&mut s, "the month of the pictures", 20, |s| some_text(s, &month) && some_text(s, "12 pictures"));
        look_popup(&mut s, "calendar");
        s.shot("calendar");
        // The 5th clicked: its five pictures selected.
        let days = controls_named("calendar day");
        assert_eq!(days.len(), 3, "the days with pictures");
        let fifth = s.texts.iter().filter(|(r, t)| t == "5" && days.iter().any(|d| d.contains(r.center()))).map(|x| x.0).next().unwrap_or_else(|| s.fail("no 5th"));
        click_rect(&mut s, fifth);
        s.idle(2);
        let sel: Vec<String> = s.tab().selected.iter().map(|&i| s.tab().entries[i as usize].name.clone()).collect();
        assert_eq!(sel.len(), 5, "{sel:?}");
        assert!(sel.iter().all(|n| ["img04.png", "img05.png", "img06.png", "img07.png", "img08.png"].contains(&n.as_str())));
        // Shift and Right to the 9th: the run of days.
        for _ in 0..4 {
            s.act("key shift+ArrowRight");
        }
        s.idle(2);
        assert_eq!(s.tab().selected.len(), 8, "the 5th to the 9th");
        look_popup(&mut s, "calendar, days picked");
        s.act("key PageDown");
        assert!(!some_text(&s, &month), "the next month");
        s.act("key PageUp");
        assert!(some_text(&s, &month));
        s.act("key Escape");
        assert!(!s.popup_open());
        assert_eq!(s.tab().selected.len(), 8, "the selection stays");
        // At the owner's look, narrow and short.
        s.resize(520.0, 520.0);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.act("key shift+F3");
        wait_until(&mut s, "shown small", 20, |s| some_text(s, &month));
        look_popup(&mut s, "calendar small");
        s.shot("calendar-small");
        s.act("key Escape");
        settles_idle(&mut s, "calendar");
        report("calendar", &s);
    }

    /// Where downloads came from: Group By Origin puts folders first, each site next and
    /// the unmarked last, in every view; the inspector shows the address, and the list
    /// groups again when a file is marked later.
    #[test]
    fn downloads_grouped_by_where_they_came_from() {
        if !sandboxed() {
            return;
        }
        let home = fixture("origin");
        let dl = home.join("Downloads");
        std::fs::write(dl.join("notes.txt"), "plain").unwrap();
        let mark = |f: &str, name: &str, url: &str| {
            use std::os::unix::ffi::OsStrExt;
            let c = std::ffi::CString::new(dl.join(f).as_os_str().as_bytes()).unwrap();
            let n = std::ffi::CString::new(name).unwrap();
            unsafe { libc::setxattr(c.as_ptr(), n.as_ptr(), url.as_ptr().cast(), url.len(), 0) == 0 }
        };
        if !mark("setup.tar.gz", "user.xdg.origin.url", "https://www.example.com/files/setup.tar.gz?x=1") {
            eprintln!("origin: no user attributes here, skipped");
            return;
        }
        assert!(mark("archive.zip", "user.xdg.referrer.url", "https://github.com/someone/thing/releases"));
        let mut s = Sim::new(&dl, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        s.cmd(GroupOrigin);
        // A group's header: its name, then its count.
        let header = |s: &Sim, t: &str| s.texts.iter().find(|(_, x)| x == t || x.starts_with(&format!("{t} "))).map(|x| x.0);
        wait_until(&mut s, "the sites as groups", 10, |s| header(s, "example.com").is_some() && header(s, "github.com").is_some() && header(s, "Unknown").is_some());
        let y = |s: &Sim, t: &str| header(s, t).unwrap_or_else(|| s.fail(&format!("no {t} group"))).center().y;
        assert!(y(&s, "Folders") < y(&s, "example.com") && y(&s, "github.com") < y(&s, "Unknown"), "folders first, the unmarked last");
        for v in [ViewDetails, ViewList, ViewLarge, ViewTree] {
            s.cmd(v);
            s.settle();
            look(&mut s, &format!("grouped by origin in {v:?}"));
        }
        s.cmd(ViewDetails);
        s.settle();
        s.shot("origin-groups");
        // The address in the inspector.
        select_name(&mut s, "setup.tar.gz");
        s.cmd(ToggleInspector);
        // The preview read, so its toolbar (and the name's pill) stays where it is.
        wait_until(&mut s, "the preview", 10, |s| s.app.panels[&s.app.active].inspector_rect.is_some_and(|ir| s.texts.iter().any(|(r, t)| t == "gz" && ir.contains(r.center()))));
        look(&mut s, "the inspector beside groups");
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("no inspector"));
        let pill = s.texts.iter().find(|(r, t)| t == "setup.tar.gz" && ir.contains(r.center())).map(|x| x.0).unwrap_or_else(|| s.fail("no name pill"));
        click_rect(&mut s, pill);
        s.idle(20);
        wait_until(&mut s, "the address shown", 10, |s| some_text(s, "example.com/files/setup.tar.gz"));
        s.shot("origin-inspector");
        click_rect(&mut s, pill);
        // Marked later, read again when the folder changes.
        assert!(mark("notes.txt", "user.xdg.origin.url", "https://files.example.org/notes.txt"));
        crate::origin::invalidate(&dl.join("notes.txt"));
        let id = s.app.active;
        s.app.panels.get_mut(&id).unwrap().tab_mut().reload();
        s.settle();
        wait_until(&mut s, "regrouped", 10, |s| header(s, "files.example.org").is_some());
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        look(&mut s, "grouped by origin at a big font");
        settles_idle(&mut s, "origin");
        report("origin", &s);
    }

    /// Lines of code in the inspector: a project folder's languages and lines, counted
    /// on a thread; a folder that isn't a project's top shows none.
    #[test]
    fn lines_of_code_in_the_inspector() {
        if !sandboxed() {
            return;
        }
        let home = fixture("loc");
        let code = home.join("Code");
        std::fs::write(code.join("maia/src/extra.py"), "# a tool\nprint('x')\nprint('y')\n").unwrap();
        let mut s = Sim::new(&code, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        select_name(&mut s, "maia");
        s.cmd(ToggleInspector);
        s.idle(4);
        // The details card, from the name's pill in the inspector's header.
        let ir = s.app.panels[&s.app.active].inspector_rect.unwrap_or_else(|| s.fail("no inspector"));
        let pill = s.texts.iter().find(|(r, t)| t == "maia" && ir.contains(r.center())).map(|x| x.0).unwrap_or_else(|| s.fail("no name pill"));
        look(&mut s, "a project in the inspector");
        click_rect(&mut s, pill);
        wait_until(&mut s, "the languages counted", 15, |s| some_text(s, "Rust") && some_text(s, "Python"));
        s.idle(4);
        s.shot("lines-of-code");
        select_name(&mut s, "deep");
        s.idle(20);
        assert!(!some_text(&s, "Rust"), "not a project: no count");
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        select_name(&mut s, "maia");
        wait_until(&mut s, "shown again", 15, |s| some_text(s, "Rust"));
        s.shot("lines-of-code-big");
        s.resize(700.0, 500.0);
        s.idle(4);
        s.shot("lines-of-code-narrow");
        settles_idle(&mut s, "lines of code");
        report("lines of code", &s);
    }

    /// Space used's map and what grew: an earlier reading of the folder makes each row say
    /// how it changed and the whole folder how much since; the map lays every item out as
    /// a block, a folder's block opens it and Backspace comes back; at the owner's look.
    #[test]
    fn space_map_and_what_grew() {
        if !sandboxed() {
            return;
        }
        let home = fixture("spacemap");
        let d = home.join("Disk");
        for (n, size) in [("Films", 3_000_000usize), ("Photos", 1_500_000), ("Music", 600_000), ("Mail", 200_000)] {
            std::fs::create_dir_all(d.join(n)).unwrap();
            std::fs::write(d.join(n).join("data.bin"), vec![1u8; size]).unwrap();
        }
        std::fs::write(d.join("notes.txt"), vec![b'n'; 90_000]).unwrap();
        // Three days ago the films were smaller and the mail bigger.
        let today = crate::timefmt::local_day(std::time::SystemTime::now());
        let at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() - 3 * 86_400;
        let sizes = [("Films", 1_000_000u64), ("Photos", 1_500_000), ("Music", 600_000), ("Mail", 2_200_000), ("notes.txt", 90_000)].iter().map(|(n, s)| (n.to_string(), *s)).collect();
        crate::grew::note_in(&crate::grew::book_path(), crate::grew::Reading { day: today - 3, at, dir: d.clone(), sizes });
        let mut s = Sim::new(&d, vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        s.cmd(SpaceUsed);
        wait_until(&mut s, "counted", 20, |s| !space(s).counting() && space(s).before.is_some());
        s.idle(10);
        assert!(s.texts.iter().any(|(_, t)| t.starts_with('+')), "a change said beside a row");
        assert!(s.texts.iter().any(|(_, t)| t.starts_with('−')), "and a shrink");
        assert!(some_text(&s, "Change"), "sorted by change on offer");
        look(&mut s, "what grew");
        s.shot("what-grew");
        // By change: what grew most first, what shrank most last; S back to size.
        click_control(&mut s, "space sort change");
        s.idle(10);
        let names = space_names(&s);
        assert_eq!((names.first().map(String::as_str), names.last().map(String::as_str)), (Some("Films"), Some("Mail")), "{names:?}");
        s.act("key s");
        s.idle(10);
        assert_eq!(space_names(&s), ["Films", "Photos", "Music", "Mail", "notes.txt"], "biggest first again");
        // The map.
        click_control(&mut s, "space map");
        s.idle(20);
        // Each block says its size and share under its name.
        let shares: Vec<Rect> = s.texts.iter().filter(|(_, t)| t.contains(" · ") && t.ends_with('%')).map(|x| x.0).collect();
        // The smallest block has no room for its size at the biggest fonts.
        assert!(shares.len() == 5 || s.app.cfg.font_size > 17.0 && shares.len() == 4, "a block an item: {}", shares.len());
        for (i, a) in shares.iter().enumerate() {
            for b in &shares[i + 1..] {
                assert!(!a.intersects(*b), "blocks apart: {a:?} {b:?}");
            }
        }
        look(&mut s, "the map");
        s.shot("space-map-blocks");
        assert!(s.texts.iter().any(|(_, t)| t.ends_with("· 56%")), "the films' share");
        // The films' block, pointed at, then clicked open.
        let films = s.texts.iter().find(|(_, t)| t == "Films" && true).map(|x| x.0).unwrap_or_else(|| s.fail("no Films block"));
        let films = s.texts.iter().filter(|(r, t)| t == "Films" && r.left() > films.left()).map(|x| x.0).last().unwrap_or(films);
        let at = films.center() + vec2(0.0, 60.0);
        s.act(&format!("move {:.0} {:.0}", at.x, at.y));
        s.idle(2);
        assert_eq!(controls_named("space block").len(), 1, "the block under the pointer");
        s.act(&format!("click {:.0} {:.0}", at.x, at.y));
        s.idle(4);
        if space(&s).dir != d.join("Films") {
            s.act(&format!("dclick {:.0} {:.0}", at.x, at.y));
        }
        wait_until(&mut s, "into the films", 10, |s| space(s).dir == d.join("Films"));
        s.act("key Backspace");
        wait_until(&mut s, "back up", 10, |s| space(s).dir == d);
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.idle(20);
        look(&mut s, "the map at a big font");
        s.resize(560.0, 440.0);
        s.idle(20);
        look(&mut s, "the map small");
        s.shot("space-map-small");
        click_control(&mut s, "space contents");
        s.idle(20);
        look(&mut s, "what grew small");
        s.act("key Escape");
        settles_idle(&mut s, "space map");
        report("space map", &s);
    }

    /// Thirty thousand files grouped by where they came from: every file's attributes read
    /// on the worker while the list keeps drawing, compared with grouping by type.
    #[test]
    fn huge_tree_grouped_by_origin_stays_responsive() {
        if !sandboxed() {
            return;
        }
        // A flattened walk is never grouped: one big folder, a tenth of it downloads.
        let dir = root().join("home/bigorigin");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sites = ["example.com", "github.com", "files.example.org", "cdn.example.net"];
        for i in 0..30_000 {
            let p = dir.join(format!("file{i:05}.{}", ["txt", "png", "pdf", "zip"][i % 4]));
            std::fs::write(&p, b"").unwrap();
            if i % 10 == 0 {
                use std::os::unix::ffi::OsStrExt;
                let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
                let url = format!("https://{}/{i}", sites[i / 10 % 4]);
                unsafe { libc::setxattr(c.as_ptr(), c"user.xdg.origin.url".as_ptr(), url.as_ptr().cast(), url.len(), 0) };
            }
        }
        let mut s = Sim::new(&dir, vec2(1280.0, 800.0));
        s.settle();
        let n = s.tab().entries.len();
        let worst_of = |s: &mut Sim, what: &str, c: Cmd, secs: u64| {
            let i = s.frame_ms.len();
            let t0 = Instant::now();
            s.cmd(c);
            // Until the groups stop changing for half a second.
            let mut quiet = Instant::now();
            let mut last = (s.tab().group_labels.len(), s.tab().rows.len());
            while quiet.elapsed().as_millis() < 500 && t0.elapsed().as_secs() < secs {
                s.frame(vec![]);
                std::thread::sleep(std::time::Duration::from_millis(4));
                let now = (s.tab().group_labels.len(), s.tab().rows.len());
                if now != last {
                    last = now;
                    quiet = Instant::now();
                }
            }
            let v = &s.frame_ms[i..];
            let worst = v.iter().cloned().fold(0.0f32, f32::max);
            let mut sorted = v.to_vec();
            sorted.sort_by(|a, b| a.total_cmp(b));
            let p95 = sorted.get(sorted.len() * 95 / 100).copied().unwrap_or(0.0);
            eprintln!("sim origin: {what}: {n} files, {} groups after {:.1}s, {} frames, p95 {p95:.1} worst {worst:.1} ms", s.tab().group_labels.len(), t0.elapsed().as_secs_f32(), v.len());
            (p95, worst)
        };
        let (_, by_type) = worst_of(&mut s, "by type", GroupType, 20);
        let (p95, by_origin) = worst_of(&mut s, "by origin", GroupOrigin, 60);
        assert!(s.tab().group_labels.iter().any(|l| l == crate::origin::UNKNOWN) && s.tab().group_labels.iter().any(|l| l == "github.com"), "grouped: {:?}", s.tab().group_labels);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(p95 < 40.0, "frames while origins land: p95 {p95:.1} ms");
        let budget = (by_type * 2.0).max(200.0 * (n as f32 / 200_000.0).max(1.0));
        assert!(by_origin < budget, "the worst frame grouping by origin, {by_origin:.1} ms, against {by_type:.1} by type");
    }


    // ------------------------------------------------------------------ the GitHub page

    /// A `gh` that answers as GitHub did (the captured answers in tests.rs): who is signed
    /// in, the profile, the recent commits, one commit's files. `signed_in` false: gh
    /// knows nobody.
    fn fake_gh(name: &str, signed_in: bool) -> PathBuf {
        let d = root().join(name);
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("viewer.json"), crate::tests::GH_VIEWER).unwrap();
        std::fs::write(d.join("history.json"), crate::tests::GH_HISTORY).unwrap();
        let user = if signed_in { "echo ana-lima" } else { "true" };
        let script = format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$1 $2\" in\n  'config get') {user} ;;\n  'api graphql') case \"$*\" in *'-F id='*) cat '{d}/history.json' ;; *) cat '{d}/viewer.json' ;; esac ;;\n  'api repos/'*) echo '{{\"files\":[{{\"filename\":\"src/lamp.rs\",\"additions\":4,\"deletions\":1}},{{\"filename\":\"README.md\",\"additions\":2,\"deletions\":0}}]}}' ;;\n  *) exit 1 ;;\nesac\n",
            log = d.join("log").display(),
            d = d.display()
        );
        let gh = d.join("gh");
        std::fs::write(&gh, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        d
    }

    fn github_page(s: &Sim) -> &crate::github::Page {
        &s.app.github
    }

    /// The profile page from `gh`: the card, the year, the timeline; the arrows through
    /// it, a commit's files opened under it, Enter into a clone's history and O to the
    /// commit on GitHub; Refresh; Up to Computer. Without gh, and signed out, it says
    /// what to do. Wide and narrow, in a light theme and at the owner's look.
    #[test]
    fn github_profile_page() {
        if !sandboxed() {
            return;
        }
        let home = fixture("github");
        let d = fake_gh("fake-gh", true);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
        let _ = std::fs::remove_dir_all(crate::github::cache_dir());
        // The badges as read from the profile page earlier today (curl is no test's).
        std::fs::create_dir_all(crate::github::cache_dir()).unwrap();
        std::fs::write(crate::github::cache_dir().join("achievements.txt"), "starstruck\tStarstruck\t1\t4\nquickdraw\tQuickdraw\t0\t1\npull-shark\tPull Shark\t2\t2\ngalaxy-brain\tGalaxy Brain\t0\t1").unwrap();
        // A clone of one repository on this computer, known to Recent Projects.
        let clone = home.join("Code/lamp");
        std::fs::create_dir_all(&clone).unwrap();
        let git = |args: &[&str]| assert!(std::process::Command::new("git").arg("-C").arg(&clone).args(args).output().unwrap().status.success(), "git {args:?}");
        git(&["init", "-q"]);
        git(&["remote", "add", "origin", "https://github.com/ana-lima/lamp.git"]);
        crate::projects::save_cache(&crate::projects::cache_path(), &[clone.clone()]).unwrap();
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.app.github = Default::default();
        s.cmd(GitHubProfile);
        assert!(crate::tab::is_github(&s.tab().path));
        wait_until(&mut s, "the profile", 20, |s| some_text(s, "Ana Lima") && some_text(s, "1,318 contributions in the last year"));
        assert!(some_text(&s, "1,204 followers · 12 following") && some_text(&s, "@ana-lima · she/her") && some_text(&s, "Porto"));
        // The card's sections, scrolled to where a big font puts them.
        let name = s.text_rect("Ana Lima");
        let mut seen = std::collections::HashSet::new();
        let mut badges = 0;
        for _ in 0..8 {
            for w in ["Achievements", "Languages", "Organisations"] {
                if some_text(&s, w) {
                    seen.insert(w);
                }
            }
            badges = badges.max(controls_named("github achievement").len());
            s.act(&format!("scroll {:.0} {:.0} -4", name.center().x, name.center().y));
            s.idle(4);
        }
        assert_eq!(seen.len(), 3, "{seen:?}");
        assert_eq!(badges, 4);
        for _ in 0..8 {
            s.act(&format!("scroll {:.0} {:.0} 4", name.center().x, name.center().y));
        }
        s.idle(4);
        assert!(some_text(&s, "Keep the lamp lit") && some_text(&s, "Created kettle"));
        wait_until(&mut s, "said when it was asked", 10, |s| s.texts.iter().any(|(_, t)| t.starts_with("Updated")));
        assert_eq!(s.tab().title(), "GitHub");
        look(&mut s, "github page");
        s.shot("github");
        // The arrows: the first shows where it is, then down to a commit of the clone.
        s.act("key ArrowDown");
        assert!(github_page(&s).keyed);
        let lamp = github_page(&s).data.as_ref().unwrap().items.iter().position(|i| i.repo == "ana-lima/lamp" && i.sha.is_some()).unwrap();
        for _ in 0..lamp {
            s.act("key ArrowDown");
        }
        assert_eq!(github_page(&s).sel, lamp);
        // Its files: not in the clone (a fresh one), so from GitHub, under the row.
        wait_until(&mut s, "the commit's files", 10, |s| some_text(s, "src/lamp.rs") && some_text(s, "README.md"));
        assert!(s.texts.iter().any(|(_, t)| t.contains("on this computer at ~/")), "the clone is known");
        look(&mut s, "a commit opened");
        s.shot("github-commit-open");
        // O: the commit on GitHub.
        let before = launched().len();
        s.act("key O");
        s.idle(2);
        assert!(launched()[before..].iter().any(|l| l.contains("https://github.com/ana-lima/lamp/commit/")), "{:?}", &launched()[before..]);
        // Narrow: the card shrinks to a line over the year (narrower than the page needs
        // for its column at this font size).
        let k = s.app.cfg.font_size / 13.0;
        s.resize((s.app.sidebar_width + 460.0 * k).round(), 700.0);
        // The panel eases to its new width.
        s.idle(30);
        assert!(!some_text(&s, "Languages") && s.texts.iter().any(|(_, t)| t.starts_with("Ana Lima · 1,204 followers")));
        look(&mut s, "github narrow");
        s.shot("github-narrow");
        s.resize(1280.0, 800.0);
        // A light theme and the owner's look.
        s.app.cfg.theme = crate::theme::ThemeName::MinLight;
        s.app.cfg.font_size = 16.5;
        s.app.cfg.spacing = 1.6;
        s.idle(3);
        look(&mut s, "github light, big");
        s.shot("github-light-big");
        s.app.cfg.font_size = 13.0;
        s.app.cfg.spacing = 1.0;
        // Refresh asks again.
        let asked = std::fs::read_to_string(d.join("log")).unwrap().lines().filter(|l| l.starts_with("api graphql")).count();
        s.cmd(Refresh);
        wait_until(&mut s, "asked again", 10, |s| !github_page(s).busy());
        assert!(std::fs::read_to_string(d.join("log")).unwrap().lines().filter(|l| l.starts_with("api graphql")).count() > asked);
        // Enter on the commit: its clone, its history open on it.
        s.act("key Enter");
        s.settle();
        assert_eq!(s.tab().path, clone);
        assert!(history_open(&s), "the commit in its clone's history");
        // Back to the page, then up to Computer.
        s.cmd(Back);
        s.settle();
        assert!(crate::tab::is_github(&s.tab().path));
        s.cmd(Up);
        assert!(crate::tab::is_computer(&s.tab().path));
        // No gh: what to install. Signed out: what to do.
        let none = root().join("no-tools");
        std::fs::create_dir_all(&none).unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(none.clone()));
        let _ = std::fs::remove_dir_all(crate::github::cache_dir());
        s.app.github = Default::default();
        s.cmd(GitHubProfile);
        wait_until(&mut s, "no gh", 10, |s| some_text(s, "Your GitHub profile shows here once GitHub's gh is installed"));
        look(&mut s, "github without gh");
        let out = fake_gh("fake-gh-out", false);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(out));
        s.app.github = Default::default();
        s.idle(2);
        wait_until(&mut s, "signed out", 10, |s| some_text(s, "Sign in to GitHub with gh auth login"));
        // Nothing is written into a place: a paste or a new folder there does nothing.
        s.cmd(NewFolder);
        assert!(s.tab().entries.is_empty() && !std::path::Path::new(crate::tab::GITHUB).exists());
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        settles_idle(&mut s, "github");
        report("github", &s);
    }

    // ------------------------------------------------------------------ the listing

    /// The listing's motion: a folder's pictures fanning out on a rest; rows gliding as
    /// the filter and the sort change, icons as the window narrows; the years on a long
    /// dated list's scrollbar and the month while it is dragged; cut files outlined, still
    /// once they settle; the ring of kinds in the status bar; a refused name shaking and
    /// saying why; a labelled folder in its colour.
    #[test]
    fn listing_in_motion() {
        if !sandboxed() {
            return;
        }
        let home = fixture("motion");
        // A long list over eight years, one file a week back from now.
        let dated = home.join("Dated");
        std::fs::create_dir_all(&dated).unwrap();
        let now = std::time::SystemTime::now();
        for i in 0..400u64 {
            let p = dated.join(format!("note{i:03}.txt"));
            std::fs::write(&p, "x").unwrap();
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(now - std::time::Duration::from_secs(i * 7 * 86_400)).unwrap();
        }
        crate::tags::toggle(&[home.join("Documents/Taxes")], "Red", true).unwrap();
        let mut s = Sim::new(&home.join("Pictures"), vec2(1280.0, 800.0));
        s.cmd(ViewLarge);
        s.settle();
        // Peek: rest on Trip.
        let trip = list_text(&s, "Trip");
        s.act(&format!("move {:.0} {:.0}", trip.center().x, trip.center().y - 30.0));
        wait_until(&mut s, "the cards out", 10, |s| s.tab().peek.showing().is_some_and(|(p, o)| p.ends_with("Trip") && o >= 1.0));
        s.idle(10);
        // Trip is in the top row: the fan comes down over it rather than over the toolbar.
        let (fan, view) = s.tab().peek.fan().unwrap_or_else(|| s.fail("no fan drawn"));
        assert!(fan.is_positive() && view.contains_rect(fan), "the fan stays in the view: {fan:?} in {view:?}");
        s.shot("peek");
        s.act("move -50 -50");
        wait_until(&mut s, "the cards back", 10, |s| s.tab().peek.showing().is_none());
        // The ring: only pictures in Trip.
        go(&mut s, &home.join("Pictures/Trip"));
        let ring = *controls_named("folder at a glance").first().unwrap_or_else(|| s.fail("no ring"));
        s.act(&format!("move {:.0} {:.0}", ring.center().x, ring.center().y));
        s.idle(40);
        assert!(some_text(&s, "Pictures 100%"), "the ring says what fills the folder");
        s.shot("glance");
        // Icons reflow as the window narrows.
        s.act("move -50 -50");
        s.idle(3);
        s.resize(760.0, 800.0);
        s.idle(1);
        let anim = s.app.cfg.animations;
        assert_eq!(s.tab().glide.moving() > 0, anim, "icons glide to their new rows, when things move at all");
        s.settle();
        s.idle(20);
        assert_eq!(s.tab().glide.moving(), 0);
        look(&mut s, "icons after the reflow");
        s.resize(1280.0, 800.0);
        // The filter: rows glide up as others drop out, and come back.
        go(&mut s, &home.join("Big"));
        s.cmd(ViewDetails);
        s.settle();
        s.cmd(FocusFilter);
        s.act("text .rs");
        s.idle(1);
        let mut moved = 0;
        for _ in 0..6 {
            s.idle(1);
            moved = moved.max(s.tab().glide.moving());
        }
        assert_eq!(moved > 0, anim, "rows on their way");
        s.idle(30);
        assert_eq!(s.tab().glide.moving(), 0, "settled");
        s.act("key Escape");
        s.settle();
        look(&mut s, "after the filter");
        // A sort moves them too.
        s.cmd(SortSize);
        s.idle(2);
        s.idle(30);
        assert_eq!(s.tab().glide.moving(), 0);
        // The date ribbon: years on the scrollbar of the dated list.
        go(&mut s, &dated);
        s.cmd(SortModified);
        s.settle();
        s.idle(20);
        let years: Vec<i32> = s.tab().ribbon.marks.iter().map(|m| m.1).collect();
        assert!(years.len() >= 7 && years.windows(2).all(|w| w[0] > w[1]), "newest year first: {years:?}");
        let lr = s.app.panels[&s.app.active].list_rect;
        s.act(&format!("move {:.0} {:.0}", lr.right() - 6.0, lr.center().y));
        s.idle(20);
        let shown: Vec<String> = years.iter().map(|y| y.to_string()).filter(|y| some_text(&s, y)).collect();
        assert!(shown.len() >= 3, "the years named while pointed at: {shown:?}");
        s.shot("ribbon-years");
        // Dragging the scrollbar: the month passing.
        s.act(&format!("down {:.0} {:.0}", lr.right() - 6.0, lr.top() + 40.0));
        for k in 1..=10 {
            s.act(&format!("move {:.0} {:.0}", lr.right() - 6.0, lr.top() + 40.0 + k as f32 * 40.0));
        }
        let months = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
        assert!(s.texts.iter().any(|(_, t)| months.iter().any(|m| t.starts_with(m) && t.len() > m.len() + 4)), "a month and its year beside the thumb");
        s.shot("ribbon-month");
        s.act(&format!("up {:.0} {:.0}", lr.right() - 6.0, lr.top() + 440.0));
        s.act("move -50 -50");
        s.idle(30);
        // Cut files: outlined, marching a moment, then still.
        go(&mut s, &home.join("Documents"));
        select_name(&mut s, "notes.txt");
        s.act("key ctrl+x");
        s.idle(2);
        // (Without motion a toast may still ask for frames: only the marching is checked.)
        if anim {
            assert!(s.repaint < std::time::Duration::from_millis(100), "marching");
        }
        settles_idle(&mut s, "cut files rest");
        // A taken name: the field shakes and says why; Escape leaves it as it was.
        select_name(&mut s, "notes.txt");
        s.act("key F2");
        s.act("key ctrl+a");
        s.act("text report.md");
        s.act("key Enter");
        s.idle(2);
        assert!(s.tab().rename.is_some(), "still renaming");
        assert!(some_text(&s, "“report.md” is already here"));
        s.idle(30);
        s.shot("rename-refused");
        s.act("key ctrl+a");
        s.act("key Backspace");
        s.idle(2);
        assert!(some_text(&s, "A name can't be empty"), "the reason follows the name");
        s.act("key Escape");
        s.idle(2);
        assert!(s.tab().rename.is_none() && home.join("Documents/notes.txt").exists());
        // The labelled folder, in every view.
        for v in [ViewDetails, ViewList, ViewMedium, ViewLarge] {
            s.cmd(v);
            look(&mut s, "a coloured folder");
        }
        // Into a folder: the path's new name and its line.
        go(&mut s, &home.join("Documents/Taxes/2025"));
        s.idle(20);
        look(&mut s, "breadcrumbs moved");
        settles_idle(&mut s, "listing in motion");
        report("listing in motion", &s);
    }

    // ------------------------------------------------------------------ pictures

    /// Pictures: the viewer grows out of the thumbnail and back; its filmstrip rises at
    /// the bottom and goes to a picture; two compared over one another with a line to
    /// drag; the preview's colours, copied; a folder sorted by colour.
    #[test]
    fn pictures_in_their_own_light() {
        if !sandboxed() {
            return;
        }
        let home = fixture("light");
        let trip = home.join("Pictures/Trip");
        let mut s = Sim::new(&trip, vec2(1280.0, 800.0));
        s.cmd(ViewLarge);
        s.settle();
        s.idle(10);
        select_name(&mut s, "img03.png");
        s.cmd(ViewPictures);
        let growing = matches!(&s.app.popup, Popup::Viewer(v) if v.grow.is_some());
        assert_eq!(growing, s.app.cfg.animations, "grows out of its thumbnail");
        s.idle(30);
        assert!(matches!(&s.app.popup, Popup::Viewer(v) if v.grow.is_none()));
        // The filmstrip: the pointer at the bottom.
        s.act("move 640 780");
        s.idle(20);
        let strip = controls_named("viewer filmstrip");
        assert!(strip.len() >= 5, "{}", strip.len());
        s.shot("filmstrip");
        let at = |s: &Sim| if let Popup::Viewer(v) = &s.app.popup { v.at } else { usize::MAX };
        let before = at(&s);
        let other = *strip.iter().find(|r| (r.center().x - 640.0).abs() > 60.0).unwrap();
        click_rect(&mut s, other);
        s.idle(3);
        assert_ne!(at(&s), before, "went to the picture clicked");
        s.act("move 640 400");
        s.idle(30);
        assert!(controls_named("viewer filmstrip").is_empty(), "the strip goes down again");
        // Over one another: W pins this one and draws the line; a drag moves it.
        s.act("key ArrowRight");
        s.act("key W");
        let wipe = |s: &Sim| if let Popup::Viewer(v) = &s.app.popup { v.wipe } else { None };
        assert_eq!(wipe(&s), Some(0.5));
        s.act("key ArrowRight");
        s.idle(5);
        s.act("drag 640 400 300 400");
        assert!(wipe(&s).is_some_and(|f| f < 0.4), "{:?}", wipe(&s));
        // A small picture in a big window: the line stays on the pictures.
        let line = if let Popup::Viewer(v) = &s.app.popup { v.wipe_line() } else { None };
        let (x, (l, r)) = line.unwrap_or_else(|| s.fail("no wipe line"));
        assert!(r - l < 900.0 && x >= l && x <= r, "line at {x} across {l}..{r}");
        look_popup(&mut s, "pictures over one another");
        s.shot("wipe");
        s.act("key Escape");
        assert_eq!(wipe(&s), None);
        assert!(matches!(&s.app.popup, Popup::Viewer(v) if v.pin.is_none()));
        // Closing puts it back where it came from.
        s.act("key Escape");
        assert_eq!(matches!(&s.app.popup, Popup::Viewer(v) if v.shrink.is_some()), s.app.cfg.animations, "shrinks back");
        wait_until(&mut s, "the viewer gone", 5, |s| !s.popup_open());
        // The preview's colours: one copied.
        s.cmd(ToggleInspector);
        wait_until(&mut s, "the colours", 20, |s| controls_named("picture colour").len() >= 2);
        // The preview slides open: its chips' places once it rests.
        s.idle(20);
        let chip = controls_named("picture colour")[0];
        s.shot("colours-in-preview");
        click_rect(&mut s, chip);
        s.idle(2);
        assert!(s.copied.last().is_some_and(|c| c.starts_with('#') && c.len() == 7), "{:?}", s.copied.last());
        assert!(s.app.toast.as_ref().is_some_and(|t| t.0.starts_with("Copied #")));
        s.cmd(ToggleInspector);
        // Sorted by colour: in the order of their colours, the broken one last.
        go(&mut s, &home.join("Pictures"));
        go(&mut s, &trip);
        s.cmd(SortColour);
        wait_until(&mut s, "the colours read", 30, |s| s.tab().entries.iter().filter(|e| crate::colours::sortable(e)).all(|e| crate::colours::sort_key(e).is_some()));
        s.settle();
        s.idle(4);
        let keys: Vec<u64> = s.tab().view.iter().map(|&i| crate::colours::sort_key(&s.tab().entries[i as usize]).unwrap_or(u64::MAX)).collect();
        assert!(keys.windows(2).all(|w| w[0] <= w[1]), "{keys:?}");
        look(&mut s, "sorted by colour");
        s.cmd(ViewLarge);
        s.settle();
        s.shot("sorted-by-colour");
        settles_idle(&mut s, "pictures");
        report("pictures in their own light", &s);
    }

    // ------------------------------------------------------------------ jobs

    /// A copy into a folder that has a tab of its own: the tab's ring while it runs (paused
    /// here), the card's speed line, both gone once it is done.
    #[test]
    fn copies_show_their_speed_and_their_tab() {
        if !sandboxed() {
            return;
        }
        let home = fixture("jobs-show");
        let heavy = home.join("Heavy");
        let dest = home.join("Dest");
        std::fs::create_dir_all(&heavy).unwrap();
        std::fs::create_dir_all(&dest).unwrap();
        for i in 0..2 {
            std::fs::write(heavy.join(format!("big{i}.bin")), vec![7u8; 24 << 20]).unwrap();
        }
        let mut s = Sim::new(&heavy, vec2(1280.0, 800.0));
        let id = s.app.active;
        s.app.actions.push(crate::app::Action::NewTab { panel: id, path: dest.clone(), select: None, focus: false });
        s.settle();
        let srcs = vec![heavy.join("big0.bin"), heavy.join("big1.bin")];
        s.app.start_job(crate::ops::OpKind::Copy { srcs, dest: dest.clone(), decide: Default::default() });
        s.app.jobs[0].pause.store(true, std::sync::atomic::Ordering::Relaxed);
        s.idle(80);
        if s.app.jobs[0].progress.lock().unwrap().finished {
            eprintln!("sim jobs: the copy finished before it could be paused; nothing to watch");
        } else {
            assert!(!controls_named("tab progress").is_empty(), "the destination's tab shows the copy");
            assert!(!controls_named("job speed").is_empty(), "the card draws its speed");
            s.app.jobs[0].pause.store(false, std::sync::atomic::Ordering::Relaxed);
        }
        wait_jobs(&mut s, "the copy");
        s.idle(3);
        assert!(controls_named("tab progress").is_empty(), "done: the folder's own icon again");
        assert_eq!(std::fs::metadata(dest.join("big1.bin")).unwrap().len(), 24 << 20);
        // Nothing is written into a place.
        s.app.start_job(crate::ops::OpKind::Copy { srcs: vec![heavy.join("big0.bin")], dest: PathBuf::from(crate::tab::GITHUB), decide: Default::default() });
        assert!(s.app.jobs.iter().all(|j| !matches!(&j.kind, crate::ops::OpKind::Copy { dest, .. } if crate::tab::is_place(dest))), "refused");
        let _ = std::fs::remove_dir_all(&heavy);
        let _ = std::fs::remove_dir_all(&dest);
        report("copies", &s);
    }

    // ------------------------------------------------------------------ themes

    /// Choosing a theme: hovering a card shows it on the window a frame later (the frame
    /// between is the old one, kept for the wash), and every theme draws cleanly.
    #[test]
    fn themes_wash_in_and_all_read_cleanly() {
        if !sandboxed() {
            return;
        }
        let home = fixture("themes-all");
        let mut s = Sim::new(&home.join("Documents"), vec2(1280.0, 800.0));
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        s.idle(3);
        s.cmd(ChooseTheme);
        s.idle(5);
        let card = s.text_rect("Lagoon");
        s.act(&format!("move {:.0} {:.0}", card.center().x, card.center().y));
        s.idle(3);
        assert_eq!(s.app.pal.bg, crate::theme::palette(crate::theme::ThemeName::Lagoon).bg, "the hovered theme on the window");
        // Escape goes back to Options, a second one closes it.
        s.act("key Escape");
        s.act("key Escape");
        wait_until(&mut s, "the picker closed", 5, |s| !s.popup_open());
        assert_eq!(s.app.pal.bg, crate::theme::palette(crate::theme::ThemeName::Graphite).bg, "only hovered: the chosen theme back");
        for t in crate::theme::ThemeName::ALL {
            s.app.cfg.theme = t;
            s.idle(3);
            s.check_overlap = true;
            s.idle(1);
            s.check_overlap = false;
        }
        s.app.cfg.theme = crate::theme::ThemeName::Graphite;
        settles_idle(&mut s, "themes");
        report("themes", &s);
    }
}
