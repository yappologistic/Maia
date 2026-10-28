# Maia

A fast, keyboard-first file manager for Linux, written in Rust with egui. One binary
of under 5 MB, drawn on the GPU, with split panels, tabs, live filtering, previews and
background file operations.

![Maia: a project's source files beside its git history](docs/maia.webp)

<p align="center">
  <a href="https://buymeacoffee.com/e_gurl">
    <img src="https://cdn.buymeacoffee.com/buttons/v2/default-yellow.png" alt="Support Maia on Buy Me a Coffee" width="217" height="60">
  </a>
</p>

## Install

You need a Wayland or X11 desktop with OpenGL and glibc 2.35 or newer (Ubuntu 22.04,
Debian 12, Fedora 36 and later). The
[latest release](https://github.com/yappologistic/Maia/releases/latest) has each of
these for x86_64 and ARM64:

| Package | For |
|---|---|
| `maia_VERSION_amd64.deb` | Debian, Ubuntu, Mint, Pop!_OS: `sudo apt install ./maia_*.deb` |
| `maia-VERSION-1.x86_64.rpm` | Fedora, RHEL, openSUSE: `sudo dnf install ./maia-*.rpm` |
| `maia-VERSION-1-x86_64.pkg.tar.zst` | Arch, Manjaro, EndeavourOS: `sudo pacman -U maia-*.pkg.tar.zst` |
| `Maia-VERSION-x86_64.AppImage` | any distribution: make it executable and run it |
| `maia-VERSION-linux-x86_64.tar.gz` | unpack and run `install.sh` (to `~/.local`, or `/usr/local` as root) |
| `maia-linux-x86_64` | the program alone: run it with `--install` |

To build it yourself you need Rust 1.88 or newer:

```bash
git clone https://github.com/yappologistic/Maia.git
cd Maia
cargo build --release
./target/release/maia --install
```

`--install` puts Maia in `~/.local/bin` and your app launcher, and `--uninstall` takes
it out again. To have other apps open folders in Maia, turn on Options > Startup &
system > Default file manager, or run `maia --set-default`.

Maia can also be the Open and Save dialog of apps that ask the desktop portal for a
file: turn on Options > Startup & system > Open and save dialog, or run
`maia --set-picker` (`--unset-picker` gives it back). Chromium, Electron, Flatpak apps
and GTK 4.18 or later ask the portal by themselves; Firefox does with
`widget.use-xdg-desktop-portal.file-picker` set to 1 in `about:config`, GTK 3 apps with
`GTK_USE_PORTAL=1` and Qt apps with `QT_QPA_PLATFORMTHEME=xdgdesktopportal`.
xdg-desktop-portal older than 1.20.1 finds Maia only when it came from the .deb or .rpm.
The dialog's window class is `io.github.yappologistic.Maia.Picker`, for a window rule
that floats it.

Maia uses these when they are installed and works without them: `bsdtar` and `7z`
(archives), `ffmpeg` (video, sound, music tags), poppler (PDFs), LibreOffice (Office
documents), ImageMagick (editing and converting pictures), `git`, `gio` (network
places), `udisksctl` (drives), `pkexec` (administrator retries), `sqlite3`, TeX
(formulas in Markdown), `ollama` (search by meaning) and your desktop's thumbnailers.

## Use

```bash
maia [PATH...]
```

Start typing to filter the folder you are in. The filter also takes conditions:
`size>100M`, `modified<7d` (and `created`, `accessed`), `tag:red`, `type:pictures`,
`ext:jpg,png`, `owner:root`, `*.rs`, `/^IMG_\d+/`, a `"quoted phrase"`. `a | b` (or
`OR`) is either, `-draft` (or `NOT`) leaves out, and parentheses group. Save As Smart
Folder keeps the whole filter in the sidebar. Every command is in the palette,
Ctrl+Shift+P, with its hotkey, and every hotkey can be changed there. Some to start
with:

| Action | Keys |
|---|---|
| Go to a place or path | Ctrl+P, Ctrl+L |
| A folder you use often, by a few letters | Ctrl+G |
| New tab, close tab, reopen | Ctrl+T, Ctrl+W, Ctrl+Shift+T |
| Split right, split below, switch panel | Ctrl+Shift+S, Ctrl+S, Tab |
| Preview, pictures full window, on a calendar | Space, F3, Shift+F3 |
| Find in the preview, edit it there | Ctrl+F, Shift+F4 |
| Terminal under the files | Ctrl+` |
| Find anywhere in home, in this project | Ctrl+K, Ctrl+O |
| Every file below this folder | Ctrl+E |
| Only what git marks as changed | Ctrl+Shift+G |
| Recent git projects | Ctrl+Alt+P |
| Rename, several at once | F2 |
| Undo, and its history | Ctrl+Z, Ctrl+Alt+Z |
| Click anything by its letters | Ctrl+J |
| Options | Ctrl+, |

The first start shows a short tour, and the first start after an update shows what is
new; both are in the palette too. Opening a folder from another app while Maia runs
adds a tab to that window.

Options > Startup & system > Check for updates fetches a newer release, checks it
against its checksum and runs it once before swapping it in; `maia --update` does the
same. Maia never checks on its own. An install from the .deb, .rpm or pacman package gets the new
package through your package manager, after your password; an AppImage replaces itself. Settings live in `~/.config/maia/config.json`.

## When something goes wrong

Maia writes a line to `~/.local/state/maia/crash.log` when it crashes or cannot
start, and shows a notification if it fails before the window opens. Run it from a
terminal to see the same messages. Include that log when you report a problem.

## Development

```bash
cargo test           # unit tests
tools/sim.sh         # simulated users, in a sandbox (needs bubblewrap)
tools/window.sh      # a real window driven through a pipe, for checking by eye
./build-dist.sh      # the small release build (nightly, rust-src and lld)
```

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). The interface icons are the
[Solar](https://github.com/saoudi-h/solar-icons) set by 480 Design, under
[CC BY 4.0](https://creativecommons.org/licenses/by/4.0/). The bundled fonts and icons keep
their own licenses, in `assets/`, and so do the Rust crates Maia is built from:
[assets/THIRD-PARTY-NOTICES.txt](assets/THIRD-PARTY-NOTICES.txt) lists each with its
license text (`tools/notices.py` writes it).
