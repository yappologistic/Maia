//! Maia: a fast, keyboard-first file manager for Linux.

mod admin;
mod anim;
mod app;
mod appearance;
mod checksum;
mod codesearch;
mod archive;
mod archwrite;
mod blame;
mod books;
mod avatars;
mod batchname;
mod bigfile;
mod findbar;
mod quickedit;
mod buildclean;
mod update;
mod versions;
mod viewer;
mod wallpaper;
mod clipboard;
mod columns;
mod commands;
mod compare;
mod contentsearch;
mod datatree;
mod envfile;
mod errands;
mod exif;
mod facts;
mod extpic;
mod config;
mod copyto;
mod convert;
mod devtools;
mod drives;
mod dupes;
mod editor;
mod ctxmenu;
mod filediff;
mod fontview;
mod formatdrive;
mod fsx;
mod fuzzy;
mod gitchanges;
mod gitcompare;
mod gitfind;
mod gitlog;
mod gitstatus;
mod gitsync;
mod gitwork;
mod holders;
mod icons;
mod imgdec;
mod inspector;
mod install;
mod listing;
mod markdown;
mod media;
mod lucide_map;
mod mime;
mod musictags;
mod namefind;
mod network;
mod office;
mod ollama;
mod ops;
mod panel;
mod perms;
mod picfix;
mod picker;
mod popups;
mod portal;
mod project;
mod projects;
mod fm1;
mod quickopen;
mod redo;
mod undofile;
mod undohist;
mod hints;
mod marks;
mod query;
mod rename;
mod runner;
mod selmatch;
mod semantic;
mod sendto;
mod share;
mod shelf;
mod sidebar;
mod sidediff;
mod smart;
mod signals;
mod sqlite;
mod single;
mod spaceuse;
mod svg;
mod syntax;
mod tags;
mod textsel;
mod tasks;
mod terminal;
mod termlink;
mod stats;
mod tab;
mod table;
mod texmath;
mod tail;
#[cfg(any(test, feature = "testkit"))]
mod testkit;
#[cfg(test)]
mod sim;
mod timefmt;
mod trashcan;
mod theme;
pub mod rtl;
mod tidy;
mod tour;
mod thumbcache;
mod thumbs;
mod ui;
mod vt;
mod watch;
mod wlclip;
mod xclip;

#[cfg(test)]
mod tests;

use std::path::PathBuf;



const HELP: &str = "Maia: fast, keyboard-first file manager

Usage: maia [OPTIONS] [PATH...]

Options:
  --install        Install to ~/.local/bin and add a launcher entry
  --set-default    Make Maia the default file manager (inode/directory)
  --set-picker     Open and save files for other apps (the desktop portal's dialog)
  --unset-picker   Give the Open and Save dialog back to the desktop
  --uninstall      Remove the launcher entry and installed binary
  --update         Update to the latest release on GitHub, if there is a newer one
  --window PATH    Open PATH in a new window
  -h, --help       Show this help
  -V, --version    Show the version
";

fn main() {
    let mut paths: Vec<PathBuf> = vec![];
    let mut new_window = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return;
            }
            "-V" | "--version" => {
                println!("maia {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--update" => {
                match update::update_now() {
                    Ok(m) => println!("{m}"),
                    Err(e) => {
                        eprintln!("maia: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            "--install" | "--set-default" | "--uninstall" => {
                let r = match a.as_str() {
                    "--install" => install::install(),
                    "--set-default" => install::set_default(),
                    _ => install::uninstall(),
                };
                match r {
                    Ok(m) => println!("{m}"),
                    Err(e) => {
                        eprintln!("maia: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            "--set-picker" | "--unset-picker" => {
                match portal::set(a == "--set-picker") {
                    Ok(m) => println!("{m}"),
                    Err(e) => {
                        eprintln!("maia: {e}");
                        std::process::exit(1);
                    }
                }
                return;
            }
            // The Open and Save dialog's backend, started by the session bus.
            "--portal" => {
                if !portal::run_backend() {
                    eprintln!("maia: no session bus, or the portal backend is already running");
                    std::process::exit(1);
                }
                return;
            }
            // One Open or Save dialog: the request on standard input, the answer on output.
            "--pick" => {
                pick();
                return;
            }
            "--which" => {
                // Diagnostics: how each file would be opened.
                for p in args.by_ref() {
                    let (label, mime, app, argv) = mime::describe_open(std::path::Path::new(&p));
                    println!("{p}\n  type: {label} ({mime})\n  app:  {app}\n  cmd:  {}", argv.join(" "));
                }
                return;
            }
            "--window" => {
                new_window = true;
                if let Some(p) = args.next() {
                    paths.push(PathBuf::from(p));
                }
            }
            other => {
                // Accept plain paths and file:// URIs (from %U).
                let p = clipboard::uri_to_path(other).unwrap_or_else(|| PathBuf::from(other));
                let p = if p.is_absolute() { p } else { std::env::current_dir().unwrap_or_default().join(p) };
                paths.push(p);
            }
        }
    }
    // A launch that isn't asking for its own window hands its folders to the open one.
    let listener = if new_window || !config::load_config().single_instance {
        None
    } else {
        let sock = single::socket_path();
        if single::forward_to(&sock, &paths) {
            return;
        }
        single::claim_at(&sock).map(|l| (l, sock))
    };
    std::thread::spawn(|| {
        install::refresh_icon();
        portal::refresh();
    });
    runner::run(
        runner::Options { title: "Maia", app_id: if cfg!(feature = "testkit") { "io.github.yappologistic.Maia.test" } else { "io.github.yappologistic.Maia" }, size: [1240.0, 780.0], min_size: [520.0, 340.0], icon: install::window_icon(), picker: None },
        paths,
        listener,
    );
}

/// A picker window for the request on standard input; its answer goes to standard
/// output once it closes.
fn pick() {
    use std::io::Read as _;
    use std::os::fd::FromRawFd as _;
    let mut req = vec![];
    let _ = std::io::stdin().take(1 << 20).read_to_end(&mut req);
    // The answer goes out on a copy of the pipe that programs started from the window
    // (an app opened from it, a thumbnailer) do not inherit: the backend reads until
    // every writer has gone, and one still running would hold the answer back.
    let answer = unsafe { libc::fcntl(1, libc::F_DUPFD_CLOEXEC, 3) };
    if answer < 0 {
        return;
    }
    unsafe {
        let null = libc::open(c"/dev/null".as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC);
        if null >= 0 {
            libc::dup2(null, 1);
            libc::close(null);
        }
    }
    let p = picker::Picker::new(portal::Pick::decode(&req));
    picker::begin();
    picker::warm(&p.pick);
    let start = p.start();
    // A class of its own, for window rules: a compositor can float and size dialogs.
    let title: &'static str = Box::leak(p.title().into_boxed_str());
    let app_id = if cfg!(feature = "testkit") { "io.github.yappologistic.Maia.test" } else { "io.github.yappologistic.Maia.Picker" };
    runner::run(runner::Options { title, app_id, size: [980.0, 640.0], min_size: [520.0, 340.0], icon: install::window_icon(), picker: Some(Box::new(p)) }, vec![start], None);
    picker::write_answer(&mut unsafe { std::fs::File::from_raw_fd(answer) });
}
