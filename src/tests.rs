//! Unit tests for the core (non-UI) logic.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::commands::{self, Cmd, Keymap};
use crate::config::{SortKey, ViewSettings};
use crate::fuzzy;
use crate::ops::{self, Conflict, OpKind};
use crate::panel::{Node, Side};
use crate::rename::{Outcome, Rename};
use crate::tab::{self, Tab, ViewCtx};

fn tmpdir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("maia-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn touch(p: &Path, content: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, content).unwrap();
}

#[test]
fn fuzzy_ranks_prefix_and_boundaries() {
    let q = fuzzy::Query::new("dl");
    assert!(fuzzy::score(&q, "downloads").is_some());
    assert!(fuzzy::score(&q, "xyz").is_none());
    let pre = fuzzy::score(&fuzzy::Query::new("doc"), "documents").unwrap();
    let mid = fuzzy::score(&fuzzy::Query::new("doc"), "my_old_doc_backup").unwrap();
    assert!(pre > mid, "prefix should outrank middle: {pre} vs {mid}");
    let (_, pos) = fuzzy::positions(&fuzzy::Query::new("rmd"), "README.md").unwrap();
    assert_eq!(pos.len(), 3);
    assert_eq!(pos[0], 0);
}

#[test]
fn natural_sort_orders_numbers() {
    let mut v = vec!["file10", "file2", "file1", "File3", "file02"];
    v.sort_by(|a, b| tab::natural_cmp(&a.to_lowercase(), &b.to_lowercase()));
    assert_eq!(v, vec!["file1", "file2", "file02", "File3", "file10"]);
}

#[test]
fn rename_selects_stem_and_mirrors_edits() {
    let items = vec![(0, PathBuf::from("/x/01 alpha.txt"), "01 alpha.txt".to_string()), (1, PathBuf::from("/x/02 beta.txt"), "02 beta.txt".to_string())];
    let mut r = Rename::new(items, false);
    assert_eq!(r.fields[0].selection(), (0, 8)); // stem only
    // Home, then shift+right twice selects "01"/"02"; delete removes them everywhere.
    use egui::{Event, Key, Modifiers};
    let key = |k: Key, m: Modifiers| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: m };
    let mut clip = None;
    let ev = vec![key(Key::Home, Modifiers::NONE), key(Key::ArrowRight, Modifiers::SHIFT), key(Key::ArrowRight, Modifiers::SHIFT), key(Key::Delete, Modifiers::NONE)];
    assert_eq!(r.handle(&ev, &mut clip), Outcome::Continue);
    assert_eq!(r.fields[0].text_string(), " alpha.txt");
    assert_eq!(r.fields[1].text_string(), " beta.txt");
    r.insert_incremental_id();
    assert_eq!(r.fields[0].text_string(), "1 alpha.txt");
    assert_eq!(r.fields[1].text_string(), "2 beta.txt");
    // Multi-line paste distributes lines.
    r.select_all();
    r.paste("a.txt\nb.txt");
    assert_eq!(r.fields[0].text_string(), "a.txt");
    assert_eq!(r.fields[1].text_string(), "b.txt");
    let ev = vec![key(Key::Enter, Modifiers::NONE)];
    assert_eq!(r.handle(&ev, &mut clip), Outcome::Commit);
}

#[test]
fn rename_detects_conflicts_and_swaps_safely() {
    let d = tmpdir("rename");
    touch(&d.join("a.txt"), "A");
    touch(&d.join("b.txt"), "B");
    touch(&d.join("c.txt"), "C");
    // Swap a <-> b (allowed: both are being renamed).
    let items = vec![(0, d.join("a.txt"), "a.txt".into()), (1, d.join("b.txt"), "b.txt".into())];
    let mut r = Rename::new(items, false);
    r.select_all();
    r.paste("b.txt\na.txt");
    assert!(r.conflicts().is_empty());
    let errs = crate::rename::apply(&r.plan());
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "B");
    assert_eq!(std::fs::read_to_string(d.join("b.txt")).unwrap(), "A");
    // Renaming onto an existing, unrelated file is a conflict.
    let mut r = Rename::new(vec![(0, d.join("a.txt"), "a.txt".into())], false);
    r.select_all();
    r.insert_all("c.txt");
    assert!(!r.conflicts().is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn hotkeys_parse_and_lookup() {
    let b = commands::parse_binding("Ctrl+Shift+P").unwrap();
    assert_eq!(commands::binding_to_string(&b), "Ctrl+Shift+P");
    let seq = commands::parse_binding("G, H").unwrap();
    assert_eq!(seq.len(), 2);
    let km = Keymap::build(&Default::default());
    let (hit, _) = km.lookup(&commands::parse_binding("Ctrl+Shift+P").unwrap());
    assert_eq!(hit, Some(Cmd::Palette));
    let (hit, _) = km.lookup(&commands::parse_binding("Ctrl+E").unwrap());
    assert_eq!(hit, Some(Cmd::ToggleFlatten));
    // Every default binding parses, and ids are unique.
    let mut ids = HashSet::new();
    for &c in commands::ALL {
        assert!(ids.insert(c.info().id), "duplicate id {}", c.info().id);
        for k in c.info().keys {
            assert!(commands::parse_binding(k).is_some(), "bad default key {k} for {}", c.info().id);
        }
    }
    // Overrides with a sequence.
    let mut ov = std::collections::BTreeMap::new();
    ov.insert("home".to_string(), vec!["G, H".to_string()]);
    let km = Keymap::build(&ov);
    let g = commands::parse_binding("G").unwrap();
    let (exact, longer) = km.lookup(&g);
    assert!(exact.is_none() && longer);
}

#[test]
fn default_hotkeys_have_no_conflicts() {
    let km = Keymap::build(&Default::default());
    let mut seen: HashMap<String, Cmd> = HashMap::new();
    for &c in commands::ALL {
        for b in km.bindings(c) {
            let s = commands::binding_to_string(b);
            if let Some(o) = seen.insert(s.clone(), c) {
                panic!("{s} bound to both {:?} and {:?}", o, c);
            }
        }
    }
}

#[test]
fn split_tree_operations() {
    let mut t = Node::Leaf(1);
    assert!(t.split(1, 2, Side::Right));
    assert!(t.split(2, 3, Side::Bottom));
    let mut leaves = vec![];
    t.leaves(&mut leaves);
    assert_eq!(leaves, vec![1, 2, 3]);
    assert!(t.remove(2));
    leaves.clear();
    t.leaves(&mut leaves);
    assert_eq!(leaves, vec![1, 3]);
    assert!(t.remove(1));
    assert!(matches!(t, Node::Leaf(3)));
}

#[test]
fn uri_roundtrip() {
    let p = PathBuf::from("/home/u/My Files/naïve #1.txt");
    let u = crate::clipboard::path_to_uri(&p);
    assert!(u.starts_with("file:///home/u/My%20Files/"));
    assert_eq!(crate::clipboard::uri_to_path(&u).unwrap(), p);
    assert_eq!(crate::clipboard::quote_if_needed("/a/b"), "/a/b");
    assert_eq!(crate::clipboard::quote_if_needed("/a b/it's"), "'/a b/it'\\''s'");
}

#[test]
fn unique_names() {
    let d = tmpdir("unique");
    touch(&d.join("x.txt"), "");
    assert_eq!(crate::fsx::unique_dest(&d, "x.txt"), d.join("x (1).txt"));
    assert_eq!(crate::fsx::unique_dest(&d, "y.txt"), d.join("y.txt"));
    std::fs::create_dir(d.join("New folder")).unwrap();
    assert_eq!(crate::fsx::unique_dest(&d, "New folder"), d.join("New folder (1)"));
    let _ = std::fs::remove_dir_all(&d);
}

fn wait_job(j: &ops::Job) {
    let start = Instant::now();
    while !j.progress.lock().unwrap().finished {
        assert!(start.elapsed() < Duration::from_secs(10), "job timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn copy_move_merge_and_conflicts() {
    let ctx = egui::Context::default();
    let d = tmpdir("ops");
    let src = d.join("src");
    let dst = d.join("dst");
    touch(&src.join("a.txt"), "new a");
    touch(&src.join("sub/b.txt"), "b");
    touch(&dst.join("a.txt"), "old a");
    touch(&dst.join("sub/keep.txt"), "keep");
    // Keep both
    let j = ops::spawn(1, OpKind::Copy { srcs: vec![src.join("a.txt")], dest: dst.clone(), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    assert_eq!(std::fs::read_to_string(dst.join("a (1).txt")).unwrap(), "new a");
    assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "old a");
    // Overwrite + merge folders
    let mut decide = HashMap::new();
    decide.insert(src.join("a.txt"), Conflict::Overwrite);
    decide.insert(src.join("sub"), Conflict::Merge);
    let j = ops::spawn(2, OpKind::Copy { srcs: vec![src.join("a.txt"), src.join("sub")], dest: dst.clone(), decide }, ctx.clone());
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty(), "{:?}", j.progress.lock().unwrap().errors);
    assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new a");
    assert!(dst.join("sub/keep.txt").exists() && dst.join("sub/b.txt").exists());
    // Skip
    let mut decide = HashMap::new();
    decide.insert(src.join("a.txt"), Conflict::Skip);
    touch(&src.join("a.txt"), "newer a");
    let j = ops::spawn(3, OpKind::Copy { srcs: vec![src.join("a.txt")], dest: dst.clone(), decide }, ctx.clone());
    wait_job(&j);
    assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new a");
    // Move into a fresh folder
    let other = d.join("other");
    std::fs::create_dir(&other).unwrap();
    let j = ops::spawn(4, OpKind::Move { srcs: vec![src.join("sub")], dest: other.clone(), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    assert!(other.join("sub/b.txt").exists() && !src.join("sub").exists());
    assert_eq!(j.progress.lock().unwrap().produced.len(), 1);
    // Copying a folder into itself is refused.
    let j = ops::spawn(5, OpKind::Copy { srcs: vec![other.clone()], dest: other.join("sub"), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    assert!(!j.progress.lock().unwrap().errors.is_empty());
    // Permanent delete
    let j = ops::spawn(6, OpKind::Delete { paths: vec![other.clone()] }, ctx);
    wait_job(&j);
    assert!(!other.exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// A copy keeps what the file carries besides its bytes: tags (extended attributes),
/// read-only files' tags too, folder dates, and big files whole.
#[test]
fn copy_keeps_tags_dates_and_bytes() {
    let ctx = egui::Context::default();
    let d = tmpdir("xattr");
    let src = d.join("src");
    touch(&src.join("sub/a.txt"), "a");
    let big: Vec<u8> = (0..5_000_000u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(src.join("big.bin"), &big).unwrap();
    if crate::tags::write(&src.join("sub/a.txt"), &["red".into(), "work".into()]).is_err() {
        return; // this file system keeps no tags
    }
    crate::tags::write(&src.join("sub"), &["blue".into()]).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(src.join("sub/a.txt"), std::fs::Permissions::from_mode(0o444)).unwrap();
    let old = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_600_000_000);
    std::fs::File::open(src.join("sub")).unwrap().set_modified(old).unwrap();
    let out = d.join("out");
    std::fs::create_dir(&out).unwrap();
    let j = ops::spawn(1, OpKind::Copy { srcs: vec![src.join("sub"), src.join("big.bin")], dest: out.clone(), decide: HashMap::new() }, ctx);
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty(), "{:?}", j.progress.lock().unwrap().errors);
    assert_eq!(crate::tags::read(&out.join("sub/a.txt")), vec!["red".to_string(), "work".to_string()]);
    assert_eq!(crate::tags::read(&out.join("sub")), vec!["blue".to_string()]);
    assert_eq!(std::fs::metadata(out.join("sub")).unwrap().modified().unwrap(), old);
    assert_eq!(std::fs::read(out.join("big.bin")).unwrap(), big);
    assert_eq!(j.progress.lock().unwrap().done_bytes, big.len() as u64 + 1);
    std::fs::set_permissions(out.join("sub/a.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    std::fs::set_permissions(src.join("sub/a.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

/// Shares mounted over the network sit in the sidebar with the disks, wherever the user
/// mounted them; other FUSE filesystems (the document portal, an encrypted vault) don't.
#[test]
fn sidebar_lists_network_shares_wherever_they_are_mounted() {
    let d = tmpdir("netfs");
    let table = d.join("mounts");
    touch(&table, "/dev/sda2 / ext4 rw 0 0\n\
nas:/export/media /home/u/media nfs4 rw 0 0\n\
//office/share /home/u/office cifs rw 0 0\n\
onedrive: /home/u/OneDrive fuse.rclone rw,nosuid 0 0\n\
portal /run/user/1000/doc fuse.portal rw,nosuid,nodev,user_id=1000 0 0\n\
vault /home/u/vault fuse.gocryptfs rw 0 0\n");
    let got: Vec<PathBuf> = crate::fsx::mounts_in(&table).into_iter().map(|m| m.path).collect();
    assert_eq!(got, ["/", "/home/u/media", "/home/u/office", "/home/u/OneDrive"].map(PathBuf::from));
    let _ = std::fs::remove_dir_all(&d);
}

/// A move to another disk copies and then deletes. A socket in the folder can't be
/// copied, so the folder stays where it was, whole; with the socket gone the same move
/// goes through and leaves only the copy, recorded for Undo.
#[test]
fn move_to_another_disk_keeps_a_folder_it_could_not_copy_whole() {
    let ctx = egui::Context::default();
    let Some(far) = other_fs("xdev-sock") else { return };
    let d = tmpdir("xdev-sock");
    touch(&d.join("trip/raw/1.raw"), "raw");
    touch(&d.join("trip/notes.txt"), "n");
    let sock = std::os::unix::net::UnixListener::bind(d.join("trip/raw/agent.sock")).unwrap();
    let move_trip = |id| {
        let j = ops::spawn(id, OpKind::Move { srcs: vec![d.join("trip")], dest: far.clone(), decide: HashMap::new() }, ctx.clone());
        wait_job(&j);
        j
    };
    let j = move_trip(1);
    let errors = j.progress.lock().unwrap().errors.clone();
    assert!(errors.iter().any(|e| e.contains("agent.sock")), "{errors:?}");
    assert_eq!(std::fs::read_to_string(d.join("trip/raw/1.raw")).unwrap(), "raw", "the original went although the socket wasn't copied");
    assert!(d.join("trip/notes.txt").exists());
    drop(sock);
    std::fs::remove_file(d.join("trip/raw/agent.sock")).unwrap();
    let _ = std::fs::remove_dir_all(far.join("trip"));
    let j = move_trip(2);
    let g = j.progress.lock().unwrap();
    assert!(g.errors.is_empty(), "{:?}", g.errors);
    assert!(!d.join("trip").exists());
    assert_eq!(std::fs::read_to_string(far.join("trip/raw/1.raw")).unwrap(), "raw");
    assert_eq!(g.gone, [d.join("trip")]);
    drop(g);
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&far);
}

/// The preview's editor saves line ends the way the file had them. A file that is CR LF
/// throughout is edited as plain lines and comes back CR LF, added lines too; a file with
/// one CR LF line among LF ones keeps that line's CR and gains none anywhere else.
#[test]
fn preview_save_keeps_each_files_line_ends() {
    use crate::quickedit::Edit;
    let d = tmpdir("qe-ends");
    let save = |name: &str, before: &str, add: &str| -> (String, String) {
        let f = d.join(name);
        std::fs::write(&f, before).unwrap();
        let mut e = Edit::open(&f).unwrap();
        let shown = e.text.clone();
        e.text.push_str(add);
        assert!(e.save(false).is_ok());
        (shown, std::fs::read_to_string(&f).unwrap())
    };
    assert_eq!(save("win.ini", "[a]\r\nk=1\r\n", "j=2\n"), ("[a]\nk=1\n".into(), "[a]\r\nk=1\r\nj=2\r\n".into()));
    assert_eq!(save("pasted.sh", "set -e\r\nls\n", "pwd\n"), ("set -e\r\nls\n".into(), "set -e\r\nls\npwd\n".into()));
    assert_eq!(save("plain.txt", "x\n", "y"), ("x\n".into(), "x\ny".into()));
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&d);
}

/// Saving a file opened through a link changes the file the link points to, in another
/// folder, and leaves the link a link, save after save.
#[test]
fn preview_save_through_a_link_writes_where_it_points() {
    let d = tmpdir("qe-points");
    let (real, link) = (d.join("dotfiles/bashrc"), d.join(".bashrc"));
    touch(&real, "alias a=b\n");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let mut e = crate::quickedit::Edit::open(&link).unwrap();
    for line in ["alias c=d\n", "alias e=f\n"] {
        e.text.push_str(line);
        assert!(e.save(false).is_ok());
        assert_eq!(std::fs::read_link(&link).unwrap(), real, "the link was replaced");
    }
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "alias a=b\nalias c=d\nalias e=f\n");
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&d);
}

/// A link pointed at another file after it was opened: saving asks first, as for a file
/// changed on disk, and neither file is written until it is told to go ahead.
#[test]
fn quick_edit_through_a_moved_symlink_asks_first() {
    let d = tmpdir("qe-relink");
    let (a, b, link) = (d.join("a.txt"), d.join("b.txt"), d.join("link.txt"));
    std::fs::write(&a, "a\n").unwrap();
    std::fs::write(&b, "b\n").unwrap();
    std::os::unix::fs::symlink("a.txt", &link).unwrap();
    let mut e = crate::quickedit::Edit::open(&link).unwrap();
    e.text = "edited\n".into();
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("b.txt", &link).unwrap();
    assert!(matches!(e.save(false), Err(crate::quickedit::SaveError::Changed)), "saved through a link that moved");
    assert_eq!((std::fs::read_to_string(&a).unwrap(), std::fs::read_to_string(&b).unwrap()), ("a\n".into(), "b\n".into()));
    let _ = std::fs::remove_dir_all(&d);
}

/// A finished move takes what it moved out of every listing that showed it, whichever
/// tab it is in: a plain listing and a tree are read again, a flattened one drops the
/// rows at once, the destination shows them, and a listing it did not touch stays put.
/// A tab whose own folder was moved away moves up.
#[test]
fn finished_move_updates_every_listing() {
    let ctx = egui::Context::default();
    let d = tmpdir("moved-away");
    let (from, to) = (d.join("from"), d.join("to"));
    for f in ["a.txt", "b.txt", "sub/c.txt", "sub/d.txt", "deep/e.txt"] {
        touch(&from.join(f), f);
    }
    touch(&d.join("elsewhere/x.txt"), "x");
    std::fs::create_dir_all(&to).unwrap();
    let stats = HashMap::new();
    let tab = |p: &Path| Tab::new(1, p.to_path_buf(), ViewSettings::default(), ctx.clone());
    let (mut plain, mut flat, mut tree, mut dest, mut other, mut deep) = (tab(&from), tab(&from), tab(&from), tab(&to), tab(&d.join("elsewhere")), tab(&from.join("deep")));
    flat.set_flatten(true);
    tree.settings.zoom = 0.1;
    for t in [&mut plain, &mut flat, &mut tree, &mut dest, &mut other, &mut deep] {
        pump(t, &stats);
    }
    tree.toggle_expand(&from.join("sub"));
    pump(&mut tree, &stats);
    let shows = |t: &Tab, p: &Path| t.view.iter().any(|&i| t.entries[i as usize].path == p);
    assert!(shows(&plain, &from.join("a.txt")) && shows(&flat, &from.join("sub/c.txt")) && shows(&tree, &from.join("sub/c.txt")));
    // Cut a.txt, sub/c.txt and the folder deep, pasted into `to`.
    let srcs = vec![from.join("a.txt"), from.join("sub/c.txt"), from.join("deep")];
    let j = ops::spawn(1, OpKind::Move { srcs: srcs.clone(), dest: to.clone(), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    let c = ops::Changed::of(&j.kind, &j.progress.lock().unwrap());
    assert_eq!(c, ops::Changed { arrived: vec![to.clone()], left: vec![from.clone(), from.join("sub")], gone: srcs.clone() });
    for t in [&mut plain, &mut flat, &mut tree, &mut dest, &mut other, &mut deep] {
        t.changed(&c);
    }
    assert!(!flat.is_loading() && !other.is_loading(), "nothing arrived under them: no walk, no read");
    assert!(plain.is_loading() && dest.is_loading(), "the folders it left and reached are read again");
    let recents = HashSet::new();
    let vc = ViewCtx { show_hidden: false, show_system: false, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
    flat.rebuild(&vc);
    assert!(!shows(&flat, &from.join("a.txt")) && !shows(&flat, &from.join("sub/c.txt")) && !shows(&flat, &from.join("deep/e.txt")), "a flattened listing drops what went at once");
    assert!(shows(&flat, &from.join("b.txt")) && shows(&flat, &from.join("sub/d.txt")));
    for t in [&mut plain, &mut tree, &mut dest, &mut deep] {
        pump(t, &stats);
    }
    assert!(!shows(&plain, &from.join("a.txt")) && !shows(&plain, &from.join("deep")) && shows(&plain, &from.join("b.txt")));
    assert!(!shows(&tree, &from.join("sub/c.txt")) && shows(&tree, &from.join("sub/d.txt")), "the unfolded folder is read again");
    assert!(shows(&dest, &to.join("a.txt")) && shows(&dest, &to.join("c.txt")) && shows(&dest, &to.join("deep")));
    assert!(deep.gone, "the tab inside the folder that moved finds it gone");
    // Moving them back (Undo) lands them in a flattened listing by walking it again.
    let c = ops::Changed::moved(&[(to.join("a.txt"), from.join("a.txt"))]);
    std::fs::rename(to.join("a.txt"), from.join("a.txt")).unwrap();
    flat.changed(&c);
    assert!(flat.is_loading(), "something arrived under it");
    pump(&mut flat, &stats);
    assert!(shows(&flat, &from.join("a.txt")) && !shows(&flat, &from.join("sub/c.txt")));
    let _ = std::fs::remove_dir_all(&d);
}

fn pump(t: &mut Tab, stats: &HashMap<PathBuf, crate::stats::DirStats>) {
    let recents = HashSet::new();
    let ctx = ViewCtx { show_hidden: false, show_system: false, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats };
    let start = Instant::now();
    while t.is_loading() {
        t.poll();
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    t.poll();
    t.rebuild(&ctx);
}

#[test]
fn tab_listing_sort_filter_flatten() {
    let d = tmpdir("tab");
    touch(&d.join("b10.txt"), "1234567890");
    touch(&d.join("b2.txt"), "1");
    touch(&d.join(".hidden"), "");
    touch(&d.join("zdir/deep/needle.rs"), "");
    touch(&d.join("adir/x.png"), "");
    let ctx = egui::Context::default();
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), ctx);
    pump(&mut t, &stats);
    let names: Vec<&str> = t.view.iter().map(|&i| t.entries[i as usize].name.as_str()).collect();
    assert_eq!(names, vec!["adir", "zdir", "b2.txt", "b10.txt"], "folders first, natural order, hidden excluded");
    assert_eq!(t.hidden_count, 1);
    assert_eq!(t.cursor, Some(t.view[0]), "cursor rests on the first item");
    assert!(t.selected.is_empty(), "no selection on entry");
    assert_eq!(t.counts, (2, 2, 2, 3), "totals include the hidden file");
    // Size sort defaults to descending.
    t.set_sort(SortKey::Size);
    assert!(t.settings.desc);
    pump(&mut t, &stats);
    let files: Vec<&str> = t.view.iter().map(|&i| t.entries[i as usize].name.as_str()).filter(|n| n.ends_with(".txt")).collect();
    assert_eq!(files, vec!["b10.txt", "b2.txt"]);
    // Filter
    t.filter = "b1".into();
    t.dirty = true;
    pump(&mut t, &stats);
    assert_eq!(t.view.len(), 1);
    // Flatten finds nested files, filter matches relative path.
    t.filter = "needle".into();
    t.set_flatten(true);
    pump(&mut t, &stats);
    let found: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].rel().map(|r| r.into_owned()).unwrap_or_default()).collect();
    assert_eq!(found, vec!["zdir/deep/needle.rs".to_string()]);
    // Flattened lists are files only.
    t.filter.clear();
    t.dirty = true;
    pump(&mut t, &stats);
    assert!(t.view.iter().all(|&i| !t.entries[i as usize].is_dir), "flatten shows files only");
    // Type filter: only the checked types are shown.
    t.set_flatten(false);
    let txt = crate::mime::mime_db().label("a.txt", "txt", false, false);
    t.settings.only_types = vec![txt.clone()];
    t.dirty = true;
    pump(&mut t, &stats);
    assert!(!t.view.is_empty());
    assert!(t.view.iter().all(|&i| t.entries[i as usize].ext() == "txt"));
    t.settings.only_types.clear();
    // Footer toggles hide folders / files.
    t.settings.show_folders = false;
    t.dirty = true;
    pump(&mut t, &stats);
    assert!(t.view.iter().all(|&i| !t.entries[i as usize].is_dir));
    t.settings.show_folders = true;
    // Details view has no tree; Tree view (10% zoom) unfolds folders inline, indented.
    assert!(!t.tree_active());
    t.settings.zoom = 0.1;
    assert!(t.tree_active());
    t.dirty = true;
    pump(&mut t, &stats);
    t.toggle_expand(&d.join("zdir"));
    pump(&mut t, &stats);
    let names: Vec<(String, u8)> = t.view.iter().map(|&i| (t.entries[i as usize].name.clone(), t.entries[i as usize].depth)).collect();
    let zpos = names.iter().position(|(n, _)| n == "zdir").unwrap();
    assert_eq!(names[zpos + 1], ("deep".to_string(), 1), "child right below its parent: {names:?}");
    t.toggle_expand(&d.join("zdir/deep"));
    pump(&mut t, &stats);
    assert!(t.view.iter().any(|&i| t.entries[i as usize].name == "needle.rs" && t.entries[i as usize].depth == 2));
    // The status bar counts the folder's own items: unfolded rows are not shown "of" it.
    let (vf, tf, vfi, tfi) = t.counts;
    assert!(vf <= tf && vfi <= tfi, "shown {vf}/{tf} folders, {vfi}/{tfi} files");
    // Zooming out of the tree (which does not mark the tab dirty) asks for new rows, and
    // they leave the unfolded folders' children out.
    t.settings.zoom = 0.0;
    assert!(t.built_tree != t.tree_active(), "the frame loop rebuilds when the tree comes or goes");
    pump(&mut t, &stats);
    assert!(!t.view.iter().any(|&i| t.entries[i as usize].depth > 0), "Details shows no tree rows");
    t.settings.zoom = 0.1;
    assert!(t.built_tree != t.tree_active());
    pump(&mut t, &stats);
    assert!(t.view.iter().any(|&i| t.entries[i as usize].depth == 2), "back in the tree, the folders are still open");
    // Filtering in Tree view keeps the hierarchy: the match stays under its folders,
    // which are kept as dimmed context because they don't match themselves.
    t.filter = "needle".into();
    t.dirty = true;
    pump(&mut t, &stats);
    let names: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
    assert_eq!(names, vec!["zdir", "deep", "needle.rs"], "{names:?}");
    let dim: Vec<String> = t.dimmed.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
    assert!(dim.contains(&"zdir".to_string()) && dim.contains(&"deep".to_string()) && !dim.contains(&"needle.rs".to_string()));
    t.filter.clear();
    t.dirty = true;
    pump(&mut t, &stats);
    t.toggle_expand(&d.join("zdir"));
    pump(&mut t, &stats);
    assert!(!t.view.iter().any(|&i| t.entries[i as usize].depth > 0), "collapsing hides descendants");
    // Ctrl+E in Tree view unfolds every folder recursively (and the title gets the […] mark).
    t.toggle_expand_all();
    for _ in 0..50 {
        pump(&mut t, &stats);
        if !t.is_loading() {
            break;
        }
    }
    assert!(t.view.iter().any(|&i| t.entries[i as usize].name == "needle.rs"), "expand all reaches deep files");
    assert!(t.title().ends_with("[…]"));
    t.toggle_expand_all();
    pump(&mut t, &stats);
    assert!(!t.view.iter().any(|&i| t.entries[i as usize].depth > 0));
    t.settings.zoom = 0.0;
    // Navigation + history + up selects the child.
    t.navigate(d.join("zdir"), None);
    pump(&mut t, &stats);
    assert!(t.can_back());
    t.up(|_| None);
    pump(&mut t, &stats);
    assert_eq!(t.cursor_path(), Some(d.join("zdir")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn group_by_name_ranges() {
    let d = tmpdir("group");
    for n in ["apple", "kiwi", "zebra", "_x"] {
        touch(&d.join(n), "");
    }
    let mut v = ViewSettings::default();
    v.group = crate::config::GroupBy::Name;
    let mut t = Tab::new(1, d.clone(), v, egui::Context::default());
    pump(&mut t, &HashMap::new());
    let headers: Vec<String> = t.header_labels();
    assert_eq!(headers, vec!["A – H", "I – P", "Q – Z", "Other"]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn view_settings_inheritance() {
    use crate::config::{SaveScope, SavedView, UserData};
    let mut ud = UserData::default();
    let mut v = ViewSettings::default();
    v.zoom = 0.6;
    ud.views.insert(PathBuf::from("/a"), SavedView { scope: SaveScope::Subfolders, view: v.clone() });
    assert_eq!(ud.view_for(Path::new("/a/b/c")).0.zoom, 0.6);
    ud.views.insert(PathBuf::from("/x"), SavedView { scope: SaveScope::ThisFolder, view: v });
    assert_eq!(ud.view_for(Path::new("/x/y")).0.zoom, 0.0);
}

#[test]
fn mime_lookup() {
    let db = crate::mime::mime_db();
    assert_eq!(db.for_name("x.png", false, false), "image/png");
    assert_eq!(db.for_name("dir", true, false), "inode/directory");
    assert_eq!(db.for_name("a.tar.gz", false, false), "application/x-compressed-tar");
}

#[test]
fn image_decoders_and_downscale() {
    let d = tmpdir("img");
    // PNG via the png encoder.
    let png_path = d.join("a.png");
    {
        let f = std::fs::File::create(&png_path).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), 40, 20);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e.write_header().unwrap();
        let data: Vec<u8> = (0..40 * 20).flat_map(|i| [(i % 40 * 6) as u8, 100, 200]).collect();
        w.write_image_data(&data).unwrap();
    }
    let (img, dims) = crate::imgdec::load(&png_path, 10).unwrap();
    assert_eq!(dims, [40, 20]);
    assert_eq!((img.w, img.h), (10, 5));
    assert_eq!(img.px.len(), 10 * 5 * 4);
    assert_eq!(img.px[3], 255);
    // 24-bit bottom-up BMP, 2x2: red, green / blue, white (rows stored bottom first).
    let mut bmp = vec![];
    let row = |a: [u8; 3], b: [u8; 3]| [a[2], a[1], a[0], b[2], b[1], b[0], 0, 0];
    let pixels = [row([0, 0, 255], [255, 255, 255]), row([255, 0, 0], [0, 255, 0])].concat();
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&((54 + pixels.len()) as u32).to_le_bytes());
    bmp.extend_from_slice(&[0; 4]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&2i32.to_le_bytes());
    bmp.extend_from_slice(&2i32.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    bmp.extend_from_slice(&pixels);
    let bmp_path = d.join("b.bmp");
    std::fs::write(&bmp_path, &bmp).unwrap();
    let (img, _) = crate::imgdec::load(&bmp_path, 64).unwrap();
    assert_eq!(&img.px[0..4], &[255, 0, 0, 255], "top-left red");
    assert_eq!(&img.px[12..16], &[255, 255, 255, 255], "bottom-right white");
    assert!(crate::imgdec::load(&d.join("missing.png"), 64).is_none());
    std::fs::write(d.join("junk.png"), b"not an image").unwrap();
    assert!(crate::imgdec::load(&d.join("junk.png"), 64).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn trash_roundtrip() {
    let d = tmpdir("trash");
    let f = d.join("doomed file.txt");
    touch(&f, "bye");
    crate::trashcan::delete(&f).unwrap();
    assert!(!f.exists());
    let items: Vec<_> = crate::trashcan::list().into_iter().filter(|i| i.original == f).collect();
    assert_eq!(items.len(), 1, "listed with its original path");
    assert!(items[0].file.exists() && items[0].info.exists());
    assert!(!items[0].deleted.is_empty());
    crate::trashcan::restore(&items[0]).unwrap();
    assert_eq!(std::fs::read_to_string(&f).unwrap(), "bye");
    // Trash again and purge.
    crate::trashcan::delete(&f).unwrap();
    let it = crate::trashcan::list().into_iter().find(|i| i.original == f).unwrap();
    crate::trashcan::purge(&it).unwrap();
    assert!(!it.file.exists() && !it.info.exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn inotify_reports_changes() {
    let d = tmpdir("watch");
    let mut w = crate::watch::Watch::new(egui::Context::default());
    w.sync(&[d.clone()].into_iter().collect());
    touch(&d.join("new.txt"), "x");
    let start = Instant::now();
    let mut got = vec![];
    while got.is_empty() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(30));
        got = w.poll();
    }
    assert_eq!(got, vec![d.clone()]);
    assert!(w.changed_files.contains(&d.join("new.txt")));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn time_formatting() {
    let t = std::time::UNIX_EPOCH + Duration::from_secs(86_400 * 400);
    assert_eq!(crate::timefmt::format(t, "%Y"), "1971");
    let now = std::time::SystemTime::now();
    assert_eq!(crate::timefmt::local_day(now) - crate::timefmt::local_day(now - Duration::from_secs(86_400)), 1);
}

#[test]
fn desktop_exec_expansion() {
    let p = vec![PathBuf::from("/tmp/a b.txt"), PathBuf::from("/tmp/c.txt")];
    assert_eq!(crate::mime::expand_exec("gedit %U", &p, "Gedit"), vec!["gedit", "/tmp/a b.txt", "/tmp/c.txt"]);
    assert_eq!(crate::mime::expand_exec("vlc --started-from-file %f", &p[..1], "VLC"), vec!["vlc", "--started-from-file", "/tmp/a b.txt"]);
    assert_eq!(crate::mime::expand_exec("\"/opt/My App/run\" --x", &p[..1], "X"), vec!["/opt/My App/run", "--x", "/tmp/a b.txt"]);
    assert_eq!(crate::mime::expand_exec("app --name=%c %%", &[], "Zed"), vec!["app", "--name=Zed", "%"]);
}

#[test]
fn trash_metadata_columns_and_groups() {
    // A fake per-volume trash: files/ plus info/*.trashinfo.
    let d = tmpdir("trashmeta");
    let files = d.join(".Trash-1000/files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::create_dir_all(d.join(".Trash-1000/info")).unwrap();
    touch(&files.join("old.txt"), "x");
    touch(&files.join("new.txt"), "y");
    let now = crate::timefmt::format(std::time::SystemTime::now(), "%Y-%m-%dT%H:%M:%S");
    touch(&d.join(".Trash-1000/info/old.txt.trashinfo"), "[Trash Info]\nPath=/srv/a/old.txt\nDeletionDate=2001-02-03T04:05:06\n");
    touch(&d.join(".Trash-1000/info/new.txt.trashinfo"), &format!("[Trash Info]\nPath=/srv/b/new.txt\nDeletionDate={now}\n"));
    assert!(crate::trashcan::is_trash_files(&files));
    assert!(!crate::trashcan::is_trash_files(&d));
    let m = crate::trashcan::info_for(&files);
    assert_eq!(m.get(&files.join("old.txt")).map(|x| x.0.clone()), Some(PathBuf::from("/srv/a/old.txt")));
    let t0 = crate::timefmt::parse_local("2001-02-03T04:05:06").unwrap();
    assert_eq!(crate::timefmt::format(t0, "%Y-%m-%dT%H:%M:%S"), "2001-02-03T04:05:06");
    // A tab on the trash groups by deletion date and by original folder.
    let stats = HashMap::new();
    let mut t = Tab::new(7, files.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    assert_eq!(t.trash_meta.len(), 2);
    t.settings.group = crate::config::GroupBy::Deleted;
    t.dirty = true;
    pump(&mut t, &stats);
    let heads: Vec<String> = t.header_labels();
    assert_eq!(heads, vec!["Today", "Older"]);
    t.settings.group = crate::config::GroupBy::Original;
    t.dirty = true;
    pump(&mut t, &stats);
    let heads: Vec<String> = t.header_labels();
    assert_eq!(heads, vec!["/srv/a", "/srv/b"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Renders the SVGs listed in $MAIA_SVG_SAMPLES (one path per line) into $MAIA_SVG_OUT
/// as PNGs, for eyeballing the renderer. Run with --ignored.
#[test]
#[ignore]
fn svg_render_samples() {
    let (Ok(list), Ok(out)) = (std::env::var("MAIA_SVG_SAMPLES"), std::env::var("MAIA_SVG_OUT")) else { return };
    std::fs::create_dir_all(&out).unwrap();
    for (i, p) in std::fs::read_to_string(list).unwrap().lines().enumerate() {
        let data = std::fs::read(p).unwrap();
        for px in [24u32, 64, 128] {
            let t = Instant::now();
            let Some((w, h, mut rgba)) = crate::svg::render(&data, px) else { eprintln!("FAILED {p}"); continue };
            // Unpremultiply for the PNG file.
            for c in rgba.chunks_exact_mut(4) {
                if c[3] > 0 && c[3] < 255 {
                    for k in 0..3 {
                        c[k] = ((c[k] as u32 * 255 + c[3] as u32 / 2) / c[3] as u32).min(255) as u8;
                    }
                }
            }
            eprintln!("{i:02} {px} {:?} {p}", t.elapsed());
            let f = std::fs::File::create(format!("{out}/{i:02}-{px}.png")).unwrap();
            let mut enc = png::Encoder::new(std::io::BufWriter::new(f), w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
    }
}

#[test]
fn zoom_steps_through_views_then_icon_sizes() {
    use crate::listing::zoom_step;
    // Up: one named view per step, then 5.5% icon steps up to 100%.
    let mut z = 0.0;
    let mut seen = vec![];
    for _ in 0..20 {
        z = zoom_step(z, 1);
        seen.push((z * 1000.0).round() as i32);
    }
    assert_eq!(&seen[..6], &[100, 200, 300, 355, 410, 465]);
    assert_eq!(*seen.last().unwrap(), 1000);
    // Down retraces through the dense views without skipping any.
    let mut z = 0.355;
    let mut down = vec![];
    for _ in 0..5 {
        z = zoom_step(z, -1);
        down.push((z * 1000.0).round() as i32);
    }
    assert_eq!(down, vec![300, 200, 100, 0, 0]);
    // From a slider position between stops, a step lands on the neighbouring stop.
    assert_eq!((zoom_step(0.15, 1) * 1000.0).round() as i32, 200);
    assert_eq!((zoom_step(0.15, -1) * 1000.0).round() as i32, 100);
}

#[test]
fn decoders_reject_huge_or_corrupt_headers_without_allocating() {
    let t = Instant::now();
    // GIF claiming a 65535x65535 screen (4.3 gigapixels) with one 1x1 frame.
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x80, 0, 0, 0, 0, 0, 0xff, 0xff, 0xff]);
    gif.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x02, 0x02, 0x44, 0x01, 0, 0x3b]);
    assert!(crate::imgdec::decode_bytes(&gif).is_none());
    // BMP claiming 32000x32000 pixels.
    let mut bmp = b"BM".to_vec();
    bmp.extend_from_slice(&[0; 8]);
    bmp.extend_from_slice(&54u32.to_le_bytes());
    bmp.extend_from_slice(&40u32.to_le_bytes());
    bmp.extend_from_slice(&32000i32.to_le_bytes());
    bmp.extend_from_slice(&32000i32.to_le_bytes());
    bmp.extend_from_slice(&1u16.to_le_bytes());
    bmp.extend_from_slice(&24u16.to_le_bytes());
    bmp.extend_from_slice(&[0; 24]);
    assert!(crate::imgdec::decode_bytes(&bmp).is_none());
    // 8-bit BMP with a palette count of 4 billion must not try to build that palette.
    let mut pal = b"BM".to_vec();
    pal.extend_from_slice(&[0; 8]);
    pal.extend_from_slice(&58u32.to_le_bytes());
    pal.extend_from_slice(&40u32.to_le_bytes());
    pal.extend_from_slice(&1i32.to_le_bytes());
    pal.extend_from_slice(&1i32.to_le_bytes());
    pal.extend_from_slice(&1u16.to_le_bytes());
    pal.extend_from_slice(&8u16.to_le_bytes());
    pal.extend_from_slice(&0u32.to_le_bytes());
    pal.extend_from_slice(&[0; 12]);
    pal.extend_from_slice(&u32::MAX.to_le_bytes());
    pal.extend_from_slice(&[0; 4 + 4 + 4]);
    let _ = crate::imgdec::decode_bytes(&pal);
    assert!(t.elapsed() < Duration::from_secs(1), "rejected quickly");
}

#[test]
fn copy_skips_special_files_and_refuses_self_nesting_through_symlinks() {
    let ctx = egui::Context::default();
    let d = tmpdir("ops_edge");
    let src = d.join("src");
    touch(&src.join("ok.txt"), "ok");
    // A FIFO inside a copied folder: must be skipped, not opened (that would block forever).
    let fifo = std::ffi::CString::new(src.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let t = Instant::now();
    let j = ops::spawn(10, OpKind::Copy { srcs: vec![src.clone()], dest: out.clone(), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    assert!(t.elapsed() < Duration::from_secs(5), "copy finished despite the FIFO");
    assert_eq!(std::fs::read_to_string(out.join("src/ok.txt")).unwrap(), "ok");
    assert!(!out.join("src/pipe").exists());
    assert!(j.progress.lock().unwrap().errors.iter().any(|e| e.contains("special file")));
    // Copying `src` into a symlink that points inside `src` is copying it into itself.
    std::os::unix::fs::symlink(&src, d.join("alias")).unwrap();
    std::fs::create_dir_all(src.join("inner")).unwrap();
    let j = ops::spawn(11, OpKind::Copy { srcs: vec![src.clone()], dest: d.join("alias/inner"), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.iter().any(|e| e.contains("into itself")), "{:?}", j.progress.lock().unwrap().errors);
    assert!(!src.join("inner/src").exists());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn deleted_folder_is_flagged_gone_but_unreadable_is_not() {
    let stats = HashMap::new();
    let d = tmpdir("gone");
    let sub = d.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    let mut t = Tab::new(21, sub.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    assert!(!t.gone);
    std::fs::remove_dir(&sub).unwrap();
    t.reload();
    pump(&mut t, &stats);
    assert!(t.gone, "missing folder reported as gone: {:?}", t.error);
    // A folder we can't read (permission denied) stays put with an error instead.
    let locked = d.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let mut t = Tab::new(22, locked.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let readable = std::fs::read_dir(&locked).is_ok(); // true when running as root
    if !readable {
        assert!(!t.gone && t.error.is_some());
    }
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn special_files_are_labelled_and_never_executable() {
    let d = tmpdir("special");
    let f = d.join("p");
    let c = std::ffi::CString::new(f.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o755) }, 0);
    let e = crate::fsx::Entry::from_path(&f).unwrap();
    assert!(e.special && !e.is_executable());
    assert_eq!(e.special_kind(), Some("Pipe"));
    let n = crate::fsx::Entry::from_path(&d).unwrap();
    assert_eq!(n.special_kind(), None);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn entry_derives_lowercase_name_extension_and_relative_path() {
    let d = tmpdir("derive");
    std::fs::create_dir_all(d.join("Sub.D/inner")).unwrap();
    for n in ["Photo.JPG", ".bashrc", "archive.tar.gz", "file.", "plain", "..odd", "Sub.D/inner/x.Rs"] {
        std::fs::write(d.join(n), b"x").unwrap();
    }
    let e = |n: &str| crate::fsx::Entry::from_path(&d.join(n)).unwrap();
    let (p, b, a, f, pl, o, dir) = (e("Photo.JPG"), e(".bashrc"), e("archive.tar.gz"), e("file."), e("plain"), e("..odd"), e("Sub.D"));
    assert_eq!((p.name.as_str(), p.lname(), p.ext()), ("Photo.JPG", "photo.jpg", "jpg"));
    assert_eq!((b.ext(), b.hidden), ("", true));
    assert_eq!((a.lname(), a.ext()), ("archive.tar.gz", "gz"));
    assert_eq!((f.ext(), pl.ext(), o.ext(), dir.ext()), ("", "", "odd", ""));
    assert_eq!(dir.lname(), "sub.d");
    assert!(p.rel().is_none());
    // Renaming a volume entry keeps the derived fields in sync.
    let mut v = e("plain");
    v.set_name("My Disk".into());
    assert_eq!((v.lname(), v.ext()), ("my disk", ""));
    // Results of a recursive walk carry their path relative to the root.
    let w = crate::fsx::walk_async(d.clone(), true, egui::Context::default());
    while !w.done.load(std::sync::atomic::Ordering::Relaxed) {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let mut rels: Vec<String> = w.rx.try_iter().flatten().filter_map(|e| e.rel().map(|r| r.into_owned())).collect();
    rels.sort();
    assert!(rels.contains(&"Sub.D/inner/x.Rs".to_string()) && rels.contains(&"plain".to_string()), "{rels:?}");
    assert!(std::mem::size_of::<crate::fsx::Entry>() <= 144, "Entry grew to {}", std::mem::size_of::<crate::fsx::Entry>());
    let _ = std::fs::remove_dir_all(&d);
}

/// The previous (char-based, allocating) natural order, kept as an oracle.
fn natural_cmp_reference(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(|c| c.is_ascii_digit()) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(|c| c.is_ascii_digit()) {
                    nb.push(c);
                    bi.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let o = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb)).then_with(|| na.len().cmp(&nb.len()));
                if o != Ordering::Equal {
                    return o;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[test]
fn natural_cmp_matches_reference_on_random_names() {
    let alphabet: Vec<char> = "ab0123456789Z._- éß日😀".chars().collect();
    let mut x: u64 = 0x2545F4914F6CDD1D;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut names = vec![];
    for _ in 0..3000 {
        let n = (next() % 9) as usize;
        names.push((0..n).map(|_| alphabet[(next() % alphabet.len() as u64) as usize]).collect::<String>());
    }
    names.extend(["file2", "file10", "file02", "file002", "a1b2", "a1b10", "", "0", "00", "007", "7"].map(String::from));
    for a in &names {
        for b in names.iter().step_by(7) {
            assert_eq!(crate::tab::natural_cmp(a, b), natural_cmp_reference(a, b), "{a:?} vs {b:?}");
        }
    }
}

#[test]
fn fast_name_sort_orders_exactly_like_natural_order() {
    let d = tmpdir("namesort");
    let parts = ["a", "A", "b", "file", "File", "file1", "file10", "file2", "img_0001", "img_10", "z", "é", "Éa", "日本", "abcdefgh", "abcdefgi", "abcdefgh1", "abcdefgh10", "x.y", "00", "0", "007"];
    let mut x: u64 = 0x9E3779B97F4A7C15;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let mut made = std::collections::HashSet::new();
    for k in 0..400 {
        let n = 1 + (next() % 3) as usize;
        let name: String = (0..n).map(|_| parts[(next() % parts.len() as u64) as usize]).collect::<Vec<_>>().join("");
        if !made.insert(name.to_lowercase()) {
            continue;
        }
        if k % 5 == 0 {
            std::fs::create_dir_all(d.join(&name)).unwrap();
        } else {
            std::fs::write(d.join(&name), b"x").unwrap();
        }
    }
    let stats = HashMap::new();
    let recents = HashSet::new();
    for (folders_first, desc, filter) in [(true, false, ""), (true, true, ""), (false, false, ""), (false, true, ""), (true, false, "file")] {
        let mut t = Tab::new(31, d.clone(), ViewSettings::default(), egui::Context::default());
        pump(&mut t, &stats);
        t.settings.desc = desc;
        t.filter = filter.to_string();
        let ctx = ViewCtx { show_hidden: true, show_system: true, folders_first, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
        t.rebuild(&ctx);
        assert!(t.view.len() > if filter.is_empty() { 120 } else { 10 }, "{} items", t.view.len());
        let q = crate::fuzzy::Query::new(filter);
        let tier = |e: &crate::fsx::Entry| crate::fuzzy::score(&q, e.lname()).map(|s| (s + 40) / 25).unwrap_or(0);
        for w in t.view.windows(2) {
            let (a, b) = (&t.entries[w[0] as usize], &t.entries[w[1] as usize]);
            if !filter.is_empty() && tier(a) != tier(b) {
                assert!(tier(a) > tier(b), "better matches first");
                continue;
            }
            if a.is_dir != b.is_dir {
                assert_eq!(a.is_dir, folders_first, "{:?} before {:?}", a.name, b.name);
                continue;
            }
            let o = crate::tab::natural_cmp(a.lname(), b.lname());
            let o = if desc { o.reverse() } else { o };
            assert_ne!(o, std::cmp::Ordering::Greater, "{:?} before {:?} (desc {desc}, folders first {folders_first})", a.name, b.name);
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn parallel_sort_is_stable_and_matches_sort_by() {
    let mut x: u64 = 88172645463325252;
    for n in [0usize, 1, 39_999, 40_000, 40_001, 123_457, 200_000] {
        let v: Vec<(u32, u32)> = (0..n as u32)
            .map(|i| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x % 1000) as u32, i) // many equal keys; .1 records the original order
            })
            .collect();
        let mut a = v.clone();
        let mut b = v.clone();
        a.sort_by(|p, q| p.0.cmp(&q.0));
        crate::tab::par_sort_by(&mut b, &|p: &(u32, u32), q: &(u32, u32)| p.0.cmp(&q.0));
        assert_eq!(a, b, "n = {n}");
    }
}

#[test]
fn parallel_filter_path_keeps_every_match_in_order() {
    let d = tmpdir("parfilter");
    std::fs::write(d.join("seed.txt"), b"x").unwrap();
    std::fs::create_dir_all(d.join(".hid")).unwrap();
    let base = crate::fsx::Entry::from_path(&d.join("seed.txt")).unwrap();
    let words = ["alpha", "beta", "gamma", "delta", "lib", "Liberty", "x11", "file10", "file2", ".dot"];
    let mut entries = vec![];
    for i in 0..45_000u32 {
        let mut e = base.clone();
        let dir = if i % 97 == 0 { ".hid/" } else { "sub/" };
        let name = format!("{}{}.txt", words[(i as usize * 7) % words.len()], i % 1000);
        e.path = d.join(format!("{dir}{name}"));
        e.set_name(name);
        e.rel_at = (d.as_os_str().len() + 1) as u16;
        entries.push(e);
    }
    let stats = HashMap::new();
    let recents = HashSet::new();
    let mut t = Tab::new(41, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    t.flatten = true;
    t.set_entries(entries.clone());
    t.filter = "lib".into();
    let ctx = ViewCtx { show_hidden: false, show_system: false, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
    t.rebuild(&ctx);
    let q = crate::fuzzy::Query::new("lib");
    let expect: usize = entries.iter().filter(|e| !e.hidden && !e.rel_hidden() && (crate::fuzzy::score(&q, e.lname()).is_some() || e.rel().is_some_and(|r| crate::fuzzy::score_ci(&q, &r).is_some()))).count();
    assert_eq!(t.view.len(), expect, "every visible match listed once");
    assert!(expect > 1000);
    let tier = |e: &crate::fsx::Entry| {
        let n = crate::fuzzy::score(&q, e.lname()).map(|s| s + 40);
        let p = e.rel().and_then(|r| crate::fuzzy::score_ci(&q, &r));
        n.max(p).unwrap() / 25
    };
    for w in t.view.windows(2) {
        let (a, b) = (&t.entries[w[0] as usize], &t.entries[w[1] as usize]);
        assert!(tier(a) > tier(b) || (tier(a) == tier(b) && crate::tab::natural_cmp(a.lname(), b.lname()) != std::cmp::Ordering::Greater), "{} before {}", a.name, b.name);
    }
    let hidden = entries.iter().filter(|e| e.rel_hidden()).count();
    assert_eq!(t.hidden_count, hidden);
    assert_eq!(t.counts.3, 45_000, "total files counted");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn compact_key_sorts_match_full_comparison_for_size_and_dates() {
    use crate::config::SortKey;
    let d = tmpdir("keysort");
    let base = std::time::SystemTime::UNIX_EPOCH;
    for i in 0..160u64 {
        let name = format!("{}{}", ["f", "F", "img", "a1b"][(i % 4) as usize], i % 37);
        let p = d.join(&name);
        if p.exists() {
            continue;
        }
        std::fs::write(&p, vec![b'x'; (i % 9) as usize * 100]).unwrap();
        let f = std::fs::File::options().write(true).open(&p).unwrap();
        // Some timestamps before 1970, many equal ones.
        let t = if i % 11 == 0 { base - std::time::Duration::from_secs(1000 + i) } else { base + std::time::Duration::from_secs(1_700_000_000 + (i % 7) * 3600) };
        f.set_modified(t).unwrap();
    }
    let stats = HashMap::new();
    let recents = HashSet::new();
    for sort in [SortKey::Size, SortKey::Modified] {
        for desc in [false, true] {
            let mut t = Tab::new(51, d.clone(), ViewSettings::default(), egui::Context::default());
            pump(&mut t, &stats);
            t.settings.sort = sort;
            t.settings.desc = desc;
            let ctx = ViewCtx { show_hidden: true, show_system: true, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
            t.rebuild(&ctx);
            assert!(t.view.len() > 100);
            for w in t.view.windows(2) {
                let (a, b) = (&t.entries[w[0] as usize], &t.entries[w[1] as usize]);
                let o = match sort {
                    SortKey::Size => a.size.cmp(&b.size),
                    _ => a.modified.unwrap().cmp(&b.modified.unwrap()),
                }
                .then_with(|| crate::tab::natural_cmp(a.lname(), b.lname()));
                let o = if desc { o.reverse() } else { o };
                assert_ne!(o, std::cmp::Ordering::Greater, "{sort:?} desc {desc}: {:?} before {:?}", a.name, b.name);
            }
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn recents_on_top_sorts_recents_first_then_folders_then_names() {
    let d = tmpdir("recentsort");
    for n in ["b.txt", "a.txt", "c10.txt", "c2.txt", "z.txt"] {
        std::fs::write(d.join(n), b"x").unwrap();
    }
    for n in ["dir1", "dir2"] {
        std::fs::create_dir_all(d.join(n)).unwrap();
    }
    let stats = HashMap::new();
    let recents: HashSet<PathBuf> = [d.join("z.txt"), d.join("c10.txt"), d.join("dir2")].into_iter().collect();
    let mut t = Tab::new(61, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let ctx = ViewCtx { show_hidden: true, show_system: true, folders_first: true, recents_on_top: true, recents: &recents, pinned: &Default::default(), stats: &stats };
    t.rebuild(&ctx);
    let names: Vec<&str> = t.view.iter().map(|&i| t.entries[i as usize].name.as_str()).collect();
    assert_eq!(names, ["dir2", "c10.txt", "z.txt", "dir1", "a.txt", "b.txt", "c2.txt"]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn md5_matches_rfc1321_and_the_thumbnail_spec_example() {
    use crate::thumbcache::md5_hex;
    assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
    assert_eq!(md5_hex(b"The quick brown fox jumps over the lazy dog"), "9e107d9d372bb6826bd81d3542a419d6");
    assert_eq!(md5_hex(&[b'a'; 1000]), "cabe45dcc9ae5b66ba86600cca6b8ba8");
    // freedesktop.org Thumbnail Managing Standard, "Thumbnail Saving" example.
    assert_eq!(md5_hex(b"file:///home/jens/photos/me.png"), "c6ee772d9e49320e97ec29a7eb5b1697");
}

#[test]
fn thumbnail_uris_encode_like_glib_and_round_trip() {
    use crate::thumbcache::{path_from_uri, uri_for};
    assert_eq!(uri_for(Path::new("/home/jens/photos/me.png")), "file:///home/jens/photos/me.png");
    assert_eq!(uri_for(Path::new("/tmp/a b/é#%.png")), "file:///tmp/a%20b/%C3%A9%23%25.png");
    for p in ["/tmp/a b/é#%.png", "/home/x/日本/file (1).txt", "/plain/path"] {
        assert_eq!(path_from_uri(&uri_for(Path::new(p))).as_deref(), Some(Path::new(p)));
    }
    assert_eq!(path_from_uri("file://localhost/etc/hosts").as_deref(), Some(Path::new("/etc/hosts")));
    assert_eq!(path_from_uri("https://example.com/x"), None);
}

#[test]
fn thumbnail_cache_round_trip_and_staleness() {
    use crate::thumbcache::{flavor, lookup_in, store_in};
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("thumbcache");
    let root = d.join("thumbnails");
    let file = d.join("photo one.png");
    std::fs::write(&file, b"x").unwrap();
    let mtime = std::fs::metadata(&file).unwrap().modified().ok();
    let img = crate::imgdec::Rgba { w: 3, h: 2, px: (0..24).map(|i| i as u8 * 10).collect() };
    store_in(&root, &file, mtime, 128, &img, 1, Some([4000, 3000]));
    let (got, dims) = lookup_in(&root, &file, mtime, 100).expect("cached");
    assert_eq!((got.w, got.h, got.px), (3, 2, img.px.clone()));
    assert_eq!(dims, Some([4000, 3000]));
    // Stored where the spec says, private.
    let name = format!("{}.png", crate::thumbcache::md5_hex(crate::thumbcache::uri_for(&file).as_bytes()));
    let meta = std::fs::metadata(root.join("normal").join(&name)).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    // Other flavours miss; a changed file is stale.
    assert!(lookup_in(&root, &file, mtime, 512).is_none());
    let later = mtime.map(|t| t + std::time::Duration::from_secs(5));
    assert!(lookup_in(&root, &file, later, 100).is_none(), "stale after the file changed");
    assert!(lookup_in(&root, &d.join("other.png"), mtime, 100).is_none());
    assert_eq!(flavor(64), ("normal", 128));
    assert_eq!(flavor(200), ("large", 256));
    assert_eq!(flavor(512), ("x-large", 512));
    assert_eq!(flavor(900), ("xx-large", 1024));
    let _ = std::fs::remove_dir_all(&d);
}

/// Read-only check against a real thumbnail cache made by other apps (run by hand:
/// `cargo test real_cache -- --ignored`): Maia's URI encoding and hashing must find
/// exactly the files they wrote.
#[test]
#[ignore]
fn real_thumbnail_cache_entries_match_our_encoding() {
    let root = crate::thumbcache::cache_root();
    let (mut checked, mut bad) = (0, vec![]);
    for flavor in ["normal", "large", "x-large", "xx-large"] {
        let Ok(rd) = std::fs::read_dir(root.join(flavor)) else { continue };
        for e in rd.flatten().take(400) {
            let Ok(bytes) = std::fs::read(e.path()) else { continue };
            let Some((_, text)) = crate::imgdec::png_with_text(&bytes) else { continue };
            let Some(uri) = text.iter().find(|(k, _)| k == "Thumb::URI").map(|(_, v)| v.clone()) else { continue };
            let Some(p) = crate::thumbcache::path_from_uri(&uri) else { continue };
            checked += 1;
            let ours = crate::thumbcache::uri_for(&p);
            let stem = e.path().file_stem().unwrap().to_string_lossy().into_owned();
            if ours != uri || crate::thumbcache::md5_hex(ours.as_bytes()) != stem {
                bad.push(format!("{uri} -> {ours}"));
            }
        }
    }
    eprintln!("real cache: {checked} thumbnails checked, {} mismatches", bad.len());
    assert!(bad.is_empty(), "{:?}", &bad[..bad.len().min(5)]);
}

#[test]
fn syntax_highlighting_tokens() {
    use crate::syntax::{highlight, lang_for, Kind};
    let spans = |name: &str, src: &[&str]| {
        let lines: Vec<String> = src.iter().map(|s| s.to_string()).collect();
        let hl = highlight(lang_for(name).expect(name), &lines, 1000);
        hl.into_iter().enumerate().flat_map(|(n, v)| v.into_iter().map(move |(a, b, k)| (n, src_owned(src, n, a, b), k))).collect::<Vec<_>>()
    };
    fn src_owned(src: &[&str], n: usize, a: usize, b: usize) -> String {
        src[n][a..b].to_string()
    }
    let rs = spans("main.rs", &["fn main() { let s = \"a \\\" b\"; // done", "/* multi", " line */ let x: u32 = 42;"]);
    assert!(rs.contains(&(0, "fn".into(), Kind::Keyword)));
    assert!(rs.contains(&(0, "\"a ".into(), Kind::Str)) && rs.contains(&(0, "\\\"".into(), Kind::Constant)) && rs.contains(&(0, " b\"".into(), Kind::Str)), "an escaped quote stays inside the string, picked out: {rs:?}");
    assert!(rs.contains(&(0, "// done".into(), Kind::Comment)));
    assert!(rs.contains(&(1, "/* multi".into(), Kind::Comment)));
    assert!(rs.contains(&(2, " line */".into(), Kind::Comment)), "block comment carried to the next line");
    assert!(rs.contains(&(2, "u32".into(), Kind::Type)));
    assert!(rs.contains(&(2, "42".into(), Kind::Number)));
    let py = spans("x.py", &["def f():", "    s = \"\"\"doc", "    more\"\"\" # tail", "a#b = 1"]);
    assert!(py.contains(&(0, "def".into(), Kind::Keyword)));
    assert!(py.contains(&(1, "\"\"\"doc".into(), Kind::Str)));
    assert!(py.contains(&(2, "    more\"\"\"".into(), Kind::Str)), "triple-quoted string spans lines: {py:?}");
    assert!(py.contains(&(2, "# tail".into(), Kind::Comment)));
    assert!(!py.iter().any(|(n, _, k)| *n == 3 && *k == Kind::Comment), "a#b is not a comment");
    let js = spans("app.ts", &["const x = `tpl", "end` // c"]);
    assert!(js.contains(&(1, "end`".into(), Kind::Str)), "template literal spans lines");
    let html = spans("index.html", &["<div class=\"a\"><!-- note --></div>"]);
    assert!(html.contains(&(0, "<div".into(), Kind::Keyword)));
    assert!(html.contains(&(0, "\"a\"".into(), Kind::Str)));
    assert!(html.contains(&(0, "<!-- note -->".into(), Kind::Comment)));
    let md = spans("README.md", &["# Title", "text `code` more"]);
    assert!(md.contains(&(0, "# Title".into(), Kind::Keyword)));
    assert!(md.contains(&(1, "`code`".into(), Kind::Str)));
    assert!(lang_for("Makefile").is_some() && lang_for("photo.png").is_none() && lang_for("notes.txt").is_none());
}

/// What a name is, read from what is around it, in real code of each kind: functions
/// defined and called, types by declaration and by case, fields after a dot, keys in data,
/// attributes, macros, decorators, lifetimes, constants, escapes and shell variables.
#[test]
fn syntax_highlighting_reads_names_from_their_place() {
    use crate::syntax::{highlight, lang_for, Kind};
    let spans = |name: &str, src: &[&str]| {
        let lines: Vec<String> = src.iter().map(|s| s.to_string()).collect();
        let hl = highlight(lang_for(name).expect(name), &lines, 1000);
        hl.iter().enumerate().flat_map(|(n, v)| v.iter().map(|&(a, b, k)| (n, lines[n][a..b].to_string(), k)).collect::<Vec<_>>()).collect::<Vec<_>>()
    };
    let has = |got: &[(usize, String, Kind)], n: usize, text: &str, k: Kind| assert!(got.contains(&(n, text.to_string(), k)), "{text:?} as {k:?} on line {n}: {got:?}");
    let rs = spans("lib.rs", &["#[derive(Debug, Clone)]", "pub struct Point<'a> { name: &'a str }", "fn main() { let p = Point::new(); println!(\"{}\\n\", p.name); let c = '\\n'; const MAX_LEN: usize = 4; }", "for i in 0..10 { x != y }"]);
    has(&rs, 0, "#[derive(Debug, Clone)]", Kind::Meta);
    has(&rs, 1, "Point", Kind::Type);
    has(&rs, 1, "'a", Kind::Meta);
    has(&rs, 2, "main", Kind::Function);
    has(&rs, 2, "Point", Kind::Type);
    has(&rs, 2, "new", Kind::Function);
    has(&rs, 2, "println!", Kind::Meta);
    has(&rs, 2, "\\n", Kind::Constant);
    has(&rs, 2, "name", Kind::Property);
    has(&rs, 2, "MAX_LEN", Kind::Constant);
    has(&rs, 3, "0", Kind::Number);
    has(&rs, 3, "10", Kind::Number);
    assert!(!rs.iter().any(|(n, t, _)| *n == 3 && t.starts_with('x')), "x != y is no macro: {rs:?}");
    let py = spans("shapes.py", &["@dataclass", "class Box(Base):", "    def area(self) -> int:", "        return self.w * MAX if x is None else len(self.items)"]);
    has(&py, 0, "@dataclass", Kind::Meta);
    has(&py, 1, "Box", Kind::Type);
    has(&py, 1, "Base", Kind::Type);
    has(&py, 2, "area", Kind::Function);
    has(&py, 3, "w", Kind::Property);
    has(&py, 3, "MAX", Kind::Constant);
    has(&py, 3, "None", Kind::Constant);
    has(&py, 3, "len", Kind::Function);
    has(&py, 3, "items", Kind::Property);
    let js = spans("app.ts", &["export function load(url: string): Promise<Data> {", "  return new Client(url).fetch(`${url}/x`);"]);
    has(&js, 0, "load", Kind::Function);
    has(&js, 0, "string", Kind::Type);
    has(&js, 0, "Data", Kind::Type);
    has(&js, 1, "Client", Kind::Type);
    has(&js, 1, "fetch", Kind::Function);
    let json = spans("package.json", &["{\"name\": \"maia\", \"n\": 1, \"ok\": true, \"none\": null}"]);
    has(&json, 0, "\"name\"", Kind::Property);
    has(&json, 0, "\"maia\"", Kind::Str);
    has(&json, 0, "1", Kind::Number);
    has(&json, 0, "true", Kind::Constant);
    has(&json, 0, "null", Kind::Constant);
    let yaml = spans("ci.yml", &["jobs:", "  - name: build", "    run: cargo test # all of it", "    enabled: true"]);
    has(&yaml, 0, "jobs", Kind::Property);
    has(&yaml, 1, "name", Kind::Property);
    has(&yaml, 2, "run", Kind::Property);
    has(&yaml, 2, "# all of it", Kind::Comment);
    has(&yaml, 3, "true", Kind::Constant);
    let toml = spans("Cargo.toml", &["[package]", "name = \"maia\"", "edition = 2024"]);
    has(&toml, 0, "[package]", Kind::Type);
    has(&toml, 1, "name", Kind::Property);
    has(&toml, 1, "\"maia\"", Kind::Str);
    let sh = spans("run.sh", &["echo \"home is $HOME and ${USER}\" '$NOT' $1", "build() { make -j\"$(nproc)\"; }"]);
    has(&sh, 0, "$HOME", Kind::Property);
    has(&sh, 0, "${USER}", Kind::Property);
    has(&sh, 0, "'$NOT'", Kind::Str);
    has(&sh, 0, "$1", Kind::Property);
    has(&sh, 1, "build", Kind::Function);
    let html = spans("page.html", &["<a href=\"x\" data-id='1'>it's &amp; done</a>"]);
    has(&html, 0, "<a", Kind::Keyword);
    has(&html, 0, "href", Kind::Property);
    has(&html, 0, "data-id", Kind::Property);
    has(&html, 0, "'1'", Kind::Str);
    has(&html, 0, "&amp;", Kind::Constant);
    assert!(!html.iter().any(|(_, t, k)| *k == Kind::Str && t.starts_with("'s")), "an apostrophe in text starts no string: {html:?}");
    let css = spans("site.css", &["a:hover {", "  font-size: -1.5em;", "  color: #fff;", "}"]);
    has(&css, 1, "font-size", Kind::Property);
    has(&css, 1, "1.5em", Kind::Number);
    has(&css, 2, "color", Kind::Property);
    has(&css, 2, "#fff", Kind::Number);
    let sql = spans("q.sql", &["SELECT count(*) FROM t WHERE x IS NULL"]);
    has(&sql, 0, "SELECT", Kind::Keyword);
    has(&sql, 0, "count", Kind::Function);
    has(&sql, 0, "IS", Kind::Keyword);
    has(&sql, 0, "NULL", Kind::Constant);
    let go = spans("main.go", &["fmt.Println(`raw \\n`, nil)"]);
    has(&go, 0, "Println", Kind::Function);
    has(&go, 0, "`raw \\n`", Kind::Str);
    has(&go, 0, "nil", Kind::Constant);
    let c = spans("main.c", &["#include <stdio.h>", "if (p == NULL) return MAX(a, b);"]);
    has(&c, 0, "#include", Kind::Keyword);
    has(&c, 0, "<stdio.h>", Kind::Str);
    has(&c, 1, "NULL", Kind::Constant);
    has(&c, 1, "MAX", Kind::Function);
    let md = spans("README.md", &["- see [the docs](https://example.org) now", "> quoted", "1. first"]);
    has(&md, 0, "-", Kind::Meta);
    has(&md, 0, "[the docs]", Kind::Property);
    has(&md, 0, "(https://example.org)", Kind::Str);
    has(&md, 1, "> quoted", Kind::Comment);
    has(&md, 2, "1.", Kind::Meta);
    let mk = spans("Makefile", &["\t$(CC) -o $@ $^"]);
    has(&mk, 0, "$(CC)", Kind::Property);
    has(&mk, 0, "$@", Kind::Property);
    let docker = spans("Dockerfile", &["FROM rust:1 AS build", "RUN cargo build --release"]);
    has(&docker, 0, "FROM", Kind::Keyword);
    has(&docker, 0, "AS", Kind::Keyword);
    has(&docker, 1, "RUN", Kind::Keyword);
    for name in ["CMakeLists.txt", "flake.nix", "Main.hs", "app.ex", "plot.R", "fit.jl", "x.pl", ".vimrc", "api.proto", "schema.graphql", "main.tf", "build.ps1", "lib.ml", "App.svelte", "Containerfile"] {
        assert!(lang_for(name).is_some(), "{name}");
    }
}

#[test]
fn syntax_highlighting_never_panics_and_spans_stay_on_char_boundaries() {
    use crate::syntax::{highlight, lang_for};
    let pieces = ["fn", " ", "\"", "'", "`", "\\", "/*", "*/", "//", "#", "<!--", "-->", "\"\"\"", "日本", "é", "😀", "<a", "42", "x", "\t", "--", "--[[", "]]", "#[", "#![", "'a", "$", "${", "$(", "@", "&", ";", "[", "](", ")", "-", ":", "=", "!", "<#", ".", "\\u{", "\\x", "Name", "MAX", "- ", "1. ", ">", "''"];
    let mut x: u64 = 0xdead_beef;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let lines: Vec<String> = (0..3000).map(|_| (0..(next() % 14)).map(|_| pieces[(next() % pieces.len() as u64) as usize]).collect()).collect();
    for name in ["a.rs", "a.c", "a.ts", "a.py", "a.sh", "a.go", "a.java", "a.lua", "a.rb", "a.php", "a.sql", "a.zig", "a.json", "a.toml", "a.yaml", "a.css", "a.html", "a.md", "Makefile", "Dockerfile", "a.nix", "a.hs", "a.ex", "a.r", "a.jl", "a.pl", "a.vim", "CMakeLists.txt", "a.proto", "a.graphql", "a.tf", "a.ps1", "a.ml"] {
        let hl = highlight(lang_for(name).unwrap(), &lines, 10_000);
        for (line, spans) in lines.iter().zip(&hl) {
            let mut end = 0;
            for &(a, b, _) in spans {
                assert!(a < b && b <= line.len() && line.is_char_boundary(a) && line.is_char_boundary(b), "{name}: bad span {a}..{b} in {line:?}");
                assert!(a >= end, "{name}: spans overlap at {a} in {line:?}: {spans:?}");
                end = b;
            }
        }
    }
}

#[test]
fn ffprobe_output_is_read_like_a_player_needs() {
    let flat = b"streams.stream.0.codec_name=\"mp3\"\nstreams.stream.0.codec_type=\"audio\"\nstreams.stream.0.disposition.attached_pic=0\nstreams.stream.1.codec_name=\"png\"\nstreams.stream.1.codec_type=\"video\"\nstreams.stream.1.width=200\nstreams.stream.1.height=200\nstreams.stream.1.disposition.attached_pic=1\nformat.duration=\"2.035000\"\nformat.tags.title=\"Test \\\"Tone\\\"\"\nformat.tags.ARTIST=\"Maia\"\n";
    let p = crate::media::parse_probe(flat).unwrap();
    assert!(p.video.is_none(), "cover art is not a video stream");
    assert!(p.audio && (p.duration - 2.035).abs() < 1e-6);
    assert_eq!(p.title.as_deref(), Some("Test \"Tone\""), "escaped quotes unescaped");
    assert_eq!(p.artist.as_deref(), Some("Maia"), "tag keys in any case");
    let v = b"streams.stream.0.codec_name=\"h264\"\nstreams.stream.0.codec_type=\"video\"\nstreams.stream.0.width=1920\nstreams.stream.0.height=1080\nstreams.stream.0.disposition.attached_pic=0\nformat.duration=\"61.5\"\n";
    let p = crate::media::parse_probe(v).unwrap();
    assert_eq!(p.video, Some((1920, 1080)));
    assert!(!p.audio);
    assert!(crate::media::parse_probe(b"format.duration=\"1\"\n").is_none());
    assert_eq!(crate::media::fmt_time(61.5), "1:01");
    assert_eq!(crate::media::fmt_time(3725.0), "1:02:05");
}

#[test]
fn lsblk_pairs_parse_into_drives_with_safe_removability() {
    use crate::drives::parse_lsblk;
    // A real machine: system disk (reports HOTPLUG=1) and a Windows NVMe, plus a USB stick.
    let text = r#"PATH="/dev/sda" PKNAME="" LABEL="" FSTYPE="" SIZE="500107862016" RM="0" HOTPLUG="1" TYPE="disk" MOUNTPOINTS=""
PATH="/dev/sda1" PKNAME="sda" LABEL="" FSTYPE="vfat" SIZE="4294967296" RM="0" HOTPLUG="1" TYPE="part" MOUNTPOINTS="/boot"
PATH="/dev/sda2" PKNAME="sda" LABEL="" FSTYPE="btrfs" SIZE="495810760704" RM="0" HOTPLUG="1" TYPE="part" MOUNTPOINTS="/var/tmp\x0a/home\x0a/"
PATH="/dev/zram0" PKNAME="" LABEL="zram0" FSTYPE="swap" SIZE="33488371712" RM="0" HOTPLUG="0" TYPE="disk" MOUNTPOINTS="[SWAP]"
PATH="/dev/nvme0n1p1" PKNAME="nvme0n1" LABEL="" FSTYPE="vfat" SIZE="104857600" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS=""
PATH="/dev/nvme0n1p2" PKNAME="nvme0n1" LABEL="" FSTYPE="" SIZE="16777216" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS=""
PATH="/dev/nvme0n1p3" PKNAME="nvme0n1" LABEL="" FSTYPE="ntfs" SIZE="680705130496" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS=""
PATH="/dev/nvme0n1p5" PKNAME="nvme0n1" LABEL="New Volume" FSTYPE="ntfs" SIZE="336961994752" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS=""
PATH="/dev/sdb" PKNAME="" LABEL="" FSTYPE="" SIZE="31004295168" RM="1" HOTPLUG="1" TYPE="disk" MOUNTPOINTS=""
PATH="/dev/sdb1" PKNAME="sdb" LABEL="My \x22USB\x22 日本" FSTYPE="exfat" SIZE="31003246592" RM="1" HOTPLUG="1" TYPE="part" MOUNTPOINTS="/run/media/me/My \x22USB\x22 日本""#;
    let d = parse_lsblk(text);
    let get = |dev: &str| d.iter().find(|x| x.dev == dev).unwrap_or_else(|| panic!("{dev}"));
    let root = get("/dev/sda2");
    assert_eq!(root.mounts, vec![PathBuf::from("/var/tmp"), PathBuf::from("/home"), PathBuf::from("/")]);
    assert!(!root.removable && !get("/dev/sda1").removable, "the system disk is never ejectable, HOTPLUG or not");
    let usb = get("/dev/sdb1");
    assert!(usb.removable && usb.mounted());
    assert_eq!(usb.label, "My \"USB\" 日本");
    assert_eq!(usb.disk, "/dev/sdb");
    assert_eq!(usb.mounts, vec![PathBuf::from("/run/media/me/My \"USB\" 日本")]);
    // What gets offered for mounting: the big Windows partitions, not EFI/MSR/swap.
    let offered: Vec<&str> = d.iter().filter(|x| x.offer_mount()).map(|x| x.dev.as_str()).collect();
    assert_eq!(offered, ["/dev/nvme0n1p3", "/dev/nvme0n1p5"]);
    assert_eq!(get("/dev/nvme0n1p5").title(), "New Volume");
    assert!(get("/dev/nvme0n1p3").title().ends_with(" Volume"));
    // Garbage and partial lines don't panic.
    assert!(parse_lsblk("PATH=\"/dev/x\\x\" LABEL=\"\\xZZ é \\x\"\nnonsense\nPATH=").len() <= 1);
}

#[test]
fn stick_mounted_inside_home_stays_ejectable() {
    use crate::drives::parse_lsblk;
    let text = "PATH=\"/dev/sdc\" PKNAME=\"\" LABEL=\"\" FSTYPE=\"\" SIZE=\"8000000000\" RM=\"1\" HOTPLUG=\"1\" TYPE=\"disk\" MOUNTPOINTS=\"\"\n\
PATH=\"/dev/sdc1\" PKNAME=\"sdc\" LABEL=\"S\" FSTYPE=\"vfat\" SIZE=\"7999000000\" RM=\"1\" HOTPLUG=\"1\" TYPE=\"part\" MOUNTPOINTS=\"/home/me/usb\"\n\
PATH=\"/dev/sdd\" PKNAME=\"\" LABEL=\"\" FSTYPE=\"\" SIZE=\"8000000000\" RM=\"1\" HOTPLUG=\"1\" TYPE=\"disk\" MOUNTPOINTS=\"\"\n\
PATH=\"/dev/sdd1\" PKNAME=\"sdd\" LABEL=\"\" FSTYPE=\"ext4\" SIZE=\"7999000000\" RM=\"1\" HOTPLUG=\"1\" TYPE=\"part\" MOUNTPOINTS=\"/home\"\n";
    let d = parse_lsblk(text);
    assert!(d.iter().find(|x| x.dev == "/dev/sdc1").unwrap().removable, "a folder inside /home is just a mount point");
    assert!(!d.iter().find(|x| x.dev == "/dev/sdd1").unwrap().removable, "but a disk holding /home itself is the system's");
    assert!(crate::drives::can_unmount(d.iter().find(|x| x.dev == "/dev/sdc1").unwrap()));
    assert!(!crate::drives::can_unmount(d.iter().find(|x| x.dev == "/dev/sdd1").unwrap()));
}

/// Encrypted volumes as lsblk lists them: a locked LUKS partition is offered (as
/// "Encrypted …" with no label), an unlocked one is not, the filesystem inside it is,
/// on the disk under it and lockable; a system disk unlocked at boot offers nothing.
#[test]
fn lsblk_encrypted_volumes_locked_and_unlocked() {
    use crate::drives::{Crypt, can_lock, parse_lsblk};
    let text = r#"PATH="/dev/nvme0n1" PKNAME="" LABEL="" FSTYPE="" SIZE="1024209543168" RM="0" HOTPLUG="0" TYPE="disk" MOUNTPOINTS=""
PATH="/dev/nvme0n1p1" PKNAME="nvme0n1" LABEL="" FSTYPE="vfat" SIZE="629145600" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS="/boot/efi"
PATH="/dev/nvme0n1p2" PKNAME="nvme0n1" LABEL="" FSTYPE="crypto_LUKS" SIZE="807453851648" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS=""
PATH="/dev/nvme0n1p3" PKNAME="nvme0n1" LABEL="" FSTYPE="crypto_LUKS" SIZE="214748364800" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS=""
PATH="/dev/mapper/luks-8c2b7d51-4c1e-4f0a-9d63-0f6e2a7b9c11" PKNAME="nvme0n1p2" LABEL="" FSTYPE="btrfs" SIZE="807437074432" RM="0" HOTPLUG="0" TYPE="crypt" MOUNTPOINTS="/home\x0a/"
PATH="/dev/sdv" PKNAME="" LABEL="" FSTYPE="" SIZE="536870912000" RM="0" HOTPLUG="1" TYPE="disk" MOUNTPOINTS=""
PATH="/dev/sdv1" PKNAME="sdv" LABEL="" FSTYPE="crypto_LUKS" SIZE="536869863424" RM="0" HOTPLUG="1" TYPE="part" MOUNTPOINTS=""
PATH="/dev/mapper/luks-3f6c0e5a-8d2b-4c1e-9a7f-2b5d8e1c4a90" PKNAME="sdv1" LABEL="Vault" FSTYPE="ext4" SIZE="536853086208" RM="0" HOTPLUG="0" TYPE="crypt" MOUNTPOINTS="/run/media/me/Vault"
PATH="/dev/sdw" PKNAME="" LABEL="" FSTYPE="" SIZE="64023257088" RM="1" HOTPLUG="1" TYPE="disk" MOUNTPOINTS=""
PATH="/dev/sdw1" PKNAME="sdw" LABEL="Diary" FSTYPE="crypto_LUKS" SIZE="64022208512" RM="1" HOTPLUG="1" TYPE="part" MOUNTPOINTS="""#;
    let d = parse_lsblk(text);
    let get = |dev: &str| d.iter().find(|x| x.dev == dev).unwrap_or_else(|| panic!("{dev}"));
    let offered: Vec<&str> = d.iter().filter(|x| x.offer_mount()).map(|x| x.dev.as_str()).collect();
    assert_eq!(offered, ["/dev/nvme0n1p3", "/dev/sdw1"], "locked volumes; not the open ones, nor what is mounted");
    assert_eq!(get("/dev/nvme0n1p3").crypt, Crypt::Locked);
    assert_eq!(get("/dev/nvme0n1p3").title(), "Encrypted 215 GB", "in thousands, as the drive's label says");
    assert_eq!(get("/dev/sdw1").title(), "Diary", "a LUKS2 label is its name");
    assert_eq!(get("/dev/sdv1").crypt, Crypt::Plain, "unlocked: what's inside stands for it");
    let vault = get("/dev/mapper/luks-3f6c0e5a-8d2b-4c1e-9a7f-2b5d8e1c4a90");
    assert_eq!(vault.crypt, Crypt::Open("/dev/sdv1".into()));
    assert_eq!(vault.disk, "/dev/sdv", "the disk under the partition it opens, to power off");
    assert!(vault.removable && can_lock(vault) && vault.title() == "Vault");
    let root = get("/dev/mapper/luks-8c2b7d51-4c1e-4f0a-9d63-0f6e2a7b9c11");
    assert_eq!(root.disk, "/dev/nvme0n1");
    assert!(!root.removable && !can_lock(root), "the system, unlocked at boot, stays as it is");
}

/// Unlocking through stand-ins for lsblk and udisksctl: the passphrase goes in on
/// udisksctl's input as a key file, exactly as typed, never on a command line; a wrong
/// one comes back as such and changes nothing; the right one unlocks and mounts what is
/// inside. Unmount, Lock and Eject close the volume again, after unmounting and before
/// the power goes.
#[test]
fn unlock_an_encrypted_drive_with_its_passphrase() {
    use crate::drives::{Crypt, Op, Passphrase, WRONG_PASSPHRASE, list, perform, unlock, wrong_passphrase};
    let d = tmpdir("unlock");
    let rows = [
        r#"PATH="/dev/sdv" PKNAME="" LABEL="" FSTYPE="" SIZE="536870912000" RM="0" HOTPLUG="1" TYPE="disk" MOUNTPOINTS="""#,
        r#"PATH="/dev/sdv1" PKNAME="sdv" LABEL="" FSTYPE="crypto_LUKS" SIZE="536869863424" RM="0" HOTPLUG="1" TYPE="part" MOUNTPOINTS="""#,
    ];
    std::fs::write(d.join("rows"), rows.join("\n") + "\n").unwrap();
    let inside = r#"PATH="/dev/mapper/luks-3f6c" PKNAME="sdv1" LABEL="Vault" FSTYPE="ext4" SIZE="536853086208" RM="0" HOTPLUG="0" TYPE="crypt" MOUNTPOINTS="""#;
    std::fs::write(d.join("inside"), format!("{inside}\n")).unwrap();
    fake_tool(&d, "lsblk", "cat \"$D/rows\"");
    let udisks = r#"case "$1" in
  unlock)
    if [ "$(cat; echo .)" = "correct horse." ]; then cat "$D/inside" >> "$D/rows"; echo "Unlocked $3 as /dev/dm-0."
    else echo "Error unlocking $3: GDBus.Error:org.freedesktop.UDisks2.Error.Failed: Error unlocking $3: Failed to activate device: Operation not permitted" >&2; exit 1; fi ;;
  mount) echo "Mounted $3 at /run/media/me/Vault." ;;
esac"#;
    fake_tool(&d, "udisksctl", udisks);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let locked = list(&t).into_iter().find(|x| x.dev == "/dev/sdv1").unwrap();
    assert_eq!(locked.crypt, Crypt::Locked);
    let pass = |s: &str| Passphrase(s.to_string());
    assert_eq!(unlock(&t, &locked, &pass("correct horse\n")).err().as_deref(), Some(WRONG_PASSPHRASE), "exactly as typed");
    assert_eq!(unlock(&t, &locked, &pass("battery staple")).err().as_deref(), Some(WRONG_PASSPHRASE));
    assert_eq!(list(&t).len(), 2, "still locked");
    assert_eq!(unlock(&t, &locked, &pass("correct horse")).unwrap(), Some(PathBuf::from("/run/media/me/Vault")));
    let calls = fake_calls(&d);
    assert!(calls.iter().filter(|c| c[0] == "udisksctl").all(|c| !c.iter().any(|a| a.contains("horse") || a.contains("staple"))), "{calls:?}");
    let progs: Vec<&[String]> = calls.iter().filter(|c| c[0] == "udisksctl").map(|c| &c[1..]).collect();
    assert_eq!(progs.len(), 4);
    assert_eq!(progs[2], ["unlock", "-b", "/dev/sdv1", "--key-file", "/dev/stdin"]);
    assert_eq!(progs[3], ["mount", "-b", "/dev/mapper/luks-3f6c"]);
    // Closing it again: whatever is mounted comes off first, the power goes last.
    let all = list(&t);
    let mut vault = all.iter().find(|x| x.dev == "/dev/mapper/luks-3f6c").unwrap().clone();
    assert_eq!((vault.crypt.clone(), vault.disk.as_str()), (Crypt::Open("/dev/sdv1".into()), "/dev/sdv"));
    let _ = std::fs::remove_file(d.join("calls.log"));
    perform(&t, Op::Lock, &vault, &all).unwrap();
    vault.mounts = vec![PathBuf::from("/run/media/me/Vault")];
    perform(&t, Op::Unmount, &vault, &all).unwrap();
    let all: Vec<_> = all.into_iter().map(|x| if x.dev == vault.dev { vault.clone() } else { x }).collect();
    perform(&t, Op::Eject, &vault, &all).unwrap();
    let calls: Vec<String> = fake_calls(&d).iter().map(|c| c[1..].join(" ")).collect();
    assert_eq!(calls, ["lock -b /dev/sdv1", "unmount -b /dev/mapper/luks-3f6c", "lock -b /dev/sdv1", "unmount -b /dev/mapper/luks-3f6c", "lock -b /dev/sdv1", "power-off -b /dev/sdv"]);
    // cryptsetup's words for a wrong passphrase, old and new.
    assert!(wrong_passphrase("Failed to activate device: Incorrect passphrase") && wrong_passphrase("No key available with this passphrase"));
    assert!(!wrong_passphrase("Not authorized to perform operation"));
}

#[test]
fn uri_lists_round_trip_any_file_name() {
    use crate::clipboard::{parse_uri_list, path_to_uri, uri_list};
    let paths: Vec<PathBuf> = ["/home/me/a b.txt", "/tmp/résumé – v3 (final).pdf", "/x/100% #1?.png", "/日本/ファイル", "/q/'quote\"s'", "/p/a+b=c&d"].iter().map(PathBuf::from).collect();
    let body = uri_list(&paths);
    assert!(body.lines().all(|l| l.starts_with("file:///") && !l.contains(' ')), "{body}");
    assert!(body.ends_with("\r\n"));
    assert_eq!(parse_uri_list(&body), paths);
    assert_eq!(path_to_uri(std::path::Path::new("/a b")), "file:///a%20b");
    // What other apps send: comments, LF or CRLF, localhost, other schemes.
    let foreign = "# copied by an app\nfile:///home/me/x.txt\r\nfile://localhost/home/me/y%20z.png\nhttps://example.com/pic.jpg\n\n  file:///tmp/t  \n";
    assert_eq!(parse_uri_list(foreign), ["/home/me/x.txt", "/home/me/y z.png", "/tmp/t"].iter().map(PathBuf::from).collect::<Vec<_>>());
    // Not valid UTF-8 on disk: still the same bytes back.
    use std::os::unix::ffi::OsStrExt;
    let odd = PathBuf::from(std::ffi::OsStr::from_bytes(b"/odd/\xff\xfe.bin"));
    assert_eq!(parse_uri_list(&uri_list(std::slice::from_ref(&odd))), vec![odd]);
    assert!(parse_uri_list("file:///bad%zz%").len() == 1, "malformed escapes are kept, not panicked on");
}

#[test]
fn copied_files_read_as_other_file_managers_write_them() {
    use crate::clipboard::{file_items, parse_gnome_files, uri_list};
    let paths: Vec<PathBuf> = ["/home/me/a b.txt", "/tmp/résumé.pdf"].iter().map(PathBuf::from).collect();
    let text = |items: &[(String, Vec<u8>)], m: &str| items.iter().find(|i| i.0 == m).map(|i| String::from_utf8_lossy(&i.1).into_owned());
    let copied = file_items(&paths, false);
    assert_eq!(text(&copied, "text/uri-list").unwrap(), uri_list(&paths));
    assert_eq!(text(&copied, "x-special/gnome-copied-files").unwrap(), "copy\nfile:///home/me/a%20b.txt\nfile:///tmp/r%C3%A9sum%C3%A9.pdf");
    assert_eq!(text(&copied, "UTF8_STRING").unwrap(), "/home/me/a b.txt\n/tmp/résumé.pdf");
    assert!(text(&copied, "application/x-kde-cutselection").is_none());
    let cut = file_items(&paths, true);
    assert!(text(&cut, "x-special/gnome-copied-files").unwrap().starts_with("cut\n"));
    assert_eq!(text(&cut, "application/x-kde-cutselection").unwrap(), "1");
    for items in [&copied, &cut] {
        let g = text(items, "x-special/gnome-copied-files").unwrap();
        assert_eq!(parse_gnome_files(&g), (paths.clone(), items == &cut));
    }
    // As Nautilus and Caja write it: a trailing newline, localhost.
    assert_eq!(parse_gnome_files("cut\nfile:///a%20b\nfile://localhost/c\n"), (vec![PathBuf::from("/a b"), PathBuf::from("/c")], true));
    assert_eq!(parse_gnome_files(""), (vec![], false));
}

/// The X11 clipboard and drag out against a real server: two clipboards of ours trade
/// file lists (a large one in pieces), and a minimal XDND target takes a drop. It takes
/// the clipboard and moves the pointer, so it runs only when asked, on a throwaway
/// server: MAIA_TEST_X11=1 DISPLAY=:N.
#[test]
fn x11_clipboard_and_drag_out() {
    use crate::clipboard::{file_items, parse_gnome_files, uri_list};
    use crate::xclip::XClip;
    use x11rb::connection::Connection;
    use x11rb::protocol::Event;
    use x11rb::protocol::xproto::*;
    use x11rb::wrapper::ConnectionExt as _;
    if std::env::var_os("MAIA_TEST_X11").is_none() {
        return;
    }
    let (a, b) = (XClip::spawn(None, 0).unwrap(), XClip::spawn(None, 0).unwrap());
    // Asked again until the other clipboard has taken over.
    let load = |c: &XClip, prefer: &[&str], want: &str| -> Vec<u8> {
        let t0 = Instant::now();
        loop {
            match c.load(prefer) {
                Ok((m, bytes)) if m == want => return bytes,
                r if t0.elapsed() > Duration::from_secs(5) => panic!("{want}: {r:?}"),
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
    };
    let paths: Vec<PathBuf> = ["/home/me/a b.txt", "/tmp/résumé.pdf"].iter().map(PathBuf::from).collect();
    a.store(file_items(&paths, true));
    let g = load(&b, &["x-special/gnome-copied-files", "text/uri-list"], "x-special/gnome-copied-files");
    assert_eq!(parse_gnome_files(&String::from_utf8_lossy(&g)), (paths.clone(), true));
    assert_eq!(load(&b, &["application/x-kde-cutselection"], "application/x-kde-cutselection"), b"1");
    assert_eq!(load(&a, &["text/uri-list"], "text/uri-list"), uri_list(&paths).into_bytes(), "our own, without the server");
    // Text under X11's own names: UTF8_STRING for plain text, STRING as Latin-1.
    b.store(vec![("UTF8_STRING".into(), "héllo ✓".into())]);
    assert_eq!(load(&a, &["text/plain;charset=utf-8", "text/plain"], "UTF8_STRING"), "héllo ✓".as_bytes());
    assert_eq!(load(&a, &["STRING"], "STRING"), "héllo ?".as_bytes());
    // Lists too large for one request go in pieces, served and read.
    let many: Vec<PathBuf> = (0..5000).map(|i| PathBuf::from(format!("/home/me/Pictures/Trip to the coast/IMG_{i:05} copy.jpg"))).collect();
    assert!(uri_list(&many).len() > crate::xclip::CHUNK);
    a.store(file_items(&many, false));
    assert_eq!(load(&b, &["text/uri-list"], "text/uri-list"), uri_list(&many).into_bytes());

    // A drop target: a window that is XDND-aware, under the pointer.
    let (conn, screen) = x11rb::xcb_ffi::XCBConnection::connect(None).unwrap();
    let root = conn.setup().roots[screen].root;
    let atom = |n: &str| conn.intern_atom(false, n.as_bytes()).unwrap().reply().unwrap().atom;
    let win = conn.generate_id().unwrap();
    conn.create_window(0, win, root, 100, 100, 200, 200, 0, WindowClass::INPUT_OUTPUT, 0, &CreateWindowAux::new().override_redirect(1)).unwrap();
    conn.change_property32(PropMode::REPLACE, win, atom("XdndAware"), AtomEnum::ATOM, &[5]).unwrap();
    conn.map_window(win).unwrap();
    conn.warp_pointer(x11rb::NONE, root, 0, 0, 0, 0, 150, 150).unwrap();
    conn.get_input_focus().unwrap().reply().unwrap();
    // No button is held, so the drag drops at once: entered, positioned, dropped, read.
    a.start_drag(paths.clone(), 1.0);
    let (copy, uris) = (atom("XdndActionCopy"), atom("text/uri-list"));
    let mut seen = vec![];
    let t0 = Instant::now();
    let got = loop {
        assert!(t0.elapsed() < Duration::from_secs(5), "drop not finished: {seen:?}");
        let Some(ev) = conn.poll_for_event().unwrap() else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        match ev {
            Event::ClientMessage(e) => {
                let l = e.data.as_data32();
                let name = String::from_utf8(conn.get_atom_name(e.type_).unwrap().reply().unwrap().name).unwrap();
                let reply = |kind: &str, data: [u32; 5]| conn.send_event(false, l[0], EventMask::NO_EVENT, ClientMessageEvent::new(32, l[0], atom(kind), data)).unwrap();
                match name.as_str() {
                    "XdndEnter" => {
                        assert_eq!((l[1] >> 24, l[1] & 1), (5, 1), "version 5, more than three types");
                        let list = conn.get_property(false, l[0], atom("XdndTypeList"), AtomEnum::ATOM, 0, 16).unwrap().reply().unwrap();
                        assert!(list.value32().unwrap().any(|t| t == uris));
                    }
                    "XdndPosition" => {
                        assert_eq!((l[2] >> 16, l[2] & 0xffff, l[4]), (150, 150, copy), "at the pointer, copying");
                        reply("XdndStatus", [win, 1, 0, 0, copy]);
                    }
                    "XdndDrop" => {
                        conn.convert_selection(win, atom("XdndSelection"), uris, atom("MAIA_TEST"), l[2]).unwrap();
                    }
                    _ => {}
                }
                seen.push(name);
            }
            Event::SelectionNotify(e) => {
                let r = conn.get_property(true, win, e.property, AtomEnum::ANY, 0, u32::MAX / 4).unwrap().reply().unwrap();
                let source = conn.get_selection_owner(atom("XdndSelection")).unwrap().reply().unwrap().owner;
                conn.send_event(false, source, EventMask::NO_EVENT, ClientMessageEvent::new(32, source, atom("XdndFinished"), [win, 1, copy, 0, 0])).unwrap();
                break r.value;
            }
            _ => {}
        }
        conn.flush().unwrap();
    };
    conn.flush().unwrap();
    assert_eq!(seen, ["XdndEnter", "XdndPosition", "XdndDrop"]);
    assert_eq!(got, uri_list(&paths).into_bytes());
    // Over Maia's own window the drag is Maia's again, in its logical pixels.
    let c = XClip::spawn(None, win).unwrap();
    crate::wlclip::take_events();
    c.start_drag(paths.clone(), 2.0);
    let t0 = Instant::now();
    let mut events = vec![];
    while !events.iter().any(|e| matches!(e, crate::wlclip::Dnd::Dropped { .. })) && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(5));
        events.extend(crate::wlclip::take_events());
    }
    assert_eq!(events.last(), Some(&crate::wlclip::Dnd::Dropped { x: 25.0, y: 25.0, paths, own: true }), "{events:?}");
    assert!(conn.poll_for_event().unwrap().is_none(), "no XDND to Maia itself");
}

/// The terminal's icons come from a Nerd Font: one is picked from fontconfig's list (lines
/// as a system with three of them installed gives them), and where one is installed the
/// monospace text has the glyphs a prompt draws rather than boxes.
#[test]
fn terminal_icons_come_from_a_nerd_font() {
    use crate::theme::pick_nerd_font as pick;
    let list = "Bold|GeistMono Nerd Font Mono|/usr/share/fonts/OTF/GeistMonoNerdFontMono-Bold.otf
Regular|FantasqueSansM Nerd Font Propo|/usr/share/fonts/TTF/FantasqueSansMNerdFontPropo-Regular.ttf
Regular|MesloLGM Nerd Font|/usr/share/fonts/TTF/MesloLGMNerdFont-Regular.ttf
Regular|MesloLGM Nerd Font Mono|/usr/share/fonts/TTF/MesloLGMNerdFontMono-Regular.ttf
Italic|MesloLGM Nerd Font Mono|/usr/share/fonts/TTF/MesloLGMNerdFontMono-Italic.ttf
Regular|GeistMono Nerd Font Mono|/usr/share/fonts/OTF/GeistMonoNerdFontMono-Regular.otf
";
    assert_eq!(pick(list), Some("/usr/share/fonts/TTF/MesloLGMNerdFont-Regular.ttf"), "full size before Mono and Propo");
    let symbols = format!("{list}Regular|Symbols Nerd Font|/usr/share/fonts/TTF/SymbolsNerdFont-Regular.ttf\n");
    assert_eq!(pick(&symbols), Some("/usr/share/fonts/TTF/SymbolsNerdFont-Regular.ttf"));
    let mono = "Regular|GeistMono Nerd Font Mono|/usr/share/fonts/OTF/GeistMonoNerdFontMono-Regular.otf\nBold|MesloLGM Nerd Font|/usr/share/fonts/TTF/MesloLGMNerdFont-Bold.ttf\n";
    assert_eq!(pick(mono), Some("/usr/share/fonts/OTF/GeistMonoNerdFontMono-Regular.otf"), "a Mono face rather than none");
    assert_eq!(pick(""), None);
    let any = std::process::Command::new("fc-list").args([":charset=e0a0 f015 f07b", "file"]).output().is_ok_and(|o| !o.stdout.is_empty());
    if !any {
        eprintln!("nerd font: skipped (none installed)");
        return;
    }
    let ctx = egui::Context::default();
    let cfg = crate::config::Config::default();
    crate::theme::install_fonts(&ctx, &cfg.font_family, &cfg.inspector_font);
    ctx.run_pass(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
    let font = egui::FontId::monospace(13.0);
    // The powerline branch, a house, a folder and a clock, as prompts draw them.
    for c in ['\u{e0a0}', '\u{f015}', '\u{f07b}', '\u{f017}'] {
        assert!(ctx.fonts_mut(|f| f.has_glyph(&font, c)), "U+{:04X} has a glyph", c as u32);
    }
}

/// Fonts for the scripts the interface font lacks come from fontconfig's own fallback
/// order, the first that covers each, every file once and in that order: the lines are
/// fc-match -s as it prints them, cut short.
#[test]
fn script_fonts_follow_fontconfigs_order() {
    let sorted = "/home/u/.local/share/fonts/Flex.ttf\taa|af|de|en|fr\n\
/usr/share/fonts/noto/NotoSans-Regular.ttf\taa|af|de|el|en|ru\n\
/usr/share/fonts/TTF/DejaVuSans.ttf\taa|ar|en|fa|he|hy|ka|lo|ur\n\
/usr/share/fonts/noto/NotoKufiArabic-Regular.ttf\tar|fa|ur\n\
/usr/share/fonts/noto/NotoSansDevanagari-Regular.ttf\ten|hi|mr|ne\n\
/usr/share/fonts/noto/NotoSansThai-Regular.ttf\ten|th\n\
/usr/share/fonts/noto/NotoSansHebrew-Regular.ttf\the\n";
    assert_eq!(crate::theme::script_fonts(sorted), ["/usr/share/fonts/TTF/DejaVuSans.ttf", "/usr/share/fonts/noto/NotoSansDevanagari-Regular.ttf", "/usr/share/fonts/noto/NotoSansThai-Regular.ttf"]);
    assert!(crate::theme::script_fonts("").is_empty() && crate::theme::script_fonts("no tab here\n").is_empty());
}

/// Persian and Hebrew names get letters, not boxes, wherever this machine has a font for
/// them: the interface font rarely does, so they come from the fallback fonts.
#[test]
fn persian_and_hebrew_letters_have_glyphs() {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx, "", "");
    ctx.run_pass(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
    for (lang, sample) in [("fa", "پرونده‌های گچ"), ("he", "קבצים")] {
        let any = std::process::Command::new("fc-list").arg(format!(":lang={lang}")).arg("file").output().is_ok_and(|o| !o.stdout.is_empty());
        if !any {
            eprintln!("{lang}: skipped (no font for it here)");
            continue;
        }
        for font in [egui::FontId::proportional(13.0), egui::FontId::monospace(13.0), crate::theme::semibold(13.0)] {
            for c in sample.chars().filter(|c| c.is_alphabetic()) {
                assert!(ctx.fonts_mut(|f| f.has_glyph(&font, c)), "{lang}: U+{:04X} in {font:?}", c as u32);
            }
        }
    }
}

#[test]
fn drag_picture_is_a_clean_premultiplied_card() {
    let ctx = egui::Context::default();
    let cfg = crate::config::Config::default();
    crate::theme::install_fonts(&ctx, &cfg.font_family, &cfg.inspector_font);
    let mut raw = egui::RawInput::default();
    raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(1.6);
    ctx.run_pass(raw, |_| {}).drop_without_applying_deltas();
    let pal = crate::theme::palette(crate::theme::ThemeName::default());
    let dir = std::env::temp_dir();
    let long = PathBuf::from("/x/a very long file name that keeps going and going well past what fits.txt");
    let cases: Vec<(&str, Vec<PathBuf>)> = vec![("one", vec![PathBuf::from("/x/photo from chat.png")]), ("many", vec![PathBuf::from("/x/a"), PathBuf::from("/x/b"), PathBuf::from("/x/c")]), ("folder", vec![dir.clone()]), ("long", vec![long])];
    for (name, paths) in cases {
        let pic = crate::ui::drag_picture(&ctx, &paths, &pal);
        let (w, h, px) = (pic.w, pic.h, &pic.px);
        assert_eq!(pic.scale, 2, "1.6 rounds up to a 2x picture");
        assert_eq!(h, 60);
        assert_eq!(px.len(), w * h * 4);
        assert!(px.chunks(4).all(|p| p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3]), "premultiplied");
        let at = |x: usize, y: usize| &px[(y * w + x) * 4..(y * w + x) * 4 + 4];
        assert_eq!(at(0, 0)[3], 0, "rounded corner is transparent");
        assert!(at(w / 2, h - 4)[3] > 230, "card is nearly opaque");
        let text = (70..w).flat_map(|x| (0..h).map(move |y| (x, y))).filter(|&(x, y)| at(x, y)[1] > 150).count();
        assert!(text > 60, "{name}: the label is drawn ({text} bright pixels)");
        assert!(w <= ((10.0 + 16.0 + 8.0 + 241.0 + 12.0) * 2.0) as usize + 2, "{name}: long names are cut ({w})");
        if let Some(d) = std::env::var_os("MAIA_ICON_DUMP") {
            let mut rgba = px.clone();
            for p in rgba.chunks_mut(4) {
                p.swap(0, 2);
            }
            let f = std::fs::File::create(std::path::Path::new(&d).join(format!("drag-{name}.png"))).unwrap();
            let mut e = png::Encoder::new(f, w as u32, h as u32);
            e.set_color(png::ColorType::Rgba);
            e.write_header().unwrap().write_image_data(&rgba).unwrap();
        }
    }
}

#[test]
fn window_icon_is_the_launcher_icon() {
    let (px, w, h) = crate::install::window_icon();
    assert_eq!((w, h), (64, 64));
    let at = |x: u32, y: u32| &px[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
    assert_eq!(at(0, 0)[3], 0, "clear around the folder");
    assert_eq!(at(32, 2)[3], 0, "clear above the papers");
    // The front paper, nearly white, stands out of the folder.
    let paper = at(16, 15);
    assert!(paper[3] > 250 && paper[0] > 200 && paper[0].abs_diff(paper[2]) < 12, "paper {paper:?}");
    // The foot of the glass front is near black.
    let glass = at(32, 51);
    assert!(glass[3] > 240 && glass[0] < 60, "glass {glass:?}");
    if let Some(d) = std::env::var_os("MAIA_ICON_DUMP") {
        let f = std::fs::File::create(std::path::Path::new(&d).join("window-icon.png")).unwrap();
        let mut e = png::Encoder::new(f, w, h);
        e.set_color(png::ColorType::Rgba);
        e.write_header().unwrap().write_image_data(&px).unwrap();
    }
}

#[test]
fn installing_the_icon_replaces_the_old_svg() {
    use crate::install::{ICON_PNG, icon_in, old_icon_in, place_icon};
    let d = tmpdir("hicolor");
    // What an install of 0.4 left behind.
    touch(&old_icon_in(&d), "<svg xmlns=\"http://www.w3.org/2000/svg\"/>");
    assert!(place_icon(&d).unwrap());
    assert!(!old_icon_in(&d).exists(), "a scalable icon would still be picked first");
    assert_eq!(std::fs::read(icon_in(&d)).unwrap(), ICON_PNG);
    let png = crate::imgdec::decode_bytes(&std::fs::read(icon_in(&d)).unwrap()).unwrap();
    assert_eq!((png.w, png.h), (256, 256), "the size its folder names");
    // An up-to-date icon is left alone, so starting Maia doesn't make launchers reload it.
    let t = std::fs::metadata(icon_in(&d)).unwrap().modified().unwrap();
    assert!(!place_icon(&d).unwrap());
    assert_eq!(std::fs::metadata(icon_in(&d)).unwrap().modified().unwrap(), t);
    // An older PNG is rewritten.
    std::fs::write(icon_in(&d), b"old").unwrap();
    assert!(place_icon(&d).unwrap());
    assert_eq!(std::fs::read(icon_in(&d)).unwrap(), ICON_PNG);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn finished_jobs_read_in_the_past_tense() {
    use crate::ui::finished_label;
    assert_eq!(finished_label("Copying 1 item"), "Copied 1 item");
    assert_eq!(finished_label("Moving 3 items"), "Moved 3 items");
    assert_eq!(finished_label("Moving 2 items to the trash"), "Moved 2 items to the trash");
    assert_eq!(crate::ui::job_undo_label("Moving notes.txt to the trash"), "Move notes.txt to the trash");
    assert_eq!(crate::ui::job_undo_label("Copying 3 items"), "Copy 3 items");
    assert_eq!(finished_label("Deleting 1 item"), "Deleted 1 item");
    assert_eq!(crate::ui::failed_label("Copying 1 item"), "Couldn't copy 1 item");
    assert_eq!(crate::ui::failed_label("Moving 2 items to the trash"), "Couldn't move 2 items to the trash");
    assert_eq!(crate::ui::failed_label("Deleting 3 items as administrator"), "Couldn't delete 3 items as administrator");
}

/// Run a tab's contents search to the end and rebuild its view.
fn pump_contents(t: &mut Tab, hidden: bool) -> Vec<String> {
    let stats = HashMap::new();
    let recents = HashSet::new();
    let ctx = ViewCtx { show_hidden: hidden, show_system: false, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
    let start = Instant::now();
    loop {
        crate::contentsearch::pump(t, hidden);
        t.poll();
        if !t.is_loading() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "contents search finished");
        std::thread::sleep(Duration::from_millis(5));
    }
    crate::contentsearch::pump(t, hidden);
    t.rebuild(&ctx);
    let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
    v.sort();
    v
}

#[test]
fn contents_search_finds_text_and_skips_binary_big_and_hidden() {
    let d = tmpdir("contents");
    touch(&d.join("a.txt"), "Hello World\n\tsecond   line has Needle here\nthird needle\n");
    touch(&d.join("b.md"), "nothing to see");
    touch(&d.join("needle-in-name.txt"), "the name matches, the text does not");
    std::fs::write(d.join("c.bin"), b"\x7fELF\x00\x00 needle").unwrap();
    touch(&d.join(".hidden.txt"), "a hidden needle");
    touch(&d.join("sub/deep.txt"), "NEEDLE at depth");
    touch(&d.join("long.txt"), &format!("{}needle{}\n", "x".repeat(500), "y".repeat(500)));
    touch(&d.join("grüße.txt"), "Viel ÄRGER heute\n");
    // A match across the 64 KB read boundary is still found.
    let mut seam = vec![b'.'; (64 << 10) - 3];
    seam.extend_from_slice(b"needle\n");
    std::fs::write(d.join("seam.txt"), seam).unwrap();
    // Over the size cap (sparse, so it costs no disk): skipped although it matches.
    let big = std::fs::File::create(d.join("big.log")).unwrap();
    std::io::Write::write_all(&mut &big, b"needle\n").unwrap();
    big.set_len(crate::contentsearch::MAX_BYTES + 1).unwrap();
    drop(big);

    let mut t = Tab::new(40, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &HashMap::new());
    t.content.on = true;
    t.filter = "needle".into();
    assert_eq!(pump_contents(&mut t, false), vec!["a.txt", "long.txt", "seam.txt"]);
    assert_eq!(t.content.snippet(&d.join("a.txt")), Some("second line has Needle here"));
    let long = t.content.snippet(&d.join("long.txt")).unwrap_or_default().to_string();
    assert!(long.starts_with('…') && long.contains("needle") && long.chars().count() < 140, "{long}");
    // Hidden files once they are shown; the subfolder only when flattened.
    assert_eq!(pump_contents(&mut t, true), vec![".hidden.txt", "a.txt", "long.txt", "seam.txt"]);
    t.set_flatten(true);
    assert_eq!(pump_contents(&mut t, false), vec!["a.txt", "deep.txt", "long.txt", "seam.txt"]);
    t.set_flatten(false);
    pump(&mut t, &HashMap::new());
    // Case-insensitive outside ASCII too.
    t.filter = "ärger".into();
    assert_eq!(pump_contents(&mut t, false), vec!["grüße.txt"]);
    assert_eq!(t.content.snippet(&d.join("grüße.txt")), Some("Viel ÄRGER heute"));
    // Back to names: the same query matches the file name, not the text.
    t.filter = "needle".into();
    t.content.on = false;
    assert_eq!(pump_contents(&mut t, false), vec!["needle-in-name.txt"]);
    assert!(t.content.take().is_none());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn contents_search_never_opens_a_fifo_and_stops_when_cancelled() {
    let d = tmpdir("contents_fifo");
    let fifo = std::ffi::CString::new(d.join("pipe").as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o644) }, 0);
    touch(&d.join("x.txt"), "needle");
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut buf = vec![0u8; 64 << 10];
    let t = Instant::now();
    assert_eq!(crate::contentsearch::find(&d.join("pipe"), "needle", &mut buf, &stop), None);
    assert!(t.elapsed() < Duration::from_secs(1), "the FIFO was not waited on");
    assert_eq!(crate::contentsearch::find(&d.join("x.txt"), "needle", &mut buf, &stop).as_deref(), Some("needle"));
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    assert_eq!(crate::contentsearch::find(&d.join("x.txt"), "needle", &mut buf, &stop), None, "a cancelled search reads nothing");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn second_launch_hands_its_folders_to_the_first() {
    use crate::single::{Request, claim_at, forward_to, parse, serve};
    let d = tmpdir("single");
    let sock = d.join("maia.sock");
    assert!(!forward_to(&sock, &[PathBuf::from("/x")]), "nobody listening: start normally");
    let l = claim_at(&sock).expect("first launch listens");
    assert!(claim_at(&sock).is_none(), "a second one does not take over");
    let (tx, rx) = std::sync::mpsc::channel();
    serve(l, tx, || {});
    let odd = PathBuf::from("/tmp/two\nlines – é/日本 #1");
    assert!(forward_to(&sock, &[PathBuf::from("/home"), odd.clone()]), "handed over");
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), Request::Open(vec![PathBuf::from("/home"), odd]));
    assert!(forward_to(&sock, &[]));
    assert_eq!(rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap(), Request::Focus);
    // A socket left by a crash (nobody answering) is replaced.
    let stale = d.join("stale.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert!(stale.exists() && claim_at(&stale).is_some(), "stale socket replaced");
    // Garbage never panics.
    assert_eq!(parse(b"open"), None);
    assert_eq!(parse(b"\0\0\0"), None);
    assert_eq!(parse(b"what\0ever\0"), None);
}

#[test]
fn default_file_manager_is_read_from_mimeapps() {
    use crate::install::default_in;
    let t = "[Added Associations]\ninode/directory=other.desktop;\n\n[Default Applications]\ntext/plain=gedit.desktop\ninode/directory=io.github.yappologistic.Maia.desktop;org.gnome.Nautilus.desktop;\n";
    assert_eq!(default_in(t).as_deref(), Some("io.github.yappologistic.Maia.desktop"));
    assert_eq!(default_in("[Default Applications]\ninode/directory= ;thunar.desktop\n").as_deref(), Some("thunar.desktop"));
    assert_eq!(default_in("[Added Associations]\ninode/directory=x.desktop\n"), None, "only Default Applications counts");
    assert_eq!(default_in(""), None);
}

#[test]
fn archive_listings_parse_like_bsdtar_prints_them() {
    use crate::archive::parse_list;
    let now = std::time::SystemTime::now();
    // Captured from bsdtar 3.8: a zip (entries stored as "./name"), a tar.gz with a
    // symlink, and a 7z that lists a folder after its file.
    let zip = "drwxr-xr-x  0 1000   1000        0 Oct 14 22:57 ./\n-rw-r--r--  0 1000   1000        0 Mar  5  2020 ./old.txt\nlrwxrwxrwx  0 1000   1000        0 Oct 14 22:57 ./link -> top.txt\n-rw-r--r--  0 1000   1000        2 Oct 14 22:57 ./top.txt\ndrwxr-xr-x  0 1000   1000        0 Oct 14 22:57 ./docs dir/\n-rw-r--r--  0 1000   1000        6 Oct 14 22:57 ./docs dir/read me.md\n";
    let items = parse_list(zip, now);
    let names: Vec<String> = items.iter().map(|i| i.path.display().to_string()).collect();
    assert_eq!(names, ["old.txt", "link", "top.txt", "docs dir", "docs dir/read me.md"], "./ stripped, the root entry dropped");
    assert!(items.iter().all(|i| i.dot), "stored with ./");
    let link = &items[1];
    assert!(link.link && !link.is_dir, "symlink without its target");
    assert!(items[3].is_dir && items[4].size == 6);
    // "Mar  5  2020" is that day; "Oct 14 22:57" is this year (or the last).
    let year_of = |t: std::time::SystemTime| crate::timefmt::format(t, "%Y");
    assert_eq!(year_of(items[0].modified.unwrap()), "2020");
    assert!(items[2].modified.unwrap() <= now + std::time::Duration::from_secs(86_400));
    // Folders that are only prefixes get rows; "../" escapes are refused.
    let bare = "-rw-r--r--  0 0      0           2 Oct 14 22:57 a/b/c.txt\n-rw-r--r--  0 0 0 1 Oct 14 22:57 ../evil\n";
    let items = parse_list(bare, now);
    let mut names: Vec<String> = items.iter().map(|i| format!("{}{}", i.path.display(), if i.is_dir { "/" } else { "" })).collect();
    names.sort();
    assert_eq!(names, ["a/", "a/b/", "a/b/c.txt"]);
    // bsdtar's escapes for odd names, and garbage lines.
    let odd = "-rw-r--r--  0 0      0           2 Oct 14 22:57 back\\\\slash \\303\\251.txt\nnonsense\n\n-rw-r--r-- 0\n";
    assert_eq!(parse_list(odd, now).iter().map(|i| i.path.display().to_string()).collect::<Vec<_>>(), ["back\\slash é.txt"]);
    assert_eq!(crate::archive::stem("photos.tar.gz"), "photos");
    assert_eq!(crate::archive::stem("Doc.ZIP"), "Doc");
    assert!(crate::archive::is_archive("x.tar.zst") && !crate::archive::is_archive(".zip") && !crate::archive::is_archive("notes.txt"));
}

#[test]
fn archives_round_trip_through_bsdtar() {
    if !crate::archive::available() {
        eprintln!("bsdtar missing: skipped");
        return;
    }
    let d = tmpdir("archive");
    let src = d.join("src");
    std::fs::create_dir_all(src.join("Trip photos")).unwrap();
    std::fs::write(src.join("Trip photos/beach – é.txt"), "sand").unwrap();
    std::fs::write(src.join("notes.txt"), "hello").unwrap();
    let names = vec![std::ffi::OsString::from("Trip photos"), std::ffi::OsString::from("notes.txt")];
    for ext in ["zip", "tar.gz", "tar.xz", "7z"] {
        let out = d.join(format!("pack.{ext}"));
        crate::archive::create(&out, &src, &names).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert!(out.is_file() && !d.join(format!(".pack.{ext}.part")).exists(), "{ext}: written in one go");
        let top: Vec<String> = crate::archive::entries(&out, std::path::Path::new("")).unwrap().iter().map(|e| e.name.clone()).collect();
        assert!(top.contains(&"Trip photos".into()) && top.contains(&"notes.txt".into()), "{ext}: {top:?}");
        let inner = crate::archive::entries(&out, std::path::Path::new("Trip photos")).unwrap();
        assert_eq!(inner.len(), 1, "{ext}");
        assert_eq!(inner[0].path, out.join("Trip photos/beach – é.txt"), "{ext}: rows continue the archive's path");
        assert!(crate::archive::entries(&out, std::path::Path::new("nope")).is_err());
        let x = d.join(format!("x-{ext}"));
        crate::archive::extract(&out, &[std::path::PathBuf::from("Trip photos")], &x).unwrap();
        assert_eq!(std::fs::read_to_string(x.join("Trip photos/beach – é.txt")).unwrap(), "sand", "{ext}");
        assert!(!x.join("notes.txt").exists(), "{ext}: only what was asked");
        let (a, i) = crate::archive::split(&out.join("Trip photos/beach – é.txt")).unwrap();
        assert_eq!((a, i), (out.clone(), std::path::PathBuf::from("Trip photos/beach – é.txt")));
    }
    assert!(crate::archive::split(&src.join("notes.txt")).is_none());
    assert!(crate::archive::create(&d.join("bad.zip"), &src, &[std::ffi::OsString::from("missing")]).is_err(), "errors come back");
    assert!(!d.join("bad.zip").exists() && !d.join(".bad.zip.part").exists(), "nothing left behind");
}

#[test]
fn merge_sort_is_stable_and_matches_sort_by() {
    let mut x: u64 = 0x2545f4914f6cdd1d;
    let mut rnd = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for n in [0usize, 1, 2, 23, 24, 25, 47, 48, 49, 100, 1000, 4097, 50_000] {
        for shape in 0..4 {
            // (key, original position): few distinct keys, so stability shows.
            let v: Vec<(u32, u32)> = (0..n as u32)
                .map(|i| match shape {
                    0 => ((rnd() % 7) as u32, i),
                    1 => (i, i),
                    2 => (n as u32 - i, i),
                    _ => ((i / 3) as u32, i),
                })
                .collect();
            let mut a = v.clone();
            let mut b = v.clone();
            crate::tab::merge_sort(&mut a, &|p: &(u32, u32), q: &(u32, u32)| p.0.cmp(&q.0));
            b.sort_by(|p, q| p.0.cmp(&q.0));
            assert_eq!(a, b, "n {n} shape {shape}");
        }
    }
}

/// Run git in `dir` (as a fixed test identity), failing the test if it fails.
pub(crate) fn git(dir: &Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .args(["-c", "user.name=Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
        .args(args)
        .current_dir(dir)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok || args.first() == Some(&"merge"), "git {args:?}");
}

/// A work tree with one change of every kind: a merge conflict, a modification, a staged
/// new file, an untracked file, a staged deletion, a rename and a change two folders down.
pub(crate) fn git_fixture(d: &Path) {
    touch(&d.join("keep.txt"), "same\n");
    touch(&d.join("mod.txt"), "one\n");
    touch(&d.join("old.txt"), "to be renamed\n");
    touch(&d.join("gone.txt"), "to be removed\n");
    touch(&d.join("conflict.txt"), "base\n");
    touch(&d.join("sub/inner/deep.txt"), "deep\n");
    git(d, &["init", "-q"]);
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "base"]);
    git(d, &["checkout", "-qb", "other"]);
    touch(&d.join("conflict.txt"), "theirs\n");
    git(d, &["commit", "-qam", "theirs"]);
    git(d, &["checkout", "-q", "main"]);
    touch(&d.join("conflict.txt"), "ours\n");
    git(d, &["commit", "-qam", "ours"]);
    git(d, &["merge", "-q", "other"]);
    touch(&d.join("mod.txt"), "two\n");
    touch(&d.join("staged.txt"), "new\n");
    git(d, &["add", "staged.txt"]);
    touch(&d.join("new.txt"), "untracked\n");
    git(d, &["rm", "-q", "gone.txt"]);
    git(d, &["mv", "old.txt", "renamed.txt"]);
    touch(&d.join("sub/inner/deep.txt"), "deeper\n");
}

#[test]
fn git_status_marks_every_kind_of_change() {
    use crate::gitstatus::{Mark, query};
    if !crate::media::have("git") {
        eprintln!("git not installed: skipped");
        return;
    }
    let d = tmpdir("git");
    git_fixture(&d);
    let r = query(&d.join("sub")).expect("a work tree");
    assert_eq!(r.root, d);
    assert_eq!(r.branch, "main");
    let m = |p: &str| r.marks.get(&d.join(p)).copied();
    assert_eq!(m("conflict.txt"), Some(Mark::Conflict));
    assert_eq!(m("mod.txt"), Some(Mark::Modified));
    assert_eq!(m("staged.txt"), Some(Mark::Staged));
    assert_eq!(m("new.txt"), Some(Mark::Untracked));
    assert_eq!(m("gone.txt"), Some(Mark::Deleted));
    assert_eq!(m("renamed.txt"), Some(Mark::Renamed));
    assert_eq!(m("old.txt"), None, "a rename's source is not marked");
    assert_eq!(m("sub/inner/deep.txt"), Some(Mark::Modified));
    assert_eq!(m("sub"), Some(Mark::Inside));
    assert_eq!(m("sub/inner"), Some(Mark::Inside));
    assert_eq!(m("keep.txt"), None);
    // Reached through a symlink, marks use the path as the user sees it.
    let link = std::env::temp_dir().join(format!("maia-test-gitlink-{}", std::process::id()));
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&d, &link).unwrap();
    let r = query(&link.join("sub/inner")).expect("a work tree through a link");
    assert_eq!(r.root, link);
    assert_eq!(r.marks.get(&link.join("mod.txt")), Some(&Mark::Modified));
    let _ = std::fs::remove_file(&link);
    // Outside a work tree, and inside .git itself, there is nothing to mark.
    let plain = tmpdir("nogit");
    assert!(query(&plain).is_none());
    assert!(query(&d.join(".git")).is_none());
    // A fresh repository with no commits yet still names its branch.
    git(&plain, &["init", "-q"]);
    touch(&plain.join("a.txt"), "a");
    let r = query(&plain).expect("an empty repository");
    assert_eq!((r.branch.as_str(), r.marks.get(&plain.join("a.txt"))), ("main", Some(&Mark::Untracked)));
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&plain);
}

/// Two folders with one difference of every kind (the compare journey uses them too):
/// a newer file, a file of another size, files and folders on one side only, a name that
/// is a file here and a folder there, and identical entries.
pub(crate) fn compare_fixture(a: &Path, b: &Path) {
    let t0 = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let file = |p: PathBuf, body: &str, when: std::time::SystemTime| {
        touch(&p, body);
        std::fs::File::options().write(true).open(&p).unwrap().set_modified(when).unwrap();
    };
    for d in [a, b] {
        file(d.join("same.txt"), "same", t0);
        std::fs::create_dir_all(d.join("both")).unwrap();
    }
    file(a.join("newer.txt"), "new", t0 + Duration::from_secs(3600));
    file(b.join("newer.txt"), "old", t0);
    file(a.join("size.txt"), "longer text", t0);
    file(b.join("size.txt"), "short", t0);
    file(a.join("only-a.txt"), "a", t0);
    file(b.join("only-b.txt"), "b", t0);
    std::fs::create_dir_all(a.join("folder-a")).unwrap();
    file(a.join("kind"), "a file", t0);
    std::fs::create_dir_all(b.join("kind")).unwrap();
}

#[test]
fn compare_marks_only_newer_and_size_differences() {
    use crate::compare::{Diff, side};
    let d = tmpdir("compare");
    let (a, b) = (d.join("a"), d.join("b"));
    compare_fixture(&a, &b);
    let (la, lb) = (crate::fsx::list_dir(&a).unwrap(), crate::fsx::list_dir(&b).unwrap());
    let (ea, eb): (Vec<_>, Vec<_>) = (la.iter().collect(), lb.iter().collect());
    let mut m = HashMap::new();
    side(&ea, &eb, &mut m);
    side(&eb, &ea, &mut m);
    let want: HashMap<PathBuf, Diff> = [
        (a.join("newer.txt"), Diff::Newer),
        (a.join("size.txt"), Diff::Size),
        (b.join("size.txt"), Diff::Size),
        (a.join("only-a.txt"), Diff::Only),
        (b.join("only-b.txt"), Diff::Only),
        (a.join("folder-a"), Diff::Only),
        (a.join("kind"), Diff::Only),
        (b.join("kind"), Diff::Only),
    ]
    .into_iter()
    .collect();
    assert_eq!(m, want, "identical files, the older side and folders on both sides stay unmarked");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn aliases_set_change_clear_and_refuse_duplicates() {
    use crate::popups::set_alias;
    let mut ud = crate::config::UserData::default();
    assert_eq!(set_alias(&mut ud, Cmd::ToggleHidden, "  hh "), Ok(()));
    assert_eq!(ud.aliases.get("toggle_hidden").map(String::as_str), Some("hh"), "trimmed and stored by command id");
    assert_eq!(set_alias(&mut ud, Cmd::ToggleSidebar, "HH"), Err(Cmd::ToggleHidden), "another command's alias, in any case, is refused");
    assert!(!ud.aliases.contains_key("toggle_sidebar"));
    assert_eq!(set_alias(&mut ud, Cmd::ToggleHidden, "hid"), Ok(()));
    assert_eq!(set_alias(&mut ud, Cmd::ToggleSidebar, "hh"), Ok(()), "a changed alias frees the old one");
    assert_eq!(set_alias(&mut ud, Cmd::ToggleHidden, "hid"), Ok(()), "setting the same alias again is fine");
    assert_eq!(set_alias(&mut ud, Cmd::ToggleHidden, ""), Ok(()));
    assert!(!ud.aliases.contains_key("toggle_hidden"), "empty text clears it");
    // An alias left behind by a command that no longer exists blocks nothing.
    ud.aliases.insert("gone_command".into(), "zz".into());
    assert_eq!(set_alias(&mut ud, Cmd::GoTo, "zz"), Ok(()));
}

#[test]
fn network_places_parse_from_gio() {
    use crate::network::{is_uri, label, normalize, parse_mounts};
    let text = "Drive(0): Micron MTFDKBA1T0TFH\n  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)\n  Volume(0): 681 GB Volume\n    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)\n    can_mount=1\nVolume(0): Pixel 7\n  Type: GProxyVolume (GProxyVolumeMonitorMTP)\n  activation_root=mtp://Google_Pixel_7_2A09/\n  can_mount=1\nVolume(1): Camera\n  Type: GProxyVolume (GProxyVolumeMonitorGPhoto2)\n  activation_root=gphoto2://Canon_EOS/\n  Mount(0): Camera -> gphoto2://Canon_EOS/\n    Type: GProxyShadowMount (GProxyVolumeMonitorGPhoto2)\nMount(0): media on nas -> smb://nas/media/\n  Type: GDaemonMount\n  default_location=smb://nas/media/\nMount(1): me on host -> sftp://me@host/\n  Type: GDaemonMount\nMount(2): Backup -> file:///run/media/me/Backup\n  Type: GProxyMount (GProxyVolumeMonitorUDisks2)\n";
    let p = parse_mounts(text);
    let got: Vec<(String, bool, bool)> = p.iter().map(|x| (x.uri.clone(), x.device, x.mounted)).collect();
    assert_eq!(
        got,
        [("mtp://Google_Pixel_7_2A09/".into(), true, false), ("gphoto2://Canon_EOS/".into(), true, true), ("smb://nas/media/".into(), false, true), ("sftp://me@host/".into(), false, true)],
        "local disks left to Drives, phones and shares kept"
    );
    assert_eq!(p[0].name, "Pixel 7");
    assert!(is_uri("smb://nas/media") && is_uri("SFTP://me@host") && !is_uri("/home/me") && !is_uri("smb://") && !is_uri("http://x"));
    assert_eq!(normalize(" ssh://me@host/srv "), "sftp://me@host/srv");
    assert_eq!(label("smb://nas/media/"), "media on nas");
    assert_eq!(label("sftp://me@host/"), "me@host");
    assert!(parse_mounts("garbage\nMount(\nVolume(3)\n").is_empty());
}

#[test]
fn administrator_commands_are_exact() {
    use crate::admin::{Task, denied};
    let s = |v: Vec<std::ffi::OsString>| v.into_iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>();
    let a = s(Task::Copy { srcs: vec!["/home/me/a b".into(), "/home/me/-x".into()], dest: "/etc".into() }.argv());
    assert!(a[0].ends_with("/cp"), "absolute program: {}", a[0]);
    assert_eq!(&a[1..], ["-a", "--", "/home/me/a b", "/home/me/-x", "/etc"], "-- before names that look like options");
    assert_eq!(&s(Task::Delete { paths: vec!["/srv/x".into()] }.argv())[1..], ["-rf", "--", "/srv/x"]);
    assert_eq!(&s(Task::Rename { from: "/etc/a".into(), to: "/etc/b".into() }.argv())[1..], ["-T", "--", "/etc/a", "/etc/b"]);
    assert!(s(Task::NewFolder("/opt/n".into()).argv())[0].ends_with("/mkdir"));
    assert!(denied(&std::io::Error::from_raw_os_error(libc::EACCES)) && denied(&std::io::Error::from_raw_os_error(libc::EPERM)));
    assert!(!denied(&std::io::Error::from_raw_os_error(libc::EROFS)), "a read-only disk isn't a permission problem");
}

#[test]
fn hand_written_png_is_valid_and_compact() {
    // A photo-like 256 x 192 image: smooth gradients with some noise.
    let (w, h) = (256u32, 192u32);
    let mut x: u32 = 7;
    let px: Vec<u8> = (0..w * h).flat_map(|i| {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        let (cx, cy) = (i % w, i / w);
        [(cx / 2) as u8, (cy + (x >> 29)) as u8, ((cx + cy) / 3) as u8, 255]
    }).collect();
    let img = crate::imgdec::Rgba { w, h, px };
    let ours = crate::thumbcache::encode_png(&img, &[("Thumb::URI", "file:///x/a%20b.jpg".into()), ("Software", "Maia".into())]).unwrap();
    let (back, text) = crate::imgdec::png_with_text(&ours).expect("decodes");
    assert_eq!((back.w, back.h), (w, h));
    assert_eq!(back.px, img.px, "lossless");
    assert!(text.iter().any(|(k, v)| k == "Thumb::URI" && v == "file:///x/a%20b.jpg"));
    // Within reach of png's own default encoder (which is what other apps write).
    let mut theirs = vec![];
    {
        let mut e = png::Encoder::new(&mut theirs, w, h);
        e.set_color(png::ColorType::Rgba);
        e.write_header().unwrap().write_image_data(&img.px).unwrap();
    }
    assert!(ours.len() < theirs.len() * 2, "ours {} vs png {}", ours.len(), theirs.len());
    eprintln!("png sizes: ours {} png crate {}", ours.len(), theirs.len());
    if let Some(d) = std::env::var_os("MAIA_ICON_DUMP") {
        std::fs::write(std::path::Path::new(&d).join("hand.png"), &ours).unwrap();
    }
}

/// Rows of `line` wrapped at `cols`, as text.
fn wrapped(line: &str, cols: usize) -> Vec<String> {
    let mut starts = vec![];
    crate::inspector::wrap_line(line, cols, &mut starts);
    starts.iter().enumerate().map(|(k, &a)| line[a as usize..starts.get(k + 1).map_or(line.len(), |&b| b as usize)].to_string()).collect()
}

#[test]
fn word_wrap_keeps_every_character_and_breaks_at_spaces() {
    let width = |s: &str| s.chars().map(crate::inspector::char_cols).sum::<usize>();
    // Real text: the licence and this crate's own sources, at narrow to wide columns.
    let text = [include_str!("../LICENSE"), include_str!("inspector.rs")].concat();
    let mut wrapped_some = 0;
    for cols in [8, 13, 40, 79] {
        for line in text.lines() {
            let rows = wrapped(line, cols);
            assert_eq!(rows.concat(), line, "nothing lost or doubled at {cols}");
            for (k, r) in rows.iter().enumerate() {
                assert!(width(r) <= cols, "{r:?} wider than {cols}");
                // A row ends at a space unless its next word would not fit on any row.
                if let Some(next) = rows.get(k + 1) {
                    let word = next.split(' ').next().unwrap_or("");
                    assert!(r.ends_with(' ') || !r.contains(' ') || width(word) + width(r.rsplit(' ').next().unwrap()) > cols, "{r:?} | {next:?} broke mid-word at {cols}");
                }
            }
            if rows.len() > 1 {
                wrapped_some += 1;
            }
            if width(line) <= cols {
                assert_eq!(rows.len(), 1, "{line:?} fits in {cols}");
            }
        }
    }
    assert!(wrapped_some > 1000, "long lines wrapped: {wrapped_some}");
    // A word longer than a row breaks inside it; wide characters count twice.
    assert_eq!(wrapped("abcdefghij", 4), ["abcd", "efgh", "ij"]);
    assert_eq!(wrapped("the quick brown fox", 10), ["the quick ", "brown fox"]);
    assert_eq!(wrapped("日本語のテキスト", 6), ["日本語", "のテキ", "スト"]);
    assert_eq!(wrapped("", 10), [""]);
}

#[test]
fn checksums_match_known_values_and_pasted_formats() {
    use crate::checksum::{Algo, Job, clean};
    let d = tmpdir("checksums");
    // A name with a newline makes the tools escape their output line.
    let odd = d.join("we\nird name.txt");
    touch(&odd, "abc");
    let ctx = egui::Context::default();
    let want = [
        (Algo::Md5, "900150983cd24fb0d6963f7d28e17f72"),
        (Algo::Sha1, "a9993e364706816aba3e25717850c26c9cd0d89d"),
        (Algo::Sha256, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
        (Algo::Sha512, "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"),
    ];
    for (algo, hex) in want {
        assert_eq!(Algo::for_hex(hex), Some(algo));
        let job = Job::start(&odd, algo, &ctx);
        let t0 = Instant::now();
        let got = loop {
            if let Some(r) = job.poll() {
                break r;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "{algo:?} never finished");
            std::thread::sleep(Duration::from_millis(5));
        };
        assert_eq!(got.as_deref(), Ok(hex), "{algo:?}");
    }
    // What download pages and tools print comes down to the digits.
    let sha = want[2].1;
    assert_eq!(clean(&format!("  {}  ", sha.to_uppercase())), sha);
    assert_eq!(clean(&format!("{sha}  ubuntu.iso")), sha);
    assert_eq!(clean(&format!("SHA256 (ubuntu.iso) = {sha}")), sha);
    assert_eq!(clean("ba7816bf 8f01cfea 414140de 5dae2223 b00361a3 96177a9c b410ff61 f20015ad"), sha);
    assert_eq!(Algo::for_hex(&clean("not a checksum")), None);
    assert_eq!(Algo::for_hex("xyz7816bf8f01cfea414140de5dae2223"), None);
    // A file that is gone says so.
    let job = Job::start(&d.join("gone"), Algo::Sha256, &ctx);
    let t0 = Instant::now();
    let r = loop {
        if let Some(r) = job.poll() {
            break r;
        }
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(r, Err("No such file or directory".into()));
}

#[test]
fn filter_conditions_by_size_and_date() {
    use std::time::SystemTime;
    let d = tmpdir("filterq");
    let now = SystemTime::now();
    let day = Duration::from_secs(86_400);
    // name, bytes, age
    let files = [("tiny.txt", 10, 0), ("page.html", 3000, 2), ("photo.jpg", 2 << 20, 10), ("movie.mkv", 150 << 20, 400), ("empty.log", 0, 40)];
    for (name, n, age) in files {
        let f = std::fs::File::create(d.join(name)).unwrap();
        f.set_len(n).unwrap();
        f.set_modified(now - day * age).unwrap();
    }
    std::fs::create_dir(d.join("Big folder")).unwrap();
    let mut stats = HashMap::new();
    let mut t = Tab::new(77, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let mut shown = |t: &mut Tab, stats: &HashMap<PathBuf, crate::stats::DirStats>, f: &str| -> Vec<String> {
        t.filter = f.into();
        pump(t, stats);
        let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
        v.sort();
        v
    };
    assert_eq!(shown(&mut t, &stats, "size>1M"), ["movie.mkv", "photo.jpg"]);
    assert_eq!(shown(&mut t, &stats, "size<2k"), ["empty.log", "tiny.txt"]);
    assert_eq!(shown(&mut t, &stats, "size<3k"), ["empty.log", "tiny.txt"], "k is 1000: 3000 bytes are not under 3k");
    assert_eq!(shown(&mut t, &stats, "size<=3k"), ["empty.log", "page.html", "tiny.txt"], "but they are 3k");
    assert_eq!(shown(&mut t, &stats, "size<=3000"), ["empty.log", "page.html", "tiny.txt"]);
    assert_eq!(shown(&mut t, &stats, "size:0"), ["empty.log"]);
    assert_eq!(shown(&mut t, &stats, "SIZE>1.5mb size<100MiB"), ["photo.jpg"]);
    assert_eq!(shown(&mut t, &stats, "modified<7d"), ["Big folder", "page.html", "tiny.txt"]);
    assert_eq!(shown(&mut t, &stats, "modified>1y"), ["movie.mkv"]);
    assert_eq!(shown(&mut t, &stats, "date:today"), ["Big folder", "tiny.txt"]);
    assert_eq!(shown(&mut t, &stats, "mod>=30d size>1k"), ["movie.mkv"]);
    // Conditions and a name search together; a folder's size counts once it is known.
    assert_eq!(shown(&mut t, &stats, "o size>1k"), ["movie.mkv", "photo.jpg"]);
    stats.insert(d.join("Big folder"), crate::stats::DirStats { size: 5 << 30, ..Default::default() });
    assert_eq!(shown(&mut t, &stats, "size>1g"), ["Big folder"]);
    // Words that only look like conditions stay part of the name search.
    assert_eq!(shown(&mut t, &stats, "size>huge"), Vec::<String>::new());
    assert_eq!(crate::query::name_part("size>huge"), "size>huge");
    assert_eq!(crate::query::name_part("report modified<2w"), "report");
    assert_eq!(crate::query::name_part("a:b"), "a:b");
    // Dates are local days, months and years.
    let tm = crate::timefmt::local(now);
    let (y, m, dd) = (tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday);
    assert_eq!(shown(&mut t, &stats, &format!("modified:{y:04}-{m:02}-{dd:02}")), ["Big folder", "tiny.txt"]);
    let before_year = shown(&mut t, &stats, &format!("modified<{y:04}"));
    assert!(before_year.contains(&"movie.mkv".to_string()) && !before_year.contains(&"tiny.txt".to_string()), "{before_year:?}");
    let this_month = shown(&mut t, &stats, &format!("modified>={y:04}-{m:02}"));
    assert!(this_month.contains(&"tiny.txt".to_string()) && !this_month.contains(&"movie.mkv".to_string()), "{this_month:?}");
    // Spans mark exactly the recognised words.
    let f = crate::query::parse("big size>10M  modified<7d x", now);
    assert_eq!(f.spans, vec![(4, 12), (14, 25)]);
    assert_eq!(f.name, "big x");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn filter_language_or_not_types_patterns_owners() {
    use std::fs::{File, FileTimes};
    use std::os::unix::fs::MetadataExt;
    use std::time::SystemTime;
    let d = tmpdir("filterlang");
    let now = SystemTime::now();
    let day = Duration::from_secs(86_400);
    // name, bytes, modified and accessed this many days ago
    let files = [
        ("report-2026.pdf", 3 << 20, 2, 400),
        ("report-draft.md", 1000, 0, 0),
        ("notes.txt", 10, 40, 1),
        ("IMG_0001.jpg", 500 << 10, 3, 3),
        ("IMG_0002.png", 400 << 10, 3, 3),
        ("photo (1).jpg", 2000, 5, 5),
        ("my-file.txt", 20, 5, 5),
        ("archive.tar.gz", 5000, 5, 5),
        ("theirs.txt", 30, 5, 5),
    ];
    for (name, n, m, a) in files {
        let f = File::create(d.join(name)).unwrap();
        f.set_len(n).unwrap();
        f.set_times(FileTimes::new().set_modified(now - day * m).set_accessed(now - day * a)).unwrap();
    }
    std::fs::create_dir_all(d.join("Projects/src")).unwrap();
    touch(&d.join("Projects/src/main.rs"), "fn main() {}");
    // Another user's file, where this user may give it away (root in the sandbox).
    let me = std::fs::metadata(d.join("notes.txt")).unwrap().uid();
    let given = std::os::unix::fs::chown(d.join("theirs.txt"), Some(65534), None).is_ok();
    let tagged = crate::tags::write(&d.join("notes.txt"), &["work".into()]).is_ok();
    let users = crate::query::users(true).unwrap();
    let stats = HashMap::new();
    let mut t = Tab::new(78, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let ctx = egui::Context::default();
    let shown = |t: &mut Tab, f: &str| -> Vec<String> {
        t.filter = f.into();
        let start = Instant::now();
        loop {
            pump(t, &stats);
            crate::tags::flush(&ctx);
            if !t.entries.iter().any(|e| crate::tags::get(&e.path, |_| ()).is_none()) || start.elapsed() > Duration::from_secs(5) {
                pump(t, &stats);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
        v.sort();
        v
    };
    // Last access, as kept by the filesystem (read before anything opens the files).
    assert_eq!(shown(&mut t, "accessed>1y"), ["report-2026.pdf"]);
    assert_eq!(shown(&mut t, "accessed<2d ext:txt,md"), ["notes.txt", "report-draft.md"]);
    // Either, written three ways.
    for q in ["ext:pdf | ext:md", "ext:pdf OR ext:md", "ext:pdf,md", "ext:pdf|ext:md"] {
        assert_eq!(shown(&mut t, q), ["report-2026.pdf", "report-draft.md"], "{q}");
    }
    // A lowercase "or" is a word, as it always was: one fuzzy search of every word.
    assert_eq!(shown(&mut t, "report or draft"), Vec::<String>::new());
    // Not: a word after it is matched as typed, a condition is turned around.
    for q in ["report -draft", "report !draft", "report NOT draft"] {
        assert_eq!(shown(&mut t, q), ["report-2026.pdf"], "{q}");
    }
    assert_eq!(shown(&mut t, "ext:md,pdf -ext:pdf"), ["report-draft.md"]);
    // 1000 bytes are 1k, not under it.
    assert_eq!(shown(&mut t, "-size<1k -type:folder"), ["IMG_0001.jpg", "IMG_0002.png", "archive.tar.gz", "photo (1).jpg", "report-2026.pdf", "report-draft.md"]);
    // A hyphen inside a word is part of it; a lone one is a word too.
    assert_eq!(shown(&mut t, "my-file"), ["my-file.txt"]);
    assert_eq!(shown(&mut t, "-"), ["my-file.txt", "report-2026.pdf", "report-draft.md"]);
    // Kinds, the smart folders' and the type's own description.
    assert_eq!(shown(&mut t, "type:pictures"), ["IMG_0001.jpg", "IMG_0002.png", "photo (1).jpg"]);
    assert_eq!(shown(&mut t, "kind:pdfs,archives"), ["archive.tar.gz", "report-2026.pdf"]);
    assert_eq!(shown(&mut t, "type:folder"), ["Projects"]);
    assert_eq!(shown(&mut t, "type:png"), ["IMG_0002.png"], "Filter By Type's label");
    // Endings: several, with or without the dot, two-part ones.
    assert_eq!(shown(&mut t, "ext:.GZ"), ["archive.tar.gz"]);
    assert_eq!(shown(&mut t, "ext:tar.gz,jpg"), ["IMG_0001.jpg", "archive.tar.gz", "photo (1).jpg"]);
    // Patterns: regular expressions on the name, globs, name:, phrases.
    assert_eq!(shown(&mut t, r"/^img_\d+\.(jpg|png)$/"), ["IMG_0001.jpg", "IMG_0002.png"]);
    assert_eq!(shown(&mut t, "re:^report|notes"), ["notes.txt", "report-2026.pdf", "report-draft.md"]);
    assert_eq!(shown(&mut t, "/(?-i)^img/"), Vec::<String>::new(), "case when asked for");
    assert_eq!(shown(&mut t, "*.jpg"), ["IMG_0001.jpg", "photo (1).jpg"]);
    assert_eq!(shown(&mut t, "name:img_*"), ["IMG_0001.jpg", "IMG_0002.png"]);
    assert_eq!(shown(&mut t, r#"name:"photo (1)""#), ["photo (1).jpg"]);
    assert_eq!(shown(&mut t, r#""to (1""#), ["photo (1).jpg"], "a phrase, its quote left open while typing");
    // A broken pattern matches nothing and is marked; an unclosed one is a word.
    assert_eq!(shown(&mut t, "/img_(/"), Vec::<String>::new());
    let f = crate::query::parse("/img_(/ x", now);
    assert_eq!((f.bad.clone(), f.spans.clone()), (vec![(0, 7)], vec![]));
    assert_eq!(crate::query::parse("/usr/lib", now).spans, vec![]);
    // Groups, in a query with an OR; without one "(1)" is part of the name as before.
    assert_eq!(shown(&mut t, "(type:pictures | ext:pdf) size>450k"), ["IMG_0001.jpg", "report-2026.pdf"]);
    assert_eq!(shown(&mut t, "-(type:pictures | ext:pdf) size>1k"), ["archive.tar.gz"]);
    assert_eq!(shown(&mut t, "photo (1)"), ["photo (1).jpg"]);
    // Everything that matched the words before still ranks first.
    t.filter = "report | ext:jpg".into();
    pump(&mut t, &stats);
    let order: Vec<&str> = t.view.iter().map(|&i| t.entries[i as usize].name.as_str()).collect();
    assert_eq!(order.len(), 4);
    assert!(order[..2].iter().all(|n| n.starts_with("report")), "{order:?}");
    // Owners, by name or number.
    let name = users.iter().find(|u| u.0 == me).map(|u| u.1.clone()).unwrap_or_else(|| me.to_string());
    let mine = shown(&mut t, &format!("owner:{name}"));
    if given {
        assert!(!mine.contains(&"theirs.txt".to_string()) && mine.len() == 9, "{mine:?}");
        assert_eq!(shown(&mut t, "owner:65534"), ["theirs.txt"]);
        assert_eq!(shown(&mut t, &format!("-owner:{name}")), ["theirs.txt"]);
    } else {
        eprintln!("files can't be given away here: owners of others skipped");
        assert_eq!(mine.len(), 10);
    }
    assert_eq!(shown(&mut t, "owner:no-such-user-here"), Vec::<String>::new());
    // Tags with the rest.
    if tagged {
        assert_eq!(shown(&mut t, "tag:work | ext:pdf"), ["notes.txt", "report-2026.pdf"]);
        assert_eq!(shown(&mut t, "ext:txt -tag:work"), ["my-file.txt", "theirs.txt"]);
    } else {
        eprintln!("no user xattrs here: tags skipped");
    }
    // In a flattened listing words and NOT see the path below the folder.
    t.set_flatten(true);
    assert_eq!(shown(&mut t, "ext:rs"), ["main.rs"]);
    assert_eq!(shown(&mut t, "ext:rs -src"), Vec::<String>::new());
    assert_eq!(shown(&mut t, "ext:rs | report -draft"), ["main.rs", "report-2026.pdf"]);
    // Marked in the accent: the operators and what is not a plain word.
    let f = crate::query::parse("report -draft | type:pdf", now);
    assert_eq!(f.spans, vec![(7, 8), (8, 13), (14, 15), (16, 24)]);
    assert_eq!(f.name, "report");
    assert_eq!(crate::query::name_part("report -draft"), "report");
    assert_eq!(crate::query::name_part("my-file"), "my-file");
    assert_eq!(crate::query::name_part(r#"todo "two words" ext:rs"#), "todo two words");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn chosen_view_stays_while_browsing_and_saved_views_still_apply() {
    use crate::config::{SaveScope, SavedView, UserData};
    let d = tmpdir("viewstays");
    for f in ["a/inner", "b", "pics/trip"] {
        std::fs::create_dir_all(d.join(f)).unwrap();
    }
    let mut ud = UserData::default();
    let mut pics = ViewSettings::default();
    pics.zoom = 0.9;
    ud.views.insert(d.join("pics"), SavedView { scope: SaveScope::Subfolders, view: pics });
    let stats = HashMap::new();
    let mut t = Tab::new(5, d.clone(), ud.view_for(&d).0, egui::Context::default());
    pump(&mut t, &stats);
    let go = |t: &mut Tab, p: PathBuf| t.navigate(p.clone(), crate::app::App::nav_view(&ud, &p));
    // Medium icons, say: kept into a folder, its child, and back up.
    t.settings.zoom = 0.4;
    go(&mut t, d.join("a"));
    assert_eq!(t.settings.zoom, 0.4, "the chosen view follows into another folder");
    go(&mut t, d.join("a/inner"));
    assert_eq!(t.settings.zoom, 0.4);
    t.up(|p| crate::app::App::nav_view(&ud, p));
    assert_eq!(t.settings.zoom, 0.4, "and back up");
    // A folder with a saved view (and its subfolders) shows that one...
    go(&mut t, d.join("pics"));
    assert_eq!(t.settings.zoom, 0.9);
    go(&mut t, d.join("pics/trip"));
    assert_eq!(t.settings.zoom, 0.9, "inherited by subfolders");
    // ...and leaving it puts back the view chosen before, with Back and Forward too.
    go(&mut t, d.join("b"));
    assert_eq!(t.settings.zoom, 0.4, "the free view returns");
    t.back(|p| crate::app::App::nav_view(&ud, p));
    assert_eq!((t.path.clone(), t.settings.zoom), (d.join("pics/trip"), 0.9));
    t.forward(|p| crate::app::App::nav_view(&ud, p));
    assert_eq!((t.path.clone(), t.settings.zoom), (d.join("b"), 0.4));
    // Changing the view again carries on from here.
    t.settings.zoom = 0.0;
    go(&mut t, d.join("a"));
    assert_eq!(t.settings.zoom, 0.0);
    let _ = std::fs::remove_dir_all(&d);
}

/// Make encrypted archives of `src` with the real tools: (ZipCrypto zip, AES zip,
/// 7z, 7z with its names encrypted), or None when zip or 7z is missing.
fn encrypted_archives(d: &Path, src: &Path, pw: &str) -> Option<[PathBuf; 4]> {
    let run = |prog: &str, args: &[&str]| std::process::Command::new(prog).args(args).current_dir(src).stdout(std::process::Stdio::null()).status().ok().filter(|s| s.success());
    let out = [d.join("crypto.zip"), d.join("aes.zip"), d.join("plain-names.7z"), d.join("hidden-names.7z")];
    let p = |i: usize| out[i].to_str().unwrap().to_string();
    run("zip", &["-q", "-r", "-P", pw, &p(0), "."])?;
    run("7z", &["a", &format!("-p{pw}"), "-tzip", "-mem=AES256", &p(1), "."])?;
    run("7z", &["a", &format!("-p{pw}"), &p(2), "."])?;
    run("7z", &["a", &format!("-p{pw}"), "-mhe=on", &p(3), "."])?;
    Some(out)
}

#[test]
fn password_protected_archives_list_extract_and_refuse() {
    use crate::archive::{self, NEEDS_PASSWORD, WRONG_PASSWORD};
    let d = tmpdir("encrypted");
    let src = d.join("src");
    std::fs::create_dir_all(src.join("Trip photos")).unwrap();
    touch(&src.join("notes.txt"), "the secret plan");
    touch(&src.join("Trip photos/beach.txt"), "sand");
    let Some(all) = encrypted_archives(&d, &src, "hunter2") else {
        eprintln!("zip or 7z missing: skipped");
        return;
    };
    for a in &all {
        let name = a.file_name().unwrap().to_string_lossy().into_owned();
        // Zips and plain-named 7z list without the password; hidden names need it.
        let listed = archive::list(a);
        if name.starts_with("hidden") {
            assert_eq!(listed.err().as_deref(), Some(NEEDS_PASSWORD), "{name}");
        } else {
            let items = listed.unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(items.iter().any(|i| i.path == Path::new("Trip photos/beach.txt")), "{name}");
        }
        // Without it: refused, and nothing half-written is left to open later.
        let x = d.join(format!("x-{name}"));
        assert_eq!(archive::extract(a, &[PathBuf::from("notes.txt")], &x).err().as_deref(), Some(NEEDS_PASSWORD), "{name}");
        assert_eq!(archive::extract_for_open(a, Path::new("notes.txt")).err().as_deref(), Some(NEEDS_PASSWORD), "{name}");
        assert!(!archive::open_cache(a).join("notes.txt").exists(), "{name}: nothing cached");
        archive::set_password(a, "wrong".into());
        let r = archive::extract_for_open(a, Path::new("notes.txt"));
        assert_eq!(r.err().as_deref(), Some(WRONG_PASSWORD), "{name}");
        assert!(!archive::open_cache(a).join("notes.txt").exists(), "{name}: no zero-filled file cached");
        // With it: listed and extracted whole, folders with their contents.
        archive::set_password(a, "hunter2".into());
        assert!(archive::list(a).unwrap().iter().any(|i| i.path == Path::new("Trip photos") && i.is_dir), "{name}");
        let f = archive::extract_for_open(a, Path::new("notes.txt")).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(std::fs::read_to_string(f).unwrap(), "the secret plan", "{name}");
        let y = d.join(format!("y-{name}"));
        archive::extract(a, &[PathBuf::from("Trip photos")], &y).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(std::fs::read_to_string(y.join("Trip photos/beach.txt")).unwrap(), "sand", "{name}");
    }
    // 7z's own listing format.
    let items = archive::parse_slt("Path = sub\nSize = 0\nModified = 2026-10-15 19:19:57.7977160\nAttributes = D drwxr-xr-x\n\nPath = sub/b.txt\nSize = 5\nModified = 2026-10-15 19:19:57.7977160\nAttributes = A -rw-r--r--\n\nPath = ../evil\nSize = 1\n\nPath = deep/x.txt\nSize = 2\n");
    let get = |p: &str| items.iter().find(|i| i.path == Path::new(p)).cloned();
    assert!(get("sub").is_some_and(|i| i.is_dir) && get("sub/b.txt").is_some_and(|i| i.size == 5 && i.modified.is_some()));
    assert!(get("deep").is_some_and(|i| i.is_dir), "implied folder");
    assert!(get("../evil").is_none(), "no climbing out");
    for c in &all {
        let _ = std::fs::remove_dir_all(archive::open_cache(c));
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn tags_live_on_the_files_and_filter_the_list() {
    use crate::tags;
    let d = tmpdir("tags");
    let (a, b, c) = (d.join("a.txt"), d.join("b.txt"), d.join("c.txt"));
    for p in [&a, &b, &c] {
        touch(p, "x");
    }
    // Stored as KDE's file managers store them: one comma-separated attribute.
    if tags::write(&a, &["Red".into(), "work".into()]).is_err() {
        eprintln!("no user xattrs here: skipped");
        return;
    }
    let raw = |p: &Path| {
        let mut buf = [0u8; 256];
        let cp = std::ffi::CString::new(p.to_str().unwrap()).unwrap();
        let n = unsafe { libc::getxattr(cp.as_ptr(), c"user.xdg.tags".as_ptr(), buf.as_mut_ptr().cast(), buf.len()) };
        (n > 0).then(|| String::from_utf8_lossy(&buf[..n as usize]).into_owned())
    };
    assert_eq!(raw(&a).as_deref(), Some("Red,work"));
    assert_eq!(tags::read(&a), ["Red", "work"]);
    assert_eq!(tags::parse(" Blue, ,taxes,Blue,"), ["Blue", "taxes"]);
    // One tag added to several files, then removed from all; the last one gone takes the
    // attribute with it.
    tags::toggle(&[a.clone(), b.clone()], "Green", true).unwrap();
    assert_eq!(tags::read(&a), ["Red", "work", "Green"]);
    assert_eq!(tags::read(&b), ["Green"]);
    tags::toggle(&[a.clone(), b.clone()], "green", false).unwrap();
    assert_eq!(tags::read(&b), Vec::<String>::new());
    assert_eq!(raw(&b), None, "no empty attribute left behind");
    // Tags written by another app are read too.
    let cp = std::ffi::CString::new(c.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::setxattr(cp.as_ptr(), c"user.xdg.tags".as_ptr(), b"Blue,taxes".as_ptr().cast(), 10, 0) }, 0);
    // The list filters by tag once the reader has them.
    let stats = HashMap::new();
    let mut t = Tab::new(91, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let ctx = egui::Context::default();
    let shown = |t: &mut Tab, f: &str| -> Vec<String> {
        t.filter = f.into();
        let start = Instant::now();
        loop {
            pump(t, &stats);
            tags::flush(&ctx);
            let pending = t.entries.iter().any(|e| tags::get(&e.path, |_| ()).is_none());
            if !pending || start.elapsed() > Duration::from_secs(5) {
                pump(t, &stats);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
        v.sort();
        v
    };
    assert_eq!(shown(&mut t, "tag:red"), ["a.txt"]);
    assert_eq!(shown(&mut t, "tag:TAXES"), ["c.txt"]);
    assert_eq!(shown(&mut t, "tag:red tag:work"), ["a.txt"]);
    assert_eq!(shown(&mut t, "tag:red tag:taxes"), Vec::<String>::new());
    assert_eq!(crate::query::name_part("report tag:work"), "report");
    // A change made elsewhere is picked up once the watcher says the file changed.
    tags::write(&a, &[]).unwrap();
    tags::invalidate(&c);
    assert_eq!(shown(&mut t, "tag:red"), Vec::<String>::new());
    assert!(tags::known().contains(&"taxes".to_string()));
    let _ = std::fs::remove_dir_all(&d);
}

/// A history for the graph, written in one `git fast-import`: `n` commits on main, a
/// side branch merged back every 25 commits, a tag every 100, a rename, and at the top
/// ten branches joined by one octopus merge (more lanes than the graph draws).
pub(crate) fn git_history_fixture(d: &Path, n: usize) {
    use std::io::Write;
    std::fs::create_dir_all(d).unwrap();
    git(d, &["init", "-q"]);
    let mut s = String::new();
    let mut mark = 0;
    let mut commit = |s: &mut String, branch: &str, msg: &str, from: Option<usize>, merges: &[usize], files: &[(&str, String)]| -> usize {
        mark += 1;
        let t = 1_700_000_000 + mark as i64 * 600;
        let who = ["Ada Lovelace", "Grace Hopper", "Linus"][mark % 3];
        s.push_str(&format!("commit refs/heads/{branch}\nmark :{mark}\nauthor {who} <dev@example.invalid> {t} +0000\ncommitter {who} <dev@example.invalid> {t} +0000\ndata {}\n{msg}\n", msg.len()));
        if let Some(f) = from {
            s.push_str(&format!("from :{f}\n"));
        }
        for m in merges {
            s.push_str(&format!("merge :{m}\n"));
        }
        for (path, body) in files {
            if let Some(old) = path.strip_prefix("R ") {
                s.push_str(&format!("R {old}\n"));
            } else {
                s.push_str(&format!("M 644 inline {path}\ndata {}\n{body}\n", body.len()));
            }
        }
        mark
    };
    let mut main = commit(&mut s, "main", "Start the notes", None, &[], &[("notes.txt", "notes\n".into()), ("old name.txt", "x\n".into())]);
    for k in 2..=n {
        if k % 25 == 0 {
            let topic = format!("topic-{k}");
            let mut t = main;
            for j in 1..=3 {
                t = commit(&mut s, &topic, &format!("Work on topic {k}, part {j}"), Some(t), &[], &[(&format!("src/topic_{k}.rs"), format!("fn part_{j}() {{}}\n"))]);
            }
            main = commit(&mut s, "main", &format!("Note {k} while topic {k} is open"), Some(main), &[], &[("notes.txt", format!("note {k}\n"))]);
            // fast-import takes a merge's tree from its first parent: bring the topic's file in.
            main = commit(&mut s, "main", &format!("Merge topic {k}"), Some(main), &[t], &[(&format!("src/topic_{k}.rs"), "fn part_3() {}\n".into())]);
        } else if k == 40 {
            main = commit(&mut s, "main", "Rename the old file", Some(main), &[], &[("R \"old name.txt\" \"new name.txt\"", String::new())]);
        } else {
            main = commit(&mut s, "main", &format!("Note {k}"), Some(main), &[], &[("notes.txt", format!("notes\nnote {k}\n"))]);
        }
        if k % 100 == 0 {
            s.push_str(&format!("reset refs/tags/v{}\nfrom :{main}\n", k / 100));
        }
    }
    let wide: Vec<usize> = (1..=10).map(|w| commit(&mut s, &format!("wide-{w}"), &format!("Wide branch {w}"), Some(main), &[], &[(&format!("wide/{w}.txt"), format!("{w}\n"))])).collect();
    let all: Vec<(String, String)> = (1..=10).map(|w| (format!("wide/{w}.txt"), format!("{w}\n"))).collect();
    commit(&mut s, "main", "Join ten branches at once", Some(main), &wide, &all.iter().map(|(p, b)| (p.as_str(), b.clone())).collect::<Vec<_>>());
    let mut c = std::process::Command::new("git").args(["fast-import", "--quiet"]).current_dir(d).stdin(std::process::Stdio::piped()).spawn().unwrap();
    c.stdin.take().unwrap().write_all(s.as_bytes()).unwrap();
    assert!(c.wait().unwrap().success(), "fast-import");
    git(d, &["reset", "-q", "--hard"]);
}

/// A PNG of `w`×`h` with a pattern made from `seed`.
pub(crate) fn png_bytes(w: u32, h: u32, seed: u32) -> Vec<u8> {
    let mut out = vec![];
    let mut e = png::Encoder::new(&mut out, w, h);
    e.set_color(png::ColorType::Rgba);
    e.set_depth(png::BitDepth::Eight);
    let px: Vec<u8> = (0..h).flat_map(|y| (0..w).flat_map(move |x| [(x * 9 + seed * 70) as u8, (y * 7 + seed * 30) as u8, (120 + seed * 50) as u8, 255])).collect();
    e.write_header().unwrap().write_image_data(&px).unwrap();
    out
}

/// A small project for the commit view, five commits by three authors. "Lay out the
/// panels" changes twelve files in several folders in every way git records: added,
/// modified, deleted, renamed with an edit, a picture changed and one added, a binary
/// file, a file that became a link. Then a long line, a deleted picture and one more
/// edit. main is in step with origin/main, v0.1 and v1.0 are tags, and origin/feature
/// is a branch only the remote has.
pub(crate) fn commit_view_fixture(d: &Path) {
    std::fs::create_dir_all(d).unwrap();
    let mut day = 0;
    let mut commit = |who: &str, msg: &str| {
        day += 1;
        let (name, email) = who.split_once(" <").unwrap();
        let date = format!("2026-09-{day:02}T10:00:00+00:00");
        let ok = std::process::Command::new("git")
            .args(["-c", &format!("user.name={name}"), "-c", &format!("user.email={}", email.trim_end_matches('>')), "-c", "commit.gpgsign=false", "add", "-A"])
            .current_dir(d)
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git add");
        let ok = std::process::Command::new("git")
            .args(["-c", &format!("user.name={name}"), "-c", &format!("user.email={}", email.trim_end_matches('>')), "-c", "commit.gpgsign=false", "commit", "-q", "-m", msg])
            .env("GIT_AUTHOR_DATE", &date)
            .env("GIT_COMMITTER_DATE", &date)
            .current_dir(d)
            .stdout(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        assert!(ok, "git commit {msg}");
    };
    let lines = |n: usize, word: &str| (1..=n).map(|i| format!("{word} line {i}\n")).collect::<String>();
    git(d, &["init", "-q"]);
    touch(&d.join("README.md"), "# Project\n\nA small project.\n");
    touch(&d.join("src/main.rs"), &format!("fn main() {{\n{}}}\n", lines(8, "    // main")));
    touch(&d.join("src/ui/view.rs"), &lines(6, "// view"));
    touch(&d.join("docs/guide.md"), &lines(12, "Guide"));
    touch(&d.join("old.txt"), &lines(3, "old"));
    touch(&d.join("notes.md"), "# Notes\n");
    touch(&d.join("tool.sh"), "#!/bin/sh\necho tool\n");
    std::fs::create_dir_all(d.join("assets/icons")).unwrap();
    std::fs::write(d.join("assets/logo.png"), png_bytes(48, 32, 1)).unwrap();
    std::fs::write(d.join("data.bin"), [0u8, 1, 2, 3, 0, 9, 8, 7]).unwrap();
    commit("Ada Lovelace <ada@example.invalid>", "Start the project");
    git(d, &["tag", "v0.1"]);
    touch(&d.join("src/main.rs"), &format!("mod ui;\n\nfn main() {{\n{}    ui::run();\n    ui::stop();\n}}\n", lines(7, "    // main")));
    touch(&d.join("src/ui/panel.rs"), &lines(12, "// panel"));
    touch(&d.join("src/ui/view.rs"), &format!("{}// view two\n// view three\n", lines(4, "// view")));
    std::fs::remove_file(d.join("old.txt")).unwrap();
    std::fs::remove_file(d.join("docs/guide.md")).unwrap();
    touch(&d.join("docs/handbook.md"), &format!("{}The handbook ends here.\n", lines(12, "Guide")));
    std::fs::write(d.join("assets/logo.png"), png_bytes(64, 40, 2)).unwrap();
    std::fs::write(d.join("assets/icons/icon.png"), png_bytes(24, 24, 3)).unwrap();
    std::fs::write(d.join("data.bin"), [0u8, 1, 2, 3, 0, 9, 8, 6, 5]).unwrap();
    std::fs::remove_file(d.join("tool.sh")).unwrap();
    std::os::unix::fs::symlink("src/main.rs", d.join("tool.sh")).unwrap();
    touch(&d.join("tests/layout.rs"), &lines(5, "// layout test"));
    touch(&d.join("tests/panel.rs"), &lines(4, "// panel test"));
    touch(&d.join("README.md"), "# Project\n\nA small project with panels.\n");
    commit("Grace Hopper <grace@example.invalid>", "Lay out the panels\n\nSplit the view into panels. The old layout code goes,\nand the guide is now the handbook.");
    git(d, &["tag", "v1.0"]);
    let long = "A long line in the notes keeps going well past the right edge of the panel, and wrapped it stays readable. ".repeat(3);
    touch(&d.join("notes.md"), &format!("# Notes\n{long}\n"));
    commit("Linus <linus@example.invalid>", "Write the long notes");
    git(d, &["update-ref", "refs/remotes/origin/feature", "HEAD"]);
    std::fs::remove_file(d.join("assets/logo.png")).unwrap();
    commit("Ada Lovelace <ada@example.invalid>", "Remove the logo");
    touch(&d.join("src/ui/panel.rs"), &format!("{}// tuned\n", lines(12, "// panel")));
    commit("Grace Hopper <grace@example.invalid>", "Tune the panel");
    git(d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
}

/// The history's commits as the graph reads them, all at once.
fn git_log_all(d: &Path) -> Vec<crate::gitlog::Commit> {
    let out = std::process::Command::new("git").arg("-C").arg(d).args(crate::gitlog::log_args()).output().unwrap();
    crate::gitlog::read_page(&mut &out.stdout[..], usize::MAX)
}

#[test]
fn git_history_lanes_edges_and_changes() {
    use crate::gitlog::{Lanes, RefKind, Row};
    if !crate::media::have("git") {
        eprintln!("git not installed: skipped");
        return;
    }
    let d = tmpdir("gitlog");
    touch(&d.join("a"), "a\n");
    git(&d, &["init", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "c1"]);
    touch(&d.join("a"), "a\nb\n");
    git(&d, &["commit", "-qam", "c2"]);
    git(&d, &["tag", "v1"]);
    git(&d, &["checkout", "-qb", "feat"]);
    touch(&d.join("f"), "f\n");
    git(&d, &["add", "f"]);
    git(&d, &["commit", "-qm", "f1"]);
    touch(&d.join("f"), "f\nf\n");
    git(&d, &["commit", "-qam", "f2"]);
    git(&d, &["checkout", "-q", "main"]);
    touch(&d.join("a"), "a\nb\nc\n");
    git(&d, &["commit", "-qam", "c3"]);
    git(&d, &["merge", "-q", "--no-ff", "-m", "merge feat", "feat"]);
    git(&d, &["mv", "a", "b"]);
    git(&d, &["commit", "-qm", "ren"]);
    let log = git_log_all(&d);
    let subjects: Vec<&str> = log.iter().map(|c| c.subject.as_str()).collect();
    assert_eq!(subjects, ["ren", "merge feat", "f2", "f1", "c3", "c2", "c1"]);
    assert_eq!(log[0].refs, [(RefKind::Head, "main".to_string())]);
    assert_eq!(log[2].refs, [(RefKind::Branch, "feat".to_string())]);
    assert_eq!(log[5].refs, [(RefKind::Tag, "v1".to_string())]);
    assert_eq!((log[1].parents.len(), log[6].parents.len(), log[0].author.as_str()), (2, 0, "Test"));
    let mut lanes = Lanes::default();
    let rows: Vec<Row> = log.iter().map(|c| lanes.place(c)).collect();
    let r = |col, up, ins: &[u16], outs: &[u16], pass: &[u16]| Row { col, up, ins: ins.to_vec(), outs: outs.to_vec(), pass: pass.to_vec() };
    // The trunk stays in lane 0; the branch takes lane 1 from the merge down to where it
    // began, and curves back into the trunk there.
    assert_eq!(rows[0], r(0, false, &[], &[0], &[]));
    assert_eq!(rows[1], r(0, true, &[], &[0, 1], &[]));
    assert_eq!(rows[2], r(1, true, &[], &[1], &[0]));
    assert_eq!(rows[3], r(1, true, &[], &[1], &[0]));
    assert_eq!(rows[4], r(0, true, &[], &[0], &[1]));
    assert_eq!(rows[5], r(0, true, &[1], &[0], &[]));
    assert_eq!(rows[6], r(0, true, &[], &[], &[]));
    // What each commit changed, and a file's diff.
    let (body, ch) = crate::gitlog::changes(&d, &log[0].hash, &log[0].parents);
    assert_eq!((body.as_str(), ch.len(), ch[0].status, ch[0].path.as_str(), ch[0].from.as_deref()), ("", 1, b'R', "b", Some("a")));
    let (_, ch) = crate::gitlog::changes(&d, &log[1].hash, &log[1].parents);
    assert_eq!(ch.iter().map(|c| (c.status, c.path.as_str())).collect::<Vec<_>>(), [(b'A', "f")], "a merge shows what it brought in");
    let (_, ch) = crate::gitlog::changes(&d, &log[6].hash, &log[6].parents);
    assert_eq!((ch.len(), ch[0].status), (1, b'A'), "the root commit adds its files");
    let (_, ch) = crate::gitlog::changes(&d, &log[4].hash, &log[4].parents);
    let (lines, _, cut) = crate::gitlog::diff(&d, &log[4].hash, &log[4].parents, &ch[0]);
    assert_eq!(lines, ["@@ -1,2 +1,3 @@", " a", " b", "+c"]);
    assert!(!cut);
    assert_eq!(crate::gitlog::ago(1000, 1000 + 3 * 86_400 + 5), "3 days ago");
    assert_eq!(crate::gitlog::ago(1000, 1000 + 3600), "1 hour ago");
    assert_eq!(crate::gitlog::ago(1000, 1010), "just now");
    // Not a repository, or one without commits: nothing to read.
    let empty = tmpdir("gitlog-empty");
    git(&empty, &["init", "-q"]);
    assert!(git_log_all(&empty).is_empty());
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&empty);
}

#[test]
fn git_history_pages_and_wide_merges() {
    use crate::gitlog::{Lanes, PAGE, read_page};
    if !crate::media::have("git") {
        eprintln!("git not installed: skipped");
        return;
    }
    let d = tmpdir("gitlog-big");
    git_history_fixture(&d, 700);
    let all = git_log_all(&d);
    assert!(all.len() > 2 * PAGE + 50, "{} commits", all.len());
    // Read page by page from one running git, lanes carried across: the same graph as
    // all at once.
    let mut child = std::process::Command::new("git").arg("-C").arg(&d).args(crate::gitlog::log_args()).stdout(std::process::Stdio::piped()).spawn().unwrap();
    let mut r = std::io::BufReader::new(child.stdout.take().unwrap());
    let (mut paged, mut one) = (Lanes::default(), Lanes::default());
    let mut n = 0;
    loop {
        let page = read_page(&mut r, PAGE);
        for c in &page {
            assert_eq!(paged.place(c), one.place(&all[n]), "row {n}");
            assert_eq!(c.hash, all[n].hash);
            n += 1;
        }
        if page.len() < PAGE {
            break;
        }
    }
    let _ = child.wait();
    assert_eq!(n, all.len());
    // The octopus at the top fans out into eleven lanes; the trunk keeps lane 0 and the
    // lanes are free again further down.
    let mut lanes = Lanes::default();
    let rows: Vec<_> = all.iter().map(|c| lanes.place(c)).collect();
    assert_eq!(all[0].subject, "Join ten branches at once");
    assert_eq!(rows[0].outs.len(), 11);
    assert!(rows[0].outs.iter().any(|&l| l as usize >= crate::gitlog::LANES), "more lanes than drawn");
    let trunk: Vec<usize> = all.iter().enumerate().filter(|(_, c)| c.subject.starts_with("Note ")).map(|(i, _)| i).collect();
    assert!(trunk.iter().all(|&i| rows[i].col == 0), "main stays in the first lane");
    let topic = all.iter().position(|c| c.subject == "Work on topic 50, part 3").unwrap();
    assert_eq!(rows[topic].col, 1);
    assert!(rows.iter().skip(20).all(|r| r.pass.len() <= 2), "lanes are given back");
    let tags: Vec<&str> = all.iter().flat_map(|c| c.refs.iter().filter(|r| r.0 == crate::gitlog::RefKind::Tag).map(|r| r.1.as_str())).collect();
    assert_eq!(tags, ["v7", "v6", "v5", "v4", "v3", "v2", "v1"]);
    let ren = all.iter().find(|c| c.subject == "Rename the old file").unwrap();
    let (_, ch) = crate::gitlog::changes(&d, &ren.hash, &ren.parents);
    assert_eq!((ch[0].status, ch[0].path.as_str(), ch[0].from.as_deref()), (b'R', "new name.txt", Some("old name.txt")));
    let _ = std::fs::remove_dir_all(&d);
}

/// Line counts as `git diff-tree -z --numstat` writes them (binary files, a rename with
/// its two paths) joined to the name-status list, then against real git.
#[test]
fn git_numstat_and_renames_in_a_commit() {
    use crate::gitlog::{Change, join_numstat, parse_changes, parse_numstat};
    let num = b"-\t-\tb.bin\x001\t0\t\x00a.txt\x00d/a2.txt\x000\t1\td/x\x001\t0\tnew\x00";
    let counts = parse_numstat(num);
    assert_eq!(counts, [(None, "b.bin".to_string()), (Some((1, 0)), "d/a2.txt".into()), (Some((0, 1)), "d/x".into()), (Some((1, 0)), "new".into())]);
    let mut files = parse_changes(b"M\x00b.bin\x00R075\x00a.txt\x00d/a2.txt\x00D\x00d/x\x00A\x00new\x00");
    join_numstat(&mut files, &counts);
    let ch = |status: u8, path: &str, from: Option<&str>, lines: Option<(u32, u32)>| Change { status, path: path.into(), from: from.map(String::from), lines };
    assert_eq!(files, [ch(b'M', "b.bin", None, None), ch(b'R', "d/a2.txt", Some("a.txt"), Some((1, 0))), ch(b'D', "d/x", None, Some((0, 1))), ch(b'A', "new", None, Some((1, 0)))]);
    // A count list cut short or out of order is matched by path.
    let mut files = parse_changes(b"A\x00new\x00D\x00d/x\x00");
    join_numstat(&mut files, &counts[2..]);
    assert_eq!((files[0].lines, files[1].lines), (Some((1, 0)), Some((0, 1))));
    if !crate::media::have("git") {
        eprintln!("git not installed: the rest skipped");
        return;
    }
    let d = tmpdir("commitview");
    commit_view_fixture(&d);
    let log = git_log_all(&d);
    let panels = log.iter().find(|c| c.subject == "Lay out the panels").unwrap();
    let (body, files) = crate::gitlog::changes(&d, &panels.hash, &panels.parents);
    assert!(body.starts_with("Split the view into panels."), "{body:?}");
    let get = |p: &str| files.iter().find(|f| f.path == p).unwrap_or_else(|| panic!("{p} in {files:?}"));
    assert_eq!(files.len(), 12);
    assert_eq!((get("src/main.rs").status, get("src/main.rs").lines), (b'M', Some((4, 1))));
    assert_eq!((get("src/ui/panel.rs").status, get("src/ui/panel.rs").lines), (b'A', Some((12, 0))));
    assert_eq!((get("old.txt").status, get("old.txt").lines), (b'D', Some((0, 3))));
    assert_eq!((get("docs/handbook.md").status, get("docs/handbook.md").from.as_deref(), get("docs/handbook.md").lines), (b'R', Some("docs/guide.md"), Some((1, 0))));
    assert_eq!((get("assets/logo.png").status, get("assets/logo.png").lines), (b'M', None), "a picture is binary");
    assert_eq!(get("data.bin").lines, None);
    assert_eq!(get("tool.sh").status, b'T', "a file that became a link");
    assert_eq!(files.iter().filter_map(|f| f.lines).map(|l| l.0).sum::<u32>(), 30);
    // Both sides of the changed picture, only the new side of the added one; the file
    // as of the commit; one file's commits followed through its rename.
    let logo = get("assets/logo.png");
    let side = |old: bool, f: &Change| crate::gitlog::picture_side(&d, &panels.hash, &panels.parents, f, old, 512).map(|p| p.1);
    assert_eq!((side(true, logo), side(false, logo)), (Some([48, 32]), Some([64, 40])));
    assert_eq!(side(true, get("assets/icons/icon.png")), None);
    assert_eq!(side(false, get("assets/icons/icon.png")), Some([24, 24]));
    match crate::gitlog::blob(&d, &panels.hash, "src/main.rs") {
        crate::gitlog::Blob::Text(l, spans, cut) => assert!(l[0] == "mod ui;" && spans.is_some() && !cut),
        _ => panic!("main.rs as text"),
    }
    assert!(matches!(crate::gitlog::blob(&d, &panels.hash, "data.bin"), crate::gitlog::Blob::Nothing("Binary file")));
    assert!(matches!(crate::gitlog::blob(&d, &panels.hash, "assets/logo.png"), crate::gitlog::Blob::Picture(_, [64, 40])));
    let out = std::process::Command::new("git").arg("-C").arg(&d).args(crate::gitlog::file_log_args("docs/handbook.md")).output().unwrap();
    let follow: Vec<String> = crate::gitlog::read_page(&mut &out.stdout[..], 100).into_iter().map(|c| c.subject).collect();
    assert_eq!(follow, ["Lay out the panels", "Start the project"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// A commit's header: author and email, and its refs from git's decorations (a remote's
/// HEAD dropped, a branch in step with its remote one pill, a remote-only branch with
/// its own, tags).
#[test]
fn git_header_refs_and_author() {
    use crate::gitlog::{RefKind, labels, parse};
    let rec = "\n0123456789abcdef0123456789abcdef01234567\x1faaaaaaa bbbbbbb\x1fAda Lovelace\x1fada@example.invalid\x1f1700000000\x1fHEAD -> refs/heads/main, refs/remotes/origin/main, refs/remotes/origin/HEAD, tag: refs/tags/v1.0, refs/remotes/origin/feature, refs/stash\x1fLay out the panels";
    let c = parse(rec.as_bytes()).expect("a record");
    assert_eq!((c.author.as_str(), c.email.as_str(), c.time, c.parents.len(), c.subject.as_str()), ("Ada Lovelace", "ada@example.invalid", 1_700_000_000, 2, "Lay out the panels"));
    assert_eq!(c.refs, [(RefKind::Head, "main".to_string()), (RefKind::Remote, "origin/main".into()), (RefKind::Tag, "v1.0".into()), (RefKind::Remote, "origin/feature".into())]);
    assert_eq!(labels(&c.refs), [(RefKind::Head, "main", Some("origin")), (RefKind::Tag, "v1.0", None), (RefKind::Remote, "origin/feature", None)]);
    let detached = parse(b"0123456789abcdef0123456789abcdef01234567\x1f\x1fX\x1f\x1f0\x1fHEAD\x1froot").unwrap();
    assert_eq!((detached.refs.clone(), detached.parents.len()), (vec![(RefKind::Head, "HEAD".to_string())], 0));
    assert!(parse(b"short\x1f\x1fX\x1f\x1f0\x1f\x1fs").is_none(), "not a hash");
}

/// An author keeps one colour: from the email, whatever its case or spacing, else from
/// the name; the hash is fixed, so it stays across sessions. Initials read the first
/// and last words.
#[test]
fn git_author_colour_is_stable() {
    use crate::gitlog::{author_color, author_hue, initials};
    let a = author_hue("ada@example.invalid", "Ada Lovelace");
    assert_eq!(a, author_hue(" ADA@Example.invalid ", "someone else"));
    assert_eq!(author_hue("", "Linus"), author_hue("", "linus"), "no email: the name");
    // Pinned: a change to the hash would recolour every author.
    assert_eq!((a, author_hue("grace@example.invalid", ""), author_hue("linus@example.invalid", "")), (8, 0, 9));
    let hues: HashSet<usize> = (0..40).map(|i| author_hue(&format!("dev{i}@example.invalid"), "")).collect();
    assert!(hues.len() >= 6, "authors spread over the colours: {hues:?}");
    let (dark, light) = (crate::theme::palette(crate::theme::ThemeName::Midnight), crate::theme::palette(crate::theme::ThemeName::Paper));
    assert_eq!(author_color(&dark, "ada@example.invalid", ""), author_color(&dark, "ADA@example.invalid", ""));
    assert_ne!(author_color(&dark, "ada@example.invalid", ""), author_color(&light, "ada@example.invalid", ""), "toned to the theme");
    assert_eq!(initials("Ada Lovelace"), "AL");
    assert_eq!(initials("Grace Brewster Murray Hopper"), "GH");
    assert_eq!(initials("linus"), "L");
    assert_eq!(initials("first.last"), "FL");
    assert_eq!(initials("  "), "?");
    assert_eq!(initials("émile zola"), "ÉZ");
}

/// Many files grouped by folder: folders first, a folder holding only a folder as one
/// heading, each heading knowing how many rows and files it holds. Few files, or files
/// in one folder, stay a list. And the counts' five blocks and summary.
#[test]
fn git_files_grouped_by_folder() {
    use crate::gitlog::{Change, Node, blocks, group, grouped, summary};
    let f = |p: &str| Change { status: b'M', path: p.into(), from: None, lines: Some((1, 1)) };
    let files: Vec<Change> = ["README.md", "src/main.rs", "src/ui/view.rs", "src/ui/panel.rs", "docs/a/b/guide.md", "tests/x.rs", "tests/y.rs", "Cargo.toml", "src/lib.rs"].iter().map(|p| f(p)).collect();
    assert!(grouped(&files));
    assert!(!grouped(&files[..8]), "eight files stay a list");
    let one: Vec<Change> = (0..20).map(|i| f(&format!("src/f{i}.rs"))).collect();
    assert!(!grouped(&one), "all in one folder");
    let dir = |path: &str, name: &str, depth: u16, len: usize, files: usize| Node::Dir { path: path.into(), name: name.into(), depth, len, files };
    let file = |idx: usize, depth: u16| Node::File { idx, depth };
    assert_eq!(
        group(&files),
        [
            dir("docs/a/b", "docs/a/b", 0, 1, 1),
            file(4, 1),
            dir("src", "src", 0, 5, 4),
            dir("src/ui", "ui", 1, 2, 2),
            file(3, 2),
            file(2, 2),
            file(8, 1),
            file(1, 1),
            dir("tests", "tests", 0, 2, 2),
            file(5, 1),
            file(6, 1),
            file(7, 0),
            file(0, 0),
        ]
    );
    assert_eq!(blocks(0, 0), (0, 0));
    assert_eq!(blocks(3, 0), (3, 0), "fewer lines than blocks");
    assert_eq!(blocks(100, 0), (5, 0));
    assert_eq!(blocks(1, 99), (1, 4), "a side with lines keeps a block");
    assert_eq!(blocks(99, 1), (4, 1));
    assert_eq!(blocks(30, 30), (3, 2));
    assert_eq!((summary(1), summary(12)), ("1 file changed".to_string(), "12 files changed".to_string()));
    assert!(crate::gitlog::is_picture("assets/Logo.PNG") && crate::gitlog::is_picture("a.svg") && !crate::gitlog::is_picture("png") && !crate::gitlog::is_picture("x.png/notes"));
}

/// The history's git runs a page ahead of nothing: after the first page it waits on its
/// pipe until asked, and closing the history kills it.
#[test]
fn git_history_reads_on_demand_and_stops() {
    use crate::gitlog::{History, PAGE};
    if !crate::media::have("git") {
        eprintln!("git not installed: skipped");
        return;
    }
    let d = tmpdir("gitlog-feed");
    git_history_fixture(&d, 700);
    let ctx = egui::Context::default();
    let mut h = History::new(d.clone(), "main".into(), false);
    let wait = |h: &mut History, n: usize| {
        let t0 = Instant::now();
        while h.state().0 < n && t0.elapsed() < Duration::from_secs(10) {
            h.poll(&ctx);
            std::thread::sleep(Duration::from_millis(5));
        }
        h.state().0
    };
    assert_eq!(wait(&mut h, PAGE), PAGE);
    std::thread::sleep(Duration::from_millis(300));
    h.poll(&ctx);
    assert_eq!(h.state().0, PAGE, "nothing more read until asked");
    let pid = h.git_pid();
    assert!(pid != 0 && std::path::Path::new(&format!("/proc/{pid}")).exists(), "git waits on its pipe");
    h.more(&ctx);
    assert_eq!(wait(&mut h, 2 * PAGE), 2 * PAGE);
    drop(h);
    let t0 = Instant::now();
    let gone = || std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |s| s.split_whitespace().nth(2) == Some("Z"));
    while !gone() && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(gone(), "git is stopped with its history");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_repositorys_own_config_runs_nothing_when_maia_reads_it() {
    let d = tmpdir("evilrepo");
    let g = |args: &[&str]| assert!(std::process::Command::new("git").args(args).current_dir(&d).stdout(std::process::Stdio::null()).status().is_ok_and(|s| s.success()), "git {args:?}");
    g(&["init", "-q"]);
    touch(&d.join("notes.txt"), "one");
    g(&["add", "notes.txt"]);
    g(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "first"]);
    // As a downloaded repository could arrive: an fsmonitor hook and filters (one named
    // with a space) that each leave a marker if run, and files whose stat no longer
    // matches the index, so status re-reads them through the filters.
    let mark = |n: &str| format!("touch '{}'", d.join(n).display());
    g(&["config", "core.fsmonitor", &mark("ran-fsmonitor")]);
    g(&["config", "filter.x.clean", &format!("sh -c \"{}; cat\"", mark("ran-clean"))]);
    g(&["config", "filter.my x.process", &mark("ran-process")]);
    touch(&d.join(".gitattributes"), "*.txt filter=x\n*.md filter=my\\ x\n");
    touch(&d.join("notes.txt"), "two");
    touch(&d.join("readme.md"), "new");
    let repo = crate::gitstatus::query(&d).expect("read the status");
    assert!(!repo.marks.is_empty(), "the status was still read");
    let (_, changed) = crate::gitlog::changes(&d, "HEAD", &[]);
    assert!(!changed.is_empty());
    for m in ["ran-fsmonitor", "ran-clean", "ran-process"] {
        assert!(!d.join(m).exists(), "{m}: the repository's config ran a command");
    }
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn ahead_behind_and_branch_labels_from_real_remotes() {
    use crate::gitlog::{RefKind, labels};
    let d = tmpdir("remotes");
    let run = |at: &Path, args: &[&str]| {
        let out = std::process::Command::new("git").args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"]).args(args).current_dir(at).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    // A "server" and two clones of it: ours, and someone else's that pushes once.
    std::fs::create_dir_all(d.join("server.git")).unwrap();
    run(&d.join("server.git"), &["init", "-q", "--bare"]);
    run(&d, &["clone", "-q", "server.git", "ours"]);
    let ours = d.join("ours");
    touch(&ours.join("a.txt"), "a");
    run(&ours, &["add", "."]);
    run(&ours, &["commit", "-qm", "first"]);
    run(&ours, &["push", "-q", "-u", "origin", "main"]);
    run(&ours, &["branch", "docs"]);
    run(&ours, &["push", "-q", "origin", "docs"]);
    run(&d, &["clone", "-q", "server.git", "theirs"]);
    touch(&d.join("theirs/b.txt"), "b");
    run(&d.join("theirs"), &["add", "."]);
    run(&d.join("theirs"), &["commit", "-qm", "theirs"]);
    run(&d.join("theirs"), &["push", "-q"]);
    for n in ["c", "e"] {
        touch(&ours.join(n), n);
        run(&ours, &["add", "."]);
        run(&ours, &["commit", "-qm", n]);
    }
    run(&ours, &["fetch", "-q"]);
    let repo = crate::gitstatus::query(&ours).expect("status");
    assert_eq!((repo.branch.as_str(), repo.ahead, repo.behind), ("main", 2, 1), "two to push, one to pull");
    // Status lines git writes, parsed.
    let line = |s: &str| crate::gitstatus::parse(ours.clone(), format!("## {s}\0").as_bytes());
    assert_eq!((line("main...origin/main [behind 7]").ahead, line("main...origin/main [behind 7]").behind), (0, 7));
    assert_eq!((line("main...origin/main [gone]").ahead, line("main...origin/main [gone]").behind), (0, 0));
    assert_eq!((line("topic").branch.as_str(), line("topic").ahead), ("topic", 0), "no upstream");
    // The refs on each commit, from git's own decorations: docs sits with origin/docs (one
    // pill), main is ahead of origin/main (two pills, the remote one marked as such).
    let log = run(&ours, &["log", "-z", "--decorate=full", crate::gitlog::FORMAT, "--branches", "--remotes", "--tags", "HEAD", "--"]);
    let commits: Vec<crate::gitlog::Commit> = log.split('\0').filter_map(|r| crate::gitlog::parse(r.as_bytes())).collect();
    let on = |subject: &str| commits.iter().find(|c| c.subject == subject).map(|c| labels(&c.refs).into_iter().map(|(k, n, r)| (k, n.to_string(), r.map(String::from))).collect::<Vec<_>>()).unwrap();
    assert_eq!(on("e"), vec![(RefKind::Head, "main".into(), None)]);
    assert_eq!(on("first"), vec![(RefKind::Branch, "docs".into(), Some("origin".into()))]);
    assert_eq!(on("theirs"), vec![(RefKind::Remote, "origin/main".into(), None)]);
    let _ = std::fs::remove_dir_all(&d);
}

fn unbase64(s: &str) -> Vec<u8> {
    let val = |c: u8| match c {
        b'A'..=b'Z' => c - b'A',
        b'a'..=b'z' => c - b'a' + 26,
        b'0'..=b'9' => c - b'0' + 52,
        b'+' => 62,
        _ => 63,
    };
    let b: Vec<u8> = s.bytes().filter(|&c| c != b'=').collect();
    b.chunks(4).flat_map(|c| {
        let n = c.iter().enumerate().fold(0u32, |n, (k, &x)| n | (val(x) as u32) << (18 - 6 * k));
        (0..c.len() - 1).map(move |k| (n >> (16 - 8 * k)) as u8)
    }).collect()
}

/// What test pictures can show; a picture's red tells which (it survives the shrinking
/// a picture gets on its way to the model).
const CAPTIONS: &[&str] = &["a puppy playing in the sand", "a spreadsheet of numbers", "icon"];

/// The largest side of a picture the fake Ollama was sent.
static PICTURE_SIDE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// What the fake Ollama "sees" in a picture.
fn caption(png: &[u8]) -> String {
    let Some(img) = crate::imgdec::decode_bytes(png) else { return String::new() };
    PICTURE_SIDE.fetch_max(img.w.max(img.h), std::sync::atomic::Ordering::Relaxed);
    let red = img.px.first().copied().unwrap_or(0) as usize;
    CAPTIONS.get(red / 40).map_or(String::new(), |c| c.to_string())
}

/// A picture for the tests, 640 by 480 (a photo's shape) showing `caption`, one of CAPTIONS.
pub(crate) fn captioned_png(path: &Path, caption: &str) {
    sized_png(path, caption, 640, 480)
}

pub(crate) fn sized_png(path: &Path, caption: &str, w: u32, h: u32) {
    let red = CAPTIONS.iter().position(|c| *c == caption).expect("a known caption") as u8 * 40 + 20;
    let mut seed = 0x2545f491u32;
    let px = (0..w * h).flat_map(|_| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        [red, (seed >> 8) as u8, (seed >> 16) as u8, 255]
    }).collect();
    let img = crate::imgdec::Rgba { w, h, px };
    std::fs::write(path, crate::thumbcache::encode_png(&img, &[]).unwrap()).unwrap();
}

/// A stand-in for Ollama on a local port, speaking its HTTP API: /api/tags, a chunked
/// /api/pull with progress, and /api/embed. Its "model" puts words about one topic in
/// the same direction (money, holidays, cooking, pets, meetings), so a question finds a
/// file about the same thing without sharing its words. Returns its address, whether the
/// model is "downloaded", and how many texts it has embedded.
pub(crate) fn fake_ollama(has_model: bool) -> (String, std::sync::Arc<std::sync::atomic::AtomicBool>, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::Relaxed};
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let model = std::sync::Arc::new(AtomicBool::new(has_model));
    let embedded = std::sync::Arc::new(AtomicUsize::new(0));
    let (m, n) = (model.clone(), embedded.clone());
    const TOPICS: &[&[&str]] = &[
        &["invoice", "bill", "receipt", "payment", "tax", "taxes", "refund", "bank", "money", "government", "owe", "paid"],
        &["holiday", "vacation", "trip", "beach", "travel", "flight", "hotel", "sea", "sand", "summer"],
        &["recipe", "cooking", "pasta", "dinner", "bake", "kitchen", "soup", "eat", "food", "sauce"],
        &["dog", "cat", "pet", "puppy", "vet", "animal", "walk", "leash"],
        &["meeting", "agenda", "minutes", "schedule", "calendar", "standup", "monday"],
    ];
    let vector = |text: &str| -> Vec<f32> {
        let body = text.rsplit_once(" | query: ").or_else(|| text.rsplit_once(" | text: ")).map_or(text, |(_, b)| b);
        let mut v = vec![0.0f32; 768];
        for w in body.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase) {
            match TOPICS.iter().position(|t| t.contains(&w.as_str())) {
                Some(t) => v[t] += 3.0,
                None => {
                    let h = w.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
                    v[32 + (h as usize % 700)] += 0.4;
                }
            }
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-6);
        v.iter().map(|x| x / norm).collect()
    };
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut line = String::new();
            if r.read_line(&mut line).is_err() {
                continue;
            }
            let (method, path) = { let mut p = line.split_whitespace(); (p.next().unwrap_or("").to_string(), p.next().unwrap_or("").to_string()) };
            let mut len = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap_or(0) == 0 || h.trim().is_empty() {
                    break;
                }
                if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; len];
            let _ = r.read_exact(&mut body);
            let mut s = s;
            let reply = |s: &mut std::net::TcpStream, code: &str, json: String| {
                let _ = write!(s, "HTTP/1.1 {code}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}", json.len());
            };
            match (method.as_str(), path.as_str()) {
                ("GET", "/api/tags") => {
                    let models = if m.load(Relaxed) { r#"[{"name":"hf.co/ggml-org/embeddinggemma-2-GGUF:Q8_0","size":864000000}]"# } else { "[]" };
                    reply(&mut s, "200 OK", format!(r#"{{"models":{models}}}"#));
                }
                ("POST", "/api/pull") => {
                    // Streamed as Ollama streams it: chunked, one JSON object per line.
                    let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n");
                    let mut lines = vec![r#"{"status":"pulling manifest"}"#.to_string()];
                    lines.extend((1..=4).map(|k| format!(r#"{{"status":"pulling","digest":"sha256:ab","total":300000000,"completed":{}}}"#, k * 75_000_000)));
                    lines.push(r#"{"status":"success"}"#.into());
                    for l in lines {
                        let chunk = format!("{l}\n");
                        let _ = write!(s, "{:x}\r\n{chunk}\r\n", chunk.len());
                        let _ = s.flush();
                        std::thread::sleep(std::time::Duration::from_millis(20));
                    }
                    let _ = write!(s, "0\r\n\r\n");
                    m.store(true, Relaxed);
                }
                ("POST", "/api/embed") if m.load(Relaxed) => {
                    let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
                    // Text as it is; a picture by the caption its PNG carries (the test's
                    // stand-in for what the real model sees in it).
                    let inputs: Vec<String> = v["input"].as_array().map(|a| a.iter().map(|x| match x.as_str() {
                        Some(t) => t.to_string(),
                        None => caption(&unbase64(x["image"].as_str().unwrap_or(""))),
                    }).collect()).unwrap_or_default();
                    n.fetch_add(inputs.len(), Relaxed);
                    let embs: Vec<Vec<f32>> = inputs.iter().map(|t| vector(t)).collect();
                    reply(&mut s, "200 OK", serde_json::json!({ "model": "embeddinggemma", "embeddings": embs }).to_string());
                }
                ("POST", "/api/embed") => reply(&mut s, "404 Not Found", r#"{"error":"model not found, try pulling it first"}"#.into()),
                _ => reply(&mut s, "404 Not Found", r#"{"error":"not found"}"#.into()),
            }
        }
    });
    (addr, model, embedded)
}

#[test]
fn search_by_meaning_through_ollama() {
    use crate::ollama::{self, Status};
    use crate::semantic::{self, Engine, Phase};
    use std::sync::atomic::Ordering::Relaxed;
    let set_host = |h: &str| *ollama::TEST_HOST.lock().unwrap() = Some(h.to_string());
    // Nothing listening: not running, said plainly and quickly.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().to_string();
    set_host(&closed);
    let t0 = Instant::now();
    assert_eq!(ollama::status(), Status::NotRunning);
    assert!(t0.elapsed() < Duration::from_secs(2));
    assert_eq!(ollama::embed(&["x".into()]).err().as_deref(), Some("Ollama isn't running"));
    // Running without the model: asked to fetch it, which streams progress.
    let (addr, _model, embedded) = fake_ollama(false);
    set_host(&addr);
    assert_eq!(ollama::status(), Status::NoModel);
    assert!(ollama::embed(&["x".into()]).err().unwrap().contains("not found"));
    let mut seen = vec![];
    ollama::pull(|d, a| seen.push((d, a)), &std::sync::atomic::AtomicBool::new(false)).unwrap();
    assert_eq!(seen.last(), Some(&(300_000_000, 300_000_000)));
    assert_eq!(seen.len(), 4, "every progress line, through the chunked stream");
    assert_eq!(ollama::status(), Status::Ready);
    let e = ollama::embed(&["task: search result | query: a bill".into(), "title: none | text: a beach".into()]).unwrap();
    assert_eq!(e.len(), 2);
    for v in &e {
        assert_eq!(v.len(), ollama::DIMS);
        assert!((v.iter().map(|x| x * x).sum::<f32>() - 1.0).abs() < 1e-3, "length 1 after cutting to {}", ollama::DIMS);
    }
    // Passages: the whole text covered, each about a passage long, on character edges.
    let text = [include_str!("../LICENSE"), "日本語のテキスト。".repeat(300).as_str()].concat();
    let ps = semantic::cut(&text);
    assert!(ps.len() > 20);
    let mut reach = 0;
    for &(a, n) in &ps {
        assert!(text.is_char_boundary(a as usize) && text.is_char_boundary((a + n) as usize));
        assert!(n as usize <= 1800 + 4);
        assert!(a as usize <= reach, "a gap before {a}");
        reach = reach.max((a + n) as usize);
    }
    assert!(reach + 20 >= text.len() || ps.len() == 120, "covered to {reach} of {}", text.len());
    // A folder of files; the engine reads them and finds them by meaning.
    let d = tmpdir("meaning");
    touch(&d.join("letter from the bank.txt"), "Dear customer, your tax refund has been paid into your account. Keep this receipt.");
    touch(&d.join("summer.md"), "# Plans\n\nTwo weeks at the beach, flight on Friday, hotel near the sea.");
    touch(&d.join("Notes/dinner.txt"), "Pasta with tomato sauce: boil, stir, eat. A kitchen classic.");
    touch(&d.join("node_modules/skip.txt"), "invoice invoice invoice");
    touch(&d.join(".hidden/skip.txt"), "invoice invoice invoice");
    captioned_png(&d.join("IMG_0412.png"), "a puppy playing in the sand");
    // Pictures that aren't someone's: an icon, an app's artwork, frames cut from a video.
    sized_png(&d.join("app.png"), "icon", 64, 64);
    std::fs::create_dir_all(d.join("theme/icons")).unwrap();
    sized_png(&d.join("theme/icons/folder.png"), "icon", 256, 256);
    std::fs::create_dir_all(d.join("clip/frames")).unwrap();
    for k in 0..45 {
        sized_png(&d.join(format!("clip/frames/frame_{k:04}.png")), "a spreadsheet of numbers", 220, 220);
    }
    touch(&d.join("clip/frames/notes.txt"), "Frames cut from the holiday video at 30 per second.");
    // The same photo copied elsewhere: read once.
    std::fs::create_dir_all(d.join("Backup")).unwrap();
    std::fs::copy(d.join("IMG_0412.png"), d.join("Backup/IMG_0412.png")).unwrap();
    let _ = std::fs::remove_file(std::env::temp_dir().join("maia/meaning.idx"));
    let ctx = egui::Context::default();
    let start = embedded.load(Relaxed);
    let engine = Engine::start(vec![d.clone()], true, &ctx);
    let t0 = Instant::now();
    while engine.phase() != Phase::Ready {
        assert!(t0.elapsed() < Duration::from_secs(20), "stuck at {:?}", engine.phase());
        std::thread::sleep(Duration::from_millis(10));
    }
    let idx = engine.index();
    assert_eq!(idx.files(), 6, "text and pictures; hidden and build folders, icons and video frames skipped");
    assert_eq!(embedded.load(Relaxed) - start, 5, "four texts and one picture: the copy shares its fingerprint");
    assert_eq!(PICTURE_SIDE.load(Relaxed), 512, "pictures shrunk before they go to the model");
    let best = |e: &Engine, q: &str| semantic::search(&e.index(), q, 3).unwrap().first().map(|h| h.path.file_name().unwrap().to_string_lossy().into_owned());
    assert_eq!(best(&engine, "money back from the government").as_deref(), Some("letter from the bank.txt"), "no word in common, same topic");
    assert_eq!(best(&engine, "travel in summer").as_deref(), Some("summer.md"));
    assert_eq!(best(&engine, "cooking tonight").as_deref(), Some("dinner.txt"));
    assert_eq!(best(&engine, "photo of my dog").as_deref(), Some("IMG_0412.png"), "a picture by what it shows");
    let dogs = semantic::search(&engine.index(), "photo of my dog", 10).unwrap().iter().filter(|h| h.path.ends_with("IMG_0412.png")).count();
    assert_eq!(dogs, 1, "the copy in Backup isn't shown twice");
    let hit = &semantic::search(&engine.index(), "food", 1).unwrap()[0];
    assert!(hit.passage.starts_with("Pasta with tomato sauce"), "the passage shown: {:?}", hit.passage);
    // Saved, and on the next start only changed files are read again.
    let before = embedded.load(Relaxed);
    drop(engine);
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(semantic::load(&std::env::temp_dir().join("maia/meaning.idx")).files(), 6, "saved to the cache");
    std::thread::sleep(Duration::from_secs(1));
    touch(&d.join("summer.md"), "# Plans\n\nWe changed our mind: a puppy instead of a trip. The dog needs a vet.");
    std::fs::remove_file(d.join("Notes/dinner.txt")).unwrap();
    let engine = Engine::start(vec![d.clone()], true, &ctx);
    let t0 = Instant::now();
    while engine.phase() != Phase::Ready {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(embedded.load(Relaxed) - before, 1, "only the changed file was read again");
    assert_eq!(engine.index().files(), 5, "the deleted file is gone from the index");
    assert_eq!(best(&engine, "summer plans changed for a pet").as_deref(), Some("summer.md"));
    drop(engine);
    // Pictures left out when "Include pictures" is off.
    let engine = Engine::start(vec![d.clone()], false, &ctx);
    let t0 = Instant::now();
    while engine.phase() != Phase::Ready {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(engine.index().files(), 3, "text only");
    drop(engine);
    // Code, data and vendored folders are left out: only writing is read. On battery a
    // new picture waits for the charger while that is still read.
    let body = (0..40).map(|k| format!("    // Step {k}: fetch the lyrics for the playing song and keep them in memory\n")).collect::<String>();
    touch(&d.join("lyrics.py"), &body);
    touch(&d.join("package-lock.json"), "{ \"name\": \"lyrics for the playing song and more words\" }");
    touch(&d.join("vendor/README.md"), "Lyrics for the playing song, fetched from a service and kept in memory for later.");
    touch(&d.join("song notes.md"), "Lyrics for the playing song, fetched from a service and kept in memory for later.");
    captioned_png(&d.join("IMG_0413.png"), "a puppy playing in the sand");
    semantic::TEST_BATTERY.store(true, Relaxed);
    let before = embedded.load(Relaxed);
    let engine = Engine::start(vec![d.clone()], true, &ctx);
    let t0 = Instant::now();
    while engine.phase() != Phase::Ready {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(embedded.load(Relaxed) - before, 1, "the note only: no code, data or vendored files");
    assert_eq!(engine.waiting(), 4, "pictures wait (the run with pictures off left them all out)");
    assert_eq!(engine.index().files(), 4, "the text is read meanwhile");
    semantic::TEST_BATTERY.store(false, Relaxed);
    engine.poke();
    let t0 = Instant::now();
    while engine.waiting() > 0 || engine.phase() != Phase::Ready {
        assert!(t0.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(engine.index().files(), 7, "plugged in: the picture is read");
    drop(engine);
    // Without the model the engine waits for the user's say-so, then fetches it.
    let (addr2, model2, _) = fake_ollama(false);
    set_host(&addr2);
    let engine = Engine::start(vec![d.clone()], true, &ctx);
    let t0 = Instant::now();
    while engine.phase() != Phase::NoModel {
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(200));
    assert!(!model2.load(Relaxed), "nothing downloaded without asking");
    engine.download();
    let t0 = Instant::now();
    while engine.phase() != Phase::Ready {
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", engine.phase());
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(model2.load(Relaxed));
    drop(engine);
    *ollama::TEST_HOST.lock().unwrap() = None;
    assert_eq!(semantic::folders(" ~/Documents , /srv/notes,, "), vec![crate::fsx::home().join("Documents"), PathBuf::from("/srv/notes")]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn machine_named_pictures_told_from_photos() {
    use crate::semantic::machine_named;
    for n in ["frame_0042.png", "clip-000123.jpg", "000017.png", "3f9a1c0e-7b2d-4c1a.webp", "out.0001.png", "shot 004.png"] {
        assert!(machine_named(Path::new(n)), "{n}");
    }
    for n in ["IMG_1234.jpg", "PXL_20260912_101500.jpg", "DSC0042.JPG", "Screenshot_2026-10-15.png", "wallhaven-lygxjl.png", "Akeno.png", "17.png", "beach day.png"] {
        assert!(!machine_named(Path::new(n)), "{n}");
    }
}

#[test]
fn names_found_anywhere_under_home() {
    let d = tmpdir("names");
    for f in ["Notes/Read Me.md", "Notes/readme-old.txt", "Code/maia/README.md", "Code/maia/src/main.rs", "Code/maia/node_modules/readme.md", ".hidden/readme.md", "Photos/Été à Paris.jpg"] {
        touch(&d.join(f), "x");
    }
    let names = crate::namefind::Names::start(d.clone(), &egui::Context::default());
    let t0 = Instant::now();
    while names.walking() {
        assert!(t0.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(5));
    }
    let find = |q: &str| names.find(q, 10).into_iter().map(|(p, _)| p.strip_prefix(&d).unwrap().display().to_string()).collect::<Vec<_>>();
    // Any case; the name that is just the word first; build and hidden folders left out.
    assert_eq!(find("readme"), vec!["Code/maia/README.md", "Notes/readme-old.txt"]);
    assert_eq!(find("read me")[0], "Notes/Read Me.md", "every word; the name that is just them first");
    assert_eq!(find("me read"), find("read me"), "in any order");
    // A word may be in a folder's name, as long as one is in the file's.
    assert_eq!(find("maia main"), vec!["Code/maia/src/main.rs"]);
    assert_eq!(find("maia"), vec!["Code/maia"], "folders too");
    assert_eq!(find("été"), vec!["Photos/Été à Paris.jpg"], "beyond ASCII");
    assert!(find("").is_empty() && find("nothing like it").is_empty());
    let _ = std::fs::remove_dir_all(&d);
}


/// Every theme reads well: its text on its list, and its code colours (the theme's own and
/// the classic set) on the preview's background, by the usual contrast measure.
#[test]
fn themes_and_their_code_colours_are_readable() {
    use crate::theme::{Syntax, ThemeName, palette};
    fn lum(c: egui::Color32) -> f32 {
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }
    let contrast = |a: egui::Color32, b: egui::Color32| {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    };
    let mut seen = std::collections::HashSet::new();
    for t in ThemeName::ALL {
        assert!(seen.insert(t.label()), "names differ");
        let p = palette(t);
        assert_eq!(p.dark, !t.is_light(), "{}", t.label());
        assert!(contrast(p.text, p.panel) >= 7.0, "{}: text {:.1}", t.label(), contrast(p.text, p.panel));
        assert!(contrast(p.text_dim, p.panel) >= 3.0, "{}: dim text {:.1}", t.label(), contrast(p.text_dim, p.panel));
        for s in [p.syntax, Syntax::classic(p.dark)] {
            for (what, c, min) in [("keyword", s.keyword, 3.0), ("type", s.kind, 3.0), ("string", s.string, 3.0), ("number", s.number, 3.0), ("comment", s.comment, 2.0), ("function", s.function, 3.0), ("property", s.property, 3.0), ("meta", s.meta, 3.0)] {
                assert!(contrast(c, p.panel) >= min, "{}: {what} {:.2}", t.label(), contrast(c, p.panel));
            }
        }
    }
    assert_eq!(palette(ThemeName::TokyoNight).syntax, Syntax::of(ThemeName::TokyoNight));
    assert_ne!(Syntax::of(ThemeName::Gruvbox), Syntax::of(ThemeName::Mocha), "each its own");
}

/// A noreply address names its GitHub account: by number when it has one, else by name.
/// Anything else, or a malformed one, names nobody.
#[test]
fn github_noreply_addresses_name_the_picture() {
    use crate::avatars::{Source, noreply_url, source};
    assert_eq!(noreply_url("12345+octo@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/u/12345?s=96"));
    assert_eq!(noreply_url(" 12345+octo@Users.NoReply.GitHub.com ").as_deref(), Some("https://avatars.githubusercontent.com/u/12345?s=96"), "any case, trimmed");
    assert_eq!(noreply_url("octo-cat@users.noreply.github.com").as_deref(), Some("https://avatars.githubusercontent.com/octo-cat?s=96"));
    for none in ["octo@example.invalid", "octo@noreply.github.com", "octo@users.noreply.github.com.example.invalid", "x1+octo@users.noreply.github.com", "+octo@users.noreply.github.com", "-octo@users.noreply.github.com", "oc/to@users.noreply.github.com", "oc to@users.noreply.github.com", "@users.noreply.github.com", "users.noreply.github.com", ""] {
        assert_eq!(noreply_url(none), None, "{none:?}");
    }
    let repo = ("octo-org".to_string(), "demo".to_string());
    let sha = "0123456789abcdef0123456789abcdef01234567";
    // The cheapest way first: a noreply address needs no lookup, even on github.com.
    assert_eq!(source("12345+octo@users.noreply.github.com", sha, Some(&repo)), Some(Source::Picture("https://avatars.githubusercontent.com/u/12345?s=96".into())));
    assert_eq!(source("ada@example.invalid", sha, Some(&repo)), Some(Source::Api(format!("https://api.github.com/repos/octo-org/demo/commits?sha={sha}&per_page=1"))));
    assert_eq!(source("ada@example.invalid", sha, None), None, "not on github.com: no picture");
    assert_eq!(source("ada@example.invalid", "not a sha", Some(&repo)), None);
    assert_eq!(source("no address", sha, Some(&repo)), None);
}

/// Only a remote on github.com itself is looked up, in every form git writes it; an
/// Enterprise host or a look-alike is not github.com.
#[test]
fn github_remote_urls() {
    use crate::avatars::github_repo;
    let ok = Some(("octo-org".to_string(), "demo".to_string()));
    for u in [
        "https://github.com/octo-org/demo.git",
        "https://github.com/octo-org/demo",
        "https://github.com/octo-org/demo/",
        "http://github.com/octo-org/demo.git",
        "https://GitHub.com/octo-org/demo.git",
        "https://www.github.com/octo-org/demo",
        "https://someone:not-a-real-token@github.com/octo-org/demo.git",
        "git@github.com:octo-org/demo.git",
        "git@github.com:octo-org/demo",
        "github.com:octo-org/demo.git",
        "ssh://git@github.com/octo-org/demo.git",
        "ssh://git@ssh.github.com:443/octo-org/demo.git",
        "git://github.com/octo-org/demo.git",
        "  https://github.com/octo-org/demo.git\n",
    ] {
        assert_eq!(github_repo(u), ok, "{u:?}");
    }
    assert_eq!(github_repo("https://github.com/octo-org/my.site.git"), Some(("octo-org".into(), "my.site".into())));
    for u in [
        "https://github.example.com/octo-org/demo.git",
        "git@github.example.com:octo-org/demo.git",
        "ssh://git@ghe.example.invalid/octo-org/demo.git",
        "https://github.com.example.invalid/octo-org/demo.git",
        "https://notgithub.com/octo-org/demo.git",
        "https://gitlab.com/octo-org/demo.git",
        "https://example.invalid/github.com/octo-org/demo.git",
        "https://github.com@example.invalid/octo-org/demo.git",
        "file:///srv/github.com/octo-org/demo.git",
        "/srv/git/demo.git",
        "../demo.git",
        "https://github.com/octo-org",
        "https://github.com/octo-org/demo/tree/main",
        "https://github.com/../demo",
        "https://github.com/octo org/demo",
        "",
    ] {
        assert_eq!(github_repo(u), None, "{u:?}");
    }
}

/// GitHub's answer about a commit gives the account's picture at the size drawn, from
/// GitHub's picture host only; an author without an account is said to have none.
#[test]
fn github_api_answer_gives_the_picture() {
    use crate::avatars::{avatar_from_api, sized_avatar};
    let one = br#"{"sha":"0123abc","commit":{"author":{"name":"Octo"}},"author":{"login":"octo","id":12345,"avatar_url":"https://avatars.githubusercontent.com/u/12345?v=4"}}"#;
    assert_eq!(avatar_from_api(one), Some(Some("https://avatars.githubusercontent.com/u/12345?v=4&s=96".into())));
    let list = br#"[{"sha":"0123abc","author":{"avatar_url":"https://avatars.githubusercontent.com/u/777"}},{"sha":"9999"}]"#;
    assert_eq!(avatar_from_api(list), Some(Some("https://avatars.githubusercontent.com/u/777?s=96".into())), "the list form: its first commit");
    assert_eq!(avatar_from_api(br#"{"sha":"0123abc","author":null}"#), Some(None), "an address no account claims");
    assert_eq!(avatar_from_api(br#"[{"sha":"0123abc"}]"#), Some(None));
    assert_eq!(avatar_from_api(br#"{"sha":"0123abc","author":{"avatar_url":"https://example.invalid/me.png"}}"#), Some(None), "another host is not followed");
    assert_eq!(avatar_from_api(br#"{"sha":"0123abc","author":{"avatar_url":"http://avatars.githubusercontent.com/u/1"}}"#), Some(None), "https only");
    for bad in [&br#"{"message":"API rate limit exceeded"}"#[..], b"[]", b"not json", b"", b"null", br#""text""#] {
        assert_eq!(avatar_from_api(bad), None, "{:?}", String::from_utf8_lossy(bad));
    }
    assert_eq!(sized_avatar("https://avatars.githubusercontent.com/octo").as_deref(), Some("https://avatars.githubusercontent.com/octo?s=96"));
    assert_eq!(sized_avatar("https://avatars.githubusercontent.com/"), None);
    assert_eq!(sized_avatar("https://avatars.githubusercontent.com/u/1 --output x"), None);
}

/// An answer holds for a week, then a picture is shown while it is asked for again and a
/// "none" is forgotten. A clock set back doesn't make an answer last for ever.
#[test]
fn github_picture_cache_staleness() {
    use crate::avatars::{Cached, MAX_AGE, fresh, key, read_cache, write_hit, write_miss};
    use std::time::SystemTime;
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let day = Duration::from_secs(86_400);
    assert!(fresh(now, now) && fresh(now - 6 * day, now));
    assert!(!fresh(now - MAX_AGE, now) && !fresh(now - 30 * day, now));
    assert!(fresh(now + day, now), "a little ahead: the clock moved back a bit");
    assert!(!fresh(now + 8 * day, now), "far ahead: asked again");
    assert_eq!(key(" Ada@Example.invalid "), key("ada@example.invalid"), "one answer per address, whatever its case");
    assert_ne!(key("ada@example.invalid"), key("grace@example.invalid"));
    assert!(key("ada@example.invalid").bytes().all(|b| b.is_ascii_hexdigit()), "no address in a file name");
    let d = tmpdir("avatar-cache");
    let k = key("ada@example.invalid");
    let age = |name: &str, ago: Duration| std::fs::File::options().write(true).open(d.join(format!("{k}.{name}"))).unwrap().set_modified(SystemTime::now() - ago).unwrap();
    let now = SystemTime::now();
    assert_eq!(read_cache(&d, &k, now), Cached::Nothing);
    write_hit(&d, &k, b"picture");
    assert_eq!(read_cache(&d, &k, now), Cached::Hit(b"picture".to_vec()));
    age("img", 8 * day);
    assert_eq!(read_cache(&d, &k, now), Cached::Stale(b"picture".to_vec()));
    write_miss(&d, &k);
    assert_eq!(read_cache(&d, &k, now), Cached::Miss, "none, this week");
    assert!(!d.join(format!("{k}.img")).exists(), "the old picture goes with it");
    age("none", 8 * day);
    assert_eq!(read_cache(&d, &k, now), Cached::Nothing, "a week on, asked again");
    write_hit(&d, &k, b"new");
    assert!(!d.join(format!("{k}.none")).exists());
    assert_eq!(std::fs::read_dir(&d).unwrap().count(), 1, "no temporary file left behind");
    let _ = std::fs::remove_dir_all(&d);
}

/// One author's picture end to end against a stand-in for GitHub: fetched once, kept,
/// then read from the cache; a "none" kept the same way; too many requests and errors
/// keep nothing; a week-old picture stands in when GitHub can't be asked.
#[test]
fn github_find_asks_once_and_keeps_the_answer() {
    use crate::avatars::{Found, Http, Source, find, key, write_hit};
    use std::cell::RefCell;
    let d = tmpdir("avatar-find");
    let png = png_bytes(120, 80, 4);
    let now = std::time::SystemTime::now();
    let asked = RefCell::new(vec![]);
    let get = |answers: Vec<(&'static str, Http)>| {
        let asked = &asked;
        move |url: &str| {
            asked.borrow_mut().push(url.to_string());
            answers.iter().find(|(k, _)| url.contains(k)).map_or(Http::Failed, |(_, h)| h.clone())
        }
    };
    let pic = Source::Picture("https://avatars.githubusercontent.com/u/12345?s=96".into());
    let k = key("12345+octo@users.noreply.github.com");
    let g = get(vec![("/u/12345", Http::Ok(png.clone()))]);
    assert!(matches!(find(&d, &k, &pic, &g, now), Found::Picture(i) if (i.w, i.h) == (120, 80)));
    assert!(matches!(find(&d, &k, &pic, &g, now), Found::Picture(_)));
    assert_eq!(asked.borrow().len(), 1, "the second time from the cache");
    // Through the API: the commit's account, then its picture.
    let api = Source::Api("https://api.github.com/repos/octo-org/demo/commits?sha=0123abc&per_page=1".into());
    let json = br#"[{"sha":"0123abc","author":{"avatar_url":"https://avatars.githubusercontent.com/u/777?v=4"}}]"#.to_vec();
    asked.borrow_mut().clear();
    let g = get(vec![("api.github.com", Http::Ok(json)), ("/u/777?v=4&s=96", Http::Ok(png.clone()))]);
    assert!(matches!(find(&d, &key("grace@example.invalid"), &api, &g, now), Found::Picture(_)));
    assert_eq!(asked.borrow().len(), 2);
    // No account behind the address: kept as none, not asked again.
    asked.borrow_mut().clear();
    let g = get(vec![("api.github.com", Http::Ok(br#"{"sha":"0123abc","author":null}"#.to_vec()))]);
    assert!(matches!(find(&d, &key("linus@example.invalid"), &api, &g, now), Found::None));
    assert!(matches!(find(&d, &key("linus@example.invalid"), &api, &g, now), Found::None));
    assert_eq!(asked.borrow().len(), 1);
    // Too many requests, or an error: nothing kept, so it is asked again later.
    for (h, limited) in [(Http::Status(403), true), (Http::Status(429), true), (Http::Status(500), false), (Http::Failed, false)] {
        let g = get(vec![("api.github.com", h)]);
        let f = find(&d, &key("edsger@example.invalid"), &api, &g, now);
        assert!(if limited { matches!(f, Found::Limited) } else { matches!(f, Found::Failed) });
    }
    assert!(!std::fs::read_dir(&d).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(&key("edsger@example.invalid"))));
    // A commit GitHub doesn't have: none, for a week.
    let g = get(vec![("api.github.com", Http::Status(422))]);
    assert!(matches!(find(&d, &key("barbara@example.invalid"), &api, &g, now), Found::None));
    // A picture that doesn't decode is not kept.
    let g = get(vec![("/u/12345", Http::Ok(b"<html>".to_vec()))]);
    assert!(matches!(find(&d, &key("12345+other@users.noreply.github.com"), &pic, &g, now), Found::Failed));
    // A week-old picture while GitHub can't be reached, or limits us.
    let ks = key("13+old@users.noreply.github.com");
    write_hit(&d, &ks, &png);
    std::fs::File::options().write(true).open(d.join(format!("{ks}.img"))).unwrap().set_modified(now - Duration::from_secs(8 * 86_400)).unwrap();
    asked.borrow_mut().clear();
    let g = get(vec![]);
    assert!(matches!(find(&d, &ks, &pic, &g, now), Found::Picture(_)));
    assert_eq!(asked.borrow().len(), 1, "a stale picture is asked for again");
    let _ = std::fs::remove_dir_all(&d);
}

/// curl's answer splits into body and status; the command reads no user settings, speaks
/// only https, caps the answer and the time, and names itself plainly.
#[test]
fn github_requests_through_curl() {
    use crate::avatars::{Http, curl_args, parse_curl};
    assert_eq!(parse_curl(b"\x89PNG\n\x00data\n200"), Http::Ok(b"\x89PNG\n\x00data".to_vec()));
    assert_eq!(parse_curl(b"{\"message\":\"rate\"}\n403"), Http::Status(403));
    assert_eq!(parse_curl(b"\n000"), Http::Failed, "no answer at all");
    assert_eq!(parse_curl(b""), Http::Failed);
    assert_eq!(parse_curl(b"cut short"), Http::Failed);
    let a = curl_args("https://avatars.githubusercontent.com/u/12345?s=96");
    assert_eq!(a[0], "-q", "first, or ~/.curlrc is read");
    let has = |a: &[String], k: &str, v: &str| a.windows(2).any(|w| w[0] == k && w[1] == v);
    assert!(has(&a, "--proto", "=https") && has(&a, "--proto-redir", "=https") && a.iter().any(|x| x == "--location"));
    assert!(has(&a, "--max-time", "10") && has(&a, "--max-filesize", "1048576") && has(&a, "--user-agent", "Maia"));
    assert!(!a.iter().any(|x| matches!(x.as_str(), "-u" | "--user" | "-n" | "--netrc" | "--netrc-optional" | "-b" | "--cookie" | "-K" | "--config")), "no credentials of any kind");
    assert!(!a.iter().any(|x| x.to_ascii_lowercase().contains("authorization")));
    assert_eq!(a.last().map(String::as_str), Some("https://avatars.githubusercontent.com/u/12345?s=96"));
    assert!(curl_args("https://api.github.com/repos/o/r/commits?sha=0123abc&per_page=1").iter().any(|x| x == "Accept: application/vnd.github+json"));
    assert!(!a.iter().any(|x| x.starts_with("Accept")), "pictures need no API header");
}

/// A picture is cut to the middle square at the size drawn, round, with a soft edge.
#[test]
fn github_picture_is_cut_round() {
    let img = crate::imgdec::decode_bytes(&png_bytes(120, 80, 2)).unwrap();
    let c = crate::avatars::round(&img, 37);
    assert_eq!(c.size, [37, 37]);
    let a = |x: usize, y: usize| c.pixels[y * 37 + x].a();
    assert_eq!((a(0, 0), a(36, 0), a(0, 36), a(36, 36)), (0, 0, 0, 0), "corners outside the circle");
    assert_eq!(a(18, 18), 255);
    assert!((1..255).contains(&a(18, 0)) || a(18, 0) == 255, "the top edge is inside or soft");
    assert!(c.pixels.iter().any(|p| (1..255).contains(&p.a())), "a soft edge, not a jagged one");
    let small = crate::imgdec::decode_bytes(&png_bytes(20, 20, 2)).unwrap();
    assert_eq!(crate::avatars::round(&small, 96).size, [20, 20], "a small picture is not blown up here");
}

/// Every option is listed in Options exactly once, in one section, with its group's
/// rows together and each sub-option right under the switch it belongs to.
#[test]
fn every_option_has_one_section() {
    use crate::popups::{OPTS, Opt, Section, opt_parent};
    // Exhaustive: a new option does not build until it is numbered here.
    let index = |o: Opt| match o {
        Opt::Theme => 0, Opt::Font => 1, Opt::InspectorFontSize => 2, Opt::RectButtons => 3, Opt::AssetCache => 4,
        Opt::FollowSystem => 5, Opt::SyntaxTheme => 6, Opt::Round => 7, Opt::FontSize => 8, Opt::Spacing => 9,
        Opt::Animations => 10, Opt::Tooltips => 11, Opt::AltRows => 12, Opt::InactiveOpacity => 13, Opt::Breadcrumbs => 14,
        Opt::CompactPaths => 15, Opt::ShowExt => 16, Opt::ShowHidden => 17, Opt::ShowSystem => 18, Opt::FoldersFirst => 19,
        Opt::HighlightRecents => 20, Opt::RecentsOnTop => 21, Opt::TrackRecents => 22, Opt::CalcSizes => 23, Opt::SizesSsd => 24,
        Opt::SizesHdd => 25, Opt::SizesNet => 26, Opt::SelectionBoxes => 27, Opt::InspectorFont => 28, Opt::SystemIcons => 29,
        Opt::Thumbnails => 30, Opt::ThumbFill => 31, Opt::ScrollBeyond => 32, Opt::DateFormat => 33, Opt::ActOnPress => 34,
        Opt::SingleClick => 35, Opt::SingleClickFolders => 36, Opt::OpenFoldersNewTab => 37, Opt::DblClickParent => 38, Opt::ReverseScroll => 39,
        Opt::CharKey => 40, Opt::MiddleClick => 41, Opt::InspectorLayout => 42, Opt::Startup => 43, Opt::NewTab => 44,
        Opt::GitPictures => 45, Opt::DefaultFm => 46, Opt::SingleInstance => 47, Opt::Meaning => 48, Opt::MeaningFolders => 49,
        Opt::MeaningPictures => 50, Opt::ConfigFile => 51, Opt::ResetAll => 52, Opt::Install => 53, Opt::LineSpacing => 54, Opt::Updates => 55,
        Opt::Editor => 56,
        Opt::IgnoredFiles => 57,
        Opt::TrashAge => 58,
        Opt::PictureViewer => 61,
        Opt::Tour => 62,
        Opt::CustomActions => 63,
        Opt::CheckCopies => 64,
        Opt::Picker => 65,
    };
    let numbered: Vec<usize> = (0..59).chain([61, 62, 63, 64, 65]).collect();
    let mut seen = vec![0; 66];
    for o in OPTS {
        seen[index(o.0)] += 1;
    }
    for (i, n) in seen.iter().enumerate() {
        assert_eq!(*n, numbered.contains(&i) as usize, "option {i} listed {n} times");
    }
    // Sections in the order of the left column, each a block; groups never split.
    let order: Vec<Section> = OPTS.iter().map(|o| o.1).fold(vec![], |mut v, s| {
        if v.last() != Some(&s) {
            v.push(s);
        }
        v
    });
    assert_eq!(order, Section::ALL.to_vec(), "every section has options, in one block, in order");
    let mut groups: Vec<(Section, &str)> = vec![];
    for o in OPTS {
        if groups.last() != Some(&(o.1, o.2)) {
            assert!(!groups.contains(&(o.1, o.2)), "{} / {:?} is split", o.1.name(), o.2);
            groups.push((o.1, o.2));
        }
    }
    for (k, o) in OPTS.iter().enumerate() {
        if let Some(p) = opt_parent(o.0) {
            let at = OPTS.iter().position(|x| x.0 == p).unwrap();
            assert!(at < k && OPTS[at + 1..k].iter().all(|x| opt_parent(x.0) == Some(p)), "{:?} sits right under its switch", o.3);
            assert_eq!((OPTS[at].1, OPTS[at].2), (o.1, o.2), "{:?} shares its switch's group", o.3);
        }
    }
    // Short, plain lines.
    for o in OPTS {
        assert!(o.4.len() <= 60 && !o.4.contains('\u{2014}'), "{:?}: {:?} is long", o.3, o.4);
        assert!(o.4.ends_with('.'), "{:?}", o.4);
    }
}

/// The search covers every section: the words people type find their option first,
/// and a word shared by options in several sections finds each of them.
#[test]
fn option_search_spans_sections() {
    use crate::popups::{OPTS, Opt, Section, find_options};
    let first = |q: &str| find_options(q).first().map(|&i| OPTS[i].0);
    for (q, o) in [
        ("color scheme", Opt::Theme),
        ("default file manager", Opt::DefaultFm),
        ("open and save dialog", Opt::Picker),
        ("search by meaning", Opt::Meaning),
        ("reuse the open window", Opt::SingleInstance),
        ("show github pictures", Opt::GitPictures),
        ("ignored", Opt::IgnoredFiles),
        ("hidden", Opt::ShowHidden),
        ("folders first", Opt::FoldersFirst),
        ("install", Opt::Install),
        ("config", Opt::ConfigFile),
    ] {
        assert_eq!(first(q), Some(o), "{q:?}");
    }
    assert!(find_options("").is_empty() && find_options("   ").is_empty(), "no search, no results");
    assert!(find_options("qqzzx").is_empty());
    let sections = |q: &str| find_options(q).iter().map(|&i| OPTS[i].1).collect::<HashSet<Section>>();
    let size = sections("size");
    for s in [Section::Appearance, Section::Files, Section::Preview, Section::System] {
        assert!(size.contains(&s), "\"size\" reaches {}", s.name());
    }
    assert!(sections("font").is_superset(&[Section::Appearance, Section::Preview].into_iter().collect()));
    // A section or group name finds what is in it.
    assert!(find_options("thumbnails").iter().any(|&i| OPTS[i].0 == Opt::ThumbFill));
    assert!(find_options("recents").iter().any(|&i| OPTS[i].0 == Opt::TrackRecents));
    assert_eq!(Section::Appearance.step(-1), Section::System, "sections wrap");
    assert_eq!(Section::System.step(1), Section::Appearance);
    assert_eq!(Section::Files.step(1), Section::Mouse);
}

/// Updates: versions compared as numbers, GitHub's answer read only for this project's
/// downloads, and an install that checks the download before it replaces anything.
#[test]
fn updates_check_compare_and_install() {
    use crate::update::{Fetch, Release, State, Updater, curl_args, newer, parse_release, parse_sum, version};
    assert_eq!(version("v1.2.3"), Some((1, 2, 3)));
    assert_eq!(version("0.10"), Some((0, 10, 0)));
    assert_eq!(version("2.0.0-beta.1"), Some((2, 0, 0)));
    assert_eq!(version("one"), None);
    assert_eq!(version("1.2.3.4"), None);
    assert!(newer("0.2.0", "v0.10.0"), "numbers, not text");
    assert!(!newer("0.2.0", "v0.2.0") && !newer("0.3.0", "0.2.9") && !newer("0.2.0", "garbage"));
    let json = |tag: &str, host: &str| format!(r#"{{"tag_name":"{tag}","body":"Faster search.\n\nNew themes.","assets":[
        {{"name":"maia-linux-x86_64","browser_download_url":"{host}/yappologistic/Maia/releases/download/{tag}/maia-linux-x86_64"}},
        {{"name":"maia-linux-x86_64.sha256","browser_download_url":"{host}/yappologistic/Maia/releases/download/{tag}/maia-linux-x86_64.sha256"}}]}}"#);
    let r = parse_release(json("v9.9.9", "https://github.com").as_bytes()).expect("a release");
    assert_eq!((r.version.as_str(), r.notes.starts_with("Faster search.")), ("v9.9.9", true));
    assert!(parse_release(json("v9.9.9", "https://evil.example").as_bytes()).is_none(), "downloads only from the project's releases");
    assert!(parse_release(br#"{"tag_name":"v9.9.9","assets":[]}"#).is_none(), "no build, no update");
    let h = "a".repeat(64);
    assert_eq!(parse_sum(&format!("{h}  maia-linux-x86_64\n")), Some(h.clone()));
    assert_eq!(parse_sum("not a sum"), None);
    let args = curl_args("https://api.github.com/repos/x/y/releases/latest", None);
    assert_eq!(args[0], "-q", "no ~/.curlrc");
    assert!(args.windows(2).any(|w| w == ["--proto", "=https"]) && args.iter().any(|a| a.starts_with("Accept:")));
    assert!(args.iter().all(|a| !a.to_lowercase().contains("authorization")), "no credentials");
    // An install against a stand-in for GitHub: the right bytes replace the program.
    let d = tmpdir("update");
    let exe = d.join("maia");
    std::fs::write(&exe, b"\x7fELF old").unwrap();
    let build = b"\x7fELF the new build".to_vec();
    let sum = |b: &[u8]| {
        let f = d.join("sum-input");
        std::fs::write(&f, b).unwrap();
        let out = std::process::Command::new("sha256sum").arg(&f).output().unwrap();
        String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().to_string()
    };
    let serve = |bin: Vec<u8>, sum_text: String| -> Fetch {
        let api = json("v9.9.9", "https://github.com");
        std::sync::Arc::new(move |url: &str, to: Option<&Path>| {
            let body = if url.contains("api.github.com") { api.clone().into_bytes() } else if url.ends_with(".sha256") { sum_text.clone().into_bytes() } else { bin.clone() };
            match to {
                Some(p) => std::fs::write(p, &body).map(|_| vec![]).map_err(|e| e.to_string()),
                None => Ok(body),
            }
        })
    };
    let wait = |u: &Updater| {
        let t0 = Instant::now();
        while matches!(u.state(), State::Checking | State::Downloading(_)) {
            assert!(t0.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        u.state()
    };
    let ctx = egui::Context::default();
    let u = Updater::for_test(serve(build.clone(), format!("{}  maia-linux-x86_64", sum(&build))), exe.clone());
    u.check(&ctx);
    let State::Available(r) = wait(&u) else { panic!("{:?}", u.state()) };
    u.install(r.clone(), &ctx);
    assert_eq!(wait(&u), State::Ready("v9.9.9".into()));
    assert_eq!(std::fs::read(&exe).unwrap(), build, "the new build in place");
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(std::fs::metadata(&exe).unwrap().permissions().mode() & 0o777, 0o755, "and runnable");
    // A download that doesn't match its checksum changes nothing, and leaves nothing.
    std::fs::write(&exe, b"\x7fELF old").unwrap();
    let u = Updater::for_test(serve(b"\x7fELF tampered".to_vec(), format!("{}  maia-linux-x86_64", sum(&build))), exe.clone());
    u.install(r.clone(), &ctx);
    assert!(matches!(wait(&u), State::Failed(e) if e.contains("checksum")));
    assert_eq!(std::fs::read(&exe).unwrap(), b"\x7fELF old");
    assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".maia-update")), "no download left behind");
    // A program in a folder Maia can't write: said so, nothing fetched.
    let u = Updater::for_test(serve(build.clone(), String::new()), PathBuf::from("/proc/maia"));
    u.install(r, &ctx);
    assert!(matches!(wait(&u), State::CantWrite(_)));
    let _ = std::fs::remove_dir_all(&d);
}

/// Updates of a packaged Maia: a .deb, .rpm or pacman install is found by asking dpkg, rpm or pacman who
/// owns the program, and gets the release's package, checked against SHA256SUMS and
/// handed to dpkg -i, rpm -U or pacman -U through pkexec once the program alone has been tried. An
/// AppImage replaces its own file. All of it on fakes: no package manager is run.
#[test]
fn updates_through_packages() {
    use crate::update::{Fetch, Installed, State, Updater, parse_release, sum_for};
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("update-pkg");
    let sum = |b: &[u8]| {
        let f = d.join("sum-input");
        std::fs::write(&f, b).unwrap();
        let out = std::process::Command::new("sha256sum").arg(&f).output().unwrap();
        String::from_utf8_lossy(&out.stdout).split_whitespace().next().unwrap().to_string()
    };
    let (deb, rpm, pac, app) = (Installed::Deb.package("v9.9.9").unwrap(), Installed::Rpm.package("v9.9.9").unwrap(), Installed::Pacman.package("v9.9.9").unwrap(), Installed::AppImage(PathBuf::new()).package("v9.9.9").unwrap());
    assert!(deb.starts_with("maia_9.9.9_") && deb.ends_with(".deb") && rpm.starts_with("maia-9.9.9-1.") && pac.starts_with("maia-9.9.9-1-") && pac.ends_with(".pkg.tar.zst") && app.starts_with("Maia-9.9.9-"));
    let names = [crate::update::ASSET.to_string(), crate::update::SUMS.to_string(), deb.clone(), rpm.clone(), pac.clone(), app.clone(), "SHA256SUMS".into()];
    let json = format!(r#"{{"tag_name":"v9.9.9","body":"","assets":[{}]}}"#, names.iter().map(|n| format!(r#"{{"name":"{n}","browser_download_url":"https://github.com/yappologistic/Maia/releases/download/v9.9.9/{n}"}}"#)).collect::<Vec<_>>().join(","));
    let r = parse_release(json.as_bytes()).expect("a release");
    assert_eq!(r.assets.len(), 7, "every download kept");
    let build = b"\x7fELF the new build".to_vec();
    let deb_bytes = b"!<arch>\ndebian-binary the new package".to_vec();
    let rpm_bytes = b"\xed\xab\xee\xdb the new package".to_vec();
    let pac_bytes = b"\x28\xb5\x2f\xfd the new package".to_vec();
    let app_bytes = b"\x7fELF AI\x02 the new AppImage".to_vec();
    let sums = format!("{}  {deb}\n{}  {rpm}\n{}  {pac}\n{}  {app}\n{}  {}\n", sum(&deb_bytes), sum(&rpm_bytes), sum(&pac_bytes), sum(&app_bytes), sum(&build), crate::update::ASSET);
    assert_eq!(sum_for(&sums, &rpm), Some(sum(&rpm_bytes)));
    assert_eq!(sum_for(&sums, "maia_1.0.0_amd64.deb"), None);
    // A stand-in for GitHub, serving `files` by name (a tampered one, say).
    let serve = |files: Vec<(String, Vec<u8>)>| -> Fetch {
        let (api, sums, bsum) = (json.clone(), sums.clone(), format!("{}  {}", sum(&build), crate::update::ASSET));
        std::sync::Arc::new(move |url: &str, to: Option<&Path>| {
            let name = url.rsplit('/').next().unwrap_or("");
            let body = if url.contains("api.github.com") {
                api.clone().into_bytes()
            } else if name == "SHA256SUMS" {
                sums.clone().into_bytes()
            } else if name.ends_with(".sha256") {
                bsum.clone().into_bytes()
            } else {
                files.iter().find(|(n, _)| n == name).map(|(_, b)| b.clone()).ok_or("404")?
            };
            match to {
                Some(p) => std::fs::write(p, &body).map(|_| vec![]).map_err(|e| e.to_string()),
                None => Ok(body),
            }
        })
    };
    let all = || vec![(crate::update::ASSET.to_string(), build.clone()), (deb.clone(), deb_bytes.clone()), (rpm.clone(), rpm_bytes.clone()), (pac.clone(), pac_bytes.clone()), (app.clone(), app_bytes.clone())];
    let wait = |u: &Updater| {
        let t0 = Instant::now();
        while matches!(u.state(), State::Downloading(_) | State::Installing(_)) {
            assert!(t0.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(5));
        }
        u.state()
    };
    // The program where only root writes, as a package puts it.
    let bin = d.join("usr/bin");
    std::fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("maia");
    std::fs::write(&exe, b"\x7fELF old").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Fakes: the manager's answer to who owns the program, its install (which keeps a
    // copy of what it was given), and pkexec, which logs and runs it or refuses.
    let fakes = |name: &str, owner: &str| {
        let f = d.join(name);
        std::fs::create_dir_all(&f).unwrap();
        let script = |p: &str, body: &str| {
            std::fs::write(f.join(p), format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(f.join(p), std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let keep = format!("echo \"$0 $*\" >> {0}/log; cp \"$2\" {0}/installed", f.display());
        if owner == "dpkg" {
            script("dpkg-query", "echo \"maia: $2\"");
            script("dpkg", &keep);
        } else if owner == "pacman" {
            script("pacman", &format!("[ \"$1\" = -Qoq ] && {{ printf maia; exit 0; }}\necho \"$0 $*\" >> {0}/log; cp \"$3\" {0}/installed", f.display()));
        } else {
            script("rpm", &format!("[ \"$1\" = -qf ] && {{ printf maia; exit 0; }}\n{keep}"));
        }
        script("pkexec", &format!("echo \"pkexec $*\" >> {0}/log; [ -e {0}/deny ] && exit 126; exec \"$@\"", f.display()));
        f
    };
    let ctx = egui::Context::default();
    let run = |fake: Option<&Path>, u: &Updater| {
        crate::drives::FAKE.with(|f| *f.borrow_mut() = fake.map(Path::to_path_buf));
        u.install(r.clone(), &ctx);
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        wait(u)
    };
    // A .deb install: the package, checked, to dpkg -i through pkexec; nothing left behind.
    let f = fakes("deb-tools", "dpkg");
    let u = Updater::for_test(serve(all()), exe.clone());
    assert_eq!(run(Some(&f), &u), State::Ready("v9.9.9".into()));
    let log = std::fs::read_to_string(f.join("log")).unwrap();
    assert!(log.starts_with(&format!("pkexec {} -i {}", f.join("dpkg").display(), d.join("usr/updates").join(&deb).display())), "{log}");
    assert_eq!(std::fs::read(f.join("installed")).unwrap(), deb_bytes, "the release's package installed");
    assert!(!d.join("usr/updates").join(&deb).exists(), "the download removed");
    assert_eq!(std::fs::read(&exe).unwrap(), b"\x7fELF old", "the program itself left to dpkg");
    // The password dialog dismissed: said so.
    std::fs::write(f.join("deny"), b"").unwrap();
    let u = Updater::for_test(serve(all()), exe.clone());
    assert!(matches!(run(Some(&f), &u), State::Failed(e) if e.contains("not given")));
    // A tampered package never reaches pkexec.
    std::fs::remove_file(f.join("deny")).unwrap();
    std::fs::remove_file(f.join("log")).unwrap();
    let mut bad = all();
    bad[1].1 = b"!<arch>\ntampered".to_vec();
    let u = Updater::for_test(serve(bad), exe.clone());
    assert!(matches!(run(Some(&f), &u), State::Failed(e) if e.contains("checksum")));
    assert!(!f.join("log").exists(), "pkexec not asked");
    // A program alone that's broken stops the update before the package is fetched.
    let mut bad = all();
    bad[0].1 = b"\x7fELF broken".to_vec();
    let u = Updater::for_test(serve(bad), exe.clone());
    assert!(matches!(run(Some(&f), &u), State::Failed(e) if e.contains("checksum")));
    assert!(!f.join("log").exists());
    // An .rpm install: rpm -U.
    let f = fakes("rpm-tools", "rpm");
    let u = Updater::for_test(serve(all()), exe.clone());
    assert_eq!(run(Some(&f), &u), State::Ready("v9.9.9".into()));
    assert!(std::fs::read_to_string(f.join("log")).unwrap().contains(&format!("{} -U ", f.join("rpm").display())));
    assert_eq!(std::fs::read(f.join("installed")).unwrap(), rpm_bytes);
    // A pacman install: pacman -U, answering its question for it (no terminal to ask in).
    let f = fakes("pacman-tools", "pacman");
    let u = Updater::for_test(serve(all()), exe.clone());
    assert_eq!(run(Some(&f), &u), State::Ready("v9.9.9".into()));
    assert!(std::fs::read_to_string(f.join("log")).unwrap().contains(&format!("{} -U --noconfirm ", f.join("pacman").display())));
    assert_eq!(std::fs::read(f.join("installed")).unwrap(), pac_bytes);
    // No manager owns it: nothing to do but say so.
    let u = Updater::for_test(serve(all()), exe.clone());
    assert_eq!(run(None, &u), State::CantWrite(bin.clone()));
    // An AppImage: its own file replaced, the program inside it untouched.
    let apps = d.join("Applications");
    std::fs::create_dir_all(&apps).unwrap();
    let file = apps.join("Maia.AppImage");
    std::fs::write(&file, b"\x7fELF old AppImage").unwrap();
    let u = Updater::for_test(serve(all()), exe.clone()).with_appimage(file.clone());
    assert_eq!(run(None, &u), State::Ready("v9.9.9".into()));
    assert_eq!(std::fs::read(&file).unwrap(), app_bytes);
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o755);
    assert_eq!(std::fs::read(&exe).unwrap(), b"\x7fELF old");
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn io_errors_read_without_their_number() {
    let e = std::fs::read_dir("/proc/self/nonexistent-dir").unwrap_err().to_string();
    assert!(e.ends_with("(os error 2)"), "std still writes the number: {e}");
    assert_eq!(crate::fsx::plain_error(&e), "No such file or directory");
    assert_eq!(crate::fsx::plain_error("Permission denied (os error 13)"), "Permission denied");
    // Only a trailing number in that form goes.
    assert_eq!(crate::fsx::plain_error("Wrong password"), "Wrong password");
    assert_eq!(crate::fsx::plain_error("odd (os error x)"), "odd (os error x)");
    assert_eq!(crate::fsx::plain_error("(os error 2) first"), "(os error 2) first");
}

/// Contrast ratio of two colours (WCAG relative luminance).
fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let lum = |c: egui::Color32| {
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

#[test]
fn selected_rows_read_in_every_theme() {
    for t in crate::theme::ThemeName::ALL {
        let p = crate::theme::palette(t);
        let (on, dim) = (p.on_sel(), p.on_sel_dim());
        assert!(contrast(on, p.selected) >= 4.5, "{}: text on the selection {:.2}", t.label(), contrast(on, p.selected));
        assert!(contrast(dim, p.selected) >= 3.0, "{}: secondary text on the selection {:.2}", t.label(), contrast(dim, p.selected));
        // The selected text stays apart from its secondary text.
        assert!(contrast(on, p.selected) > contrast(dim, p.selected));
    }
}

#[test]
fn time_left_reads_as_minutes_and_hours() {
    use crate::ui::time_left;
    assert_eq!(time_left(0.4), "0s");
    assert_eq!(time_left(42.0), "42s");
    assert_eq!(time_left(61.0), "2 min");
    assert_eq!(time_left(3599.0), "60 min");
    assert_eq!(time_left(3725.0), "1 h 2 min");
    assert_eq!(time_left(-3.0), "0s");
}

#[test]
fn an_svg_says_its_own_size() {
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="64" height="48" viewBox="0 0 64 48"><rect width="64" height="48"/></svg>"#;
    assert_eq!(crate::svg::size(svg), Some([64, 48]));
    let vb = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 150"/>"#;
    assert_eq!(crate::svg::size(vb), Some([300, 150]), "the viewBox when there is no size");
    let (w, h, _) = crate::svg::render(svg, 2048).unwrap();
    assert_eq!((w, h), (2048, 1536), "drawn larger than it says it is");
}

#[test]
fn a_broken_link_is_not_executable() {
    let d = tmpdir("brokenlink");
    std::os::unix::fs::symlink(d.join("nowhere"), d.join("gone")).unwrap();
    std::fs::write(d.join("there"), "x").unwrap();
    std::os::unix::fs::symlink(d.join("there"), d.join("good")).unwrap();
    let gone = crate::fsx::Entry::from_path(&d.join("gone")).unwrap();
    assert!(gone.is_link && gone.is_broken_link() && !gone.is_executable(), "lrwxrwxrwx is not a program");
    let good = crate::fsx::Entry::from_path(&d.join("good")).unwrap();
    assert!(good.is_link && !good.is_broken_link());
    let _ = std::fs::remove_dir_all(&d);
}

/// How long Search Code takes over a big tree (MAIA_BENCH_DIR, else /usr/include), warm,
/// for plain text, whole words and expressions that match little, so all of it is read.
/// How long the filter takes over a flattened tree of about 350,000 files (a real folder,
/// /usr by default, repeated until it is that big), for plain words and for long queries.
/// `cargo test --release filter_query_speed -- --ignored --nocapture`
#[test]
#[ignore]
fn filter_query_speed() {
    let dir = PathBuf::from(std::env::var("MAIA_BENCH_DIR").unwrap_or_else(|_| "/usr".into()));
    let _ = crate::query::users(true);
    let w = crate::fsx::walk_async(dir.clone(), true, egui::Context::default());
    let mut found = vec![];
    while !w.done.load(std::sync::atomic::Ordering::Relaxed) {
        found.extend(w.rx.try_iter().flatten());
        std::thread::sleep(Duration::from_millis(2));
    }
    found.extend(w.rx.try_iter().flatten());
    let mut entries = Vec::with_capacity(360_000);
    while entries.len() < 350_000 && !found.is_empty() {
        entries.extend(found.iter().cloned());
    }
    let stats = HashMap::new();
    let recents = HashSet::new();
    let ctx = ViewCtx { show_hidden: true, show_system: true, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
    let mut t = Tab::new(42, dir, ViewSettings::default(), egui::Context::default());
    t.flatten = true;
    t.set_entries(entries);
    let queries = std::env::var("MAIA_BENCH_QUERIES").unwrap_or_else(|_| ";lib;lib size>10k;lib modified<1y size>1k;lib -test ext:so,h,py;(lib | share) -doc type:code,pictures size>1k;/^lib.*\\.so(\\.\\d+)*$/;re:^[a-m] -ext:gz,png modified>30d owner:root;type:png;*.h -name:config".into());
    // Queries are split at ';' (`|` is the filter's own).
    for q in queries.split(';') {
        t.filter = q.to_string();
        let mut ms = vec![];
        for _ in 0..9 {
            let t0 = Instant::now();
            t.rebuild(&ctx);
            ms.push(t0.elapsed().as_secs_f64() * 1000.0);
        }
        ms.sort_by(|a, b| a.total_cmp(b));
        eprintln!("{:>7.1} ms least {:>7.1} median  {:>7} of {} shown  {q:?}", ms[0], ms[4], t.view.len(), t.entries.len());
    }
}

#[test]
#[ignore]
fn code_search_speed() {
    use crate::codesearch::{Query, Search};
    let dir = PathBuf::from(std::env::var("MAIA_BENCH_DIR").unwrap_or_else(|_| "/usr/include".into()));
    let ctx = egui::Context::default();
    let qs = [
        ("plain", Query { text: "xyzzy_frob".into(), ..Default::default() }),
        ("plain, match case", Query { text: "XYZZY_FROB".into(), case: true, ..Default::default() }),
        ("whole word", Query { text: "frobnicate".into(), word: true, ..Default::default() }),
        ("regex literal", Query { text: "xyzzy_frob".into(), regex: true, ..Default::default() }),
        ("regex class", Query { text: r"struct\s+xyzzy_\w+".into(), regex: true, ..Default::default() }),
        ("regex alternation", Query { text: "xyzzy|plugh|frobozz".into(), regex: true, case: true, ..Default::default() }),
        ("regex with no literal", Query { text: "[A-Z][a-z]+_[0-9]{3}x".into(), regex: true, case: true, ..Default::default() }),
        ("todo", Query { todo: true, ..Default::default() }),
    ];
    for round in 0..2 {
        for (label, q) in &qs {
            let t = Instant::now();
            let s = Search::start(dir.clone(), false, q, &ctx).unwrap();
            let r = s.wait();
            if round == 1 {
                eprintln!("{label}: {:.0} ms, {} files with matches", t.elapsed().as_secs_f64() * 1000.0, r.len());
            }
        }
    }
}

// ------------------------------------------------------------------ developer tools

/// Run a search to the end and give back (rel path, line numbers) per file.
fn code_hits(dir: &Path, q: &crate::codesearch::Query) -> Vec<(String, Vec<u32>)> {
    let ctx = egui::Context::default();
    let s = crate::codesearch::Search::start(dir.to_path_buf(), false, q, &ctx).unwrap();
    s.wait().into_iter().map(|f| (f.rel, f.hits.iter().map(|h| h.line).collect())).collect()
}

fn cq(text: &str) -> crate::codesearch::Query {
    crate::codesearch::Query { text: text.into(), ..Default::default() }
}

/// Plain text ignores case unless asked, whole words need a boundary on both sides, an
/// expression finds what it describes, letters outside ASCII fold too, and a broken
/// expression says what is wrong instead of searching.
#[test]
fn code_search_modes() {
    use crate::codesearch::{Matcher, hits_in};
    let text = "let Needle = 1;\nneedles(x);\n  NEEDLE_2 // needle\nnöt here\nÄrger und ärger\n";
    let lines = |m: &Matcher| -> Vec<u32> {
        let (mut low, mut sp) = (vec![], vec![]);
        hits_in(text, m, None, 100, &mut low, &mut sp).0.iter().map(|h| h.line).collect()
    };
    assert_eq!(lines(&Matcher::new("needle", false, false, false).unwrap()), vec![1, 2, 3]);
    assert_eq!(lines(&Matcher::new("Needle", true, false, false).unwrap()), vec![1]);
    assert_eq!(lines(&Matcher::new("needle", false, true, false).unwrap()), vec![1, 3], "needles and NEEDLE_2 are other words");
    assert_eq!(lines(&Matcher::new("needle", true, true, false).unwrap()), vec![3]);
    assert_eq!(lines(&Matcher::new(r"needles?\(", false, false, true).unwrap()), vec![2]);
    assert_eq!(lines(&Matcher::new(r"^\s*NEEDLE_\d", true, false, true).unwrap()), vec![3], "^ starts each line");
    assert_eq!(lines(&Matcher::new("ärger", false, false, false).unwrap()), vec![5]);
    let (mut low, mut sp) = (vec![], vec![]);
    let (h, _) = hits_in(text, &Matcher::new("ärger", false, false, false).unwrap(), None, 100, &mut low, &mut sp);
    assert_eq!(h[0].spans.len(), 2, "both cases of Ä are found: {:?}", h[0]);
    assert_eq!(lines(&Matcher::new("nöt", false, false, false).unwrap()), vec![4]);
    // Several matches on a line are one hit with a span each, on the line as shown.
    let (h, total) = hits_in(text, &Matcher::new("needle", false, false, false).unwrap(), None, 100, &mut low, &mut sp);
    assert_eq!(total, 3);
    let third = &h[2];
    assert_eq!(third.text, "NEEDLE_2 // needle", "indentation is left off");
    assert_eq!(third.spans.iter().map(|&(a, b)| &third.text[a..b]).collect::<Vec<_>>(), vec!["NEEDLE", "needle"]);
    let err = Matcher::new("needle(", false, false, true).err().unwrap();
    assert!(!err.is_empty() && !err.contains('\n'), "{err:?}");
    assert!(Matcher::new("", false, false, false).is_err());
}

/// The literal text an expression needs, for passing over files without it: the longest
/// plain run, one per top-level branch, never a character a quantifier may leave out.
#[test]
fn code_search_expression_prefilter() {
    use crate::codesearch::required_literals as req;
    let one = |p: &str| req(p).map(|v| v.join("|"));
    assert_eq!(one("needle").as_deref(), Some("needle"));
    assert_eq!(one(r"struct\s+Foo\w*").as_deref(), Some("struct"));
    assert_eq!(one(r"fn \w+_parse\(").as_deref(), Some("_parse("));
    assert_eq!(one("colou?r").as_deref(), Some("colo"), "u? may be missing");
    assert_eq!(one("abc|defg").as_deref(), Some("abc|defg"));
    assert_eq!(one(r"(get|set)Value").as_deref(), Some("Value"), "inside a group nothing is certain");
    assert_eq!(one(r"\d{4}-\d{2}"), None, "nothing literal to look for");
    assert_eq!(one("ab|[a-z]+"), None, "a branch without a literal");
    assert_eq!(one(r"a\.b\.c").as_deref(), Some("a.b.c"));
    assert_eq!(one("x{2}yzw").as_deref(), Some("yzw"), "a counted character is left out");
    assert_eq!(one("x{2}yz"), None, "two characters are too few to look for");
    // The prefilter never changes what is found.
    let text = "Colour and color\nset_value getValue setValue\n2026-10-16\n";
    for pat in ["colou?r", r"(get|set)Value", r"\d{4}-\d{2}", "Colour|setV"] {
        let m = crate::codesearch::Matcher::new(pat, false, false, true).unwrap();
        let re = regex_lite::Regex::new(&format!("(?mi){pat}")).unwrap();
        let (mut low, mut sp) = (vec![], vec![]);
        m.find(text, &mut low, &mut sp);
        let want: Vec<(usize, usize)> = re.find_iter(text).map(|m| (m.start(), m.end())).collect();
        assert_eq!(sp, want, "{pat}");
    }
}

/// gitignore rules as git reads them: names anywhere, slashes anchor, a trailing slash
/// means folders only, `**` crosses folders, `!` takes a file back, the last rule wins.
#[test]
fn gitignore_rules() {
    use crate::codesearch::{decide, parse_gitignore, wildmatch};
    assert!(wildmatch(b"*.rs", b"main.rs") && !wildmatch(b"*.rs", b"src/main.rs"));
    assert!(wildmatch(b"src/**/*.rs", b"src/a/b/main.rs") && wildmatch(b"src/**/*.rs", b"src/main.rs"));
    assert!(wildmatch(b"**/build", b"x/y/build") && wildmatch(b"**/build", b"build"));
    assert!(wildmatch(b"docs/**", b"docs/a/b") && !wildmatch(b"docs/**", b"docsx/a"));
    assert!(wildmatch(b"f[a-c]?.txt", b"fbz.txt") && !wildmatch(b"f[!a-c]?.txt", b"fbz.txt"));
    assert!(wildmatch(b"\\*lit", b"*lit") && !wildmatch(b"\\*lit", b"xlit"));
    let rules = parse_gitignore("# comment\n\ntarget/\n*.log\n!keep.log\n/build\ndocs/**/*.tmp\n\\#hash\ntrailing   \n**/node_modules\nsrc/gen/\n");
    let ig = |p: &str, dir: bool| decide(&rules, p, dir);
    assert_eq!(ig("target", true), Some(true));
    assert_eq!(ig("a/b/target", true), Some(true), "a name without a slash matches at any depth");
    assert_eq!(ig("target", false), None, "target/ is for folders only");
    assert_eq!(ig("x/debug.log", false), Some(true));
    assert_eq!(ig("x/keep.log", false), Some(false), "taken back by !");
    assert_eq!(ig("build", true), Some(true));
    assert_eq!(ig("sub/build", true), None, "/build is anchored to its folder");
    assert_eq!(ig("docs/a/b/x.tmp", false), Some(true));
    assert_eq!(ig("#hash", false), Some(true));
    assert_eq!(ig("trailing", false), Some(true), "trailing spaces are dropped");
    assert_eq!(ig("deep/er/node_modules", true), Some(true));
    assert_eq!(ig("src/gen", true), Some(true));
    assert_eq!(ig("other/src/gen", true), None, "a slash inside anchors it");
    assert_eq!(ig("main.rs", false), None);
}

/// A real project walked as Search Code walks it: its .gitignores (nested ones win, and
/// the repository's exclude file counts), hidden and binary files left out by default,
/// all of them searched with All files; the file filter by type and glob.
#[test]
fn code_search_walk_skips_ignored_hidden_and_binary() {
    use crate::codesearch::Query;
    let d = tmpdir("codewalk");
    std::fs::create_dir_all(d.join(".git/info")).unwrap();
    touch(&d.join(".git/info/exclude"), "secret.txt\n");
    touch(&d.join(".gitignore"), "target/\n*.log\n");
    touch(&d.join("src/main.rs"), "fn main() {\n    let needle = 1;\n}\n");
    touch(&d.join("src/lib.rs"), "// needle in lib\n");
    touch(&d.join("src/.gitignore"), "gen.rs\n!keep.log\n");
    touch(&d.join("src/gen.rs"), "needle generated\n");
    touch(&d.join("src/keep.log"), "needle kept\n");
    touch(&d.join("notes.log"), "needle in a log\n");
    touch(&d.join("target/debug/out.rs"), "needle built\n");
    touch(&d.join(".hidden/h.rs"), "needle hidden\n");
    touch(&d.join("secret.txt"), "needle excluded\n");
    touch(&d.join("docs/guide.md"), "the needle, in prose\n");
    std::fs::write(d.join("blob.bin"), b"\x00\x01needle\x02").unwrap();
    let files = |q: &Query| code_hits(&d, q).into_iter().map(|f| f.0).collect::<Vec<_>>();
    assert_eq!(files(&cq("needle")), vec!["docs/guide.md", "src/keep.log", "src/lib.rs", "src/main.rs"], "in path order");
    let all = Query { all: true, ..cq("needle") };
    let got = files(&all);
    for f in ["notes.log", "target/debug/out.rs", ".hidden/h.rs", "secret.txt", "src/gen.rs", "blob.bin"] {
        assert!(got.contains(&f.to_string()), "All files reaches {f}: {got:?}");
    }
    let ctx = egui::Context::default();
    let bin = crate::codesearch::Search::start(d.clone(), false, &all, &ctx).unwrap().wait().into_iter().find(|f| f.rel == "blob.bin").unwrap();
    assert!(bin.binary && bin.hits.is_empty() && bin.total == 1, "a binary file is listed without lines");
    assert_eq!(files(&Query { files: "rust".into(), ..cq("needle") }), vec!["src/lib.rs", "src/main.rs"]);
    assert_eq!(files(&Query { files: "!*.rs, !docs/".into(), ..cq("needle") }), vec!["src/keep.log"]);
    assert_eq!(files(&Query { files: "src/**".into(), ..cq("needle") }), vec!["src/keep.log", "src/lib.rs", "src/main.rs"]);
    assert_eq!(files(&Query { files: ".md".into(), ..cq("needle") }), vec!["docs/guide.md"]);
    // From a folder inside the repository the .gitignores above it still apply.
    let ctx = egui::Context::default();
    let sub: Vec<String> = crate::codesearch::Search::start(d.join("src"), false, &cq("needle"), &ctx).unwrap().wait().into_iter().map(|f| f.rel).collect();
    assert_eq!(sub, vec!["keep.log", "lib.rs", "main.rs"]);
    // And Project scope searches from the repository's top.
    let top = crate::codesearch::Search::start(d.join("src"), true, &cq("needle"), &ctx).unwrap();
    let root = loop {
        if let Some(r) = top.root() {
            break r;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(root, d);
    assert_eq!(top.wait().len(), 4);
    let _ = std::fs::remove_dir_all(&d);
}

/// Each hit carries its neighbours, without indentation; a long line is cut around the
/// match with the spans moved to match; Windows line ends are dropped.
#[test]
fn code_search_context_lines() {
    use crate::codesearch::{Matcher, hits_in};
    let long = format!("{}needle{}", "x".repeat(300), "y".repeat(300));
    let text = format!("first\r\n\tbefore it\r\n    the needle here\r\n\r\nend\n{long}\nlast\n");
    let (mut low, mut sp) = (vec![], vec![]);
    let (h, _) = hits_in(&text, &Matcher::new("needle", false, false, false).unwrap(), None, 100, &mut low, &mut sp);
    assert_eq!(h.len(), 2);
    assert_eq!((h[0].line, h[0].text.as_str(), h[0].before.as_deref(), h[0].after.as_deref()), (3, "the needle here", Some("before it"), None), "an empty neighbour is not shown");
    assert_eq!(&h[0].text[h[0].spans[0].0..h[0].spans[0].1], "needle");
    let l = &h[1];
    assert_eq!(l.line, 6);
    assert!(l.cut.0 && l.cut.1 && l.text.len() <= 240, "cut on both sides: {} bytes", l.text.len());
    assert_eq!(&l.text[l.spans[0].0..l.spans[0].1], "needle", "the span follows the cut");
    assert_eq!((l.before.as_deref(), l.after.as_deref()), (Some("end"), Some("last")));
    // The first line has nothing before it.
    let (h, _) = hits_in("needle\nnext\n", &Matcher::new("needle", false, false, false).unwrap(), None, 100, &mut low, &mut sp);
    assert_eq!((h[0].before.as_deref(), h[0].after.as_deref()), (None, Some("next")));
}

/// Search Code colours what it finds as the file's language: a match inside a block
/// comment reads as comment (the file is read from its top), the code around a match is
/// highlighted too, and a file in no known language stays plain.
#[test]
fn code_search_highlights_its_lines() {
    use crate::syntax::Kind;
    let d = tmpdir("code-colours");
    touch(&d.join("src/main.rs"), "fn main() {\n    /* the start\n       needle inside it */\n    let needle = MAX.len();\n    run();\n}\n");
    touch(&d.join("notes.txt"), "a needle in a note\n");
    let ctx = egui::Context::default();
    let q = crate::codesearch::Query { text: "needle".into(), ..Default::default() };
    let found = crate::codesearch::Search::start(d.clone(), false, &q, &ctx).unwrap().wait();
    let file = |rel: &str| found.iter().find(|f| f.rel == rel).unwrap_or_else(|| panic!("{rel} in {:?}", found.iter().map(|f| &f.rel).collect::<Vec<_>>()));
    let rs = file("src/main.rs");
    let kinds = |text: &str, spans: &[(usize, usize, Kind)]| spans.iter().map(|&(a, b, k)| (text[a..b].to_string(), k)).collect::<Vec<_>>();
    let inside = &rs.hits[0];
    assert_eq!(inside.line, 3);
    assert_eq!(kinds(&inside.text, &inside.syntax[1]), [("needle inside it */".to_string(), Kind::Comment)]);
    assert_eq!(kinds(inside.before.as_deref().unwrap(), &inside.syntax[0]), [("/* the start".to_string(), Kind::Comment)]);
    let code = &rs.hits[1];
    let got = kinds(&code.text, &code.syntax[1]);
    for want in [("let", Kind::Keyword), ("MAX", Kind::Constant), ("len", Kind::Function)] {
        assert!(got.contains(&(want.0.to_string(), want.1)), "{want:?} in {got:?}");
    }
    assert_eq!(kinds(code.after.as_deref().unwrap(), &code.syntax[2]), [("run".to_string(), Kind::Function)]);
    assert!(file("notes.txt").hits[0].syntax.iter().all(Vec::is_empty), "plain text has no colours");
    let _ = std::fs::remove_dir_all(&d);
}

/// A search stops at MAX_MATCHES lines and says so; a file shows PER_FILE lines and
/// counts the rest.
#[test]
fn code_search_result_cap() {
    use crate::codesearch::{MAX_MATCHES, PER_FILE, Search};
    let d = tmpdir("codecap");
    touch(&d.join("big.txt"), &"needle\n".repeat(PER_FILE + 50));
    let ctx = egui::Context::default();
    let one = Search::start(d.clone(), false, &cq("needle"), &ctx).unwrap().wait();
    assert_eq!((one[0].hits.len(), one[0].total), (PER_FILE, PER_FILE + 50));
    for i in 0..(MAX_MATCHES / 40 + 10) {
        touch(&d.join(format!("many/f{i:04}.txt")), &"a needle\n".repeat(40));
    }
    let s = Search::start(d.clone(), false, &cq("needle"), &ctx).unwrap();
    while !s.done() {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(s.capped(), "the cap was reached");
    let shown: usize = s.take().iter().map(|f| f.hits.len()).sum();
    assert!(shown <= MAX_MATCHES && shown > MAX_MATCHES - 200, "{shown} lines kept");
    let _ = std::fs::remove_dir_all(&d);
}

/// TODO, FIXME, HACK and XXX in capitals and as words, whatever the comment style; an
/// owner in brackets and the colon are not part of what it says. The TODO finder counts
/// each and narrows by the query.
#[test]
fn todo_parsing_and_finder() {
    use crate::codesearch::{Query, todo_tag};
    assert_eq!(todo_tag("// TODO: fix the cap"), Some((0, "fix the cap")));
    assert_eq!(todo_tag("# FIXME(ana) - wrong on Mondays"), Some((1, "wrong on Mondays")));
    assert_eq!(todo_tag("/* HACK: until 2.0 */"), Some((2, "until 2.0")));
    assert_eq!(todo_tag("<!-- XXX remove -->"), Some((3, "remove")));
    assert_eq!(todo_tag("let x = 1; // FIXME TODO both"), Some((1, "TODO both")), "the first tag on the line");
    assert_eq!(todo_tag("TODOS and xTODO and todo"), None, "only whole words in capitals");
    let d = tmpdir("todos");
    touch(&d.join("a.rs"), "// TODO: one\nfn x() {} // FIXME: two\n// todo: not this\n// TODOS: nor this\n");
    touch(&d.join("b.py"), "# HACK: three\n# XXX four about the cache\n# TODO cache too\n");
    let ctx = egui::Context::default();
    let got = crate::codesearch::Search::start(d.clone(), false, &Query { todo: true, ..Default::default() }, &ctx).unwrap().wait();
    let lines: Vec<(String, Vec<u32>)> = got.iter().map(|f| (f.rel.clone(), f.hits.iter().map(|h| h.line).collect())).collect();
    assert_eq!(lines, vec![("a.rs".to_string(), vec![1, 2]), ("b.py".to_string(), vec![1, 2, 3])]);
    let tags = got.iter().fold([0u32; 4], |mut a, f| {
        a.iter_mut().zip(f.tags).for_each(|(x, y)| *x += y);
        a
    });
    assert_eq!(tags, [2, 1, 1, 1]);
    let narrowed = code_hits(&d, &Query { todo: true, text: "cache".into(), ..Default::default() });
    assert_eq!(narrowed, vec![("b.py".to_string(), vec![2, 3])]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Every editor gets the line its own way; Auto takes $VISUAL, then $EDITOR, then the
/// first installed; a command with {file} and {line} is filled in; terminal editors are
/// marked to run in a terminal.
#[test]
fn editor_command_lines() {
    use crate::editor::{command, resolve};
    let none = |_: &str| false;
    let line = |setting: &str| {
        let c = resolve(setting, None, None, &none).unwrap();
        (command(&c, "/p/main.rs", Some(42)).join(" "), c.terminal)
    };
    for (ed, want, term) in [
        ("code", "code -g /p/main.rs:42", false),
        ("codium", "codium -g /p/main.rs:42", false),
        ("cursor", "cursor -g /p/main.rs:42", false),
        ("zed", "zed /p/main.rs:42", false),
        ("zeditor", "zeditor /p/main.rs:42", false),
        ("subl", "subl /p/main.rs:42", false),
        ("idea", "idea --line 42 /p/main.rs", false),
        ("clion", "clion --line 42 /p/main.rs", false),
        ("kate", "kate --line 42 /p/main.rs", false),
        ("gedit", "gedit +42 /p/main.rs", false),
        ("emacs", "emacs +42 /p/main.rs", false),
        ("nvim", "nvim +42 /p/main.rs", true),
        ("vim", "vim +42 /p/main.rs", true),
        ("hx", "hx /p/main.rs:42", true),
        ("helix", "helix /p/main.rs:42", true),
        ("nano", "nano +42 /p/main.rs", true),
        ("micro", "micro +42 /p/main.rs", true),
        ("kak", "kak +42 /p/main.rs", true),
        ("/opt/bin/code --new-window", "/opt/bin/code --new-window -g /p/main.rs:42", false),
        ("flatpak run org.x.Ed --line={line} {file}", "flatpak run org.x.Ed --line=42 /p/main.rs", false),
        ("myedit", "myedit /p/main.rs", false),
    ] {
        assert_eq!(line(ed), (want.to_string(), term), "{ed}");
    }
    let c = resolve("code", None, None, &none).unwrap();
    assert_eq!(command(&c, "/p/dir", None), vec!["code", "/p/dir"], "without a line, just the path");
    // Auto: the environment first, an unknown $EDITOR in a terminal; then what is installed,
    // windowed editors before terminal ones.
    let has = |b: &str| matches!(b, "nvim" | "zeditor" | "micro");
    assert_eq!(resolve("", Some("hx"), Some("nano"), &has).unwrap().label, "Helix");
    assert_eq!(resolve("", None, Some("nano"), &has).unwrap().label, "nano");
    let odd = resolve("", None, Some("ed -p x"), &has).unwrap();
    assert!(odd.terminal && command(&odd, "/f", Some(3)) == vec!["ed", "-p", "x", "/f"]);
    let auto = resolve("", None, None, &has).unwrap();
    assert_eq!((auto.label.as_str(), command(&auto, "/f", Some(3)).join(" ")), ("Zed", "zeditor /f:3".to_string()));
    assert_eq!(resolve("", None, None, &|b: &str| b == "nvim").unwrap().label, "Neovim");
    assert!(resolve("", None, None, &none).is_none());
    assert!(resolve("  ", None, None, &none).is_none(), "spaces are empty");
}

/// Tasks from real manifests: scripts in their order with the package manager the
/// lockfile names, Cargo's commands with each binary and example, Makefile targets and
/// justfile recipes without the private and special ones, pyproject's script tables,
/// deno tasks under comments, composer scripts, Taskfile tasks and Go's commands; and the
/// folders up to the project's top.
#[test]
fn task_detection_from_manifests() {
    use crate::tasks::*;
    let d = tmpdir("tasks");
    let names = |g: &Group| g.tasks.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
    touch(&d.join("web/package.json"), r#"{"name":"w","scripts":{"dev":"vite","build":"vite build","test":"vitest run"}}"#);
    touch(&d.join("web/pnpm-lock.yaml"), "");
    let g = detect_dir(&d.join("web"));
    assert_eq!((g[0].source.as_str(), names(&g[0])), ("package.json · pnpm", vec!["dev".to_string(), "build".into(), "test".into()]), "the manifest's order");
    assert_eq!(g[0].tasks[1].argv, vec!["pnpm", "run", "build"]);
    assert_eq!(g[0].tasks[1].detail, "vite build");
    for (lock, pm) in [("yarn.lock", "yarn"), ("bun.lockb", "bun"), ("package-lock.json", "npm")] {
        let e = tmpdir(&format!("pm-{pm}"));
        touch(&e.join(lock), "");
        assert_eq!(package_manager(&e, "{}"), pm);
        let _ = std::fs::remove_dir_all(&e);
    }
    assert_eq!(package_manager(&d, r#"{"packageManager":"yarn@4.1.0"}"#), "yarn", "packageManager wins");
    assert_eq!(package_manager(&d, "{}"), "npm");
    touch(&d.join("Cargo.toml"), "[package]\nname = \"app\"\nversion = \"0.1.0\"\n\n[[bin]]\nname = \"helper\"\npath = \"src/helper.rs\"\n\n[dependencies]\nserde = { version = \"1\", features = [\n  \"derive\",\n] }\n");
    touch(&d.join("src/main.rs"), "fn main() {}");
    touch(&d.join("src/bin/extra.rs"), "fn main() {}");
    touch(&d.join("examples/demo.rs"), "fn main() {}");
    touch(&d.join("examples/multi/main.rs"), "fn main() {}");
    let cargo = cargo_tasks(&d, &std::fs::read_to_string(d.join("Cargo.toml")).unwrap());
    let cn: Vec<&str> = cargo.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(cn, vec!["build", "run app", "run helper", "run extra", "check", "test", "example demo", "example multi"]);
    assert_eq!(cargo[2].argv, vec!["cargo", "run", "--bin", "helper"]);
    assert_eq!(cargo[6].argv, vec!["cargo", "run", "--example", "demo"]);
    let single = tmpdir("cargo1");
    touch(&single.join("src/main.rs"), "");
    let one = cargo_tasks(&single, "[package]\nname = \"x\"\n");
    assert_eq!(one.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["build", "run", "check", "test"]);
    assert_eq!(cargo_tasks(&single, "[workspace]\nmembers = [\"a\", \"b\"]\n").len(), 3, "a workspace builds, checks and tests");
    assert!(cargo_tasks(&single, "[dependencies]\n").is_empty());
    let _ = std::fs::remove_dir_all(&single);
    let mk = make_targets("CC := gcc\nVAR = a:b\n.PHONY: all clean\n# Build it all\nall: app lib\n\tgcc x\napp lib: main.o\n%.o: %.c\n\t$(CC) -c $<\nclean: ## remove output\n\trm -f x\ninstall::\n$(OUT): x\n");
    assert_eq!(mk.iter().map(|t| t.name.as_str()).collect::<Vec<_>>(), vec!["all", "app", "lib", "clean", "install"]);
    assert_eq!((mk[0].detail.as_str(), mk[3].detail.as_str()), ("Build it all", "remove output"));
    let just = just_recipes("set shell := [\"bash\", \"-c\"]\nalias b := build\nversion := \"1\"\n\n# Build everything\nbuild:\n    cargo build\n\ntest filter='':\n    cargo test {{filter}}\n\n_helper:\n    echo\n\n[private]\nhidden:\n    echo\n\n@deploy env +args: build\n    echo {{env}}\n");
    assert_eq!(just.iter().map(|t| (t.name.as_str(), t.detail.as_str())).collect::<Vec<_>>(), vec![("build", "Build everything"), ("test", "filter=''"), ("deploy", "env +args")]);
    assert_eq!(just[0].argv, vec!["just", "build"]);
    touch(&d.join("py/pyproject.toml"), "[project]\nname = \"p\"\n[project.scripts]\nserve = \"p.app:main\"\n\n[tool.poe.tasks]\nlint = \"ruff check .\"\nfmt = { cmd = \"ruff format\" }\n");
    touch(&d.join("py/uv.lock"), "");
    let py = pyproject_scripts(&d.join("py"), &std::fs::read_to_string(d.join("py/pyproject.toml")).unwrap());
    assert_eq!(py.len(), 2);
    assert_eq!((py[0].0.as_str(), py[0].1[0].argv.clone()), ("pyproject.toml · uv", vec!["uv".to_string(), "run".into(), "serve".into()]));
    assert_eq!(py[1].1.iter().map(|t| t.argv.join(" ")).collect::<Vec<_>>(), vec!["poe lint", "poe fmt"]);
    let deno = deno_tasks("{\n  // dev server\n  \"tasks\": { \"start\": \"deno run -A main.ts\", /* x */ \"url\": \"echo https://x.y\" }\n}");
    assert_eq!(deno.iter().map(|t| (t.name.as_str(), t.detail.as_str())).collect::<Vec<_>>(), vec![("start", "deno run -A main.ts"), ("url", "echo https://x.y")]);
    let tf = taskfile_tasks("version: '3'\ntasks:\n  build:\n    desc: Build the app\n    cmds:\n      - go build\n  \"lint\":\n    cmds: [golangci-lint run]\nvars:\n  X: y\n");
    assert_eq!(tf.iter().map(|t| (t.name.as_str(), t.detail.as_str())).collect::<Vec<_>>(), vec![("build", "Build the app"), ("lint", "")]);
    touch(&d.join("go/go.mod"), "module x\n");
    touch(&d.join("go/main.go"), "package main\n");
    touch(&d.join("go/composer.json"), r#"{"scripts":{"test":"phpunit","cs":["phpcs","phpstan"]}}"#);
    let go = detect_dir(&d.join("go"));
    assert_eq!(go.iter().map(|g| g.source.as_str()).collect::<Vec<_>>(), vec!["composer.json", "go.mod"]);
    assert_eq!(go[0].tasks[1].detail, "phpcs && phpstan");
    assert_eq!(go[1].tasks.iter().map(|t| t.argv.join(" ")).collect::<Vec<_>>(), vec!["go build ./...", "go run .", "go test ./...", "go vet ./..."]);
    // From web/src: web's scripts first, then the project's top (a repository).
    std::fs::create_dir_all(d.join(".git")).unwrap();
    std::fs::create_dir_all(d.join("web/src")).unwrap();
    let all = detect(&d.join("web/src"));
    assert_eq!(all.iter().map(|g| (g.dir.clone(), g.source.clone())).collect::<Vec<_>>(), vec![(d.join("web"), "package.json · pnpm".to_string()), (d.clone(), "Cargo.toml".to_string())]);
    let _ = std::fs::remove_dir_all(&d);
}

/// A task runs held open in the terminal: its exit status, then the user's shell.
#[test]
fn tasks_keep_their_terminal_open() {
    let v = crate::mime::held(&["npm".into(), "run".into(), "a b".into()]);
    assert_eq!(&v[..2], &["sh", "-c"]);
    assert_eq!(&v[3..], &["sh", "npm", "run", "a b"], "the task's words are passed whole, never through the shell");
    assert!(v[2].starts_with("\"$@\"") && v[2].contains("exec \"${SHELL:-/bin/sh}\""));
    let out = std::process::Command::new(&v[0]).args(&v[1..3]).arg("sh").args(["printf", "%s|", "a b"]).env("SHELL", "true").output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "a b|\n[exit 0]\n");
}

/// Build folders are what a tool makes beside its manifest: target with a Cargo.toml (not
/// without), node_modules once (not the ones inside it), dist and build beside a
/// package.json or pyproject, environments by their pyvenv.cfg, caches by CACHEDIR.TAG;
/// sizes are added up after.
#[test]
fn build_folder_detection() {
    use crate::buildclean::{Msg, Scan};
    let d = tmpdir("builds");
    touch(&d.join("rust/Cargo.toml"), "[package]\n");
    touch(&d.join("rust/target/debug/app"), &"x".repeat(5000));
    touch(&d.join("rust/src/target/keep.rs"), "a source folder called target");
    touch(&d.join("web/package.json"), "{}");
    touch(&d.join("web/node_modules/a/index.js"), &"y".repeat(3000));
    touch(&d.join("web/node_modules/a/node_modules/b/i.js"), "z");
    touch(&d.join("web/dist/app.js"), "1");
    touch(&d.join("web/.next/cache/x"), "2");
    touch(&d.join("plain/dist/readme.txt"), "dist without a manifest is someone's");
    touch(&d.join("plain/build/notes.txt"), "same");
    touch(&d.join("py/pyproject.toml"), "");
    touch(&d.join("py/.venv/pyvenv.cfg"), "home = /usr/bin\n");
    touch(&d.join("py/.venv/lib/site.py"), "s");
    touch(&d.join("py/env/pyvenv.cfg"), "");
    touch(&d.join("py/pkg/__pycache__/m.cpython-313.pyc"), "c");
    touch(&d.join("py/.pytest_cache/v"), "p");
    touch(&d.join("misc/cache/CACHEDIR.TAG"), "Signature: 8a477f597d28d172789f06886806bc55\n");
    touch(&d.join(".hidden/node_modules/x.js"), "hidden folders are not walked");
    let ctx = egui::Context::default();
    let s = Scan::start(d.clone(), &ctx);
    let (mut found, mut sizes) = (vec![], std::collections::HashMap::new());
    let t = Instant::now();
    loop {
        match s.rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Msg::Found(f)) => found.push(f),
            Ok(Msg::Size(p, n)) => {
                sizes.insert(p, n);
            }
            Ok(Msg::Done) | Err(_) => break,
        }
    }
    assert!(t.elapsed() < Duration::from_secs(5));
    let mut got: Vec<(String, &str)> = found.iter().map(|f| (crate::project::relative(&f.path, &d), f.kind)).collect();
    got.sort();
    assert_eq!(got, vec![
        ("misc/cache".to_string(), "Cache"),
        ("py/.pytest_cache".to_string(), "Python tool cache"),
        ("py/.venv".to_string(), "Python environment"),
        ("py/env".to_string(), "Python environment"),
        ("py/pkg/__pycache__".to_string(), "Python bytecode"),
        ("rust/target".to_string(), "Rust build"),
        ("web/.next".to_string(), "Next.js build"),
        ("web/dist".to_string(), "Build output"),
        ("web/node_modules".to_string(), "Node packages"),
    ]);
    assert_eq!(sizes.get(&d.join("rust/target")), Some(&5000));
    assert_eq!(sizes.get(&d.join("web/node_modules")), Some(&3001), "everything inside, nested packages too");
    assert_eq!(sizes.len(), found.len(), "every folder is sized");
    assert!(found.iter().all(|f| f.modified.is_some()));
    let _ = std::fs::remove_dir_all(&d);
}

/// Paths written from the project's top: its repository, else its manifest, else the
/// folder itself; never the home folder or / for a folder inside them.
#[test]
fn path_variants_from_the_project_root() {
    use crate::project::{git_root, relative, root, with_line};
    let d = tmpdir("paths");
    std::fs::create_dir_all(d.join("repo/.git")).unwrap();
    touch(&d.join("repo/crates/a/Cargo.toml"), "");
    touch(&d.join("repo/crates/a/src/lib.rs"), "");
    touch(&d.join("loose/pkg/package.json"), "{}");
    std::fs::create_dir_all(d.join("loose/pkg/src")).unwrap();
    std::fs::create_dir_all(d.join("nothing/here")).unwrap();
    assert_eq!(root(&d.join("repo/crates/a/src")), d.join("repo"), "the repository beats the nearer manifest");
    assert_eq!(root(&d.join("loose/pkg/src")), d.join("loose/pkg"));
    assert_eq!(root(&d.join("nothing/here")), d.join("nothing/here"));
    assert_eq!(git_root(&d.join("nothing/here")), None);
    let f = d.join("repo/crates/a/src/lib.rs");
    assert_eq!(relative(&f, &root(&d.join("repo/crates/a/src"))), "crates/a/src/lib.rs");
    assert_eq!(with_line(&relative(&f, &d.join("repo")), 12), "crates/a/src/lib.rs:12");
    assert_eq!(relative(&f, &d.join("elsewhere")), f.to_string_lossy(), "outside the root: the whole path");
    assert_eq!(relative(&d, &d), d.to_string_lossy());
    let _ = std::fs::remove_dir_all(&d);
}

/// A repository for changing things in: its own identity, no signing, and hooks only
/// from its own `.git/hooks` (the machine's git config must not leak into a test).
pub(crate) fn work_repo(d: &Path) {
    git(d, &["init", "-q"]);
    for (k, v) in [("user.name", "Test"), ("user.email", "test@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", ".git/hooks")] {
        git(d, &["config", k, v]);
    }
}

fn work_of(d: &Path) -> crate::gitwork::Work {
    crate::gitwork::work(d).expect("a work tree")
}

fn letters(v: &[crate::gitlog::Change]) -> Vec<(String, char)> {
    v.iter().map(|c| (c.path.clone(), c.status as char)).collect()
}

/// Forty numbered lines: two changes far apart make two hunks.
fn numbered(change: &[(usize, &str)]) -> String {
    (1..=40).map(|i| change.iter().find(|c| c.0 == i).map_or(format!("line {i}\n"), |c| format!("{}\n", c.1))).collect()
}

#[test]
fn git_changes_stage_and_unstage_files_and_hunks() {
    use crate::gitwork::{Op, file_diff, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitwork");
    work_repo(&d);
    touch(&d.join("src/lib.rs"), &numbered(&[]));
    touch(&d.join("old.txt"), "old\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    touch(&d.join("src/lib.rs"), &numbered(&[(3, "line three"), (36, "line thirty-six")]));
    std::fs::remove_file(d.join("old.txt")).unwrap();
    touch(&d.join("notes.md"), "one\ntwo\nthree");
    let w = work_of(&d);
    assert!(w.staged.is_empty() && w.head.is_some());
    assert_eq!(letters(&w.unstaged), [("old.txt".into(), 'D'), ("src/lib.rs".into(), 'M'), ("notes.md".into(), '?')]);
    let lib = w.unstaged[1].clone();
    assert_eq!(lib.lines, Some((2, 2)), "the counts");
    assert_eq!(w.unstaged[2].lines, Some((3, 0)), "an untracked file counts its lines");
    // Two hunks, each applying alone: stage the second only.
    let diff = file_diff(&d, &lib, false, "HEAD");
    assert_eq!(diff.hunks.len(), 2, "two hunks");
    assert!(diff.hunks[1].lines.contains(&"+line thirty-six".to_string()) && diff.hunks[1].head.starts_with("@@ -33,"));
    let patch = diff.hunks[1].patch().expect("applies alone").to_vec();
    run(&d, &Op::Hunk { patch, staged: false }).unwrap();
    let w = work_of(&d);
    assert_eq!(letters(&w.staged), [("src/lib.rs".into(), 'M')]);
    assert_eq!(w.staged[0].lines, Some((1, 1)), "one hunk staged");
    let staged = file_diff(&d, &w.staged[0], true, "HEAD");
    assert_eq!(staged.hunks.len(), 1);
    assert!(staged.hunks[0].lines.contains(&"+line thirty-six".to_string()));
    let left = file_diff(&d, &lib, false, "HEAD");
    assert_eq!(left.hunks.len(), 1, "the other hunk is still unstaged");
    assert!(left.hunks[0].lines.contains(&"+line three".to_string()));
    // And out again.
    let patch = staged.hunks[0].patch().unwrap().to_vec();
    run(&d, &Op::Hunk { patch, staged: true }).unwrap();
    assert!(work_of(&d).staged.is_empty(), "the hunk unstaged");
    assert_eq!(std::fs::read_to_string(d.join("src/lib.rs")).unwrap(), numbered(&[(3, "line three"), (36, "line thirty-six")]), "the file itself untouched");
    // Whole files: a deletion and an untracked file.
    run(&d, &Op::Stage(vec!["old.txt".into(), "notes.md".into()])).unwrap();
    let w = work_of(&d);
    assert_eq!(letters(&w.staged), [("notes.md".into(), 'A'), ("old.txt".into(), 'D')]);
    assert_eq!(letters(&w.unstaged), [("src/lib.rs".into(), 'M')]);
    run(&d, &Op::Unstage(vec!["notes.md".into()])).unwrap();
    assert_eq!(letters(&work_of(&d).staged), [("old.txt".into(), 'D')]);
    assert_eq!(run(&d, &Op::StageAll).unwrap(), "Staged every change");
    let w = work_of(&d);
    assert_eq!(w.staged.len(), 3);
    assert!(w.unstaged.is_empty());
    run(&d, &Op::UnstageAll).unwrap();
    let w = work_of(&d);
    assert!(w.staged.is_empty() && w.unstaged.len() == 3, "{w:?}");
    // Before the first commit there is no HEAD to unstage to.
    let e = tmpdir("gitwork-new");
    work_repo(&e);
    touch(&e.join("a.txt"), "a\n");
    let w = work_of(&e);
    assert_eq!((w.head.clone(), letters(&w.unstaged)), (None, vec![("a.txt".into(), '?')]));
    run(&e, &Op::Stage(vec!["a.txt".into()])).unwrap();
    let w = work_of(&e);
    assert_eq!(letters(&w.staged), [("a.txt".into(), 'A')]);
    assert_eq!(w.staged[0].lines, Some((1, 0)));
    let base = crate::gitwork::base(&e, None).unwrap();
    assert_eq!(file_diff(&e, &w.staged[0], true, &base).hunks[0].lines, ["+a"]);
    run(&e, &Op::Unstage(vec!["a.txt".into()])).unwrap();
    run(&e, &Op::StageAll).unwrap();
    run(&e, &Op::UnstageAll).unwrap();
    assert!(work_of(&e).staged.is_empty());
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&e);
}

#[test]
fn git_discard_keeps_what_it_discards() {
    use crate::gitwork::{Op, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitdiscard");
    work_repo(&d);
    touch(&d.join("a.txt"), "one\n");
    touch(&d.join("b.txt"), "bee\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    // A staged change and an unstaged one on top: only the unstaged one goes.
    touch(&d.join("a.txt"), "two\n");
    git(&d, &["add", "a.txt"]);
    touch(&d.join("a.txt"), "three\n");
    touch(&d.join("b.txt"), "buzz\n");
    let a = work_of(&d).unstaged.into_iter().find(|c| c.path == "a.txt").unwrap();
    let said = run(&d, &Op::Discard(a)).unwrap();
    assert!(said.contains("kept in the stash"), "{said}");
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "two\n", "back to the staged version");
    assert_eq!(std::fs::read_to_string(d.join("b.txt")).unwrap(), "buzz\n", "another file is left alone");
    let w = work_of(&d);
    assert_eq!(letters(&w.staged), [("a.txt".into(), 'M')], "the staged change stays");
    let refs = crate::gitwork::refs(&d);
    assert_eq!(refs.stashes.len(), 1);
    assert!(refs.stashes[0].subject.ends_with("Discarded in Maia: a.txt"), "{:?}", refs.stashes[0]);
    let out = std::process::Command::new("git").arg("-C").arg(&d).args(["show", &format!("{}:a.txt", refs.stashes[0].hash)]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout), "three\n", "the discarded text is in the stash");
    // An untracked file goes to the trash, from where it comes back (only when the
    // trash used is not the home folder's).
    use std::os::unix::fs::MetadataExt;
    let dev = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).ok();
    if dev(&d) == dirs::data_dir().and_then(|h| dev(&h)) {
        let _ = std::fs::remove_dir_all(&d);
        return;
    }
    touch(&d.join("scratch.txt"), "draft\n");
    let s = work_of(&d).unstaged.into_iter().find(|c| c.status == b'?').unwrap();
    assert!(run(&d, &Op::Discard(s)).unwrap().contains("trash"));
    assert!(!d.join("scratch.txt").exists());
    let it = crate::trashcan::list().into_iter().find(|i| i.original == d.join("scratch.txt")).expect("in the trash");
    crate::trashcan::restore(&it).unwrap();
    assert_eq!(std::fs::read_to_string(d.join("scratch.txt")).unwrap(), "draft\n");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn git_commit_amend_and_a_failing_hook() {
    use crate::gitwork::{Op, head_message, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitcommit");
    work_repo(&d);
    touch(&d.join("a.txt"), "a\n");
    let commit = |s: &str, b: &str, amend: bool| run(&d, &Op::Commit { summary: s.into(), body: b.into(), amend });
    assert_eq!(commit("First", "", false).unwrap_err(), "Nothing is staged");
    assert_eq!(commit("  ", "", false).unwrap_err(), "Write a summary first");
    run(&d, &Op::Stage(vec!["a.txt".into()])).unwrap();
    let said = commit("Add the first file", "Why it is there.\n\nTwo paragraphs.", false).unwrap();
    assert!(said.starts_with("Committed "), "{said}");
    assert_eq!(head_message(&d).unwrap(), ("Add the first file".to_string(), "Why it is there.\n\nTwo paragraphs.".to_string()));
    assert!(work_of(&d).is_empty());
    // Amending changes the message (and takes what is staged), not the count.
    touch(&d.join("b.txt"), "b\n");
    run(&d, &Op::Stage(vec!["b.txt".into()])).unwrap();
    assert!(commit("Add two files", "", true).unwrap().starts_with("Amended "));
    let count = std::process::Command::new("git").arg("-C").arg(&d).args(["rev-list", "--count", "HEAD"]).output().unwrap();
    assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");
    assert_eq!(head_message(&d).unwrap().0, "Add two files");
    // A hook that refuses: its words come back, and nothing is committed.
    let hook = d.join(".git/hooks/pre-commit");
    touch(&hook, "#!/bin/sh\necho 'lint: trailing space in c.txt' >&2\nexit 1\n");
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    touch(&d.join("c.txt"), "c \n");
    run(&d, &Op::Stage(vec!["c.txt".into()])).unwrap();
    let e = commit("Add c", "", false).unwrap_err();
    assert!(e.contains("lint: trailing space in c.txt"), "{e}");
    assert_eq!(letters(&work_of(&d).staged), [("c.txt".into(), 'A')], "still staged");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn git_branches_switch_create_track_and_stash() {
    use crate::gitwork::{Op, refs, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitbranches");
    let server = d.join("server.git");
    std::fs::create_dir_all(&server).unwrap();
    git(&server, &["init", "-q", "--bare"]);
    let ours = d.join("ours");
    std::fs::create_dir_all(&ours).unwrap();
    work_repo(&ours);
    touch(&ours.join("a.txt"), "a\n");
    git(&ours, &["add", "-A"]);
    git(&ours, &["commit", "-qm", "Start"]);
    git(&ours, &["remote", "add", "origin", server.to_str().unwrap()]);
    git(&ours, &["push", "-q", "-u", "origin", "main"]);
    git(&ours, &["branch", "topic"]);
    git(&ours, &["push", "-q", "origin", "topic"]);
    git(&ours, &["branch", "-q", "-D", "topic"]);
    touch(&ours.join("a.txt"), "a2\n");
    git(&ours, &["commit", "-qam", "Ahead"]);
    let r = refs(&ours);
    assert_eq!(r.local.len(), 1);
    assert!(r.local[0].current && r.local[0].name == "main" && r.local[0].upstream == "origin/main" && (r.local[0].ahead, r.local[0].behind) == (1, 0), "{:?}", r.local);
    assert_eq!(r.remote, ["origin/topic"], "a remote branch nothing tracks");
    // A new branch from HEAD, then back.
    assert_eq!(run(&ours, &Op::Create("docs".into())).unwrap(), "Created and switched to docs");
    assert!(run(&ours, &Op::Create("-x".into())).is_err() && run(&ours, &Op::Create("a..b".into())).is_err(), "not branch names");
    let r = refs(&ours);
    assert!(r.local.iter().any(|b| b.name == "docs" && b.current));
    touch(&ours.join("a.txt"), "on docs\n");
    git(&ours, &["commit", "-qam", "Docs"]);
    // Switching would overwrite a local change: git refuses, and says why.
    touch(&ours.join("a.txt"), "local\n");
    let e = run(&ours, &Op::Switch("main".into())).unwrap_err();
    assert!(e.contains("would be overwritten") && e.contains("a.txt"), "{e}");
    assert!(!e.starts_with("error:"));
    assert_eq!(std::fs::read_to_string(ours.join("a.txt")).unwrap(), "local\n", "nothing lost");
    // Stash it, switch, come back, pop it.
    git(&ours, &["stash", "push", "-q", "-m", "Local tweak"]);
    let s = refs(&ours).stashes;
    assert_eq!((s.len(), s[0].sel.as_str()), (1, "stash@{0}"));
    assert!(s[0].subject.contains("Local tweak"));
    run(&ours, &Op::Switch("main".into())).unwrap();
    assert_eq!(std::fs::read_to_string(ours.join("a.txt")).unwrap(), "a2\n");
    run(&ours, &Op::Switch("docs".into())).unwrap();
    // A selector that moved since it was read is refused.
    let mut stale = s[0].clone();
    stale.hash = "0".repeat(40);
    assert!(run(&ours, &Op::StashDrop(stale)).unwrap_err().contains("changed"));
    run(&ours, &Op::StashApply(s[0].clone())).unwrap();
    assert_eq!(std::fs::read_to_string(ours.join("a.txt")).unwrap(), "local\n");
    assert_eq!(refs(&ours).stashes.len(), 1, "apply keeps it");
    git(&ours, &["checkout", "-q", "--", "a.txt"]);
    run(&ours, &Op::StashPop(s[0].clone())).unwrap();
    assert!(refs(&ours).stashes.is_empty(), "pop drops it");
    git(&ours, &["stash", "push", "-q"]);
    let s = refs(&ours).stashes;
    run(&ours, &Op::StashDrop(s[0].clone())).unwrap();
    assert!(refs(&ours).stashes.is_empty());
    // A remote branch checked out tracks it.
    assert_eq!(run(&ours, &Op::Track("origin/topic".into())).unwrap(), "Switched to topic");
    let r = refs(&ours);
    assert!(r.local.iter().any(|b| b.name == "topic" && b.current && b.upstream == "origin/topic"));
    assert!(r.remote.is_empty());
    // Another work tree of the repository is listed with its branch.
    git(&ours, &["worktree", "add", "-q", d.join("wt").to_str().unwrap(), "docs"]);
    let r = refs(&ours);
    assert_eq!(r.worktrees, [crate::gitwork::Worktree { path: d.join("wt"), branch: "docs".into() }]);
    assert_eq!(refs(&d.join("wt")).worktrees.iter().map(|w| w.path.clone()).collect::<Vec<_>>(), [ours.clone()]);
    let _ = std::fs::remove_dir_all(&d);
}

/// What git prints for `args` in `d`, trimmed.
fn git_said(d: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(d).args(args).output().unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// The commit menu's changes that need no other side: a commit checked out on its own
/// (refused while a change would be lost), branches and tags made at a commit, renamed
/// and deleted (an unmerged branch only when forced), and the branch reset to a commit,
/// keeping the files or not, the old commit said and uncommitted work kept in the stash.
#[test]
fn git_commits_checked_out_branched_tagged_and_reset() {
    use crate::gitwork::{Op, UNMERGED, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitcommits");
    work_repo(&d);
    let mut hashes = vec![];
    for (n, text) in [("Start", "one\n"), ("Second", "two\n"), ("Third", "three\n")] {
        touch(&d.join("a.txt"), text);
        git(&d, &["add", "-A"]);
        git(&d, &["commit", "-qm", n]);
        hashes.push(git_said(&d, &["rev-parse", "HEAD"]));
    }
    let (first, second, third) = (hashes[0].clone(), hashes[1].clone(), hashes[2].clone());
    let short = |h: &str| h[..7].to_string();
    // Checking out a commit git would overwrite a change for: its words, nothing lost.
    touch(&d.join("a.txt"), "mine\n");
    let e = run(&d, &Op::Checkout(second.clone())).unwrap_err();
    assert!(e.contains("would be overwritten") && e.contains("a.txt"), "{e}");
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "mine\n");
    git(&d, &["checkout", "-q", "--", "a.txt"]);
    assert_eq!(run(&d, &Op::Checkout(second.clone())).unwrap(), format!("Checked out {}, on no branch", short(&second)));
    assert_eq!((git_said(&d, &["rev-parse", "HEAD"]), git_said(&d, &["branch", "--show-current"])), (second.clone(), String::new()));
    run(&d, &Op::Switch("main".into())).unwrap();
    // A branch at a commit, and one switched to.
    assert_eq!(run(&d, &Op::Branch { name: " old ".into(), at: first.clone(), switch: false }).unwrap(), format!("Created old at {}", short(&first)));
    assert_eq!(git_said(&d, &["rev-parse", "old"]), first);
    assert!(run(&d, &Op::Branch { name: "a..b".into(), at: first.clone(), switch: false }).unwrap_err().contains("can't be a branch name"));
    assert_eq!(run(&d, &Op::Branch { name: "fix".into(), at: second.clone(), switch: true }).unwrap(), format!("Created fix at {} and switched to it", short(&second)));
    assert_eq!(git_said(&d, &["branch", "--show-current"]), "fix");
    touch(&d.join("b.txt"), "only on fix\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Only on fix"]);
    run(&d, &Op::Switch("main".into())).unwrap();
    // Tags: a light one, and one with a message.
    assert_eq!(run(&d, &Op::Tag { name: "v1".into(), at: first.clone(), message: String::new() }).unwrap(), format!("Tagged {} as v1", short(&first)));
    run(&d, &Op::Tag { name: "v2".into(), at: third.clone(), message: "The second release\n".into() }).unwrap();
    assert_eq!((git_said(&d, &["cat-file", "-t", "v1"]), git_said(&d, &["cat-file", "-t", "v2"])), ("commit".to_string(), "tag".to_string()));
    assert_eq!(git_said(&d, &["tag", "-l", "--format=%(contents)", "v2"]), "The second release");
    assert!(run(&d, &Op::Tag { name: "v1".into(), at: third.clone(), message: String::new() }).unwrap_err().contains("already exists"));
    assert_eq!(run(&d, &Op::DeleteTag("v1".into())).unwrap(), format!("Deleted the tag v1 · it was at {}", short(&first)));
    assert_eq!(git_said(&d, &["tag"]), "v2");
    // Deleting: a merged branch goes, one with its own commits only when forced.
    assert_eq!(run(&d, &Op::DeleteBranch { name: "old".into(), force: false }).unwrap(), format!("Deleted old · it was at {}", short(&first)));
    assert_eq!(run(&d, &Op::DeleteBranch { name: "fix".into(), force: false }).unwrap_err(), UNMERGED);
    assert!(git_said(&d, &["branch"]).contains("fix"), "kept");
    let tip = git_said(&d, &["rev-parse", "--short", "fix"]);
    assert_eq!(run(&d, &Op::DeleteBranch { name: "fix".into(), force: true }).unwrap(), format!("Deleted fix · it was at {tip}"));
    assert!(run(&d, &Op::DeleteBranch { name: "main".into(), force: true }).is_err(), "not the branch HEAD is on");
    git(&d, &["branch", "topic"]);
    assert_eq!(run(&d, &Op::RenameBranch { from: "topic".into(), to: "feature".into() }).unwrap(), "Renamed topic to feature");
    assert_eq!(git_said(&d, &["branch", "--format=%(refname:short)"]), "feature\nmain");
    // Reset keeping the files: the later commits' change is left unstaged.
    assert_eq!(run(&d, &Op::Reset { to: first.clone(), hard: false }).unwrap(), format!("Reset main to {} · it was at {}", short(&first), short(&third)));
    assert_eq!(git_said(&d, &["rev-parse", "HEAD"]), first);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "three\n");
    assert_eq!(letters(&work_of(&d).unstaged), [("a.txt".into(), 'M')]);
    // And back, discarding: the uncommitted change goes to the stash first.
    let said = run(&d, &Op::Reset { to: third.clone(), hard: true }).unwrap();
    assert_eq!(said, format!("Reset main to {} · it was at {}, your changes kept in the stash", short(&third), short(&first)));
    assert!(work_of(&d).is_empty());
    assert!(git_said(&d, &["stash", "list"]).contains("Before resetting main in Maia"));
    // With nothing uncommitted, nothing is stashed.
    assert_eq!(run(&d, &Op::Reset { to: second.clone(), hard: true }).unwrap(), format!("Reset main to {} · it was at {}", short(&second), short(&third)));
    assert_eq!(git_said(&d, &["stash", "list"]).lines().count(), 1);
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "two\n");
    let _ = std::fs::remove_dir_all(&d);
}

/// Cherry-picking and reverting: a clean pick, one already there (said, and nothing
/// left in progress), one that stops at a conflict and is given up on, then settled
/// and continued; a revert, and one that stops and is skipped.
#[test]
fn git_cherry_pick_and_revert_with_conflicts() {
    use crate::gitsync::{CONFLICTS, Doing, Side, doing};
    use crate::gitwork::{Op, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitpick");
    work_repo(&d);
    touch(&d.join("notes.txt"), "one\ntwo\nthree\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    let start = git_said(&d, &["rev-parse", "HEAD"]);
    git(&d, &["switch", "-q", "-c", "topic"]);
    touch(&d.join("notes.txt"), "one\nTWO from topic\nthree\n");
    git(&d, &["commit", "-qam", "Shout two"]);
    let shout = git_said(&d, &["rev-parse", "HEAD"]);
    touch(&d.join("c.txt"), "see\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Add c"]);
    let add_c = git_said(&d, &["rev-parse", "HEAD"]);
    git(&d, &["switch", "-q", "main"]);
    touch(&d.join("notes.txt"), "one\ntwo on main\nthree\n");
    git(&d, &["commit", "-qam", "Two on main"]);
    let pick = |h: &str| run(&d, &Op::CherryPick { hash: h.into(), merge: false });
    assert_eq!(pick(&add_c).unwrap(), format!("Cherry-picked {} onto main", &add_c[..7]));
    assert_eq!(std::fs::read_to_string(d.join("c.txt")).unwrap(), "see\n");
    assert_eq!(pick(&add_c).unwrap_err(), format!("main already has what {} changes.", &add_c[..7]));
    assert_eq!(doing(&d), None, "nothing left in progress");
    assert_eq!(pick(&start).unwrap_err(), format!("{} is already on main.", &start[..7]));
    // A conflict: given up on, everything as before.
    assert_eq!(pick(&shout).unwrap_err(), CONFLICTS);
    assert_eq!((doing(&d), work_of(&d).doing), (Some(Doing::CherryPick), Some(Doing::CherryPick)));
    assert_eq!(run(&d, &Op::Abort).unwrap(), "Stopped cherry-picking and went back");
    assert_eq!((doing(&d), std::fs::read_to_string(d.join("notes.txt")).unwrap()), (None, "one\ntwo on main\nthree\n".to_string()));
    // Again, settled with theirs and continued: the commit keeps its message.
    assert_eq!(pick(&shout).unwrap_err(), CONFLICTS);
    assert_eq!(run(&d, &Op::Continue).unwrap_err(), "Some files are still in conflict.");
    run(&d, &Op::Settle { path: "notes.txt".into(), at: 0, side: Side::Theirs }).unwrap();
    assert_eq!(run(&d, &Op::Continue).unwrap(), "Finished the cherry-pick");
    assert_eq!((doing(&d), git_said(&d, &["log", "-1", "--format=%s"])), (None, "Shout two".to_string()));
    assert_eq!(std::fs::read_to_string(d.join("notes.txt")).unwrap(), "one\nTWO from topic\nthree\n");
    // A revert: a new commit that undoes one.
    let picked_c = git_said(&d, &["rev-parse", "HEAD~1"]);
    assert_eq!(run(&d, &Op::Revert { hash: picked_c.clone(), merge: false }).unwrap(), format!("Reverted {}", &picked_c[..7]));
    assert!(!d.join("c.txt").exists());
    assert_eq!(git_said(&d, &["log", "-1", "--format=%s"]), "Revert \"Add c\"");
    // One that stops: the line it would take back changed since. Skipped.
    let shouted = git_said(&d, &["rev-parse", "HEAD~1"]);
    touch(&d.join("notes.txt"), "one\ntwo, quietly\nthree\n");
    git(&d, &["commit", "-qam", "Quiet two"]);
    assert_eq!(run(&d, &Op::Revert { hash: shouted, merge: false }).unwrap_err(), CONFLICTS);
    assert_eq!(doing(&d), Some(Doing::Revert));
    assert_eq!(run(&d, &Op::Skip).unwrap(), "Skipped that commit");
    assert_eq!((doing(&d), git_said(&d, &["log", "-1", "--format=%s"])), (None, "Quiet two".to_string()));
    let _ = std::fs::remove_dir_all(&d);
}

/// Lines of a hunk staged on their own, and one of them unstaged again: the index
/// gets exactly the picked lines, the file itself is left alone.
#[test]
fn git_lines_staged_and_unstaged() {
    use crate::gitwork::{Op, file_diff, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitlines");
    work_repo(&d);
    touch(&d.join("f.txt"), &numbered(&[]));
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    let changed = numbered(&[(3, "line three"), (4, "line 4\nnew A\nnew B")]);
    touch(&d.join("f.txt"), &changed);
    let f = work_of(&d).unstaged[0].clone();
    let diff = file_diff(&d, &f, false, "HEAD");
    let h = &diff.hunks[0];
    assert_eq!(h.lines[..7], [" line 1", " line 2", "-line 3", "+line three", " line 4", "+new A", "+new B"]);
    assert!(h.splits());
    let pick = |h: &crate::gitwork::Hunk, at: &[usize]| (0..h.lines.len()).map(|i| at.contains(&i)).collect::<Vec<bool>>();
    assert!(h.lines_patch(&pick(h, &[0, 1]), false).is_none(), "no change picked");
    // The removed line 3 and the first new line.
    let patch = h.lines_patch(&pick(h, &[2, 5]), false).unwrap();
    assert_eq!(run(&d, &Op::Lines { patch, staged: false, n: 2 }).unwrap(), "Staged 2 lines");
    let cached = git_said(&d, &["diff", "--cached"]);
    let hunk = |s: &str| s[s.find("@@").unwrap()..].to_string();
    assert_eq!(hunk(&cached), "@@ -1,7 +1,7 @@\n line 1\n line 2\n-line 3\n line 4\n+new A\n line 5\n line 6\n line 7");
    assert_eq!(std::fs::read_to_string(d.join("f.txt")).unwrap(), changed, "the file itself untouched");
    // Back out: the new line only.
    let w = work_of(&d);
    let staged = file_diff(&d, &w.staged[0], true, "HEAD");
    let h = &staged.hunks[0];
    let at = h.lines.iter().position(|l| l == "+new A").unwrap();
    let patch = h.lines_patch(&pick(h, &[at]), true).unwrap();
    assert_eq!(run(&d, &Op::Lines { patch, staged: true, n: 1 }).unwrap(), "Unstaged 1 line");
    assert_eq!(hunk(&git_said(&d, &["diff", "--cached"])), "@@ -1,6 +1,5 @@\n line 1\n line 2\n-line 3\n line 4\n line 5\n line 6");
    assert_eq!(hunk(&git_said(&d, &["diff"])), "@@ -1,6 +1,9 @@\n line 1\n line 2\n+line three\n line 4\n+new A\n+new B\n line 5\n line 6\n line 7");
    // A file added whole goes only whole.
    touch(&d.join("new.txt"), "a\nb\n");
    run(&d, &Op::Stage(vec!["new.txt".into()])).unwrap();
    let n = work_of(&d).staged.into_iter().find(|c| c.path == "new.txt").unwrap();
    assert!(!file_diff(&d, &n, true, "HEAD").hunks[0].splits());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn git_ignored_files_are_known_and_left_out_when_hidden() {
    use crate::gitstatus::{ignored, inside_ignored, query};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitignored");
    work_repo(&d);
    touch(&d.join(".gitignore"), "node_modules/\n*.log\n");
    for i in 0..300 {
        touch(&d.join(format!("node_modules/p{i}/index.js")), "");
    }
    touch(&d.join("src/app.js"), "");
    touch(&d.join("src/debug.log"), "");
    touch(&d.join("README.md"), "");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    let r = query(&d).expect("a work tree");
    let mut set: Vec<&Path> = r.ignored.iter().map(Path::new).collect();
    set.sort();
    assert_eq!(set, [d.join("node_modules").as_path(), d.join("src/debug.log").as_path()], "an ignored folder is one entry");
    assert!(ignored(Some(&r), &d.join("node_modules"), &d));
    assert!(ignored(Some(&r), &d.join("node_modules/p3/index.js"), &d), "inside an ignored folder, seen from above");
    assert!(ignored(Some(&r), &d.join("src/debug.log"), &d.join("src")));
    assert!(!ignored(Some(&r), &d.join("src/app.js"), &d.join("src")));
    assert!(!ignored(Some(&r), &d.join("node_modules/p3"), &d.join("node_modules")), "inside it, nothing is singled out");
    assert!(inside_ignored(&r, &d.join("node_modules/p3")) && !inside_ignored(&r, &d.join("src")) && !inside_ignored(&r, &d));
    // Hidden: left out of the view, counted as hidden, and back when shown.
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let names = |t: &Tab| t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&t), ["node_modules", "src", "README.md"]);
    t.git_hide = Some(r.ignored.clone());
    t.dirty = true;
    pump(&mut t, &stats);
    assert_eq!(names(&t), ["src", "README.md"]);
    assert_eq!(t.hidden_count, 3, ".git, .gitignore and node_modules");
    t.git_hide = None;
    pump(&mut t, &stats);
    assert_eq!(names(&t).len(), 3);
    let _ = std::fs::remove_dir_all(&d);
}

/// The guard holds for changes too: a repository's fsmonitor never runs, and what would
/// pass through its own filters is refused instead of stored unfiltered.
#[test]
fn a_repositorys_own_config_runs_nothing_when_maia_changes_it() {
    use crate::gitwork::{FILTERED, Op, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("evilwork");
    work_repo(&d);
    touch(&d.join("notes.txt"), "one\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    let mark = |n: &str| format!("touch '{}'", d.join(n).display());
    git(&d, &["config", "core.fsmonitor", &mark("ran-fsmonitor")]);
    touch(&d.join("notes.txt"), "two\n");
    run(&d, &Op::Stage(vec!["notes.txt".into()])).unwrap();
    run(&d, &Op::Commit { summary: "Two".into(), body: String::new(), amend: false }).unwrap();
    git(&d, &["branch", "side"]);
    run(&d, &Op::Switch("side".into())).unwrap();
    assert!(!d.join("ran-fsmonitor").exists(), "the fsmonitor hook ran");
    // Filters of its own: staging through them emptied would store the wrong bytes.
    git(&d, &["config", "filter.x.clean", &format!("sh -c \"{}; cat\"", mark("ran-clean"))]);
    touch(&d.join(".gitattributes"), "*.txt filter=x\n");
    touch(&d.join("notes.txt"), "three\n");
    assert_eq!(run(&d, &Op::Stage(vec!["notes.txt".into()])).unwrap_err(), FILTERED);
    assert_eq!(run(&d, &Op::StageAll).unwrap_err(), FILTERED);
    assert_eq!(run(&d, &Op::Switch("main".into())).unwrap_err(), FILTERED);
    let w = work_of(&d);
    assert!(w.staged.is_empty(), "nothing staged: {w:?}");
    // What only moves the index or names a branch still works.
    run(&d, &Op::Create("other".into())).unwrap();
    assert!(!d.join("ran-clean").exists() && !d.join("ran-fsmonitor").exists(), "the repository's config ran a command");
    let _ = std::fs::remove_dir_all(&d);
}

/// A work tree with changes of every kind for the Changes page: a file changed in two
/// places (two hunks), a long line (for wrapping), a deletion, an untracked file and a
/// staged change.
pub(crate) fn changes_fixture(d: &Path) {
    std::fs::create_dir_all(d).unwrap();
    work_repo(d);
    touch(&d.join("src/lib.rs"), &numbered(&[]));
    touch(&d.join("src/view.rs"), "fn view() {}\n");
    touch(&d.join("README.md"), "# Project\n");
    touch(&d.join("old.txt"), "old\n");
    git(d, &["add", "-A"]);
    git(d, &["commit", "-qm", "Start the project"]);
    touch(&d.join("src/lib.rs"), &numbered(&[(3, "line three"), (36, "line thirty-six")]));
    let long = "A long line in a change keeps going past the edge of the panel, and wrapped it stays readable. ".repeat(4);
    touch(&d.join("README.md"), &format!("# Project\n{long}\n"));
    std::fs::remove_file(d.join("old.txt")).unwrap();
    touch(&d.join("notes.md"), "one\ntwo\nthree\n");
    touch(&d.join("src/view.rs"), "fn view() { draw(); }\n");
    git(d, &["add", "src/view.rs"]);
}

// ---------------------------------------------------------------- developer previews

/// A README as projects write them: every kind of block, emphasis inside links inside
/// lists, references, pictures on disk and on the web, a table with alignments.
pub(crate) const README_MD: &str = r#"# Maia
A file manager that *stays out of the way*, with **bold claims** and ***both***.

[![Build](https://img.example.invalid/ci.svg)](https://ci.example.invalid/maia)

<p align="center"><img src="shot.png" alt="The window"></p>

Install it with `cargo build --release`, or read [the guide](docs/guide.md) and
the [notes][n]. See https://example.invalid/maia. for more, or <mail@example.invalid>.

Setext title
------------

> A quote that goes on
lazily, with a list:
> - one
> - two

3. third
4. fourth
   - nested **strong** item
   - [link in a list](#setext-title)

- [x] done task
- [ ] open task

| Key | Does | Count |
|:----|:----:|------:|
| `Space` | opens the inspector | 1 |
| Ctrl+P \| G | goes | 22 |

```rust
fn main() {
    println!("hi"); // greet
}
```

    indented code

~~gone~~ and snake_case_words and 2 * 3 * 4 stay plain.

***

![logo](logo.png "The logo")

[n]: notes.md "Notes"
"#;

/// A folder with one file of every developer kind, committed by two people, a database
/// and a growing log beside them.
pub(crate) fn dev_fixture(d: &Path) {
    let _ = std::fs::remove_dir_all(d);
    std::fs::create_dir_all(d.join("docs")).unwrap();
    touch(&d.join("README.md"), README_MD);
    std::fs::write(d.join("logo.png"), png_bytes(64, 40, 2)).unwrap();
    std::fs::write(d.join("shot.png"), png_bytes(320, 180, 3)).unwrap();
    touch(&d.join("docs/guide.md"), "# Guide\n");
    touch(&d.join("notes.md"), "# Notes\n");
    touch(&d.join("package.json"), "{\n  // settings files have comments\n  \"name\": \"maia-web\",\n  \"version\": \"0.2.0\",\n  \"private\": true,\n  \"id\": 12345678901234567890,\n  \"scripts\": { \"build\": \"vite build\", \"test\": \"vitest\" },\n  \"files\": [\"dist\", \"README.md\"],\n  \"nothing\": null,\n  \"nested\": { \"deeper\": { \"deepest\": [1, 2.5, -3e2] } },\n}\n");
    touch(&d.join("events.jsonl"), "{\"t\": 1, \"msg\": \"start\"}\n{\"t\": 2, \"msg\": \"tick\", \"tags\": [\"a\", \"b\"]}\n\n{\"t\": 3, \"msg\": \"stop\"}\n");
    touch(&d.join("config.toml"), "# Settings\ntitle = \"Maia \\\"dev\\\"\"\nwhen = 1979-05-27T07:32:00Z\nhex = 0xdead_beef\nsite.name = \"x\"\n\n[server]\nport = 8080\nhosts = [\n  \"alpha\", # first\n  \"beta\",\n]\npath = '''\nC:\\raw\\path\n'''\n\n[[plugin]]\nname = \"one\"\n[[plugin]]\nname = \"two\"\ninline = { a = 1, b.c = true }\n");
    touch(&d.join("ci.yml"), "name: CI\non:\n  push:\n    branches: [main, 'release/*']\ndefaults: &defaults\n  runs-on: ubuntu-latest\n  timeout: 30\njobs:\n  build:\n    <<: *defaults\n    steps:\n      - uses: actions/checkout@v4\n      - name: Test\n        run: |\n          cargo test\n          cargo build\n      - when: 2026-10-16\n        ok: yes\n        none: ~\n");
    touch(&d.join("people.csv"), "id,name,email,joined,score\n1,Ada Lovelace,ada@example.invalid,1843-01-01,99.5\n2,\"Grace \"\"Amazing\"\" Hopper\",grace@example.invalid,1952-05-01,98\n3,\"Line\nbreak\",x@example.invalid,2000-01-01,7\n4,Linus,linus@example.invalid,1991-08-25,\n");
    touch(&d.join("prices.tsv"), "item\tprice\tqty\nkeyboard\t49.90\t2\nmouse\t19.00\t5\n");
    let wide: String = (0..30).map(|c| format!("column {c}")).collect::<Vec<_>>().join(";") + "\n" + &(0..40).map(|r| (0..30).map(|c| format!("{}", r * c)).collect::<Vec<_>>().join(";")).collect::<Vec<_>>().join("\n") + "\n";
    touch(&d.join("wide.csv"), &wide);
    touch(&d.join(".env"), "# Local secrets\nexport API_KEY=sk-live-0123456789\nDATABASE_URL=\"postgres://u:pw@localhost/db\"\nMULTI=\"first line\nsecond line\"\nEMPTY=\nnot a pair here\n");
    touch(&d.join("prod.env"), "TOKEN=abc\n");
    touch(&d.join("app.log"), &(1..=40).map(|i| format!("2026-10-16 10:00:{:02} INFO request {i} served\n", i % 60)).collect::<String>());
    touch(&d.join("broken.json"), "{\"a\": 1,, }\n");
    std::fs::write(d.join("data.db"), b"SQLite format 3\0fake pages follow").unwrap();
    std::fs::write(d.join("other.db"), b"not a database at all, just text\n").unwrap();
    touch(&d.join("src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n");
    if !crate::media::have("git") {
        return;
    }
    let commit = |who: &str, mail: &str, date: &str, msg: &str| {
        for args in [vec!["add", "-A"], vec!["commit", "-qm", msg]] {
            let ok = std::process::Command::new("git")
                .args(["-c", &format!("user.name={who}"), "-c", &format!("user.email={mail}"), "-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main"])
                .args(&args)
                .env("GIT_AUTHOR_DATE", date)
                .env("GIT_COMMITTER_DATE", date)
                .current_dir(d)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "git {args:?}");
        }
    };
    git(d, &["init", "-q"]);
    commit("Ada Lovelace", "ada@example.invalid", "2024-03-01T10:00:00+00:00", "Start the library");
    touch(&d.join("src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n\npub fn two() -> u32 {\n    2\n}\n");
    commit("Grace Hopper", "grace@example.invalid", "2026-10-10T10:00:00+00:00", "Add two");
    // A line changed and not committed.
    touch(&d.join("src/lib.rs"), "pub fn one() -> u32 {\n    1\n}\n\npub fn two() -> u32 {\n    2 // two\n}\n");
}

/// What sqlite3 3.50 printed for the fixture's queries (the schema listing, the first
/// rows with a blob, a NULL, a comma and a line break), and a fake that answers with it.
pub(crate) const SQLITE_LIST: &str = "table,\"odd \"\"name\"\"\"\ntable,people\nview,adults\n";
pub(crate) const SQLITE_ROWS: &str = "id,name,age,photo\n1,Ada,36,\"<12 bytes>\"\n2,\"Grace, R.\",45,NULL\n3,\"Line\nbreak\",28,NULL\n";

pub(crate) fn fake_sqlite(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/sqlite3.log\"\nfor a; do last=\"$a\"; done\ncase \"$1\" in -version) echo '3.50.6 2025-09-22 fake'; exit 0;; esac\ncase \"$last\" in\n*sqlite_master*) printf '%s' '{}';;\n*count*people*) echo 3;;\n*count*adults*) echo 2;;\n*count*) echo 0;;\n*pragma_table_info*) printf 'id\\nname\\nage\\nphoto\\n';;\n*people*|*adults*) printf '%s' '{}';;\n*) exit 1;;\nesac\n",
        SQLITE_LIST.replace('\'', "'\\''"),
        SQLITE_ROWS.replace('\'', "'\\''")
    );
    let p = dir.join("sqlite3");
    std::fs::write(&p, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn md_lines(s: &str) -> Vec<String> {
    crate::inspector::text_lines(s.as_bytes()).unwrap()
}

#[test]
fn markdown_reads_every_block_and_inline_form() {
    use crate::markdown::{Align, Block, CODE, EM, STRIKE, STRONG, plain};
    let d = tmpdir("md");
    dev_fixture(&d);
    let doc = crate::markdown::parse(&md_lines(README_MD), Some(&d));
    let b = &doc.blocks;
    let kinds: Vec<&str> = b.iter().map(|b| match b {
        Block::Heading(..) => "h",
        Block::Para(_) => "p",
        Block::Code { .. } => "code",
        Block::Quote(_) => "quote",
        Block::List { .. } => "list",
        Block::Table { .. } => "table",
        Block::Rule => "rule",
        Block::Images(_) => "img",
        Block::Math(_) => "math",
    }).collect();
    assert_eq!(kinds, ["h", "p", "img", "img", "p", "h", "quote", "list", "list", "table", "code", "code", "p", "rule", "img"], "{b:#?}");
    // Emphasis, strong and both; a heading's text.
    let Block::Heading(1, h) = &b[0] else { panic!() };
    assert_eq!(plain(h), "Maia");
    let Block::Para(p) = &b[1] else { panic!() };
    assert!(p.iter().any(|r| r.text == "stays out of the way" && r.st == EM));
    assert!(p.iter().any(|r| r.text == "bold claims" && r.st == STRONG));
    assert!(p.iter().any(|r| r.text == "both" && r.st == STRONG | EM));
    // A linked badge from the web, a picture inside HTML.
    let Block::Images(badge) = &b[2] else { panic!() };
    assert_eq!(badge.len(), 1);
    assert_eq!(doc.images[badge[0].0 as usize - 1].src, "https://img.example.invalid/ci.svg");
    assert_eq!(doc.links[badge[0].1 as usize - 1].url, "https://ci.example.invalid/maia");
    let Block::Images(shot) = &b[3] else { panic!() };
    let im = &doc.images[shot[0].0 as usize - 1];
    assert_eq!((im.src.as_str(), im.alt.as_str()), ("shot.png", "The window"));
    assert!(im.path.is_some(), "a picture beside the file is found");
    // Code, an inline link to a file, a reference, a bare address less its full stop, an
    // address in brackets.
    let Block::Para(p) = &b[4] else { panic!() };
    assert!(p.iter().any(|r| r.text == "cargo build --release" && r.st == CODE));
    let link = |text: &str| p.iter().find(|r| r.text == text).map(|r| &doc.links[r.link as usize - 1]).unwrap_or_else(|| panic!("{text}: {p:?}"));
    assert_eq!(link("the guide").url, "docs/guide.md");
    assert_eq!(link("the guide").path.as_deref(), Some(d.join("docs/guide.md").as_path()));
    assert_eq!(link("notes").url, "notes.md");
    assert_eq!(link("https://example.invalid/maia").url, "https://example.invalid/maia");
    assert_eq!(link("mail@example.invalid").url, "mailto:mail@example.invalid");
    // Setext heading; a quote with a lazy line and a list in it.
    assert!(matches!(&b[5], Block::Heading(2, h) if plain(h) == "Setext title"));
    let Block::Quote(q) = &b[6] else { panic!() };
    assert!(matches!(&q[0], Block::Para(p) if plain(p) == "A quote that goes on lazily, with a list:"), "{q:?}");
    assert!(matches!(&q[1], Block::List { start: None, items } if items.len() == 2));
    // An ordered list from 3 with a nested list of strong text and an anchor link.
    let Block::List { start: Some(3), items } = &b[7] else { panic!("{:?}", b[7]) };
    assert_eq!(items.len(), 2);
    let Block::List { items: nested, .. } = &items[1].1[1] else { panic!("{:?}", items[1].1) };
    assert!(matches!(&nested[0].1[0], Block::Para(p) if p.iter().any(|r| r.text == "strong" && r.st == STRONG)));
    assert!(matches!(&nested[1].1[0], Block::Para(p) if p.iter().any(|r| doc.links.get(r.link as usize - 1).is_some_and(|l| l.url == "#setext-title"))));
    // Tasks.
    let Block::List { items, .. } = &b[8] else { panic!() };
    assert_eq!(items.iter().map(|i| i.0).collect::<Vec<_>>(), [Some(true), Some(false)]);
    // A table: alignments, code in a cell, an escaped bar.
    let Block::Table { aligns, head, rows } = &b[9] else { panic!() };
    assert_eq!(aligns, &[Align::Left, Align::Center, Align::Right]);
    assert_eq!(head.iter().map(|c| plain(c)).collect::<Vec<_>>(), ["Key", "Does", "Count"]);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0][0][0].st, CODE);
    assert_eq!(plain(&rows[1][0]), "Ctrl+P | G");
    // Fenced code with its language's colours; indented code.
    let Block::Code { lang, lines, spans } = &b[10] else { panic!() };
    assert_eq!((lang.as_str(), lines.len()), ("rust", 3));
    assert!(spans.as_ref().is_some_and(|s| s.iter().flatten().count() >= 3));
    assert!(matches!(&b[11], Block::Code { lines, .. } if lines == &["indented code"]));
    // Strikethrough; underscores inside words and lone stars stay text.
    let Block::Para(p) = &b[12] else { panic!() };
    assert!(p.iter().any(|r| r.text == "gone" && r.st == STRIKE));
    assert!(plain(p).contains("snake_case_words") && plain(p).contains("2 * 3 * 4"), "{p:?}");
    // A picture on its own; the link definition is no text.
    let Block::Images(logo) = &b[14] else { panic!() };
    assert!(doc.images[logo[0].0 as usize - 1].path.is_some());
    assert!(!doc.blocks.iter().any(|b| matches!(b, Block::Para(p) if plain(p).contains("[n]"))));
    assert_eq!(crate::markdown::slug(&[crate::markdown::Run { text: "Setext title".into(), ..Default::default() }]), "setext-title");
}

#[test]
fn markdown_survives_broken_and_hostile_input() {
    // Unclosed everything, deep nesting, private-use characters that look like Maia's
    // own picture marker: text comes out, nothing panics.
    let nasty = format!("{}\n\n{}\n\n**open *em `code [link](x \"t\n\n\u{F8FF}0 \u{F8FF}99 ![a](\n\n|a|b|\n|-|\n|c|\n\n```\nnever closed", ">".repeat(200), "- ".repeat(100));
    let doc = crate::markdown::parse(&md_lines(&nasty), None);
    assert!(!doc.blocks.is_empty());
    let big = "a line of a long document\n".repeat(40_000);
    let doc = crate::markdown::parse(&md_lines(&big), None);
    assert!(doc.cut, "past the cap: the rest is left to Source");
}

#[test]
fn json_tree_keeps_order_exact_numbers_and_comments() {
    use crate::datatree::{K, V, flatten, parse_json};
    let d = tmpdir("json");
    dev_fixture(&d);
    let v = parse_json(&std::fs::read_to_string(d.join("package.json")).unwrap()).unwrap();
    let V::Map(m) = &v else { panic!() };
    assert_eq!(m.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["name", "version", "private", "id", "scripts", "files", "nothing", "nested"], "file order, not sorted");
    assert_eq!(m[3].1, V::Num("12345678901234567890".into()), "a big number stays exact");
    let mut t = flatten(&v, None);
    let deepest = t.nodes.iter().position(|n| n.key == "deepest").unwrap();
    assert_eq!(t.path(deepest), ".nested.deeper.deepest");
    assert_eq!(t.path(deepest + 2), ".nested.deeper.deepest[1]");
    assert_eq!(t.nodes[deepest].count, 3);
    assert_eq!(t.nodes[deepest + 3].text, "-3e2");
    assert_eq!(t.nodes[0].kind, K::Str);
    assert_eq!(t.nodes[0].text, "\"maia-web\"");
    // Small: open whole. Folding hides a subtree; keys walk and fold it.
    let all = t.rows().len();
    assert_eq!(all, t.nodes.len());
    let scripts = t.nodes.iter().position(|n| n.key == "scripts").unwrap();
    t.sel = scripts;
    t.focus = true;
    t.key(egui::Key::ArrowLeft);
    assert_eq!(t.rows().len(), all - 2, "folded");
    t.key(egui::Key::ArrowRight);
    t.key(egui::Key::ArrowRight);
    assert_eq!(t.path(t.sel), ".scripts.build", "opened, then stepped in");
    t.key(egui::Key::ArrowLeft);
    assert_eq!(t.sel, scripts, "left steps out to the parent");
    assert!(parse_json("{\"a\": 1,, }").unwrap_err().contains("line 1"));
    // JSON Lines: one record per line, blank lines skipped.
    let v = crate::datatree::parse_jsonl(&std::fs::read_to_string(d.join("events.jsonl")).unwrap()).unwrap();
    assert!(matches!(&v, V::List(l) if l.len() == 3));
    // A huge array stops at the cap with what fits.
    let huge = format!("[{}]", (0..300_000).map(|i| i.to_string()).collect::<Vec<_>>().join(","));
    let v = parse_json(&huge).unwrap();
    let t = flatten(&v, None);
    assert!(t.nodes.len() < crate::datatree::MAX_NODES && t.nodes.len() > 150_000, "{}", t.nodes.len());
    // Escapes and a surrogate pair.
    assert_eq!(parse_json(r#""a\"\n\u00e9\ud83d\ude00""#).unwrap(), V::Str("a\"\né😀".into()));
}

#[test]
fn toml_tree_reads_tables_arrays_of_tables_and_every_value() {
    use crate::datatree::{V, parse_toml};
    let d = tmpdir("toml");
    dev_fixture(&d);
    let v = parse_toml(&std::fs::read_to_string(d.join("config.toml")).unwrap()).unwrap();
    let V::Map(m) = &v else { panic!() };
    let get = |m: &Vec<(String, V)>, k: &str| m.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap_or_else(|| panic!("{k}"));
    assert_eq!(m.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["title", "when", "hex", "site", "server", "plugin"]);
    assert_eq!(get(m, "title"), V::Str("Maia \"dev\"".into()));
    assert_eq!(get(m, "when"), V::Date("1979-05-27T07:32:00Z".into()));
    assert_eq!(get(m, "hex"), V::Num("0xdead_beef".into()));
    let V::Map(server) = get(m, "server") else { panic!() };
    assert_eq!(get(&server, "port"), V::Num("8080".into()));
    assert_eq!(get(&server, "hosts"), V::List(vec![V::Str("alpha".into()), V::Str("beta".into())]));
    assert_eq!(get(&server, "path"), V::Str("C:\\raw\\path\n".into()));
    let V::List(plugins) = get(m, "plugin") else { panic!() };
    assert_eq!(plugins.len(), 2);
    let V::Map(two) = &plugins[1] else { panic!() };
    let V::Map(inline) = get(two, "inline") else { panic!() };
    assert_eq!(get(&inline, "a"), V::Num("1".into()));
    assert!(matches!(get(&inline, "b"), V::Map(c) if c[0] == ("c".into(), V::Bool(true))));
    // The project's own manifest and lock file.
    let cargo = parse_toml(include_str!("../Cargo.toml")).unwrap();
    let V::Map(c) = &cargo else { panic!() };
    assert_eq!(c[0].0, "package", "tables in the order written");
    let lock = parse_toml(include_str!("../Cargo.lock")).unwrap();
    assert!(matches!(&lock, V::Map(l) if l.iter().any(|(k, v)| k == "package" && matches!(v, V::List(p) if p.len() > 50))));
    // Mistakes are reported with their line.
    assert!(parse_toml("a = 1\na = 2\n").unwrap_err().contains("line 2"));
    assert!(parse_toml("[x]\nk = \n").is_err());
    assert!(parse_toml("s = \"open\n").is_err());
}

#[test]
fn yaml_tree_follows_anchors_block_scalars_and_documents() {
    use crate::datatree::{V, parse_yaml};
    let d = tmpdir("yaml");
    dev_fixture(&d);
    let v = parse_yaml(&std::fs::read_to_string(d.join("ci.yml")).unwrap()).unwrap();
    let V::Map(m) = &v else { panic!() };
    let get = |m: &Vec<(String, V)>, k: &str| m.iter().find(|x| x.0 == k).map(|x| x.1.clone()).unwrap_or_else(|| panic!("{k} in {m:?}"));
    assert_eq!(m.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["name", "on", "defaults", "jobs"]);
    let V::Map(on) = get(m, "on") else { panic!() };
    let V::Map(push) = get(&on, "push") else { panic!() };
    assert_eq!(get(&push, "branches"), V::List(vec![V::Str("main".into()), V::Str("release/*".into())]));
    let V::Map(jobs) = get(m, "jobs") else { panic!() };
    let V::Map(build) = get(&jobs, "build") else { panic!() };
    // The alias shows what the anchor names.
    assert!(matches!(get(&build, "<<"), V::Map(x) if x.len() == 2 && x[1] == ("timeout".into(), V::Num("30".into()))));
    let V::List(steps) = get(&build, "steps") else { panic!() };
    let V::Map(test) = &steps[1] else { panic!() };
    assert_eq!(get(test, "run"), V::Str("cargo test\ncargo build\n".into()));
    let V::Map(last) = &steps[2] else { panic!() };
    assert_eq!(get(last, "when"), V::Date("2026-10-16".into()));
    assert_eq!(get(last, "ok"), V::Str("yes".into()), "YAML 1.2: yes is a word");
    assert_eq!(get(last, "none"), V::Null);
    // Several documents are a list of them; a bad file says where.
    assert!(matches!(parse_yaml("a: 1\n---\nb: 2\n").unwrap(), V::List(l) if l.len() == 2));
    assert!(parse_yaml("a: [1, 2\nb: 3\n").is_err());
}

#[test]
fn csv_delimiters_quotes_headers_and_caps() {
    use crate::table::{Table, detect, parse};
    let d = tmpdir("csv");
    dev_fixture(&d);
    let text = std::fs::read_to_string(d.join("people.csv")).unwrap();
    assert_eq!(detect(&text, "people.csv"), b',');
    let t = Table::from_text(&text, "people.csv", false);
    assert_eq!(t.header.as_deref(), Some(&["id", "name", "email", "joined", "score"].map(String::from)[..]));
    assert_eq!(t.rows.len(), 4);
    assert_eq!(t.rows[1][1], "Grace \"Amazing\" Hopper", "doubled quotes");
    assert_eq!(t.rows[2][1], "Line\nbreak", "a line break inside quotes");
    assert_eq!(t.numeric, [true, false, false, false, true], "numbers right-aligned; an empty cell does not count");
    assert_eq!(t.note, None);
    assert_eq!(detect(&std::fs::read_to_string(d.join("wide.csv")).unwrap(), "wide.csv"), b';');
    assert_eq!(detect("a|b|c\n1|2|3\n", "x.txt"), b'|');
    assert_eq!(detect("a,b\n1,2\n", "prices.tsv"), b'\t', "the extension decides for .tsv");
    let tsv = Table::from_text(&std::fs::read_to_string(d.join("prices.tsv")).unwrap(), "prices.tsv", false);
    assert_eq!(tsv.cols, 3);
    // A comma inside a semicolon file is not a delimiter.
    assert_eq!(detect("name;city\n\"Doe, J\";Paris\nRoe;Rome\n", "x.csv"), b';');
    // No header when the first row is data; CRLF; a file cut off says so.
    let t = Table::from_text("1,2\r\n3,4\r\n", "n.csv", true);
    assert!(t.header.is_none() && t.rows.len() == 2 && t.rows[1] == ["3", "4"]);
    assert!(t.note.is_some());
    let (rows, more) = parse(&"x\n".repeat(50), b',', 10);
    assert!(rows.len() == 10 && more);
}

#[test]
fn sqlite_lists_tables_and_rows_read_only() {
    use crate::sqlite::{parse_list, parse_version, quote_ident, rows_query, uri};
    let items = parse_list(crate::tests::SQLITE_LIST);
    assert_eq!(items.iter().map(|i| (i.name.as_str(), i.view)).collect::<Vec<_>>(), [("odd \"name\"", false), ("people", false), ("adults", true)]);
    assert_eq!(quote_ident("odd \"name\""), "\"odd \"\"name\"\"\"");
    assert!(rows_query("people", &["photo".into()]).contains("typeof(\"photo\") = 'blob'"));
    assert_eq!(uri(Path::new("/a b/c?d#e.db")), "file:/a%20b/c%3Fd%23e.db?mode=ro&immutable=1");
    assert_eq!(parse_version("3.50.6 2025-09-22 18:50:24 9cc55437 (64-bit)"), Some((3, 50)));
    assert!(crate::sqlite::is_db_header(b"SQLite format 3\0..."));
    // Through the fake: the arguments say read-only, immutable and safe.
    let d = tmpdir("sqlite");
    let fake = d.join("fake");
    fake_sqlite(&fake);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
    let tools = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let db = d.join("x.db");
    std::fs::write(&db, b"SQLite format 3\0").unwrap();
    let (items, safe) = crate::sqlite::tables(&tools, &db).unwrap();
    assert!(safe && items.len() == 3);
    let t = crate::sqlite::read_rows(&tools, &db, "people").unwrap();
    assert_eq!(t.header.as_ref().map(|h| h.len()), Some(4));
    assert_eq!(t.rows.len(), 3);
    assert_eq!(t.rows[0][3], "<12 bytes>");
    assert_eq!(t.rows[2][1], "Line\nbreak");
    let log = std::fs::read_to_string(fake.join("sqlite3.log")).unwrap();
    for l in log.lines().filter(|l| !l.contains("-version")) {
        assert!(l.contains("-readonly") && l.contains("-safe") && l.contains("immutable=1") && l.contains("-init /dev/null"), "{l}");
    }
    // The real program, when this machine has it: a database made here.
    if crate::media::have("sqlite3") {
        let real = d.join("real");
        std::fs::create_dir_all(&real).unwrap();
        let bin = std::env::split_paths(&std::env::var_os("PATH").unwrap()).map(|p| p.join("sqlite3")).find(|p| p.is_file()).unwrap();
        std::os::unix::fs::symlink(&bin, real.join("sqlite3")).unwrap();
        let db = d.join("real.db");
        let ok = std::process::Command::new(&bin).arg(&db).arg("CREATE TABLE people(id INTEGER PRIMARY KEY, name TEXT, photo BLOB); INSERT INTO people VALUES(1,'Ada',x'00ff00ff'),(2,'Grace, R.',NULL); CREATE VIEW named AS SELECT name FROM people;").status().is_ok_and(|s| s.success());
        assert!(ok);
        let before = std::fs::read(&db).unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(real.clone()));
        let tools = crate::drives::Tools::current().unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        let (items, _) = crate::sqlite::tables(&tools, &db).unwrap();
        assert_eq!(items.iter().map(|i| i.name.as_str()).collect::<Vec<_>>(), ["people", "named"]);
        let t = crate::sqlite::read_rows(&tools, &db, "people").unwrap();
        assert_eq!(t.rows, [["1", "Ada", "<4 bytes>"], ["2", "Grace, R.", "NULL"]]);
        assert_eq!(std::fs::read(&db).unwrap(), before, "the file is unchanged");
        let names: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with("real.db")).collect();
        assert_eq!(names, ["real.db"], "no journal or lock files beside it");
    } else {
        eprintln!("sqlite3 not installed: the real database was not tried");
    }
}

#[test]
fn env_values_are_masked_keys_are_not() {
    use crate::envfile::{Line, MASK, classify, is_env_name, shown};
    for (n, yes) in [(".env", true), (".env.local", true), ("prod.env", true), ("env", false), (".envrc", false), ("my.environment", false)] {
        assert_eq!(is_env_name(n), yes, "{n}");
    }
    let d = tmpdir("env");
    dev_fixture(&d);
    let lines = md_lines(&std::fs::read_to_string(d.join(".env")).unwrap());
    let k = classify(&lines);
    assert_eq!(k[0], Line::Comment);
    let masked: Vec<String> = lines.iter().zip(&k).map(|(l, &k)| shown(l, k, false)).collect();
    assert_eq!(masked[1], format!("export API_KEY={MASK}"));
    assert_eq!(masked[2], format!("DATABASE_URL={MASK}"));
    assert_eq!(masked[3], format!("MULTI={MASK}"));
    assert_eq!(masked[4], MASK, "the rest of a quoted value over lines");
    assert_eq!(masked[5], "EMPTY=", "nothing to hide");
    assert_eq!(masked[6], MASK, "a line that is no pair is hidden whole");
    let all = masked.join("\n");
    for secret in ["sk-live", "pw@localhost", "second line", "not a pair"] {
        assert!(!all.contains(secret), "{secret} leaked");
    }
    assert_eq!(shown(&lines[1], k[1], true), lines[1]);
}

#[test]
fn followed_file_appends_rotates_and_stops() {
    use std::io::Write;
    let d = tmpdir("follow");
    let p = d.join("app.log");
    std::fs::write(&p, "one\ntwo\n").unwrap();
    let ctx = egui::Context::default();
    let tail = crate::tail::Tail::start(p.clone(), Some(8), ctx).unwrap();
    let wait = |what: &str| -> (bool, Vec<u8>) {
        let t0 = Instant::now();
        loop {
            if let Some(got) = tail.take() {
                return got;
            }
            assert!(t0.elapsed() < Duration::from_secs(5), "{what}");
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"three\nfou").unwrap();
    let (reset, data) = wait("appended lines");
    assert!(!reset);
    assert_eq!(data, b"three\nfou");
    std::fs::OpenOptions::new().append(true).open(&p).unwrap().write_all(b"r\n").unwrap();
    assert_eq!(wait("the rest of the line").1, b"r\n");
    // Truncated: read again from the start.
    std::fs::write(&p, "fresh\n").unwrap();
    let (reset, data) = wait("truncation");
    assert!(reset);
    assert_eq!(data, b"fresh\n");
    // Rotated: a new file under the name.
    std::fs::rename(&p, d.join("app.log.1")).unwrap();
    std::fs::write(&p, "rotated and longer than before\n").unwrap();
    let (reset, data) = wait("rotation");
    assert!(reset && data.ends_with(b"rotated and longer than before\n"));
    // Dropped: the worker ends (its thread finishes; nothing more is read). Only threads
    // by its name are counted: tests running beside this one start and end their own.
    let threads = || std::fs::read_dir("/proc/self/task").map(|r| r.flatten().filter(|t| std::fs::read_to_string(t.path().join("comm")).is_ok_and(|c| c.trim() == "follow")).count()).unwrap_or(0);
    let before = threads();
    drop(tail);
    let t0 = Instant::now();
    while threads() >= before && t0.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(threads() < before, "the follow worker ended");
}

#[test]
fn blame_reads_authors_ages_and_uncommitted_lines() {
    if !crate::media::have("git") {
        eprintln!("git not installed: skipped");
        return;
    }
    let d = tmpdir("blame");
    dev_fixture(&d);
    let b = crate::blame::run(&d, &d.join("src/lib.rs")).unwrap();
    assert_eq!(b.lines.len(), 7);
    let who: Vec<&str> = b.lines.iter().map(|&k| b.commits[k as usize].author.as_str()).collect();
    assert_eq!(&who[..3], ["Ada Lovelace"; 3]);
    assert_eq!(who[4], "Grace Hopper");
    let six = &b.commits[b.lines[5] as usize];
    assert!(!six.committed(), "the edited line is not committed");
    let first = &b.commits[b.lines[0] as usize];
    assert_eq!(first.summary, "Start the library");
    assert_eq!(first.time, 1709287200);
    assert_eq!(crate::blame::short_ago(first.time, first.time + 3 * 86_400), "3d");
    assert_eq!(crate::blame::short_ago(first.time, first.time + 400 * 86_400), "1y");
    assert_eq!(crate::blame::short_author("Grace Hopper"), "Grace");
    // Outside the repository, or not tracked: no blame.
    touch(&d.join("new.txt"), "x\n");
    assert!(crate::blame::run(&d, &d.join("new.txt")).is_err());
}

/// Copy As Code Blocks on real files: each under its path from the project's top in a
/// fence its text can't close, a folder giving the files git keeps (not hidden or
/// ignored ones), a big file cut at a line's end, a binary one left out and said so.
#[test]
fn share_code_blocks() {
    use crate::share::{Blocks, FILE_CAP, code_blocks};
    let d = tmpdir("share-blocks");
    git(&d, &["init", "-q"]);
    touch(&d.join(".gitignore"), "target/\n*.log\n");
    touch(&d.join("src/main.rs"), "fn main() {}\n");
    touch(&d.join("src/lib.rs"), "pub fn f() {}");
    touch(&d.join("src/.hidden.rs"), "secret\n");
    touch(&d.join("src/run.log"), "noise\n");
    touch(&d.join("target/out.rs"), "built\n");
    touch(&d.join("README.md"), "Use it like this:\n\n```sh\ncargo run\n```\n");
    touch(&d.join("Makefile"), "all:\n\ttrue\n");
    std::fs::write(d.join("logo.png"), b"\x89PNG\x00\x01").unwrap();
    let big: String = (0..20_000).map(|i| format!("line {i}\n")).collect();
    touch(&d.join("big.txt"), &big);

    let b = code_blocks(&[d.join("src"), d.join("README.md"), d.join("Makefile")], &d);
    assert_eq!(b.text, "src/lib.rs\n```rust\npub fn f() {}\n```\n\nsrc/main.rs\n```rust\nfn main() {}\n```\n\nREADME.md\n````markdown\nUse it like this:\n\n```sh\ncargo run\n```\n````\n\nMakefile\n```makefile\nall:\n\ttrue\n```\n");
    assert_eq!((b.files, b.cut, b.skipped.len()), (4, 0, 0));
    assert_eq!(b.note(), "Copied 4 files as code blocks");

    let b = code_blocks(&[d.join("big.txt"), d.join("logo.png")], &d);
    assert_eq!(b.skipped, vec![("logo.png".to_string(), "binary")]);
    assert_eq!((b.files, b.cut), (1, 1));
    let first = b.text.lines().next().unwrap();
    assert!(first.starts_with("big.txt (the first 100 KB of "), "{first}");
    let body: Vec<&str> = b.text.lines().skip(2).collect();
    assert_eq!(body.last(), Some(&"```"), "the fence is closed");
    assert!(body[..body.len() - 1].iter().all(|l| l.starts_with("line ")), "cut at a line's end");
    assert!(b.text.len() <= FILE_CAP + 200);
    assert_eq!(b.note(), "Copied 1 file as code blocks, 1 file cut at 100 KB; left out logo.png (binary)");
    let none = code_blocks(&[d.join("logo.png")], &d);
    assert_eq!(none, Blocks { text: String::new(), files: 0, cut: 0, skipped: vec![("logo.png".into(), "binary")] });
    assert_eq!(crate::share::fence_lang("app.tsx"), "tsx");
    assert_eq!(crate::share::fence_lang("Dockerfile"), "dockerfile");
    assert_eq!(crate::share::fence_lang("notes"), "");
    let _ = std::fs::remove_dir_all(&d);
}

/// Remote URLs as git prints them, and the website and link each one gives.
#[test]
fn share_web_links_from_remotes() {
    use crate::share::{Forge, file_url, web_base};
    let ok = |u: &str| web_base(u).unwrap_or_else(|e| panic!("{u}: {e}"));
    assert_eq!(ok("git@github.com:acme/tidepool.git"), ("https://github.com/acme/tidepool".into(), Forge::GitHub));
    assert_eq!(ok("https://github.com/acme/tidepool"), ("https://github.com/acme/tidepool".into(), Forge::GitHub));
    assert_eq!(ok("https://user:token@github.com/acme/tidepool.git/"), ("https://github.com/acme/tidepool".into(), Forge::GitHub));
    assert_eq!(ok("ssh://git@github.com:22/acme/tidepool.git"), ("https://github.com/acme/tidepool".into(), Forge::GitHub));
    assert_eq!(ok("git@github.com-work:acme/tidepool.git"), ("https://github.com/acme/tidepool".into(), Forge::GitHub), "an ssh alias for a second account");
    assert_eq!(ok("git@gitlab.com:group/sub/proj.git"), ("https://gitlab.com/group/sub/proj".into(), Forge::GitLab));
    assert_eq!(ok("https://gitlab.example.com:8443/team/proj"), ("https://gitlab.example.com:8443/team/proj".into(), Forge::GitLab));
    assert_eq!(ok("https://codeberg.org/ana/notes.git"), ("https://codeberg.org/ana/notes".into(), Forge::Gitea));
    assert_eq!(ok("git@bitbucket.org:team/repo.git"), ("https://bitbucket.org/team/repo".into(), Forge::Bitbucket));
    assert_eq!(ok("https://git.sr.ht/~ana/notes"), ("https://git.sr.ht/~ana/notes".into(), Forge::SourceHut));
    assert!(web_base("/srv/git/notes.git").is_err());
    assert!(web_base("file:///srv/git/notes.git").is_err());
    assert_eq!(web_base("git@example.com:x/y.git").unwrap_err(), "No web page is known for example.com");

    let sha = "0123456789abcdef0123456789abcdef01234567";
    let gh = "https://github.com/acme/tidepool";
    assert_eq!(file_url(gh, Forge::GitHub, sha, "src/store.rs", false, Some(18)), format!("{gh}/blob/{sha}/src/store.rs#L18"));
    assert_eq!(file_url(gh, Forge::GitHub, sha, "src", true, Some(18)), format!("{gh}/tree/{sha}/src"), "no line on a folder");
    assert_eq!(file_url(gh, Forge::GitHub, sha, "", true, None), format!("{gh}/tree/{sha}"));
    assert_eq!(file_url(gh, Forge::GitHub, sha, "docs/Field notes #2.md", false, None), format!("{gh}/blob/{sha}/docs/Field%20notes%20%232.md"));
    assert_eq!(file_url("https://gitlab.com/g/p", Forge::GitLab, sha, "a.rs", false, Some(3)), format!("https://gitlab.com/g/p/-/blob/{sha}/a.rs#L3"));
    assert_eq!(file_url("https://codeberg.org/a/n", Forge::Gitea, sha, "a.rs", false, Some(3)), format!("https://codeberg.org/a/n/src/commit/{sha}/a.rs#L3"));
    assert_eq!(file_url("https://bitbucket.org/t/r", Forge::Bitbucket, sha, "a.rs", false, Some(3)), format!("https://bitbucket.org/t/r/src/{sha}/a.rs#lines-3"));
    assert_eq!(file_url("https://git.sr.ht/~a/n", Forge::SourceHut, sha, "a.rs", false, Some(3)), format!("https://git.sr.ht/~a/n/tree/{sha}/item/a.rs#L3"));
}

/// Copy Web Link in a real repository: the commit checked out, the line a search
/// showed, and a caution while the commit isn't pushed or a file isn't committed.
#[test]
fn share_web_links_in_a_repository() {
    let d = tmpdir("share-links");
    work_repo(&d);
    touch(&d.join("src/store.rs"), "a\nb\n");
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    let sha = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&d).args(["rev-parse", "HEAD"]).output().unwrap().stdout).unwrap().trim().to_string();
    let store = d.join("src/store.rs");
    assert_eq!(crate::share::web_links(&[(store.clone(), false)], None).unwrap_err(), "The repository has no remote");
    git(&d, &["remote", "add", "origin", "git@github.com:acme/tidepool.git"]);
    let base = format!("https://github.com/acme/tidepool/blob/{sha}");
    let (links, caution) = crate::share::web_links(&[(store.clone(), false)], Some((store.clone(), 2))).unwrap();
    assert_eq!(links, format!("{base}/src/store.rs#L2"));
    assert_eq!(caution.as_deref(), Some("this commit isn't pushed, so the link won't open until it is"));
    // Pushed: what the remote has is the commit.
    git(&d, &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let (links, caution) = crate::share::web_links(&[(store.clone(), false), (d.join("src"), true), (d.clone(), true)], None).unwrap();
    assert_eq!(links, format!("{base}/src/store.rs\nhttps://github.com/acme/tidepool/tree/{sha}/src\nhttps://github.com/acme/tidepool/tree/{sha}"));
    assert_eq!(caution, None);
    touch(&d.join("src/new.rs"), "x\n");
    let (_, caution) = crate::share::web_links(&[(d.join("src/new.rs"), false)], None).unwrap();
    assert_eq!(caution.as_deref(), Some("src/new.rs isn't committed, so its link won't open yet"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Add To .gitignore: lines git reads as the exact names (checked with git itself),
/// added once, after a last line that had no line break, and a word about files git
/// still tracks.
#[test]
fn share_gitignore_lines() {
    use crate::share::{add_ignores, ignore_ext_line, ignore_line, ignore_note, shared_ext};
    assert_eq!(ignore_line("build", true), "/build/");
    assert_eq!(ignore_line("notes [draft]*.txt", false), "/notes \\[draft]\\*.txt");
    assert_eq!(ignore_line("trailing ", false), "/trailing\\ ");
    assert_eq!(ignore_ext_line("log"), "*.log");
    let d = tmpdir("share-ignore");
    work_repo(&d);
    touch(&d.join(".gitignore"), "target/");
    touch(&d.join("notes [draft]*.txt"), "x\n");
    touch(&d.join("notes d.txt"), "x\n");
    touch(&d.join("tracked.env"), "A=1\n");
    touch(&d.join("logs/a.log"), "x\n");
    git(&d, &["add", "tracked.env"]);
    git(&d, &["commit", "-qm", "Start"]);
    let lines = vec![ignore_line("notes [draft]*.txt", false), ignore_line("logs", true), ignore_line("tracked.env", false)];
    let paths = vec![d.join("notes [draft]*.txt"), d.join("logs"), d.join("tracked.env")];
    let (added, tracked) = add_ignores(&d, &lines, &paths).unwrap();
    assert_eq!((added, tracked.clone()), (3, vec!["tracked.env".to_string()]));
    assert_eq!(std::fs::read_to_string(d.join(".gitignore")).unwrap(), "target/\n/notes \\[draft]\\*.txt\n/logs/\n/tracked.env\n");
    assert_eq!(ignore_note(&lines, added, &tracked), "Added 3 lines to .gitignore; git still tracks tracked.env until it's removed with git rm --cached");
    let ignored = |p: &str| std::process::Command::new("git").arg("-C").arg(&d).args(["check-ignore", "-q", "--no-index", p]).status().unwrap().success();
    assert!(ignored("notes [draft]*.txt") && ignored("logs/a.log"));
    assert!(!ignored("notes d.txt"), "the brackets and star are the name's own, not a pattern");
    assert_eq!(add_ignores(&d, &lines[..1], &paths[..1]).unwrap(), (0, vec![]), "already there");
    assert_eq!(ignore_note(&lines[..1], 0, &[]), ".gitignore already has /notes \\[draft]\\*.txt");
    let p = |s: &str| PathBuf::from(s);
    assert_eq!(shared_ext([(p("/a/x.log").as_path(), false), (p("/a/y.log").as_path(), false)].into_iter()), Some("log".into()));
    assert_eq!(shared_ext([(p("/a/x.log").as_path(), false), (p("/a/y.txt").as_path(), false)].into_iter()), None);
    assert_eq!(shared_ext([(p("/a/.env").as_path(), false)].into_iter()), None, "a dotfile has no extension");
    assert_eq!(shared_ext([(p("/a/x.log").as_path(), true)].into_iter()), None);
    let _ = std::fs::remove_dir_all(&d);
}

/// Compare Files on real files with git's diff: what changed from the first file to the
/// second, counted; the same files said to be the same; binary files and a missing one
/// refused with a reason; the title naming the folders when the names match.
#[test]
fn compare_two_files() {
    use crate::filediff::{Compare, diff, parse};
    let d = tmpdir("compare-files");
    touch(&d.join("a/config.toml"), "port = 8080\nworkers = 4\nlog = \"info\"\n");
    touch(&d.join("b/config.toml"), "port = 9090\nworkers = 4\nlog = \"info\"\ncache = true\n");
    touch(&d.join("copy.toml"), "port = 8080\nworkers = 4\nlog = \"info\"\n");
    std::fs::write(d.join("x.bin"), b"\x00\x01\x02").unwrap();
    std::fs::write(d.join("y.bin"), b"\x00\x01\x03").unwrap();
    let r = diff(&d.join("a/config.toml"), &d.join("b/config.toml")).unwrap();
    assert_eq!(r.lines, ["@@ -1,3 +1,4 @@", "-port = 8080", "+port = 9090", " workers = 4", " log = \"info\"", "+cache = true"]);
    assert_eq!((r.added, r.removed, r.truncated), (2, 1, false));
    assert_eq!(diff(&d.join("a/config.toml"), &d.join("copy.toml")).unwrap(), Default::default(), "the same");
    assert_eq!(diff(&d.join("x.bin"), &d.join("y.bin")).unwrap_err(), "They're binary files, and they differ");
    assert!(diff(&d.join("a/config.toml"), &d.join("gone.toml")).is_err());
    // git's header (the file names) is left out; a long diff stops with a line saying so.
    let long: String = format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1,60000 @@\n{}", "+x\n".repeat(60_000));
    let p = parse(&long).unwrap();
    assert!(p.truncated && p.lines.len() == 50_001 && p.lines[0].starts_with("@@"));
    assert_eq!(p.lines.last().map(String::as_str), Some("… the diff goes on past 50,000 lines"));
    let ctx = egui::Context::default();
    assert_eq!(Compare::start(d.join("a/config.toml"), d.join("b/config.toml"), &ctx).title(), "a/config.toml → b/config.toml");
    assert_eq!(Compare::start(d.join("a/config.toml"), d.join("copy.toml"), &ctx).title(), "config.toml → copy.toml");
    assert!(crate::inspector::is_patch("fix.patch") && crate::inspector::is_patch("A.DIFF"));
    assert!(!crate::inspector::is_patch(".patch") && !crate::inspector::is_patch("patch.txt"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Find Duplicates on real files: copies found whatever their names and folders, the
/// oldest first in each set; files of one size but other contents, or alike only in
/// their first 64 KB, kept apart; hard links to one file, empty files, hidden folders
/// and installed packages left out; the sets with the most to free first.
#[test]
fn duplicates_found_by_contents() {
    use crate::dupes::{Msg, find};
    let d = tmpdir("dupes");
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(86_400 * 30);
    let photo = vec![7u8; 300_000];
    touch(&d.join("Pictures/beach.jpg"), "");
    std::fs::write(d.join("Pictures/beach.jpg"), &photo).unwrap();
    std::fs::File::options().write(true).open(d.join("Pictures/beach.jpg")).unwrap().set_modified(old).unwrap();
    std::fs::create_dir_all(d.join("Downloads")).unwrap();
    std::fs::write(d.join("Downloads/IMG_2041.jpg"), &photo).unwrap();
    std::fs::write(d.join("Downloads/beach (1).jpg"), &photo).unwrap();
    // The same first 64 KB, then different.
    let mut other = photo.clone();
    other[200_000] = 8;
    std::fs::write(d.join("Downloads/almost.jpg"), &other).unwrap();
    touch(&d.join("notes.txt"), "same words\n");
    touch(&d.join("Documents/notes copy.txt"), "same words\n");
    touch(&d.join("Documents/other.txt"), "else words\n");
    std::fs::hard_link(d.join("Documents/other.txt"), d.join("Documents/linked.txt")).unwrap();
    touch(&d.join("empty-a"), "");
    touch(&d.join("empty-b"), "");
    touch(&d.join(".cache/notes.txt"), "same words\n");
    touch(&d.join("web/package.json"), "{}\n");
    touch(&d.join("web/node_modules/x/notes.txt"), "same words\n");
    let mut sets = vec![];
    let stop = std::sync::atomic::AtomicBool::new(false);
    find(&d, &stop, &mut |m| {
        if let Msg::Set(s) = m {
            sets.push(s);
        }
    });
    let names = |s: &crate::dupes::Set| s.files.iter().map(|c| crate::project::relative(&c.path, &d)).collect::<Vec<_>>();
    assert_eq!(sets.len(), 2, "{:?}", sets.iter().map(names).collect::<Vec<_>>());
    assert_eq!(names(&sets[0])[0], "Pictures/beach.jpg", "the oldest is kept");
    let mut rest = names(&sets[0])[1..].to_vec();
    rest.sort();
    assert_eq!(rest, ["Downloads/IMG_2041.jpg", "Downloads/beach (1).jpg"]);
    assert_eq!((sets[0].size, sets[0].spare()), (300_000, 600_000));
    let mut small = names(&sets[1]);
    small.sort();
    assert_eq!(small, ["Documents/notes copy.txt", "notes.txt"], "not the hidden or installed copies");
    let _ = std::fs::remove_dir_all(&d);
}

/// Batch Rename's names: a number padded to the run of #, the old name kept in place of
/// {name}, find and replace in plain text or with an expression's groups, a change of
/// case, extensions kept (and none taken from a folder or a dotfile); and the names
/// that can't be used, each with why.
#[test]
fn batch_rename_names_and_clashes() {
    use crate::batchname::{Case, Rules, names, problems, split};
    let items: Vec<(String, bool)> = ["IMG_2041.JPG", "IMG_2042.jpg", "beach day.png", "Old photos", ".hidden"].iter().map(|n| (n.to_string(), *n == "Old photos")).collect();
    assert_eq!(split("IMG_2041.JPG", false), ("IMG_2041", ".JPG"));
    assert_eq!(split("v1.2", true), ("v1.2", ""), "a folder keeps its dots");
    assert_eq!(split(".hidden", false), (".hidden", ""));
    let r = |f: &dyn Fn(&mut Rules)| {
        let mut r = Rules { start: "1".into(), ..Default::default() };
        f(&mut r);
        names(&items, &r).unwrap()
    };
    assert_eq!(r(&|_| {}), ["IMG_2041.JPG", "IMG_2042.jpg", "beach day.png", "Old photos", ".hidden"], "nothing asked, nothing changes");
    assert_eq!(r(&|r| r.name = "Trip ###".into()), ["Trip 001.JPG", "Trip 002.jpg", "Trip 003.png", "Trip 004", "Trip 005"]);
    assert_eq!(r(&|r| {
        r.name = "{name} #".into();
        r.start = "9".into();
    }), ["IMG_2041 9.JPG", "IMG_2042 10.jpg", "beach day 11.png", "Old photos 12", ".hidden 13"]);
    assert_eq!(r(&|r| {
        r.find = "img_".into();
        r.replace = "Photo ".into();
    }), ["Photo 2041.JPG", "Photo 2042.jpg", "beach day.png", "Old photos", ".hidden"], "plain text, any case");
    assert_eq!(r(&|r| {
        r.find = "img_".into();
        r.case_sensitive = true;
    })[0], "IMG_2041.JPG");
    assert_eq!(r(&|r| {
        r.find = r"IMG_(\d+)".into();
        r.replace = "$1 from the camera".into();
        r.regex = true;
    })[..2], ["2041 from the camera.JPG", "2042 from the camera.jpg"]);
    assert_eq!(r(&|r| {
        r.find = "$".into();
        r.replace = "$1".into();
    })[0], "IMG_2041.JPG", "a plain $ is just a $, and is not in the name");
    assert_eq!(r(&|r| r.case = Case::Title)[..3], ["Img_2041.JPG", "Img_2042.jpg", "Beach Day.png"], "the extension keeps its case");
    assert_eq!(r(&|r| r.case = Case::Lower)[2], "beach day.png");
    let mut bad = Rules { find: "(".into(), regex: true, ..Default::default() };
    assert!(names(&items, &bad).is_err(), "a broken expression says so");
    bad.regex = false;
    assert_eq!(names(&items, &bad).unwrap()[0], "IMG_2041.JPG", "as plain text it is just a bracket");
    // Clashes: two the same, one taken by a file not being renamed, an empty one, a slash.
    let olds: Vec<(String, bool)> = ["a.txt", "b.txt", "c.txt", "d.txt", "e.txt"].iter().map(|n| (n.to_string(), false)).collect();
    let new: Vec<String> = ["x.txt", "x.txt", "taken.txt", ".txt", "f/g.txt"].iter().map(|n| n.to_string()).collect();
    let others: std::collections::HashSet<String> = ["taken.txt", "a.txt"].iter().map(|n| n.to_string()).collect();
    assert_eq!(problems(&olds, &new, &others), [Some("same as another"), Some("same as another"), Some("already there"), None, Some("can't have /")]);
    // Swapping two names among the renamed is fine.
    let swap: Vec<String> = ["b.txt", "a.txt"].iter().map(|n| n.to_string()).collect();
    assert_eq!(problems(&olds[..2], &swap, &others), [None, None]);
    let empty: Vec<String> = vec!["".to_string()];
    assert_eq!(problems(&olds[..1], &empty, &others), [Some("no name")]);
}

/// Emptying the trash by age on a real trash folder: what was trashed before the cutoff
/// goes with its .trashinfo, a newer item stays, and one whose date can't be read stays.
#[test]
fn trash_emptied_by_age() {
    let d = tmpdir("trash-age");
    let t = d.join("Trash");
    let item = |name: &str, date: &str| {
        touch(&t.join("files").join(name).join("inside.txt"), "x");
        touch(&t.join("info").join(format!("{name}.trashinfo")), &format!("[Trash Info]\nPath=/home/someone/{name}\nDeletionDate={date}\n"));
    };
    item("old", "2026-08-01T10:00:00");
    item("new", "2026-10-15T10:00:00");
    item("undated", "not a date");
    let cutoff = crate::timefmt::parse_local("2026-09-16T00:00:00").unwrap();
    assert_eq!(crate::trashcan::empty_older(std::slice::from_ref(&t), cutoff), 1);
    assert!(!t.join("files/old").exists() && !t.join("info/old.trashinfo").exists());
    for keep in ["new", "undated"] {
        assert!(t.join("files").join(keep).join("inside.txt").exists() && t.join("info").join(format!("{keep}.trashinfo")).exists(), "{keep} stays");
    }
    assert_eq!(crate::config::Config::default().trash_days, 0, "off unless chosen");
    let _ = std::fs::remove_dir_all(&d);
}

/// A stand-in for LibreOffice: it logs its arguments, writes a two-page PDF (a real one,
/// poppler reads it) named as LibreOffice names its output, fails on a "broken" file and
/// sleeps on a "slow" one, its sleep's pid written down so a test can see it killed.
pub(crate) fn fake_soffice(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join("sample.pdf"), crate::sim::sample_pdf(&["0.2 0.4 0.8", "0.8 0.3 0.2"])).unwrap();
    let script = r#"#!/bin/sh
D="$(dirname "$0")"
echo "$*" >> "$D/soffice.log"
out=""; prev=""
for a; do [ "$prev" = --outdir ] && out="$a"; prev="$a"; last="$a"; done
case "$last" in
*slow*) sleep 30 & echo $! > "$D/sleep.pid"; wait; exit 0;;
*broken*) exit 1;;
esac
name=$(basename "$last")
cp "$D/sample.pdf" "$out/${name%.*}.pdf"
"#;
    let p = dir.join("soffice");
    std::fs::write(&p, script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Whether a process is gone (or only a zombie waiting to be reaped), after up to 3 s.
pub(crate) fn process_gone(pid: &str) -> bool {
    let t0 = std::time::Instant::now();
    while t0.elapsed().as_secs() < 3 {
        match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => return true,
            Ok(s) if s.rsplit_once(')').is_some_and(|(_, rest)| rest.trim_start().starts_with('Z')) => return true,
            _ => std::thread::sleep(std::time::Duration::from_millis(20)),
        }
    }
    false
}

/// Office documents through LibreOffice (a stand-in that writes a real PDF): converted
/// once into the cache with a profile of Maia's own, read back the second time without
/// starting it, again when the file changes; a file it can't read refused; a slow one
/// killed with its children, when the preview goes away and at the time limit.
#[test]
fn office_documents_convert_to_pdf_once() {
    use crate::office::{TIMEOUT, cached, convert, is_office, program};
    let d = tmpdir("office");
    let fake = d.join("tools");
    fake_soffice(&fake);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.join("none")));
    assert_eq!(program(&crate::drives::Tools::current().unwrap()), None, "no LibreOffice: no conversion");
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    assert_eq!(program(&t), Some("soffice"));
    for x in ["docx", "doc", "odt", "rtf", "xlsx", "xls", "ods", "pptx", "ppt", "odp"] {
        assert!(is_office(x), "{x}");
    }
    assert!(!is_office("pdf") && !is_office("txt") && !is_office("docm"));
    let src = d.join("Q3 report.v2.docx");
    touch(&src, "PK not really a document");
    let meta = |p: &Path| std::fs::metadata(p).map(|m| (m.len(), m.modified().ok())).unwrap();
    let (size, mtime) = meta(&src);
    let log = || std::fs::read_to_string(fake.join("soffice.log")).unwrap_or_default();
    let pdf = convert(&t, "soffice", &src, size, mtime, TIMEOUT, &|| false).unwrap();
    assert_eq!(pdf, cached(&src, size, mtime));
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF-1.4"));
    if crate::media::have("pdfinfo") {
        assert_eq!(crate::media::pdf_pages(&pdf), Some(2), "the PDF reads");
    }
    let args = log();
    assert_eq!(args.lines().count(), 1);
    let profile = crate::clipboard::path_to_uri(&crate::office::cache_dir().join("profile"));
    assert!(args.starts_with(&format!("-env:UserInstallation={profile} --headless --norestore --nolockcheck --convert-to pdf --outdir ")), "{args}");
    assert!(args.trim_end().ends_with(&src.display().to_string()), "{args}");
    assert!(profile.starts_with("file:///") && !profile.contains("/.config/libreoffice"));
    // The second look: from the cache, LibreOffice not started.
    assert_eq!(convert(&t, "soffice", &src, size, mtime, TIMEOUT, &|| false).unwrap(), pdf);
    assert_eq!(log().lines().count(), 1);
    // Changed: converted again, to a file of its own.
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&src, "PK a longer document than before").unwrap();
    let (size2, mtime2) = meta(&src);
    let pdf2 = convert(&t, "soffice", &src, size2, mtime2, TIMEOUT, &|| false).unwrap();
    assert_ne!(pdf2, pdf);
    assert_eq!(log().lines().count(), 2);
    let stage = crate::office::cache_dir().join(format!(".part-{}", pdf2.file_stem().unwrap().to_string_lossy()));
    assert!(!stage.exists(), "the staging folder is gone");
    let broken = d.join("broken.xlsx");
    touch(&broken, "x");
    let (bs, bm) = meta(&broken);
    assert_eq!(convert(&t, "soffice", &broken, bs, bm, TIMEOUT, &|| false).unwrap_err(), "Can't preview this document");
    // Slow: stopped (the preview went away), then timed out; LibreOffice's children die too.
    let slow = d.join("slow.pptx");
    touch(&slow, "x");
    let (ss, sm) = meta(&slow);
    let t0 = std::time::Instant::now();
    let r = convert(&t, "soffice", &slow, ss, sm, TIMEOUT, &|| t0.elapsed().as_millis() > 400);
    assert!(r.is_err() && t0.elapsed().as_secs() < 5);
    let pid = std::fs::read_to_string(fake.join("sleep.pid")).unwrap();
    assert!(process_gone(pid.trim()), "the child's child was killed");
    let _ = std::fs::remove_file(fake.join("sleep.pid"));
    let t0 = std::time::Instant::now();
    let r = convert(&t, "soffice", &slow, ss, sm, std::time::Duration::from_millis(500), &|| false);
    assert_eq!(r.unwrap_err(), "LibreOffice took too long to read this document");
    assert!(t0.elapsed().as_secs() < 5);
    let pid = std::fs::read_to_string(fake.join("sleep.pid")).unwrap();
    assert!(process_gone(pid.trim()));
    assert!(!cached(&slow, ss, sm).exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// A stand-in program `name` in `dir`: it appends its name and arguments, one per line
/// and a blank line after, to `calls.log`, then runs `body` (sh).
pub(crate) fn fake_tool(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\nD=\"$(dirname \"$0\")\"\n{{ echo {name}; for a; do printf '%s\\n' \"$a\"; done; echo; }} >> \"$D/calls.log\"\n{body}\n")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// The calls the stand-ins in `dir` received, each as its program and arguments.
pub(crate) fn fake_calls(dir: &Path) -> Vec<Vec<String>> {
    let log = std::fs::read_to_string(dir.join("calls.log")).unwrap_or_default();
    log.split("\n\n").filter(|c| !c.trim().is_empty()).map(|c| c.lines().map(String::from).collect()).collect()
}

/// Set As Wallpaper picks each desktop's own way: Noctalia (and the Quickshell one before
/// it) over the compositor it runs on; GNOME's two pictures, Cinnamon, MATE, KDE, XFCE's
/// every monitor and workspace, sway's quoted command, swww and hyprpaper; nothing for a
/// desktop it doesn't know or a tool that isn't there. Then the programs are run
/// (stand-ins): an optional step may fail, a needed one says why, and hyprctl's refusal
/// printed with exit 0 counts as failing.
#[test]
fn wallpaper_each_desktop_its_way() {
    use crate::wallpaper::{Desk, Way, pick, set, steps, xfce_props};
    let desk = |de: &str, run: &[&str]| Desk { desktop: de.into(), running: run.iter().map(|s| s.to_string()).collect(), outputs: vec!["DP-1".into(), "eDP-1".into()] };
    let all = |_: &str| true;
    let has = |list: &'static [&'static str]| move |n: &str| list.contains(&n);
    assert_eq!(pick(&desk("Hyprland", &["Hyprland", "noctalia"]), all), Some(Way::Noctalia));
    assert_eq!(pick(&desk("Hyprland", &["noctalia"]), has(&["hyprctl"])), None, "noctalia not on PATH");
    assert_eq!(pick(&desk("niri", &["qs:noctalia-shell", "qs"]), all), Some(Way::NoctaliaQs));
    assert_eq!(pick(&desk("Hyprland", &["hyprpaper"]), all), Some(Way::Hyprpaper));
    assert_eq!(pick(&desk("Hyprland", &["hyprpaper", "swww-daemon"]), all), Some(Way::Swww));
    assert_eq!(pick(&desk("Hyprland", &[]), all), None, "nothing draws a wallpaper");
    assert_eq!(pick(&desk("sway", &[]), all), Some(Way::Sway));
    assert_eq!(pick(&desk("ubuntu:GNOME", &[]), all), Some(Way::Gnome));
    assert_eq!(pick(&desk("GNOME", &[]), has(&["swww"])), None, "no gsettings");
    assert_eq!(pick(&desk("X-Cinnamon", &[]), all), Some(Way::Cinnamon));
    assert_eq!(pick(&desk("MATE", &[]), all), Some(Way::Mate));
    assert_eq!(pick(&desk("KDE", &["swww-daemon"]), all), Some(Way::Kde));
    assert_eq!(pick(&desk("XFCE", &[]), all), Some(Way::Xfce));
    assert_eq!(pick(&desk("LXQt", &[]), all), None);
    assert_eq!(pick(&desk("", &[]), all), None);
    let p = Path::new("/home/ana/Pictures/My \"best\" shot.jpg");
    let plain = Path::new("/home/ana/Pictures/dunes.jpg");
    let args = |w: Way, p: &Path, xf: &str| -> Vec<Vec<String>> { steps(w, p, &desk("", &[]), xf).iter().map(|s| s.args.iter().map(|a| a.to_string_lossy().into_owned()).collect()).collect() };
    assert_eq!(args(Way::Noctalia, plain, ""), [["noctalia", "msg", "wallpaper-set", "/home/ana/Pictures/dunes.jpg"]]);
    assert_eq!(args(Way::Noctalia, p, ""), [["noctalia", "msg", "wallpaper-set", "DP-1", p.to_str().unwrap()], ["noctalia", "msg", "wallpaper-set", "eDP-1", p.to_str().unwrap()]], "a path with a space goes to each output");
    assert_eq!(args(Way::NoctaliaQs, plain, ""), [["qs", "-c", "noctalia-shell", "ipc", "call", "wallpaper", "set", "/home/ana/Pictures/dunes.jpg", "all"]]);
    let uri = "file:///home/ana/Pictures/My%20%22best%22%20shot.jpg";
    assert_eq!(args(Way::Gnome, p, ""), [["gsettings", "set", "org.gnome.desktop.background", "picture-uri", uri], ["gsettings", "set", "org.gnome.desktop.background", "picture-uri-dark", uri]]);
    assert_eq!(args(Way::Cinnamon, p, ""), [["gsettings", "set", "org.cinnamon.desktop.background", "picture-uri", uri]]);
    assert_eq!(args(Way::Mate, p, ""), [["gsettings", "set", "org.mate.background", "picture-filename", p.to_str().unwrap()]]);
    assert_eq!(args(Way::Kde, p, ""), [["plasma-apply-wallpaperimage", p.to_str().unwrap()]]);
    assert_eq!(args(Way::Sway, p, ""), [["swaymsg", r#"output * bg "/home/ana/Pictures/My \"best\" shot.jpg" fill"#]]);
    assert_eq!(args(Way::Swww, plain, ""), [["swww", "img", "/home/ana/Pictures/dunes.jpg"]]);
    assert_eq!(args(Way::Hyprpaper, plain, ""), [["hyprctl", "hyprpaper", "preload", "/home/ana/Pictures/dunes.jpg"], ["hyprctl", "hyprpaper", "wallpaper", ",/home/ana/Pictures/dunes.jpg"]]);
    // xfconf-query -c xfce4-desktop -l, from a laptop with an external screen.
    let listing = "/backdrop/screen0/monitorHDMI-1/workspace0/color-style\n/backdrop/screen0/monitorHDMI-1/workspace0/image-style\n/backdrop/screen0/monitorHDMI-1/workspace0/last-image\n/backdrop/screen0/monitoreDP-1/workspace0/last-image\n/backdrop/screen0/monitoreDP-1/workspace1/last-image\n/backdrop/single-workspace-mode\n/desktop-icons/style\n";
    assert_eq!(xfce_props(listing), ["/backdrop/screen0/monitorHDMI-1/workspace0/last-image", "/backdrop/screen0/monitoreDP-1/workspace0/last-image", "/backdrop/screen0/monitoreDP-1/workspace1/last-image"]);
    assert_eq!(args(Way::Xfce, plain, listing).len(), 3);
    assert_eq!(args(Way::Xfce, plain, listing)[1], ["xfconf-query", "-c", "xfce4-desktop", "-p", "/backdrop/screen0/monitoreDP-1/workspace0/last-image", "-s", "/home/ana/Pictures/dunes.jpg"]);
    assert_eq!(args(Way::Xfce, plain, "")[0][3..6], ["-n", "-t", "string"], "no property yet: made");
    // Run through stand-ins.
    let d = tmpdir("wallpaper");
    let fake = d.join("tools");
    fake_tool(&fake, "gsettings", "case \"$3\" in picture-uri-dark) echo \"No such key\" >&2; exit 1;; esac");
    fake_tool(&fake, "xfconf-query", &format!("[ \"$3\" = -l ] && printf '{}'\nexit 0", listing.replace('\n', "\\n")));
    fake_tool(&fake, "hyprctl", "echo \"wallpaper failed (not preloaded)\"");
    fake_tool(&fake, "plasma-apply-wallpaperimage", "echo 'Unable to find the wallpaper plugin' >&2; exit 1");
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    assert_eq!(set(&t, &desk("GNOME", &[]), p), Ok(Way::Gnome), "the dark picture is optional");
    assert_eq!(fake_calls(&fake).len(), 2);
    assert_eq!(set(&t, &desk("XFCE", &[]), plain), Ok(Way::Xfce));
    let calls = fake_calls(&fake);
    assert_eq!(calls.len(), 6, "listed, then each of three set");
    assert_eq!(calls[2], ["xfconf-query", "-c", "xfce4-desktop", "-l"]);
    assert_eq!(set(&t, &desk("Hyprland", &["hyprpaper"]), plain).unwrap_err(), "Couldn't set the wallpaper: wallpaper failed (not preloaded)");
    assert_eq!(set(&t, &desk("KDE", &[]), plain).unwrap_err(), "Couldn't set the wallpaper: Unable to find the wallpaper plugin");
    assert_eq!(set(&t, &desk("sway", &[]), plain).unwrap_err(), "Maia can't set the wallpaper on this desktop", "swaymsg isn't there");
    assert!(crate::wallpaper::is_wallpaper("jpg") && crate::wallpaper::is_wallpaper("webp") && !crate::wallpaper::is_wallpaper("gif") && !crate::wallpaper::is_wallpaper("svg"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Email: xdg-email with each file attached; without it, the mail app's own way for
/// Thunderbird (and a flatpak one), Evolution, KMail and Claws Mail, and nothing for one
/// that can't take attachments; no mail app at all is said so and nothing starts.
#[test]
fn email_attaches_files() {
    use crate::sendto::{MAIL_APP, email, email_args};
    let f = [PathBuf::from("/home/ana/Q3 report.pdf"), PathBuf::from("/home/ana/photo,1.jpg")];
    let s = |v: Option<Vec<std::ffi::OsString>>| v.map(|v| v.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>());
    assert_eq!(s(email_args(&f, true, None)).unwrap(), ["xdg-email", "--attach", "/home/ana/Q3 report.pdf", "--attach", "/home/ana/photo,1.jpg"]);
    let tb = ["thunderbird", "-compose", "attachment='file:///home/ana/Q3%20report.pdf,file:///home/ana/photo%2C1.jpg'"];
    assert_eq!(s(email_args(&f, false, Some("thunderbird %u"))).unwrap(), tb);
    assert_eq!(s(email_args(&f, false, Some("/usr/bin/flatpak run --branch=stable org.mozilla.Thunderbird %u"))).unwrap()[..5], ["/usr/bin/flatpak", "run", "--branch=stable", "org.mozilla.Thunderbird", "-compose"]);
    assert_eq!(s(email_args(&f, false, Some("evolution %U"))).unwrap(), ["evolution", "mailto:?attach=file:///home/ana/Q3%20report.pdf&attach=file:///home/ana/photo%2C1.jpg"]);
    assert_eq!(s(email_args(&f[..1], false, Some("kmail -caption \"%c\" %u"))).unwrap()[3..], ["--attach", "file:///home/ana/Q3%20report.pdf"]);
    assert_eq!(s(email_args(&f[..1], false, Some("claws-mail --compose %u"))).unwrap(), ["claws-mail", "--compose", "--attach", "/home/ana/Q3 report.pdf"]);
    assert_eq!(email_args(&f, false, Some("firefox %u")), None, "a browser can't attach files");
    assert_eq!(email_args(&f, false, None), None);
    let d = tmpdir("email");
    let fake = d.join("tools");
    fake_tool(&fake, "xdg-email", "exit 0");
    crate::drives::FAKE.with(|x| *x.borrow_mut() = Some(fake.clone()));
    let n = crate::mime::LAUNCHED.lock().unwrap().len();
    assert_eq!(email(&f).unwrap_err(), "No mail app is set up");
    MAIL_APP.with(|m| *m.borrow_mut() = Some("thunderbird %u".into()));
    email(&f).unwrap();
    let launched = crate::mime::LAUNCHED.lock().unwrap().clone();
    assert!(launched[n..].iter().any(|l| l.contains("xdg-email") && l.contains("\"--attach\" \"/home/ana/Q3 report.pdf\" \"--attach\" \"/home/ana/photo,1.jpg\"")), "{:?}", &launched[n..]);
    // No xdg-email: Thunderbird itself.
    crate::drives::FAKE.with(|x| *x.borrow_mut() = Some(d.join("none")));
    email(&f).unwrap();
    assert!(crate::mime::LAUNCHED.lock().unwrap()[n..].iter().any(|l| l.starts_with("\"thunderbird\" \"-compose\"")));
    MAIL_APP.with(|m| *m.borrow_mut() = Some("firefox %u".into()));
    assert_eq!(email(&f).unwrap_err(), "Attaching files to an email needs xdg-email");
    assert_eq!(crate::needs::tool_in(&email(&f).unwrap_err()), Some("xdg-email"), "the toast offers how to install it");
    MAIL_APP.with(|m| *m.borrow_mut() = None);
    crate::drives::FAKE.with(|x| *x.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&d);
}

/// Print: CUPS's printers read from lpstat (names, and the default when it is one of
/// them), lp's arguments, which files are offered, and the printers read through a
/// stand-in lpstat, under the C locale.
#[test]
fn print_reads_cups_printers() {
    use crate::sendto::{Printers, cups, lp_args, parse_printers, printable, printers};
    let e = "Office_Laser\nHP_ENVY_6000\nPDF\n";
    assert_eq!(parse_printers(e, "system default destination: HP_ENVY_6000\n"), Printers { names: vec!["Office_Laser".into(), "HP_ENVY_6000".into(), "PDF".into()], default: Some("HP_ENVY_6000".into()) });
    assert_eq!(parse_printers(e, "no system default destination\n").default, None);
    assert_eq!(parse_printers("", "no system default destination\n"), Printers::default());
    assert_eq!(parse_printers("lpstat: No destinations added.\n", "").names, Vec::<String>::new(), "a complaint is no printer");
    assert_eq!(parse_printers("A\n", "system default destination: Gone\n").default, None);
    let a: Vec<String> = lp_args("Office_Laser", 2, &[PathBuf::from("/x/a b.pdf"), PathBuf::from("/x/c.png")]).iter().map(|a| a.to_string_lossy().into_owned()).collect();
    assert_eq!(a, ["-d", "Office_Laser", "-n", "2", "/x/a b.pdf", "/x/c.png"]);
    for (n, x) in [("a.pdf", "pdf"), ("b.PS", "ps"), ("c.jpg", "jpg"), ("notes.txt", "txt"), ("main.rs", "rs"), ("README.md", "md")] {
        assert!(printable(n, x), "{n}");
    }
    for (n, x) in [("a.docx", "docx"), ("b.zip", "zip"), ("c.mp4", "mp4"), ("d.ods", "ods")] {
        assert!(!printable(n, x), "{n}");
    }
    let d = tmpdir("print");
    let fake = d.join("tools");
    fake_tool(&fake, "lpstat", "[ \"$LC_ALL\" = C ] || exit 3\ncase \"$1\" in -e) printf 'Office_Laser\\nHP_ENVY_6000\\n';; -d) echo 'system default destination: HP_ENVY_6000';; esac");
    crate::drives::FAKE.with(|x| *x.borrow_mut() = Some(fake.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|x| *x.borrow_mut() = None);
    assert!(!cups(&t), "lp missing");
    fake_tool(&fake, "lp", "exit 0");
    assert!(cups(&t));
    assert_eq!(printers(&t).default.as_deref(), Some("HP_ENVY_6000"));
    assert_eq!(printers(&t).names.len(), 2);
    let _ = std::fs::remove_dir_all(&d);
}

/// `jpeg` with an EXIF block saying it is stored turned (`orient`, 1 to 8), as a camera
/// held upright writes it.
pub(crate) fn with_exif_orientation(jpeg: &[u8], orient: u16) -> Vec<u8> {
    let mut tiff = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&orient.to_be_bytes());
    tiff.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    let mut app1 = b"Exif\x00\x00".to_vec();
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// The real ImageMagick on the PATH, as a stand-in folder holding only it (and none).
pub(crate) fn imagemagick_tools(dir: &Path) -> Option<PathBuf> {
    let real = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join("magick")).find(|m| m.is_file()))?;
    std::fs::create_dir_all(dir).unwrap();
    let _ = std::os::unix::fs::symlink(real, dir.join("magick"));
    Some(dir.to_path_buf())
}

/// Picture fixes through the real ImageMagick on real files: a photo stored turned (EXIF
/// orientation 6) measured upright, turned right in place with its original in the
/// trash, turned again without a second copy there, and put back by Undo byte for
/// byte; copies at half, within a box and a longer side, named by their size and never
/// over another file; a smaller picture left alone by Fit; a crop of the upright
/// picture; a name ImageMagick would misread; a broken file refused with nothing
/// changed. Sizes as typed.
#[test]
fn picture_fixes_with_imagemagick() {
    use crate::picfix::{Size, crop, crop_px, cropped_name, dims, magick, parse_size, resize, resized_name, rotate, target, undo_replaced};
    assert_eq!(parse_size("1280"), Some(Size::Long(1280)));
    for s in ["1280 × 720", "1280x720", "1280 X 720", "1280*720", " 1280 720 "] {
        assert_eq!(parse_size(s), Some(Size::Within(1280, 720)), "{s:?}");
    }
    assert_eq!(parse_size("50%"), Some(Size::Scale(0.5)));
    assert_eq!(parse_size("12.5 %"), Some(Size::Scale(0.125)));
    for s in ["", "0", "big", "1x2x3", "-5", "40000", "0%"] {
        assert_eq!(parse_size(s), None, "{s:?}");
    }
    assert_eq!(target([4032, 3024], Size::Fit(1920)), Some([1920, 1440]));
    assert_eq!(target([1000, 800], Size::Fit(1920)), None, "already within");
    assert_eq!(target([4032, 3024], Size::Scale(0.25)), Some([1008, 756]));
    assert_eq!(target([300, 200], Size::Long(600)), Some([600, 400]), "a longer side asked for grows it");
    assert_eq!(target([300, 200], Size::Within(100, 100)), Some([100, 67]));
    assert_eq!(target([300, 200], Size::Scale(1.0)), None);
    assert_eq!(resized_name("photo.jpg", [1920, 1440]), "photo (1920 px).jpg");
    assert_eq!(resized_name("scan.v2.png", [600, 800]), "scan.v2 (800 px).png");
    assert_eq!(cropped_name("photo.jpg"), "photo (cropped).jpg");
    assert_eq!(cropped_name("README"), "README (cropped)");
    assert_eq!(crop_px(egui::Rect::from_min_max(egui::pos2(0.25, 0.0), egui::pos2(0.75, 0.5)), [20, 40]), Some([5, 0, 10, 20]));
    assert_eq!(crop_px(egui::Rect::from_min_max(egui::pos2(0.5, 0.5), egui::pos2(0.51, 0.9)), [20, 40]), None, "under a pixel wide");
    let d = tmpdir("picfix");
    let Some(fake) = imagemagick_tools(&d.join("tools")) else {
        eprintln!("picture fixes: no ImageMagick, skipped");
        return;
    };
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.join("none")));
    assert_eq!(magick(&crate::drives::Tools::current().unwrap()), None);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let prog = magick(&t).unwrap();
    // A photo 40 × 20 as stored, upright 20 × 40.
    let raw = d.join("raw.jpg");
    assert!(std::process::Command::new(fake.join("magick")).args(["-size", "40x20", "xc:#c84"]).arg(&raw).status().unwrap().success());
    let photo = d.join("photo.jpg");
    let original = with_exif_orientation(&std::fs::read(&raw).unwrap(), 6);
    std::fs::write(&photo, &original).unwrap();
    let shown = |p: &Path| crate::imgdec::load(p, 64).map(|x| x.1);
    assert_eq!(shown(&photo), Some([20, 40]));
    assert_eq!(dims(&t, prog, &photo).unwrap(), [20, 40], "measured upright");
    let trashed = |p: &Path| crate::trashcan::list().into_iter().filter(|it| it.original == p).count();
    rotate(&t, prog, &photo, true, false).unwrap();
    assert_eq!(shown(&photo), Some([40, 20]), "turned right from upright, no orientation left to apply twice");
    assert_eq!(trashed(&photo), 1, "the original kept in the trash");
    rotate(&t, prog, &photo, true, true).unwrap();
    assert_eq!(shown(&photo), Some([20, 40]));
    assert_eq!(trashed(&photo), 1, "a second turn keeps no second copy");
    assert!(!d.join(".photo.jpg.part").exists());
    assert_eq!(undo_replaced(&[photo.clone()]), 0);
    assert_eq!(std::fs::read(&photo).unwrap(), original, "the original back, byte for byte");
    // Copies.
    let big = d.join("big.png");
    std::fs::write(&big, png_bytes(400, 300, 3)).unwrap();
    let half = resize(&t, prog, &big, Size::Scale(0.5)).unwrap().unwrap();
    assert_eq!(half, d.join("big (200 px).png"));
    assert_eq!(shown(&half), Some([200, 150]));
    assert_eq!(resize(&t, prog, &big, Size::Fit(1920)).unwrap(), None);
    let boxed = resize(&t, prog, &big, Size::Within(100, 100)).unwrap().unwrap();
    assert_eq!((boxed.file_name().unwrap().to_str().unwrap(), shown(&boxed)), ("big (100 px).png", Some([100, 75])));
    let again = resize(&t, prog, &big, Size::Scale(0.5)).unwrap().unwrap();
    assert_eq!(again, d.join("big (200 px) (1).png"), "never over the first copy");
    let long = resize(&t, prog, &photo, Size::Long(80)).unwrap().unwrap();
    assert_eq!((long.file_name().unwrap().to_str().unwrap(), shown(&long)), ("photo (80 px).jpg", Some([40, 80])), "resized upright");
    // A crop of the upright picture.
    let c = crop(&t, prog, &photo, egui::Rect::from_min_max(egui::pos2(0.25, 0.0), egui::pos2(0.75, 0.5))).unwrap();
    assert_eq!((c.file_name().unwrap().to_str().unwrap(), shown(&c)), ("photo (cropped).jpg", Some([10, 20])));
    // A name with brackets and a colon.
    let odd = d.join("shot [1]: png.png");
    std::fs::write(&odd, png_bytes(30, 10, 1)).unwrap();
    rotate(&t, prog, &odd, false, false).unwrap();
    assert_eq!(shown(&odd), Some([10, 30]));
    // Broken: refused, nothing changed or trashed.
    let broken = d.join("broken.png");
    std::fs::write(&broken, b"\x89PNG\r\n\x1a\nnot really").unwrap();
    assert!(rotate(&t, prog, &broken, true, false).is_err());
    assert_eq!(std::fs::read(&broken).unwrap(), b"\x89PNG\r\n\x1a\nnot really");
    assert_eq!(trashed(&broken), 0);
    assert!(!d.join(".broken.png.part").exists());
    assert!(resize(&t, prog, &broken, Size::Scale(0.5)).is_err());
    for it in crate::trashcan::list().into_iter().filter(|it| it.original.starts_with(&d)) {
        let _ = crate::trashcan::purge(&it);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Two clones of one bare repository, each with its own identity and no signing.
fn sync_fixture(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let d = tmpdir(name);
    std::fs::create_dir_all(d.join("server.git")).unwrap();
    git(&d.join("server.git"), &["init", "-q", "--bare", "-b", "main"]);
    let a = d.join("a");
    std::fs::create_dir_all(&a).unwrap();
    work_repo(&a);
    touch(&a.join("notes.txt"), "one\ntwo\nthree\n");
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-qm", "Start"]);
    git(&a, &["remote", "add", "origin", d.join("server.git").to_str().unwrap()]);
    git(&a, &["push", "-q", "-u", "origin", "main"]);
    git(&d, &["clone", "-q", d.join("server.git").to_str().unwrap(), "b"]);
    let b = d.join("b");
    for (k, v) in [("user.name", "Test"), ("user.email", "test@example.invalid"), ("commit.gpgsign", "false"), ("core.hooksPath", ".git/hooks")] {
        git(&b, &["config", k, v]);
    }
    (d, a, b)
}

/// Fetch, pull and push against a real bare repository: what came in counted, a pull
/// that can't fast-forward and a push the remote refuses each handed back to be asked
/// about, then merged, rebased or replaced; a new branch published to origin.
#[test]
fn fetch_pull_and_push() {
    use crate::gitsync::{BEHIND, DIVERGED};
    use crate::gitwork::{Op, run};
    let (d, a, b) = sync_fixture("sync");
    let commit = |at: &Path, file: &str, text: &str, msg: &str| {
        touch(&at.join(file), text);
        git(at, &["add", "-A"]);
        git(at, &["commit", "-qm", msg]);
    };
    commit(&a, "a.txt", "from a\n", "A's work");
    assert_eq!(run(&a, &Op::Push(false)).unwrap(), "Pushed 1 commit to origin/main");
    assert_eq!(run(&a, &Op::Push(false)).unwrap(), "Nothing to push");
    assert_eq!(run(&b, &Op::Fetch).unwrap(), "Fetched: 1 commit to pull");
    assert_eq!(run(&b, &Op::Pull(None)).unwrap(), "Pulled 1 commit");
    assert!(b.join("a.txt").exists());
    assert_eq!(run(&b, &Op::Pull(None)).unwrap(), "Already up to date");
    // Both move on: the pull asks, the push asks.
    commit(&a, "a2.txt", "more from a\n", "More from A");
    run(&a, &Op::Push(false)).unwrap();
    commit(&b, "b.txt", "from b\n", "B's work");
    assert_eq!(run(&b, &Op::Pull(None)).unwrap_err(), DIVERGED);
    assert_eq!(run(&b, &Op::Push(false)).unwrap_err(), BEHIND);
    assert_eq!(run(&b, &Op::Pull(Some(true))).unwrap(), "Rebased onto origin/main");
    assert!(b.join("a2.txt").exists() && b.join("b.txt").exists());
    assert_eq!(run(&b, &Op::Push(false)).unwrap(), "Pushed 1 commit to origin/main");
    // Merged instead of rebased.
    commit(&b, "b2.txt", "x\n", "B again");
    run(&b, &Op::Push(false)).unwrap();
    commit(&a, "a3.txt", "y\n", "A again");
    assert_eq!(run(&a, &Op::Pull(None)).unwrap_err(), DIVERGED);
    assert_eq!(run(&a, &Op::Pull(Some(false))).unwrap(), "Merged origin/main");
    let parents = String::from_utf8(std::process::Command::new("git").arg("-C").arg(&a).args(["log", "-1", "--format=%P"]).output().unwrap().stdout).unwrap();
    assert_eq!(parents.split_whitespace().count(), 2, "a merge commit");
    // Replacing what the remote has, after rewriting history it already holds.
    run(&a, &Op::Push(false)).unwrap();
    git(&a, &["commit", "-q", "--amend", "-m", "A again, reworded"]);
    assert_eq!(run(&a, &Op::Push(false)).unwrap_err(), BEHIND);
    assert_eq!(run(&a, &Op::Push(true)).unwrap(), "Pushed, replacing origin/main");
    // A new branch has no upstream: Push publishes it, Pull says why it can't.
    git(&a, &["switch", "-q", "-c", "topic"]);
    assert_eq!(run(&a, &Op::Pull(None)).unwrap_err(), "topic doesn't track a remote branch yet. Push publishes it.");
    assert_eq!(run(&a, &Op::Push(false)).unwrap(), "Published topic to origin");
    assert_eq!(crate::gitsync::upstream(&a).as_deref(), Some("origin/topic"));
    // Somewhere it can't reach: said plainly.
    git(&a, &["remote", "set-url", "origin", d.join("gone.git").to_str().unwrap()]);
    assert_eq!(run(&a, &Op::Fetch).unwrap_err(), "The remote isn't there, or you can't see it.");
    let _ = std::fs::remove_dir_all(&d);
}

/// Conflicts read from the text git leaves (with or without a base section) and
/// settled one at a time, keeping one side, the other, or both, the rest untouched.
#[test]
fn conflicts_read_and_settled() {
    use crate::gitsync::{Side, conflicts, resolve};
    let text = "top\n<<<<<<< HEAD\nours 1\nours 2\n=======\ntheirs 1\n>>>>>>> topic\nmiddle\n<<<<<<< HEAD\nmine\n||||||| base\nold\n=======\nyours\n>>>>>>> 1a2b3c4 (Fix it)\nend\n";
    let c = conflicts(text);
    assert_eq!(c.len(), 2);
    assert_eq!((c[0].ours.as_str(), c[0].theirs.as_str(), c[0].line), ("HEAD", "topic", 2));
    assert_eq!((c[0].ours_lines.clone(), c[0].theirs_lines.clone()), (vec!["ours 1".to_string(), "ours 2".into()], vec!["theirs 1".to_string()]));
    assert_eq!((c[1].ours_lines.clone(), c[1].theirs_lines.clone()), (vec!["mine".to_string()], vec!["yours".to_string()]), "the base section left out");
    assert_eq!(c[1].theirs, "1a2b3c4 (Fix it)");
    let first = resolve(text, 0, Side::Theirs).unwrap();
    assert!(first.starts_with("top\ntheirs 1\nmiddle\n<<<<<<< HEAD\n"), "{first}");
    assert_eq!(conflicts(&first).len(), 1);
    assert_eq!(resolve(&first, 0, Side::Both).unwrap(), "top\ntheirs 1\nmiddle\nmine\nyours\nend\n");
    assert_eq!(resolve(text, 1, Side::Ours).unwrap(), text.replace("<<<<<<< HEAD\nmine\n||||||| base\nold\n=======\nyours\n>>>>>>> 1a2b3c4 (Fix it)\n", "mine\n"));
    assert!(resolve(text, 2, Side::Ours).is_none(), "no third conflict");
    assert!(conflicts("no markers\n======= alone\n").is_empty());
}

/// A pull that stops at conflicts: they are shown in place of the file's diff, the
/// file can't be staged with its markers in, settling the last one stages it, and
/// Continue makes the merge commit; another one, aborted, goes back to before.
#[test]
fn merge_conflicts_settled_and_continued() {
    use crate::gitsync::{CONFLICTS, Doing, Side, doing};
    use crate::gitwork::{Op, file_diff, run, work};
    let (d, a, b) = sync_fixture("conflicts");
    touch(&a.join("notes.txt"), "one\nTWO from a\nthree\n");
    git(&a, &["commit", "-qam", "A edits"]);
    run(&a, &Op::Push(false)).unwrap();
    touch(&b.join("notes.txt"), "one\ntwo from b\nthree\n");
    git(&b, &["commit", "-qam", "B edits"]);
    assert_eq!(run(&b, &Op::Pull(Some(false))).unwrap_err(), CONFLICTS);
    assert_eq!(doing(&b), Some(Doing::Merge));
    let w = work(&b).unwrap();
    assert_eq!(w.doing, Some(Doing::Merge));
    let f = w.unstaged.iter().find(|c| c.path == "notes.txt").unwrap().clone();
    assert_eq!(f.status, b'U');
    let diff = file_diff(&b, &f, false, "HEAD");
    assert_eq!(diff.conflicts.len(), 1);
    assert_eq!((diff.conflicts[0].ours_lines.clone(), diff.conflicts[0].theirs_lines.clone()), (vec!["two from b".to_string()], vec!["TWO from a".to_string()]));
    assert_eq!(run(&b, &Op::Stage(vec!["notes.txt".into()])).unwrap_err(), "It still has conflict markers: settle them first.");
    assert_eq!(run(&b, &Op::Continue).unwrap_err(), "Some files are still in conflict.");
    assert_eq!(run(&b, &Op::Settle { path: "notes.txt".into(), at: 0, side: Side::Both }).unwrap(), "Resolved notes.txt");
    assert_eq!(std::fs::read_to_string(b.join("notes.txt")).unwrap(), "one\ntwo from b\nTWO from a\nthree\n");
    assert_eq!(run(&b, &Op::Continue).unwrap(), "Finished the merge");
    assert_eq!(doing(&b), None);
    // Another one, given up on.
    touch(&a.join("notes.txt"), "one\nagain from a\nthree\n");
    git(&a, &["commit", "-qam", "A again"]);
    run(&a, &Op::Push(false)).unwrap();
    touch(&b.join("notes.txt"), "one\nagain from b\nthree\n");
    git(&b, &["commit", "-qam", "B again"]);
    let before = std::fs::read_to_string(b.join("notes.txt")).unwrap();
    assert_eq!(run(&b, &Op::Pull(Some(true))).unwrap_err(), CONFLICTS);
    assert_eq!(doing(&b), Some(Doing::Rebase));
    assert_eq!(run(&b, &Op::Abort).unwrap(), "Stopped rebasing and went back");
    assert_eq!((doing(&b), std::fs::read_to_string(b.join("notes.txt")).unwrap()), (None, before));
    let _ = std::fs::remove_dir_all(&d);
}

/// Show Only Changed Files: the list keeps what git marks as changed and the folders
/// with changes inside, and everything comes back when it is turned off.
#[test]
fn only_changed_files_listed() {
    let d = tmpdir("only-changed");
    work_repo(&d);
    for f in ["src/app.rs", "src/lib.rs", "docs/guide.md", "README.md", "LICENSE"] {
        touch(&d.join(f), "x\n");
    }
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-qm", "Start"]);
    touch(&d.join("src/lib.rs"), "y\n");
    touch(&d.join("NEW.txt"), "new\n");
    let r = crate::gitstatus::query(&d).expect("a repository");
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let names = |t: &Tab| t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&t), ["docs", "src", "LICENSE", "NEW.txt", "README.md"]);
    t.git_only = Some(std::sync::Arc::new(r.marks.keys().cloned().collect()));
    t.dirty = true;
    pump(&mut t, &stats);
    assert_eq!(names(&t), ["src", "NEW.txt"]);
    t.git_only = None;
    t.dirty = true;
    pump(&mut t, &stats);
    assert_eq!(names(&t).len(), 5);
}

/// Formulas found in Markdown as GitHub writes them: `$…$` in a sentence (but not
/// "$5 and $10"), `$$…$$` and `\(…\)`, blocks between `$$` or `\[` lines and fenced
/// as `math`; code spans and escaped dollars left alone.
#[test]
fn formulas_found_in_markdown() {
    use crate::markdown::{Block, MATH, parse};
    use crate::texmath::{block_at, inline_at};
    assert_eq!(inline_at("$x^2$ is", 0), Some((5, "x^2".into(), false)));
    assert_eq!(inline_at("costs $5 and $10", 6), None, "money");
    assert_eq!(inline_at("$ x$", 0), None, "a space after the opening one");
    assert_eq!(inline_at("$x $", 0), None, "a space before the closing one");
    assert_eq!(inline_at("$a\\$b$", 0), Some((6, "a\\$b".into(), false)), "an escaped dollar inside");
    assert_eq!(inline_at("$$\\sum_i x_i$$", 0), Some((14, "\\sum_i x_i".into(), true)));
    assert_eq!(inline_at("\\(\\alpha\\)", 0), Some((10, "\\alpha".into(), false)));
    let lines = ["$$", "\\int_0^1 x\\,dx", "= \\tfrac12", "$$", "after"];
    assert_eq!(block_at(&lines, 0), Some(("\\int_0^1 x\\,dx\n= \\tfrac12".into(), 4)));
    assert_eq!(block_at(&["\\[ e^{i\\pi} + 1 = 0 \\]"], 0), Some(("e^{i\\pi} + 1 = 0".into(), 1)));
    assert_eq!(block_at(&["$$ unclosed"], 0), None);
    let md: Vec<String> = "Energy is $E=mc^2$, not `$code$`, and \\$4.\n\n$$\na^2+b^2=c^2\n$$\n\n```math\n\\frac{1}{2}\n```\n".lines().map(String::from).collect();
    let doc = parse(&md, None);
    assert_eq!(doc.maths, vec![("E=mc^2".to_string(), false), ("a^2+b^2=c^2".into(), true), ("\\frac{1}{2}".into(), true)]);
    let Block::Para(runs) = &doc.blocks[0] else { panic!("{:?}", doc.blocks[0]) };
    let m: Vec<(&str, u32)> = runs.iter().filter(|r| r.st & MATH != 0).map(|r| (r.text.as_str(), r.link)).collect();
    assert_eq!(m, [("E=mc^2", 1)]);
    let text: String = runs.iter().filter(|r| r.st & MATH == 0).map(|r| r.text.as_str()).collect();
    assert!(text.contains("$code$") && text.ends_with("and $4."), "{text}");
    assert!(matches!(doc.blocks[1], Block::Math(2)) && matches!(doc.blocks[2], Block::Math(3)));
    assert_eq!(crate::texmath::depths("Preview: Snippet 1 ended.(368640+1505299x1347574).\nother\nPreview: Snippet 2 ended.(10+0x5)."), vec![(368640.0, 1505299.0), (10.0, 0.0)]);
}

/// A stand-in for pdflatex: a real PDF with a page per formula and a log in the preview
/// package's words; a formula with `\broken` in it fails as TeX does. It notes each run.
pub(crate) fn fake_tex(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    for n in 1..=4 {
        std::fs::write(dir.join(format!("pages-{n}.pdf")), crate::sim::sized_pdf(&vec!["0 0 0"; n], [40, 14], [1, 1, 38, 12])).unwrap();
    }
    let script = format!(
        "#!/bin/sh\necho run >> {calls}\nif grep -q 'broken' f.tex; then echo '! Undefined control sequence.' > f.log; exit 1; fi\nn=$(grep -c 'begin{{preview}}' f.tex)\ncp {dir}/pages-$n.pdf f.pdf\n: > f.log\ni=1\nwhile [ $i -le $n ]; do echo \"Preview: Snippet $i ended.(458752+196608x1000000).\" >> f.log; i=$((i+1)); done\n",
        calls = dir.join("calls").display(),
        dir = dir.display()
    );
    std::fs::write(dir.join("pdflatex"), script).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(dir.join("pdflatex"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::os::unix::fs::symlink("/usr/bin/pdftocairo", dir.join("pdftocairo"));
}

/// Formulas typeset through a TeX and poppler's pdftocairo: one run for them all, each
/// a white picture with its depth from the log; one TeX can't read shows why while the
/// rest are typeset; the second time they come from the cache without TeX.
#[test]
fn formulas_typeset_through_tex_and_poppler() {
    use crate::texmath::{Engine, engine, typeset};
    if !crate::media::have("pdftocairo") {
        return;
    }
    let d = tmpdir("texmath");
    fake_tex(&d.join("tools"));
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.join("tools")));
    let tools = crate::drives::Tools::current().unwrap();
    assert_eq!(engine(&tools), Some(Engine::Pdflatex));
    // Unique to this run, so nothing comes from an earlier one's cache.
    let n = format!("{:?}", std::time::Instant::now());
    let items = vec![(format!("x^2 % {n}"), false), (format!("\\int_0^1 f % {n}"), true)];
    let got = typeset(&tools, Engine::Pdflatex, &items, &|| false);
    let runs = || std::fs::read_to_string(d.join("tools/calls")).unwrap_or_default().lines().count();
    assert_eq!(runs(), 1, "one TeX run for the lot");
    for g in &got {
        let t = g.as_ref().unwrap();
        assert!(t.img.w > 100 && t.img.h > 30);
        assert!(t.img.px.chunks(4).all(|p| p[..3] == [255, 255, 255]), "white, to be tinted");
        assert!(t.img.px.chunks(4).any(|p| p[3] == 255), "with its ink solid");
        // Depth from the log: (3 pt + 1 pt border) of (7 + 3 + 2 border) pt.
        let share = t.depth / t.img.h as f32;
        assert!((share - 4.0 / 12.0).abs() < 0.01, "{share}");
    }
    // From the cache: no TeX.
    let again = typeset(&tools, Engine::Pdflatex, &items, &|| false);
    assert!(again.iter().all(|g| g.is_ok()));
    assert_eq!(runs(), 1);
    // One broken among good ones: the batch fails, each is tried alone.
    let mixed = vec![(format!("a+b % {n}"), false), (format!("\\broken % {n}"), false)];
    let got = typeset(&tools, Engine::Pdflatex, &mixed, &|| false);
    assert!(got[0].is_ok());
    assert_eq!(got[1].as_ref().err().map(String::as_str), Some("TeX: Undefined control sequence"));
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&d);
}

/// The palette's search over the real commands: a whole title first, words started in
/// any order, letters from the starts of words, the category's name, an alias typed in
/// full ahead of everything; scattered letters find nothing.
#[test]
fn palette_finds_commands() {
    use crate::commands::Cmd;
    use crate::popups::find_commands;
    let mut ud = crate::config::UserData::with_defaults();
    let find = |ud: &crate::config::UserData, q: &str| find_commands(ud, q);
    assert_eq!(find(&ud, "Toggle Hidden Files").first(), Some(&Cmd::ToggleHidden));
    assert_eq!(find(&ud, "hid tog"), vec![Cmd::ToggleHidden]);
    assert_eq!(find(&ud, "tohi").first(), Some(&Cmd::ToggleHidden));
    assert_eq!(&find(&ud, "sort size")[..3], &[Cmd::SortSize, Cmd::SortSizeAsc, Cmd::SortSizeDesc]);
    // GitHub Profile starts with the word too: the git commands follow it in their order.
    let git = find(&ud, "git");
    assert_eq!(&git[..4], &[Cmd::GitHubProfile, Cmd::GitBranches, Cmd::GitHistory, Cmd::GitChanges]);
    assert_eq!(find(&ud, "github").first(), Some(&Cmd::GitHubProfile));
    assert!(!git.contains(&Cmd::Quit), "no G from General and I, T from Quit");
    // Every View command answers to the category's name, after the titles with the word.
    let view = find(&ud, "view");
    assert!(view[..7].iter().all(|c| c.info().title.starts_with("View ")) && view[..7].contains(&Cmd::ViewDetails));
    assert!(view.contains(&Cmd::ZoomIn));
    assert!(find(&ud, "zqx").is_empty());
    crate::popups::set_alias(&mut ud, Cmd::ToggleSidebar, "sb").unwrap();
    assert_eq!(find(&ud, "SB").first(), Some(&Cmd::ToggleSidebar));
}

/// Recent Projects on a real home folder: repositories found at any sensible depth, a
/// work tree's `.git` file followed to its branch, nothing found inside a repository,
/// in hidden or installed-package folders, through links or deeper than five levels;
/// the branch read from HEAD, detached ones as a short commit; the last visited first,
/// then by git's own activity; the list kept in the cache and read back.
#[test]
fn projects_found_ranked_and_cached() {
    use crate::projects::{branch_of, find, load_cache, rank, read, save_cache};
    let h = tmpdir("projects");
    let repo = |p: &str, head: &str| {
        touch(&h.join(p).join(".git/HEAD"), head);
        touch(&h.join(p).join(".git/index"), "");
    };
    repo("Code/maia", "ref: refs/heads/main\n");
    repo("Work/client/site", "3f2a9c41d7e8b0a1c2d3e4f5a6b7c8d9e0f1a2b3\n");
    repo("Code/maia/vendor/lib", "ref: refs/heads/main\n");
    repo(".hidden/secret", "ref: refs/heads/main\n");
    touch(&h.join("web/package.json"), "{}\n");
    repo("web/node_modules/pkg", "ref: refs/heads/main\n");
    repo("a/b/c/d/e/f/deep", "ref: refs/heads/main\n");
    repo("src/github.com/ana/tool", "ref: refs/heads/fix/menu\n");
    // A second work tree of maia: its .git is a file naming the real one.
    touch(&h.join("Code/maia/.git/worktrees/spike/HEAD"), "ref: refs/heads/spike\n");
    touch(&h.join("Code/spike/.git"), &format!("gitdir: {}\n", h.join("Code/maia/.git/worktrees/spike").display()));
    std::os::unix::fs::symlink(h.join("Code"), h.join("Code link")).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut found: Vec<String> = find(&h, &stop).iter().map(|p| crate::project::relative(p, &h)).collect();
    found.sort();
    assert_eq!(found, ["Code/maia", "Code/spike", "Work/client/site", "src/github.com/ana/tool"]);
    assert_eq!(branch_of("ref: refs/heads/fix/menu\n"), "fix/menu");
    assert_eq!(branch_of("3f2a9c41d7e8b0a1c2d3e4f5a6b7c8d9e0f1a2b3\n"), "3f2a9c4");
    assert_eq!(read(&h.join("Code/spike")).unwrap().branch, "spike", "a work tree's own branch");
    assert_eq!(read(&h.join("Work/client/site")).unwrap().branch, "3f2a9c4");
    assert!(read(&h.join("web")).is_none());
    // Activity: the site was committed to last; maia an hour ago, the tool a day ago.
    let ago = |p: &str, secs: u64| {
        let t = std::time::SystemTime::now() - Duration::from_secs(secs);
        for f in ["HEAD", "index"] {
            let f = std::fs::File::options().write(true).open(h.join(p).join(".git").join(f));
            if let Ok(f) = f {
                f.set_modified(t).unwrap();
            }
        }
    };
    ago("Code/maia", 3600);
    ago("src/github.com/ana/tool", 86_400);
    ago("Work/client/site", 60);
    std::fs::File::options().write(true).open(h.join("Code/maia/.git/worktrees/spike/HEAD")).unwrap().set_modified(std::time::SystemTime::now() - Duration::from_secs(7 * 86_400)).unwrap();
    let all: Vec<_> = find(&h, &stop).iter().filter_map(|p| read(p)).collect();
    let names = |v: &[crate::projects::Project]| v.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&rank(all.clone(), &[])), ["site", "maia", "tool", "spike"], "by activity");
    // Visited: the tool's subfolder last, maia before it; the rest by activity after them.
    let recents = vec![h.join("src/github.com/ana/tool/src"), h.join("Documents"), h.join("Code/maia")];
    assert_eq!(names(&rank(all.clone(), &recents)), ["tool", "maia", "site", "spike"]);
    // The cache: the roots, in order, read back.
    let cache = h.join("cache/maia/projects.txt");
    let roots: Vec<PathBuf> = rank(all, &recents).iter().map(|p| p.root.clone()).collect();
    save_cache(&cache, &roots).unwrap();
    assert_eq!(load_cache(&cache), roots);
    assert!(load_cache(&h.join("cache/none.txt")).is_empty());
    let _ = std::fs::remove_dir_all(&h);
}

/// Unsaved work asked of git itself: a clean work tree has none; an untracked file, an
/// edit and a staged file each count; commits not pushed to its remote and stashes count
/// too, and a commit to pull is told apart from one to push.
#[test]
fn projects_unsaved_work_asked_of_git() {
    use crate::projects::{Status, parse_status, status};
    if !crate::media::have("git") {
        return;
    }
    let base = tmpdir("project-changes");
    let (server, d, other) = (base.join("server.git"), base.join("work"), base.join("other"));
    std::fs::create_dir_all(&server).unwrap();
    git(&server, &["init", "-q", "--bare", "-b", "main"]);
    std::fs::create_dir_all(&d).unwrap();
    touch(&d.join("README"), "hello\n");
    git(&d, &["init", "-q", "-b", "main"]);
    for (k, v) in [("user.name", "Test"), ("user.email", "test@example.invalid"), ("commit.gpgsign", "false")] {
        git(&d, &["config", k, v]);
    }
    git(&d, &["add", "."]);
    git(&d, &["commit", "-q", "-m", "first"]);
    git(&d, &["remote", "add", "origin", server.to_str().unwrap()]);
    git(&d, &["push", "-q", "-u", "origin", "main"]);
    assert_eq!(status(&d), Some(Status { upstream: true, ..Default::default() }));
    assert!(!status(&d).unwrap().unsaved());
    touch(&d.join("new.txt"), "x\n");
    assert_eq!(status(&d).map(|s| s.changed), Some(1), "untracked");
    touch(&d.join("README"), "hello again\n");
    git(&d, &["add", "README"]);
    assert_eq!(status(&d).map(|s| s.changed), Some(2), "staged and untracked");
    git(&d, &["stash", "-q", "-u"]);
    let s = status(&d).unwrap();
    assert_eq!((s.changed, s.stashes), (0, 1), "stashed away");
    assert!(s.unsaved(), "a stash is unsaved work");
    git(&d, &["stash", "pop", "-q"]);
    git(&d, &["add", "-A"]);
    git(&d, &["commit", "-q", "-m", "second"]);
    let s = status(&d).unwrap();
    assert_eq!((s.changed, s.ahead, s.behind), (0, 1, 0), "not pushed");
    git(&d, &["push", "-q"]);
    // Someone else pushes: one to pull, nothing unsaved here.
    git(&base, &["clone", "-q", server.to_str().unwrap(), "other"]);
    for (k, v) in [("user.name", "Other"), ("user.email", "other@example.invalid"), ("commit.gpgsign", "false")] {
        git(&other, &["config", k, v]);
    }
    touch(&other.join("theirs.txt"), "t\n");
    git(&other, &["add", "."]);
    git(&other, &["commit", "-q", "-m", "theirs"]);
    git(&other, &["push", "-q"]);
    git(&d, &["fetch", "-q"]);
    let s = status(&d).unwrap();
    assert_eq!((s.ahead, s.behind, s.unsaved()), (0, 1, false));
    assert_eq!(status(&base.join("nowhere")), None);
    // A rename's record carries its old path, which is not a second change.
    let v2 = b"# branch.oid 1a2b\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +3 -2\02 R. N... 100644 100644 100644 aa bb R100 new name.txt\0old name.txt\0? scratch\0";
    assert_eq!(parse_status(v2), Status { changed: 2, ahead: 3, behind: 2, stashes: 0, upstream: true });
    let _ = std::fs::remove_dir_all(&base);
}

/// What's using a path, against processes this test starts: a `sleep` working in a
/// folder with a file open on its input, and a copy of `sleep` run from inside the
/// folder. Each is found with how it holds it; stopping one ends it, one that ignores
/// SIGTERM outlasts it and is ended by SIGKILL; nothing else is ever signalled.
#[test]
fn holders_of_files_and_folders() {
    use crate::holders::{Hold, OURS, REFUSED, alive, holding, info, stop};
    let Some(sleep) = ["/usr/bin/sleep", "/bin/sleep"].iter().map(PathBuf::from).find(|p| p.exists()) else { return };
    let d = tmpdir("holders");
    touch(&d.join("Photos/beach.jpg"), "jpeg");
    touch(&d.join("notes.txt"), "words\n");
    touch(&d.join("other.txt"), "else\n");
    std::fs::create_dir_all(d.join("bin")).unwrap();
    std::fs::copy(&sleep, d.join("bin/napper")).unwrap();
    let mut a = std::process::Command::new(&sleep).arg("30").current_dir(d.join("Photos")).stdin(std::fs::File::open(d.join("notes.txt")).unwrap()).spawn().unwrap();
    // A child another test forks while the copy is being written holds it open for
    // writing until it runs its own program: "text file busy" for a moment.
    let napper = || std::process::Command::new(d.join("bin/napper")).arg("30").current_dir(&d).stdin(std::process::Stdio::null()).spawn();
    let mut tries = 0;
    let mut b = loop {
        match napper() {
            Err(e) if e.raw_os_error() == Some(libc::ETXTBSY) && tries < 50 => {
                tries += 1;
                std::thread::sleep(Duration::from_millis(20));
            }
            r => break r.unwrap(),
        }
    };
    OURS.lock().unwrap().extend([a.id(), b.id()]);
    let pids = [a.id(), b.id(), std::process::id()];
    let t0 = Instant::now();
    while (info(a.id()).is_none_or(|p| p.name != "sleep") || info(b.id()).is_none_or(|p| p.name != "napper")) && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pa = info(a.id()).unwrap();
    assert_eq!(pa.name, "sleep");
    assert!(pa.mine());
    let of = |paths: &[PathBuf]| -> Vec<(u32, Hold)> { holding(paths, &pids).into_iter().map(|(p, h)| (p.pid, h)).collect() };
    assert_eq!(of(&[d.join("Photos")]), [(a.id(), Hold::Inside(d.join("Photos")))]);
    assert_eq!(of(&[d.join("notes.txt")]), [(a.id(), Hold::Open(d.join("notes.txt")))]);
    assert_eq!(of(&[d.join("bin")]), [(b.id(), Hold::Runs(d.join("bin/napper")))]);
    assert!(of(&[d.join("other.txt")]).is_empty());
    let both = of(&[d.clone()]);
    assert_eq!(both.len(), 2, "{both:?}");
    assert_eq!(info(b.id()).unwrap().name, "napper");
    // Stopped: SIGTERM ends it (a zombie until reaped counts as gone).
    assert!(stop(&pa, libc::SIGTERM, Duration::from_secs(3)));
    let _ = a.wait();
    assert!(!alive(&pa));
    // A process ignoring SIGTERM is still there; SIGKILL ends it.
    let mut c = std::process::Command::new("sh").args(["-c", "trap '' TERM; exec sleep 30"]).current_dir(&d).spawn().unwrap();
    OURS.lock().unwrap().push(c.id());
    let t0 = Instant::now();
    while info(c.id()).is_none_or(|p| p.name != "sleep") && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(10));
    }
    let pc = info(c.id()).unwrap();
    assert!(!stop(&pc, libc::SIGTERM, Duration::from_millis(600)), "outlasts SIGTERM");
    assert!(alive(&pc));
    assert!(stop(&pc, libc::SIGKILL, Duration::from_secs(2)));
    let _ = c.wait();
    // Anything the test didn't start is never signalled, Maia's own process included.
    let me = info(std::process::id()).unwrap();
    assert!(!stop(&me, libc::SIGTERM, Duration::from_millis(100)));
    assert!(REFUSED.lock().unwrap().contains(&(std::process::id(), libc::SIGTERM)));
    // A PID that went to another process isn't signalled either.
    let pb = info(b.id()).unwrap();
    let stale = crate::holders::Proc { start: pb.start + 1, ..pb.clone() };
    assert!(stop(&stale, libc::SIGTERM, Duration::from_millis(100)), "the one asked about is gone");
    assert!(alive(&pb), "and the one there now is left alone");
    assert!(stop(&pb, libc::SIGTERM, Duration::from_secs(3)));
    let _ = b.wait();
    assert!(crate::holders::is_busy(&std::io::Error::from_raw_os_error(libc::EBUSY)));
    assert!(!crate::holders::is_busy(&std::io::Error::from_raw_os_error(libc::EACCES)));
    assert!(crate::holders::says_busy("Error unmounting /dev/sdx1: GDBus.Error:org.freedesktop.UDisks2.Error.DeviceBusy: Error unmounting /dev/sdx1: target is busy"));
    assert!(!crate::holders::says_busy("Wrong fs type, bad option"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Ports from the kernel's own tables: a TCP listener and a UDP socket bound by this test
/// are listed with their numbers and addresses, a connected socket isn't; the owner is
/// found through the socket's inode, and a port held by a child only is the child's. Then
/// captured lines: IPv6 any, a v4 address mapped into v6, a socket that isn't listening.
#[test]
fn ports_and_their_owners() {
    use crate::holders::{OURS, listening, parse_table, socket_owners, stop};
    let tcp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let udp = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let (tp, up) = (tcp.local_addr().unwrap().port(), udp.local_addr().unwrap().port());
    let client = std::net::TcpStream::connect(("127.0.0.1", tp)).unwrap();
    let cp = client.local_addr().unwrap().port();
    let all = listening();
    let t = all.iter().find(|p| p.port == tp && p.tcp).unwrap_or_else(|| panic!("{tp} not in {all:?}"));
    assert_eq!(t.addr, std::net::IpAddr::from([127, 0, 0, 1]));
    assert!(all.iter().any(|p| p.port == up && !p.tcp), "udp {up}");
    assert!(!all.iter().any(|p| p.port == cp), "a connection isn't a listener");
    let me = std::process::id();
    let owners = socket_owners(&HashSet::from([t.inode]), &[me]);
    assert_eq!(owners.get(&t.inode), Some(&vec![me]));
    // Handed to a child and closed here: the child's alone.
    let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let hp = held.local_addr().unwrap().port();
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;
    // Only this child gets it: the listener stays close-on-exec for any other.
    let fd = held.as_raw_fd();
    let mut cmd = std::process::Command::new("sleep");
    cmd.arg("30");
    unsafe {
        cmd.pre_exec(move || if libc::dup2(fd, 200) < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) });
    }
    let mut child = cmd.spawn().unwrap();
    drop(held);
    OURS.lock().unwrap().push(child.id());
    let port = listening().into_iter().find(|p| p.port == hp).expect("still listening in the child");
    let owners = socket_owners(&HashSet::from([port.inode]), &[me, child.id()]);
    assert_eq!(owners.get(&port.inode), Some(&vec![child.id()]));
    let proc = crate::holders::info(child.id()).unwrap();
    assert!(stop(&proc, libc::SIGTERM, Duration::from_secs(3)));
    let _ = child.wait();
    assert!(!listening().iter().any(|p| p.port == hp), "gone with it");
    let captured6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   0: 00000000000000000000000000000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 52345 1 0000000000000000 100 0 0 10 0\n   1: 0000000000000000FFFF00000100007F:0050 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 52346 1 0000000000000000 100 0 0 10 0\n   2: 0000000000000000FFFF00000100007F:A3B2 0000000000000000FFFF00000100007F:1F90 01 00000000:00000000 00:00000000 00000000  1000        0 52347 1 0000000000000000 20 4 30 10 -1\n";
    let v = parse_table(captured6, true);
    assert_eq!(v.len(), 2);
    assert_eq!((v[0].port, v[0].addr, v[0].uid, v[0].inode), (8080, std::net::IpAddr::from(std::net::Ipv6Addr::UNSPECIFIED), 1000, 52345));
    assert_eq!((v[1].port, v[1].addr, v[1].uid), (80, std::net::IpAddr::from([127, 0, 0, 1]), 0));
    let captured_udp = "   sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n  981: 00000000:14E9 00000000:0000 07 00000000:00000000 00:00000000 00000000   975        0 9921 2 0000000000000000 0\n  982: 0100A8C0:D2A1 0101A8C0:0035 01 00000000:00000000 00:00000000 00000000  1000        0 9922 2 0000000000000000 0\n";
    let v = parse_table(captured_udp, false);
    assert_eq!(v.len(), 1, "the connected one is left out");
    assert_eq!((v[0].port, v[0].addr), (5353, std::net::IpAddr::from([0, 0, 0, 0])));
    drop((tcp, udp, client));
}

/// Smart folders: names by part or by pattern, kinds, age and size, tested on a real
/// folder: hidden folders, installed packages and links left out; a search's filter
/// turned into one; sizes as people write them; the dialog's fields checked; and kept in
/// the user data file, where an older file without them still loads and a kind from a
/// later version doesn't stop it loading.
#[test]
fn smart_folders_find_what_they_say() {
    use crate::smart::{SmartFolder, glob, search};
    let d = tmpdir("smart");
    let old = std::time::SystemTime::now() - Duration::from_secs(86_400 * 90);
    let file = |p: &str, len: usize, aged: bool| {
        let p = d.join(p);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, vec![b'x'; len]).unwrap();
        if aged {
            std::fs::File::options().write(true).open(&p).unwrap().set_modified(old).unwrap();
        }
    };
    file("Invoices/2026/invoice-031.pdf", 2_000_000, false);
    file("Invoices/2026/Invoice-030.PDF", 1000, false);
    file("Invoices/2025/invoice-001.pdf", 3_000_000, true);
    file("Invoices/notes.txt", 10, false);
    file("Photos/IMG_0001.jpg", 500_000, false);
    file("Photos/IMG_0002.png", 400_000, true);
    file("Photos/holiday.mp4", 9_000_000, false);
    file(".cache/invoice-cached.pdf", 10, false);
    file("web/package.json", 2, false);
    file("web/node_modules/pdfjs/invoice-test.pdf", 10, false);
    std::fs::create_dir_all(d.join("Photos/invoice-folder.pdf")).unwrap();
    std::os::unix::fs::symlink(d.join("Invoices"), d.join("Links")).unwrap();
    let found = |sf: &SmartFolder| -> Vec<String> {
        let mut v = vec![];
        let (stop, cut) = (std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false));
        search(sf, &mut |b| {
            v.extend(b.into_iter().map(|e| e.rel().map(|r| r.into_owned()).unwrap_or_default()));
            true
        }, &stop, &cut);
        v.sort();
        v
    };
    let sf = |has: &str, kinds: &[&str], days: Option<u32>, bigger: Option<u64>| SmartFolder { id: 1, name: "x".into(), root: d.clone(), name_has: has.into(), kinds: kinds.iter().map(|k| k.to_string()).collect(), days, bigger, ..Default::default() };
    assert_eq!(found(&sf("invoice", &[], None, None)), ["Invoices/2025/invoice-001.pdf", "Invoices/2026/Invoice-030.PDF", "Invoices/2026/invoice-031.pdf"], "any case; no folders, hidden, installed or linked copies");
    assert_eq!(found(&sf("invoice-0??.pdf", &[], None, None)), ["Invoices/2025/invoice-001.pdf", "Invoices/2026/Invoice-030.PDF", "Invoices/2026/invoice-031.pdf"]);
    assert_eq!(found(&sf("IMG_*", &[], None, None)), ["Photos/IMG_0001.jpg", "Photos/IMG_0002.png"]);
    assert_eq!(found(&sf("", &["pictures"], None, None)), ["Photos/IMG_0001.jpg", "Photos/IMG_0002.png"]);
    assert_eq!(found(&sf("", &["pictures", "videos"], Some(30), None)), ["Photos/IMG_0001.jpg", "Photos/holiday.mp4"], "the old picture left out");
    assert_eq!(found(&sf("", &["pdfs"], Some(30), Some(1_000_000))), ["Invoices/2026/invoice-031.pdf"]);
    assert_eq!(found(&sf("", &["documents"], None, None)).len(), 4, "the PDFs and the notes");
    assert_eq!(found(&sf("", &["code"], None, None)), ["web/package.json"], "not the installed package's");
    assert!(glob(&"a*b?c".chars().collect::<Vec<_>>(), &"axxbyc".chars().collect::<Vec<_>>()));
    assert!(!glob(&"a*b?c".chars().collect::<Vec<_>>(), &"axxbc".chars().collect::<Vec<_>>()));
    assert!(glob(&"*".chars().collect::<Vec<_>>(), &[]));
    // From a search in the filter: all of it.
    let from = crate::smart::from_filter(&d, " invoice size>1M modified<30d ");
    assert_eq!((from.name.as_str(), from.query.as_str()), ("invoice", "invoice size>1M modified<30d"));
    assert_eq!(found(&from), ["Invoices/2026/invoice-031.pdf"]);
    assert_eq!(crate::smart::from_filter(&d.join("Photos"), "size>=2k").name, "Photos");
    // The whole language, tested by the filter's own code.
    let q = |text: &str| SmartFolder { id: 2, name: "q".into(), root: d.clone(), query: text.into(), ..Default::default() };
    assert_eq!(found(&q("ext:pdf -invoice-001")), ["Invoices/2026/Invoice-030.PDF", "Invoices/2026/invoice-031.pdf"]);
    assert_eq!(found(&q("type:pictures | size>5M")), ["Photos/IMG_0001.jpg", "Photos/IMG_0002.png", "Photos/holiday.mp4"]);
    assert_eq!(found(&q(r"/^img_\d+\.jpg$/ | (ext:txt notes)")), ["Invoices/notes.txt", "Photos/IMG_0001.jpg"]);
    assert_eq!(found(&q("2026 ext:pdf")), ["Invoices/2026/Invoice-030.PDF", "Invoices/2026/invoice-031.pdf"], "words see the path below the folder");
    let me = std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(d.join("Invoices/notes.txt")).unwrap());
    assert_eq!(found(&q(&format!("owner:{me} ext:txt"))), ["Invoices/notes.txt"]);
    if crate::tags::write(&d.join("Photos/IMG_0002.png"), &["keep".into()]).is_ok() {
        assert_eq!(found(&q("tag:keep | ext:mp4")), ["Photos/IMG_0002.png", "Photos/holiday.mp4"], "tags read from the files");
    }
    // An older smart folder's fields, translated into the query they amount to: the
    // same files as above, and what the dialog shows.
    let old = sf("invoice", &["pdfs", "holograms"], Some(30), Some(1_000_000));
    assert_eq!(old.query_text(), "name:invoice type:pdfs modified<30d size>1M");
    assert_eq!(found(&old), ["Invoices/2026/invoice-031.pdf"]);
    assert_eq!(sf("IMG_*", &[], None, Some(1500)).query_text(), "name:IMG_* size>1500");
    assert_eq!(SmartFolder { query: "a | b".into(), ..sf("my scan", &[], Some(2), None) }.query_text(), r#"name:"my scan" modified<2d (a | b)"#);
    assert_eq!(sf("", &[], None, Some(1_048_576)).query_text(), "size>1048576", "not a whole number of thousands: the bytes");
    assert_eq!(sf("", &[], None, Some(3_000_000_000)).query_text(), "size>3G");
    assert!(old.summary().ends_with("“name:invoice type:pdfs modified<30d size>1M”"), "{}", old.summary());
    // The dialog's fields: what it saves is the query alone.
    let crate::popups::Popup::SmartEdit(mut e) = crate::smart::edit(&old, true) else { panic!() };
    assert_eq!(e.query, "name:invoice type:pdfs modified<30d size>1M");
    let saved = e.build().unwrap();
    assert_eq!(saved, SmartFolder { id: 1, name: "x".into(), root: d.clone(), query: e.query.clone(), ..Default::default() });
    assert_eq!(found(&saved), found(&old));
    e.query = "invoice /(/".into();
    assert_eq!(e.build().unwrap_err(), "Find: /(/ is not a regular expression");
    e.query = "  ".into();
    assert_eq!(e.build().unwrap().query, "", "every file is a smart folder too");
    e.root = d.join("nowhere").display().to_string();
    assert!(e.build().unwrap_err().starts_with("There is no folder"));
    e.root = d.display().to_string();
    e.name = "  ".into();
    assert!(e.build().is_err());
    // Kept in the user data file; older files load.
    let path = d.join("userdata.json");
    let mut ud = crate::config::UserData::with_defaults();
    ud.smart.push(sf("invoice", &["pdfs"], Some(30), None));
    ud.smart.push(q("ext:pdf | tag:keep"));
    crate::config::save_json(&path, &ud).unwrap();
    let back: crate::config::UserData = crate::config::load_json(&path);
    assert_eq!(back.smart, ud.smart);
    std::fs::write(&path, r#"{"bookmarks": ["/tmp"], "recents": []}"#).unwrap();
    let older: crate::config::UserData = crate::config::load_json(&path);
    assert_eq!((older.bookmarks.len(), older.smart.len()), (1, 0));
    std::fs::write(&path, r#"{"bookmarks": ["/tmp"], "smart": [{"id": 4, "name": "Scans", "root": "/tmp", "kinds": ["pdfs", "holograms"], "colour": "red"}]}"#).unwrap();
    let later: crate::config::UserData = crate::config::load_json(&path);
    assert_eq!((later.bookmarks.len(), later.smart[0].name.as_str(), later.smart[0].kinds.len()), (1, "Scans", 2));
    let _ = std::fs::remove_dir_all(&d);
}

/// Fake snapshot trees: Snapper's numbered snapshots with info.xml, Timeshift's dated
/// btrfs and rsync snapshots with info.json, each found through a mount table written
/// the way /proc/self/mountinfo is.
pub fn snapshot_fixture(d: &Path) -> (PathBuf, String) {
    let vol = d.join("vol");
    let file = vol.join("docs/report.txt");
    let at = |p: &Path, days: u64| {
        let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_792_001 - days * 86_400);
        std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
    };
    touch(&file, "fourth draft, the one in use\n");
    at(&file, 0);
    let snap = |n: u32, date: &str, desc: &str, text: Option<(&str, u64)>| {
        let s = vol.join(format!(".snapshots/{n}"));
        std::fs::create_dir_all(s.join("snapshot/docs")).unwrap();
        std::fs::write(s.join("info.xml"), format!("<?xml version=\"1.0\"?>\n<snapshot>\n  <type>single</type>\n  <num>{n}</num>\n  <date>{date}</date>\n  <description>{desc}</description>\n  <cleanup>timeline</cleanup>\n</snapshot>\n")).unwrap();
        if let Some((t, days)) = text {
            let f = s.join("snapshot/docs/report.txt");
            std::fs::write(&f, t).unwrap();
            at(&f, days);
        }
    };
    // 5 is the file as it is now; 4 has no such file; 2 holds what 3 does under an
    // older time; 1 is as long as 3 with other words in it.
    snap(5, "2026-10-12 06:00:00", "timeline", Some(("fourth draft, the one in use\n", 0)));
    snap(4, "2026-10-11 06:00:00", "timeline", None);
    snap(3, "2026-10-10 06:00:00", "before &amp; after", Some(("third\n", 2)));
    snap(2, "2026-10-09 06:00:00", "", Some(("third\n", 3)));
    snap(1, "2026-10-08 06:00:00", "first", Some(("frist\n", 4)));
    std::fs::create_dir_all(vol.join(".snapshots/0/snapshot/docs")).unwrap();
    std::fs::write(vol.join(".snapshots/0/snapshot/docs/report.txt"), "zero").unwrap();
    // Timeshift on btrfs: the file system's top mounted beside it.
    let ts = d.join("top/timeshift-btrfs/snapshots/2026-10-07_09-00-00");
    touch(&ts.join("@home/docs/report.txt"), "an old btrfs copy\n");
    std::fs::write(ts.join("info.json"), r#"{"created" : "1791363600", "sys-uuid" : "x", "tags" : "D", "comments" : "before update"}"#).unwrap();
    // Timeshift with rsync, on a backup disk.
    let rs = d.join("backup/timeshift/snapshots/2026-10-06_10-00-00");
    touch(&rs.join("localhost").join(file.strip_prefix("/").unwrap()), "rsync copy, the oldest\n");
    std::fs::write(rs.join("info.json"), r#"{"tags" : "W M"}"#).unwrap();
    let esc = |p: &Path| p.display().to_string().replace(' ', "\\040");
    let table = format!(
        "40 1 0:33 /@ / rw,noatime shared:1 - btrfs /dev/sda2 rw,subvolid=256,subvol=/@\n\
         38 40 0:7 / /dev rw,nosuid shared:2 - devtmpfs devtmpfs rw,size=16195776k\n\
         60 40 0:50 /@home {} rw,noatime shared:9 - btrfs /dev/sdb1 rw,subvolid=257,subvol=/@home\n\
         61 40 0:50 / {} rw,noatime shared:10 - btrfs /dev/sdb1 rw,subvolid=5,subvol=/\n\
         62 40 8:17 / {} rw,relatime shared:11 - ext4 /dev/sdc1 rw\n\
         63 40 0:60 / {}/net rw shared:12 - nfs4 nas:/x rw\n",
        esc(&vol),
        esc(&d.join("top")),
        esc(&d.join("backup")),
        esc(d)
    );
    (file, table)
}

/// A folder for Show Space Used: a folder of 500,000 bytes in two levels, a file of
/// 400,000, a folder of 10 bytes, an empty one, and a link to the big folder.
pub fn space_fixture(d: &Path) {
    std::fs::create_dir_all(d.join("Big/Sub")).unwrap();
    std::fs::write(d.join("Big/a.bin"), vec![1u8; 300_000]).unwrap();
    std::fs::write(d.join("Big/Sub/b.bin"), vec![2u8; 200_000]).unwrap();
    std::fs::write(d.join("movie.mkv"), vec![3u8; 400_000]).unwrap();
    std::fs::create_dir_all(d.join("Small")).unwrap();
    std::fs::write(d.join("Small/c.txt"), "ten bytes!").unwrap();
    std::fs::create_dir_all(d.join("Empty")).unwrap();
    let _ = std::os::unix::fs::symlink(d.join("Big"), d.join("big-link"));
}

#[test]
fn space_used_lists_biggest_first_as_sizes_land() {
    use crate::spaceuse::State;
    let d = tmpdir("space");
    space_fixture(&d);
    let ctx = egui::Context::default();
    let mut stats = crate::stats::Stats::new(ctx.clone());
    let mut st = State::new(d.clone(), &ctx);
    let t0 = Instant::now();
    while st.counting() {
        assert!(t0.elapsed() < Duration::from_secs(10), "sizes never came");
        stats.pump();
        st.pump(&mut stats, &ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    let shown: Vec<(&str, Option<u64>)> = st.order.iter().map(|&k| (st.items[k as usize].entry.name.as_str(), st.items[k as usize].size)).collect();
    assert_eq!(shown, vec![("Big", Some(500_000)), ("movie.mkv", Some(400_000)), ("Small", Some(10)), ("big-link", Some(0)), ("Empty", Some(0))], "folders by everything under them, a link by itself");
    assert_eq!(st.total(), 900_010);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn picture_viewer_steps_fits_and_fades_its_name() {
    use crate::viewer::{Pic, State, fit_scale, is_picture};
    assert!(["jpg", "jpeg", "png", "webp", "gif", "bmp", "svg"].iter().all(|e| is_picture(e)));
    assert!(!is_picture("txt") && !is_picture("mp4") && !is_picture("pdf"));
    let pics: Vec<Pic> = (0..3).map(|i| Pic { path: PathBuf::from(format!("/p/{i}.png")), name: format!("{i}.png"), modified: None }).collect();
    let mut st = State::new(1, pics, 1, 10.0);
    assert_eq!(st.label_alpha(11.0, true), 1.0, "shown while the pointer has just moved");
    assert!((st.label_alpha(12.2, true) - 0.5).abs() < 1e-3, "fading out");
    assert_eq!(st.label_alpha(13.0, true), 0.0, "then gone");
    assert_eq!(st.label_alpha(12.1, false), 0.0, "without animations it goes at once");
    st.zoom = 3.0;
    st.go(2, 20.0, true);
    assert_eq!((st.at, st.zoom, st.fading_from()), (2, 1.0, Some(1)), "the next fits the window, the last fading out");
    st.go(5, 21.0, true);
    assert_eq!(st.at, 2, "no picture past the last");
    st.go(0, 22.0, false);
    assert_eq!((st.at, st.fading_from()), (0, None), "no fade without animations");
    let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 500.0));
    assert_eq!(fit_scale(area, [4000, 1000]), 0.25, "a wide picture by its width");
    assert_eq!(fit_scale(area, [1000, 2000]), 0.25, "a tall one by its height");
    assert_eq!(fit_scale(area, [40, 30]), 1.0, "a small one never blown up");
}

/// A photograph turned on its side by its EXIF tag reaches the viewer upright: the
/// thumbnail cache's decode, as the viewer asks for it.
#[test]
fn viewer_pictures_decode_upright_off_the_frame() {
    let d = tmpdir("viewer-exif");
    // A JPEG 64 × 32 as stored whose tag says it is turned a quarter (orientation 6).
    let Some(t) = imagemagick_tools(&d.join("bin")) else { return };
    let src = d.join("wide.png");
    std::fs::write(&src, png_bytes(64, 32, 2)).unwrap();
    let jpg = d.join("side.jpg");
    let ok = std::process::Command::new(t.join("magick")).arg(&src).arg(&jpg).status().is_ok_and(|s| s.success());
    if !ok {
        return;
    }
    let tagged = with_exif_orientation(&std::fs::read(&jpg).unwrap(), 6);
    std::fs::write(&jpg, tagged).unwrap();
    let ctx = egui::Context::default();
    let mut thumbs = crate::thumbs::Thumbs::new(ctx.clone(), 1);
    let mtime = std::fs::metadata(&jpg).unwrap().modified().ok();
    let t0 = Instant::now();
    let dims = loop {
        ctx.run_pass(egui::RawInput::default(), |ctx| {
            thumbs.pump(ctx);
        }).drop_without_applying_deltas();
        if let Some(l) = thumbs.get(&jpg, mtime, 1600.0) {
            break (l.dims, l.tex.size());
        }
        assert!(t0.elapsed() < Duration::from_secs(10) && !thumbs.failed(&jpg), "never decoded");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(dims, ([32, 64], [32, 64]), "upright, at its full size: 1600 px asked for, the bucket above it");
    let _ = std::fs::remove_dir_all(&d);
}

/// The shelf keeps one of each, reads the details of what was dropped on it and lets go
/// of what is gone, on its own thread.
#[test]
fn shelf_gathers_once_and_drops_what_vanished() {
    let d = tmpdir("shelf");
    touch(&d.join("a/report.md"), "# r");
    touch(&d.join("b/photo.png"), "png");
    std::fs::create_dir_all(d.join("c/folder")).unwrap();
    let mut sh = crate::shelf::Shelf::default();
    let items = vec![(d.join("a/report.md"), None), (d.join("b/photo.png"), None), (d.join("c/folder"), None)];
    assert_eq!(sh.add(items), 3);
    assert_eq!(sh.add(vec![(d.join("a/report.md"), None)]), 0, "once only");
    std::fs::remove_file(d.join("b/photo.png")).unwrap();
    let ctx = egui::Context::default();
    let t0 = Instant::now();
    while sh.items.len() == 3 || sh.items.iter().any(|i| i.entry.is_none()) {
        assert!(t0.elapsed() < Duration::from_secs(5), "never looked");
        sh.pump(&ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(sh.paths(), vec![d.join("a/report.md"), d.join("c/folder")], "the vanished one gone, the order kept");
    assert!(sh.items[1].entry.as_ref().is_some_and(|e| e.is_dir), "details read for the icon");
    sh.remove(&d.join("a/report.md"));
    assert_eq!(sh.paths(), vec![d.join("c/folder")]);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn mountinfo_lines_read_with_their_subvolumes() {
    use crate::versions::parse_mountinfo;
    let m = parse_mountinfo("40 1 0:33 /@ / rw,noatime shared:1 - btrfs /dev/sda2 rw,subvol=/@\n63 40 8:17 / /run/media/me/My\\040Disk rw,nosuid,nodev,relatime shared:40 - exfat /dev/sdc1 rw\nnot a line\n");
    assert_eq!(m.len(), 2);
    assert_eq!((m[0].point.as_path(), m[0].root.as_path(), m[0].dev.as_str(), m[0].fstype.as_str()), (Path::new("/"), Path::new("/@"), "0:33", "btrfs"));
    assert_eq!(m[1].point, PathBuf::from("/run/media/me/My Disk"));
    assert_eq!(m[1].fstype, "exfat");
    assert_eq!(crate::versions::parse_utc("2026-10-12 08:00:01"), Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_792_001)));
    assert_eq!(crate::versions::parse_utc("2000-02-29 23:59:59"), Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(951_868_799)));
    assert_eq!(crate::versions::parse_utc("2026-13-01 00:00:00"), None);
}

#[test]
fn previous_versions_from_snapper_and_timeshift_skip_the_unchanged() {
    use crate::versions::{Tool, find, parse_mountinfo};
    let d = tmpdir("versions");
    let (file, table) = snapshot_fixture(&d);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let f = find(&file, &parse_mountinfo(&table), &stop);
    let got: Vec<(String, String, Tool)> = f.versions.iter().map(|v| (std::fs::read_to_string(&v.entry.path).unwrap(), v.label.clone(), v.tool)).collect();
    let want = [("third\n", "before & after", Tool::Snapper), ("frist\n", "first", Tool::Snapper), ("an old btrfs copy\n", "before update", Tool::Timeshift), ("rsync copy, the oldest\n", "weekly, monthly", Tool::Timeshift)];
    assert_eq!(got, want.map(|(a, b, c)| (a.to_string(), b.to_string(), c)).to_vec(), "newest first; the copy like the file now, and the one like the next newer, left out");
    assert_eq!(f.versions[0].when, crate::versions::parse_utc("2026-10-10 06:00:00"), "Snapper's dates are in UTC");
    assert_eq!(f.versions[2].when, Some(std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_791_363_600)), "Timeshift's own time when it gives one");
    assert_eq!(f.versions[3].when, crate::timefmt::parse_local("2026-10-06_10-00-00"), "else the folder's name, in local time");
    assert_eq!(f.snapshots, 7, "five Snapper snapshots and one of each Timeshift kind");
    assert!(f.locked.is_empty());
    // A file no snapshot has, and one on no known mount.
    touch(&d.join("vol/docs/new.txt"), "new");
    let f = find(&d.join("vol/docs/new.txt"), &parse_mountinfo(&table), &stop);
    assert!(f.versions.is_empty() && f.snapshots > 0);
    assert!(find(&file, &[], &stop).versions.is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn snapshots_only_root_reads_are_said_plainly() {
    use crate::versions::{Tool, find, locked_note, parse_mountinfo};
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("versions-locked");
    let (file, table) = snapshot_fixture(&d);
    let dot = d.join("vol/.snapshots");
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o000)).unwrap();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let f = find(&file, &parse_mountinfo(&table), &stop);
    if unsafe { libc::geteuid() } != 0 {
        assert_eq!(f.locked, vec![Tool::Snapper]);
        assert!(!f.versions.is_empty() && f.versions.iter().all(|v| v.tool == Tool::Timeshift), "the others still found");
        assert_eq!(locked_note(&f.locked).as_deref(), Some("Snapper's snapshots here can only be read by an administrator."));
    }
    assert_eq!(locked_note(&[Tool::Snapper, Tool::Timeshift]).as_deref(), Some("The snapshots here can only be read by an administrator."));
    std::fs::set_permissions(&dot, std::fs::Permissions::from_mode(0o755)).unwrap();
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_version_restored_in_place_or_as_a_copy() {
    use crate::versions::{copy_name, find, parse_mountinfo, restore};
    let d = tmpdir("versions-restore");
    let (file, table) = snapshot_fixture(&d);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let f = find(&file, &parse_mountinfo(&table), &stop);
    let v = &f.versions[1];
    let day = crate::timefmt::format(v.when.unwrap(), "%Y-%m-%d");
    assert_eq!(copy_name(&file, v.when), format!("report (from {day}).txt"));
    assert_eq!(copy_name(Path::new("/x/Makefile"), None), "Makefile (from a snapshot)");
    let copy = restore(&v.entry.path, &file, v.when, true).unwrap();
    assert_eq!(copy.parent(), file.parent());
    assert_eq!(std::fs::read_to_string(&copy).unwrap(), "frist\n");
    assert_eq!(std::fs::metadata(&copy).unwrap().modified().unwrap(), v.entry.modified.unwrap(), "the version's time kept");
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fourth draft, the one in use\n", "a copy leaves the file alone");
    let again = restore(&v.entry.path, &file, v.when, true).unwrap();
    assert_eq!(again.file_name().unwrap().to_string_lossy(), format!("report (from {day}) (1).txt"), "never over the first copy");
    // In place: the file goes to the trash, the version takes its name.
    restore(&v.entry.path, &file, v.when, false).unwrap();
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "frist\n");
    assert!(!d.join("vol/docs/.report.txt.restore").exists());
    let trashed = crate::trashcan::list().into_iter().filter(|it| it.original == file).count();
    assert_eq!(trashed, 1, "what it replaced is in the trash");
    assert_eq!(crate::picfix::undo_replaced(std::slice::from_ref(&file)), 0);
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fourth draft, the one in use\n", "undone");
    // A version gone from under it: nothing changed.
    assert!(restore(&d.join("nowhere"), &file, None, false).is_err());
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "fourth draft, the one in use\n");
    for it in crate::trashcan::list().into_iter().filter(|it| it.original.starts_with(&d)) {
        let _ = crate::trashcan::purge(&it);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The real converters on the PATH (ImageMagick, ffmpeg and ffprobe, poppler's
/// pdfunite and pdfseparate, libheif's heif-dec) linked into a stand-in folder; None
/// when one is missing.
pub(crate) fn convert_tools(dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    for t in ["magick", "ffmpeg", "ffprobe", "pdfunite", "pdfseparate", "heif-dec"] {
        let real = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(t)).find(|m| m.is_file()))?;
        let _ = std::os::unix::fs::symlink(real, dir.join(t));
    }
    Some(dir.to_path_buf())
}

/// Real inputs for conversions, made by the real tools: a see-through PNG, a JPEG stored
/// turned (EXIF orientation 6), a two-frame GIF, a HEIC, a tone with a cover and tags
/// as MP3, a short video with odd sides, and PDFs of three and two pages.
pub(crate) fn convert_inputs(d: &Path, tools: &Path) {
    let run = |prog: &str, args: &[&str]| {
        let ok = std::process::Command::new(tools.join(prog)).args(args).current_dir(d).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
        assert!(ok, "{prog} {args:?}");
    };
    run("magick", &["-size", "120x80", "xc:none", "-fill", "#e04030", "-draw", "rectangle 0,0 59,79", "clear.png"]);
    run("magick", &["-size", "400x240", "gradient:#e94-#36c", "raw.jpg"]);
    let turned = with_exif_orientation(&std::fs::read(d.join("raw.jpg")).unwrap(), 6);
    std::fs::write(d.join("turned.jpg"), turned).unwrap();
    std::fs::remove_file(d.join("raw.jpg")).unwrap();
    run("magick", &["-delay", "10", "-size", "64x48", "xc:red", "xc:blue", "moving.gif"]);
    run("magick", &["-size", "90x60", "gradient:#2a7-#fc3", "phone.heic"]);
    run("ffmpeg", &["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=size=64x64", "-frames:v", "1", "cover.png"]);
    run("ffmpeg", &["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "sine=frequency=440:duration=2", "-i", "cover.png", "-map", "0", "-map", "1", "-c:a", "libmp3lame", "-c:v", "png", "-disposition:v", "attached_pic", "-id3v2_version", "3", "-metadata", "title=Test Tone", "-metadata", "artist=Maia", "tone.mp3"]);
    std::fs::remove_file(d.join("cover.png")).unwrap();
    run("ffmpeg", &["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=1:size=161x121:rate=15", "-f", "lavfi", "-i", "sine=duration=1", "-c:v", "mpeg4", "-c:a", "pcm_s16le", "-shortest", "clip.avi"]);
    std::fs::write(d.join("three.pdf"), crate::sim::sample_pdf(&["1 0 0", "0 1 0", "0 0 1"])).unwrap();
    std::fs::write(d.join("two.pdf"), crate::sim::sample_pdf(&["0.5 0.5 0", "0 0.5 0.5"])).unwrap();
}

/// A conversion job run to its end, as the app runs it.
fn convert_job(srcs: &[PathBuf], to: crate::convert::Target, out: Option<PathBuf>, tools: &Path) -> ops::Progress {
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.to_path_buf()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let job = ops::spawn(1, OpKind::Convert { srcs: srcs.to_vec(), to, out, tools: t }, egui::Context::default());
    let t0 = Instant::now();
    while !job.progress.lock().unwrap().finished {
        assert!(t0.elapsed() < Duration::from_secs(120), "{to:?} took too long");
        std::thread::sleep(Duration::from_millis(20));
    }
    std::mem::take(&mut *job.progress.lock().unwrap())
}

/// Hidden part files left in `d` (a failed or cancelled conversion leaves none).
fn parts_in(d: &Path) -> Vec<String> {
    std::fs::read_dir(d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.')).collect()
}

/// What each selection is offered: only formats other than its own, only files of one
/// kind, merging for two PDFs or more and splitting for one, nothing without the tool.
#[test]
fn conversions_offered_by_kind_and_tool() {
    use crate::convert::{Target as T, offered};
    let d = tmpdir("convert-offer");
    let Some(all) = convert_tools(&d.join("all")) else { return };
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(all.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.join("none")));
    std::fs::create_dir_all(d.join("none")).unwrap();
    let none = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    assert_eq!(offered(&["png"], &t), vec![T::Jpg, T::Webp, T::PicturePdf]);
    assert_eq!(offered(&["jpeg", "jpg"], &t), vec![T::Png, T::Webp, T::PicturePdf]);
    assert_eq!(offered(&["png", "jpg", "heic"], &t), vec![T::Jpg, T::Png, T::Webp, T::PicturePdf]);
    assert_eq!(offered(&["flac"], &t), vec![T::Mp3, T::Opus]);
    assert_eq!(offered(&["mp3", "wav"], &t), vec![T::Mp3, T::Opus, T::Flac]);
    assert_eq!(offered(&["mkv"], &t), vec![T::Mp4, T::Webm, T::SoundMp3]);
    assert_eq!(offered(&["pdf"], &t), vec![T::SplitPdf]);
    assert_eq!(offered(&["pdf", "pdf"], &t), vec![T::MergePdf]);
    assert!(offered(&["mp3", "pdf"], &t).is_empty(), "two kinds");
    assert!(offered(&["txt"], &t).is_empty());
    assert!(offered(&[], &t).is_empty());
    for exts in [vec!["png"], vec!["mp3"], vec!["mp4"], vec!["pdf"], vec!["pdf", "pdf"]] {
        assert!(offered(&exts, &none).is_empty(), "{exts:?} without the tools");
    }
    assert_eq!(T::of(Cmd::ConvertSoundMp3), Some(T::SoundMp3));
    assert!(T::ALL.iter().all(|t| T::of(t.cmd()) == Some(*t)));
}

/// Pictures through the real ImageMagick: transparency on white in a JPG, a photo stored
/// turned made upright, one frame of an animated GIF, a HEIC read, a name already taken
/// kept; then several pictures as one PDF written by Maia, each page A4 the right way
/// round with the picture on it; libheif read a HEIC an ImageMagick without HEIC refuses.
#[test]
fn pictures_converted_through_imagemagick() {
    use crate::convert::{Target as T, jpeg_size, page_for};
    let d = tmpdir("convert-pictures");
    let Some(tools) = convert_tools(&d.join("tools")) else { return };
    let w = d.join("work");
    std::fs::create_dir_all(&w).unwrap();
    convert_inputs(&w, &tools);
    let shown = |p: &Path| crate::imgdec::load(p, 4096).unwrap_or_else(|| panic!("{} decodes", p.display()));
    std::fs::write(w.join("clear.jpg"), "taken").unwrap();
    let p = convert_job(&[w.join("clear.png")], T::Jpg, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.produced, vec![(w.join("clear.png"), w.join("clear (1).jpg"))]);
    let (img, dims) = shown(&w.join("clear (1).jpg"));
    assert_eq!(dims, [120, 80]);
    let px = |x: u32, y: u32| img.px[((y * img.w + x) * 4) as usize..][..3].to_vec();
    assert!(px(100, 40).iter().all(|&c| c > 240), "see-through became white: {:?}", px(100, 40));
    assert!(px(20, 40)[0] > 180 && px(20, 40)[2] < 90, "the red stays: {:?}", px(20, 40));
    assert_eq!(std::fs::read_to_string(w.join("clear.jpg")).unwrap(), "taken");
    let p = convert_job(&[w.join("turned.jpg"), w.join("moving.gif"), w.join("phone.heic")], T::Png, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.done_items, 3);
    assert_eq!(shown(&w.join("turned.png")).1, [240, 400], "upright, and stored so");
    assert_eq!(shown(&w.join("moving.png")).1, [64, 48]);
    assert_eq!(shown(&w.join("phone.png")).1, [90, 60]);
    assert!(std::fs::read(w.join("moving.png")).unwrap().starts_with(b"\x89PNG"));
    let p = convert_job(&[w.join("phone.heic")], T::Webp, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(shown(&w.join("phone.webp")).1, [90, 60]);
    // An ImageMagick that can't read HEIC: libheif decodes it instead.
    let picky = d.join("picky");
    convert_tools(&picky).unwrap();
    std::fs::remove_file(picky.join("magick")).unwrap();
    let real = std::fs::read_link(tools.join("magick")).unwrap();
    fake_tool(&picky, "magick", &format!("t=\"$D/in.$$\"; cat > \"$t\"\nif head -c 12 \"$t\" | grep -q ftyp; then rm -f \"$t\"; echo \"magick: no decode delegate for this image format 'HEIC'\" >&2; exit 1; fi\n{} \"$@\" < \"$t\"; r=$?; rm -f \"$t\"; exit $r", real.display()));
    let p = convert_job(&[w.join("phone.heic")], T::Jpg, None, &picky);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(shown(&w.join("phone.jpg")).1, [90, 60]);
    assert!(parts_in(&w).is_empty(), "{:?}", parts_in(&w));
    // Without libheif the refusal is said.
    std::fs::remove_file(picky.join("heif-dec")).unwrap();
    let p = convert_job(&[w.join("phone.heic")], T::Png, None, &picky);
    assert_eq!(p.errors.len(), 1);
    assert!(p.errors[0].contains("no decode delegate"), "{:?}", p.errors);
    // Pictures as one PDF: a page each, landscape for the wide one, A4.
    let out = w.join("Album.pdf");
    let p = convert_job(&[w.join("clear.png"), w.join("turned.jpg"), w.join("phone.heic")], T::PicturePdf, Some(out.clone()), &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.produced.len(), 1);
    assert_eq!(crate::media::pdf_pages(&out), Some(3));
    let info = std::process::Command::new("pdfinfo").args(["-f", "1", "-l", "3"]).arg(&out).output().unwrap();
    let info = String::from_utf8_lossy(&info.stdout);
    let sizes: Vec<&str> = info.lines().filter(|l| l.starts_with("Page") && l.contains("size:")).map(|l| l.split("size:").nth(1).unwrap().trim()).collect();
    assert_eq!(sizes.len(), 3, "{info}");
    assert!(sizes[0].starts_with("841.89 x 595.28"), "the wide picture on a landscape page: {sizes:?}");
    assert!(sizes[1].starts_with("595.28 x 841.89"), "the upright photo on a portrait one: {sizes:?}");
    // The page shows the picture: red on the left, white where it was clear.
    let page = crate::media::pdf_page(&out, 1, 400).expect("page 1 renders");
    let at = |fx: f32, fy: f32| {
        let (x, y) = ((page.w as f32 * fx) as u32, (page.h as f32 * fy) as u32);
        page.px[((y * page.w + x) * 4) as usize..][..3].to_vec()
    };
    assert!(at(0.25, 0.5)[0] > 180 && at(0.25, 0.5)[1] < 100, "red on the page: {:?}", at(0.25, 0.5));
    assert!(at(0.75, 0.5).iter().all(|&c| c > 230), "white beside it: {:?}", at(0.75, 0.5));
    let jpeg = std::fs::read(w.join("phone.jpg")).unwrap();
    assert_eq!(jpeg_size(&jpeg), Some((90, 60, 3)));
    assert_eq!(jpeg_size(b"not a jpeg"), None);
    assert_eq!(jpeg_size(&jpeg[..20]), None);
    let ((pw, ph), [x, y, dw, dh]) = page_for(1000, 500);
    assert!((pw - 841.89).abs() < 0.01 && (ph - 595.28).abs() < 0.01);
    assert!((dw / dh - 2.0).abs() < 0.01 && x > 0.0 && y > 0.0 && x + dw <= pw && y + dh <= ph);
    let (_, [x, _, dw, _]) = page_for(10, 4000);
    assert!(x > 28.0 && dw < 10.0, "a sliver stays in the middle");
    assert!(parts_in(&w).is_empty(), "{:?}", parts_in(&w));
}

/// Sound and video through the real ffmpeg: tags and the cover kept in a FLAC and an
/// MP3, an Opus without the cover it can't hold, a video with odd sides as MP4 (H.264
/// at even sides) and WebM (VP9), its sound alone as MP3. A broken file fails with
/// ffmpeg's words and leaves nothing; a long conversion shows how far it got, and
/// Cancel stops ffmpeg and takes its part file away.
#[test]
fn sound_and_video_converted_through_ffmpeg() {
    use crate::convert::{Target as T, duration, progress_us};
    let d = tmpdir("convert-media");
    let Some(tools) = convert_tools(&d.join("tools")) else { return };
    let w = d.join("work");
    std::fs::create_dir_all(&w).unwrap();
    convert_inputs(&w, &tools);
    let probe = |p: &Path| crate::media::probe(p).unwrap_or_else(|| panic!("{} probes", p.display()));
    let streams = |p: &Path| {
        let o = std::process::Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=codec_name,codec_type:stream_disposition=attached_pic", "-of", "csv=p=0"]).arg(p).output().unwrap();
        String::from_utf8_lossy(&o.stdout).lines().map(String::from).collect::<Vec<_>>()
    };
    let p = convert_job(&[w.join("tone.mp3")], T::Flac, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let flac = w.join("tone.flac");
    let pr = probe(&flac);
    assert_eq!((pr.codec.as_str(), pr.title.as_deref(), pr.artist.as_deref()), ("flac", Some("Test Tone"), Some("Maia")));
    assert!(streams(&flac).iter().any(|s| s.starts_with("png,video,1")), "the cover kept: {:?}", streams(&flac));
    assert!((pr.duration - 2.0).abs() < 0.2, "{}", pr.duration);
    let p = convert_job(&[flac.clone()], T::Mp3, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.produced, vec![(flac.clone(), w.join("tone (1).mp3"))], "beside the first, not over it");
    let back = probe(&w.join("tone (1).mp3"));
    assert_eq!((back.codec.as_str(), back.title.as_deref()), ("mp3", Some("Test Tone")));
    assert!(streams(&w.join("tone (1).mp3")).iter().any(|s| s.ends_with(",video,1")));
    let p = convert_job(&[w.join("tone.mp3")], T::Opus, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let opus = w.join("tone.opus");
    assert_eq!(probe(&opus).codec, "opus");
    assert_eq!(streams(&opus).len(), 1, "no cover in Opus: {:?}", streams(&opus));
    // Video, odd sides made even.
    let p = convert_job(&[w.join("clip.avi")], T::Mp4, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let mp4 = probe(&w.join("clip.mp4"));
    assert_eq!((mp4.codec.as_str(), mp4.video, mp4.audio), ("h264", Some((160, 120)), true));
    let p = convert_job(&[w.join("clip.avi")], T::Webm, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let webm = probe(&w.join("clip.webm"));
    assert_eq!((webm.codec.as_str(), webm.video), ("vp9", Some((161, 121))));
    let p = convert_job(&[w.join("clip.mp4")], T::SoundMp3, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let snd = probe(&w.join("clip.mp3"));
    assert!(snd.video.is_none() && snd.audio && snd.codec == "mp3");
    // A broken file: ffmpeg's own words, nothing left behind.
    std::fs::write(w.join("broken.wav"), "RIFF but not really").unwrap();
    let p = convert_job(&[w.join("broken.wav"), w.join("tone.mp3")], T::Flac, None, &tools);
    assert_eq!(p.errors.len(), 1, "{:?}", p.errors);
    assert!(p.errors[0].starts_with("broken.wav: ") && p.errors[0].len() > 14, "{:?}", p.errors);
    assert!(!w.join("broken.flac").exists());
    assert_eq!(p.done_items, 2, "the next file still converted");
    assert!(parts_in(&w).is_empty(), "{:?}", parts_in(&w));
    assert_eq!(progress_us("out_time_us=1500000"), Some(1_500_000));
    assert_eq!(progress_us("out_time_ms=20"), Some(20));
    assert_eq!(progress_us("progress=continue"), None);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    assert!(duration(&t, &w.join("tone.mp3")).is_some_and(|d| (d - 2.0).abs() < 0.2));
    assert_eq!(duration(&t, &w.join("broken.wav")), None);
    // A long one: progress inside the file, then Cancel.
    let ok = std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i", "testsrc=duration=120:size=640x480:rate=30", "-c:v", "mpeg4", "-q:v", "20"]).arg(w.join("long.avi")).status().unwrap().success();
    assert!(ok);
    let job = ops::spawn(2, OpKind::Convert { srcs: vec![w.join("long.avi")], to: T::Webm, out: None, tools: t }, egui::Context::default());
    assert_eq!(job.label, "Converting long.avi to WebM");
    let t0 = Instant::now();
    while job.progress.lock().unwrap().within.unwrap_or(0.0) <= 0.0 {
        assert!(t0.elapsed() < Duration::from_secs(60), "no progress from ffmpeg");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(w.join(".long.webm.part").exists(), "written to a part file");
    let ff = |s: &str| std::fs::read_dir("/proc").unwrap().flatten().any(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == "ffmpeg") && std::fs::read(e.path().join("cmdline")).is_ok_and(|c| String::from_utf8_lossy(&c).contains(s)));
    assert!(ff(".long.webm.part"));
    job.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let t0 = Instant::now();
    while !job.progress.lock().unwrap().finished {
        assert!(t0.elapsed() < Duration::from_secs(10), "Cancel didn't stop it");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!w.join(".long.webm.part").exists() && !w.join("long.webm").exists());
    let t0 = Instant::now();
    while ff(".long.webm.part") && t0.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!ff(".long.webm.part"), "ffmpeg killed");
    assert!(job.progress.lock().unwrap().errors.is_empty(), "a cancel is no error");
    let _ = std::fs::remove_dir_all(&d);
}

/// PDFs through poppler: two merged into one with every page in order, under the name
/// given; one split into a folder of one-page PDFs named after it, a % in its name
/// included; a broken PDF refused with nothing left behind.
#[test]
fn pdfs_merged_and_split_through_poppler() {
    use crate::convert::Target as T;
    let d = tmpdir("convert-pdf");
    let Some(tools) = convert_tools(&d.join("tools")) else { return };
    let w = d.join("work");
    std::fs::create_dir_all(&w).unwrap();
    convert_inputs(&w, &tools);
    let out = w.join("Both.pdf");
    let p = convert_job(&[w.join("three.pdf"), w.join("two.pdf")], T::MergePdf, Some(out.clone()), &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(crate::media::pdf_pages(&out), Some(5));
    // Page four is the second file's first: yellowish.
    let pg = crate::media::pdf_page(&out, 4, 200).unwrap();
    let c = &pg.px[(((pg.h / 2) * pg.w + pg.w / 2) * 4) as usize..][..3];
    assert!(c[0] > 100 && c[1] > 100 && c[2] < 40, "{c:?}");
    std::fs::rename(w.join("three.pdf"), w.join("50% off.pdf")).unwrap();
    std::fs::create_dir(w.join("50% off pages")).unwrap();
    let p = convert_job(&[w.join("50% off.pdf")], T::SplitPdf, None, &tools);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    let folder = w.join("50% off pages (1)");
    assert_eq!(p.produced, vec![(w.join("50% off.pdf"), folder.clone())]);
    let mut names: Vec<String> = std::fs::read_dir(&folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, ["50% off 1.pdf", "50% off 2.pdf", "50% off 3.pdf"]);
    for n in &names {
        assert_eq!(crate::media::pdf_pages(&folder.join(n)), Some(1), "{n}");
    }
    std::fs::write(w.join("broken.pdf"), "%PDF-1.4 nothing").unwrap();
    let p = convert_job(&[w.join("broken.pdf")], T::SplitPdf, None, &tools);
    assert_eq!(p.errors.len(), 1, "{:?}", p.errors);
    let p = convert_job(&[w.join("broken.pdf"), w.join("two.pdf")], T::MergePdf, Some(w.join("Bad.pdf")), &tools);
    assert_eq!(p.errors.len(), 1, "{:?}", p.errors);
    assert!(!w.join("Bad.pdf").exists() && parts_in(&w).is_empty(), "{:?}", parts_in(&w));
    let _ = std::fs::remove_dir_all(&d);
}

/// A JPEG made by ImageMagick with a date taken and, when given, a place written into its
/// EXIF by exiv2; None without the two tools.
pub(crate) fn exif_photo(path: &Path, size: &str, taken: &str, gps: Option<[&str; 4]>) -> Option<()> {
    if !crate::media::have("magick") || !crate::media::have("exiv2") {
        return None;
    }
    let ok = std::process::Command::new("magick").args(["-size", size, "gradient:#e94-#36c"]).arg(path).status().ok()?.success();
    assert!(ok, "magick");
    let mut c = std::process::Command::new("exiv2");
    c.arg(format!("-Mset Exif.Photo.DateTimeOriginal Ascii {taken}"));
    if let Some([lat_ref, lat, lon_ref, lon]) = gps {
        c.arg(format!("-Mset Exif.GPSInfo.GPSLatitudeRef {lat_ref}")).arg(format!("-Mset Exif.GPSInfo.GPSLatitude {lat}"));
        c.arg(format!("-Mset Exif.GPSInfo.GPSLongitudeRef {lon_ref}")).arg(format!("-Mset Exif.GPSInfo.GPSLongitude {lon}"));
    }
    let ok = c.arg("mo").arg(path).stderr(std::process::Stdio::null()).status().ok()?.success();
    assert!(ok, "exiv2");
    Some(())
}

/// EXIF as exiv2 writes it, read back: the date a photo was taken, its place (south and
/// west negative), its orientation, in a JPEG and a WebP; a photo without EXIF and a
/// file cut short give nothing and don't fail.
#[test]
fn exif_dates_and_places_from_real_photos() {
    let d = tmpdir("exif");
    let jpg = d.join("sydney.jpg");
    if exif_photo(&jpg, "64x48", "2023:07:14 10:20:30", Some(["S", "33/1 51/1 3540/100", "E", "151/1 12/1 4020/100"])).is_none() {
        return;
    }
    let x = crate::exif::read(&jpg).expect("EXIF read");
    assert_eq!(x.taken, Some([2023, 7, 14, 10, 20, 30]));
    let (lat, lon) = x.gps.expect("a place");
    assert!((lat + 33.8598).abs() < 1e-3 && (lon - 151.2112).abs() < 1e-3, "{lat} {lon}");
    assert_eq!(x.orientation, 1);
    let ok = std::process::Command::new("exiv2").args(["-Mset Exif.Image.Orientation 6", "-Mset Exif.GPSInfo.GPSLongitudeRef W", "mo"]).arg(&jpg).status().unwrap().success();
    assert!(ok);
    let x = crate::exif::read(&jpg).unwrap();
    assert_eq!(x.orientation, 6);
    assert!(x.gps.unwrap().1 < -151.0, "west is negative");
    // The decoder turns it by the same field.
    assert_eq!(crate::imgdec::load(&jpg, 64).unwrap().1, [48, 64]);
    let webp = d.join("plain.webp");
    assert!(std::process::Command::new("magick").args(["-size", "40x30", "xc:teal"]).arg(&webp).status().unwrap().success());
    assert!(crate::exif::read(&webp).is_none_or(|x| x.taken.is_none()), "no EXIF, no date");
    let ok = std::process::Command::new("exiv2").args(["-Mset Exif.Photo.DateTimeOriginal Ascii 2021:12:31 23:59:00", "mo"]).arg(&webp).stderr(std::process::Stdio::null()).status().unwrap().success();
    if ok {
        assert_eq!(crate::exif::read(&webp).and_then(|x| x.taken), Some([2021, 12, 31, 23, 59, 0]), "WebP's EXIF chunk");
    }
    let bytes = std::fs::read(&jpg).unwrap();
    for cut in [2, 10, 40, 100] {
        let _ = crate::exif::block(&bytes[..cut]).map(crate::exif::parse);
    }
    assert_eq!(crate::exif::parse(b"Exif\0\0MM\0*\0\0\0\x08\xff\xff"), crate::exif::Exif { orientation: 1, ..Default::default() });
    assert!(crate::exif::read(&d).is_none(), "a folder is no photo");
}

/// Tidy Folder's plans for a real folder: by kind, installers before archives, folders,
/// hidden and unknown files left alone, a folder already named for a kind keeping its
/// own; photos by the month they were taken from their EXIF, else by when they last
/// changed, said so.
#[test]
fn tidy_plans_by_kind_and_month() {
    use crate::tidy::{Mode, kind_folder, month_folder, read_plans};
    for (n, k) in [("setup.deb", Some("Installers")), ("Game.AppImage", Some("Installers")), ("yay-12.4-1-x86_64.pkg.tar.zst", Some("Installers")), ("Tool.flatpakref", Some("Installers")), ("photos.tar.gz", Some("Archives")), ("a.ZIP", Some("Archives")), ("IMG_1.JPG", Some("Pictures")), ("scan.heic", Some("Pictures")), ("report.pdf", Some("Documents")), ("notes.txt", Some("Documents")), ("song.flac", Some("Music")), ("clip.MOV", Some("Videos")), ("main.rs", None), ("README", None), (".zip", None), ("Harbour.cbz", Some("Documents")), ("book.epub", Some("Documents"))] {
        assert_eq!(kind_folder(n), k, "{n}");
    }
    assert_eq!(month_folder(2024, 6), "2024/2024-06");
    let d = tmpdir("tidy");
    let dl = d.join("Downloads");
    std::fs::create_dir_all(dl.join("Old stuff")).unwrap();
    for f in ["setup.deb", "photos.tar.gz", "report.pdf", "song.mp3", "clip.mp4", "main.rs", ".hidden.png", "screen.png"] {
        touch(&dl.join(f), f);
    }
    let shot = dl.join("trip.jpg");
    if exif_photo(&shot, "32x24", "2022:03:09 08:00:00", None).is_none() {
        return;
    }
    // Last changed on 2025-05-20 at noon, local time.
    let may = crate::timefmt::parse_local("2025-05-20T12:00:00").unwrap();
    for f in ["screen.png", "clip.mp4", "trip.jpg"] {
        std::fs::File::options().write(true).open(dl.join(f)).unwrap().set_modified(may).unwrap();
    }
    let p = read_plans(&dl).unwrap();
    let kind: Vec<(String, String)> = p.kind.iter().map(|m| (m.src.file_name().unwrap().to_string_lossy().into_owned(), m.to.clone())).collect();
    let want: Vec<(String, String)> = [("clip.mp4", "Videos"), ("photos.tar.gz", "Archives"), ("report.pdf", "Documents"), ("screen.png", "Pictures"), ("setup.deb", "Installers"), ("song.mp3", "Music"), ("trip.jpg", "Pictures")].iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    assert_eq!(kind, want);
    let month: Vec<(String, String, bool)> = p.month.iter().map(|m| (m.src.file_name().unwrap().to_string_lossy().into_owned(), m.to.clone(), m.taken)).collect();
    assert_eq!(month, [("clip.mp4".to_string(), "2025/2025-05".to_string(), false), ("screen.png".into(), "2025/2025-05".into(), false), ("trip.jpg".into(), "2022/2022-03".into(), true)]);
    // Already a Pictures folder: its pictures stay.
    let pics = d.join("Pictures");
    std::fs::create_dir_all(&pics).unwrap();
    touch(&pics.join("a.png"), "x");
    touch(&pics.join("b.pdf"), "x");
    let p = read_plans(&pics).unwrap();
    assert_eq!(p.kind.iter().map(|m| m.to.as_str()).collect::<Vec<_>>(), ["Documents"]);
    assert_eq!(p.month.len(), 1, "by month it is still sorted");
    assert!(read_plans(&d.join("gone")).is_err());
    let _ = Mode::Kind;
}

/// The moves run as one job: folders made as needed, a name already taken kept apart,
/// and Undo putting every file back and taking away the folders it made (not one that
/// was there before).
#[test]
fn tidy_moves_and_undo() {
    let d = tmpdir("tidy-job");
    std::fs::create_dir_all(d.join("Music")).unwrap();
    touch(&d.join("Music/song.mp3"), "old");
    for f in ["song.mp3", "a.png", "b.pdf"] {
        touch(&d.join(f), f);
    }
    let moves = vec![(d.join("song.mp3"), d.join("Music")), (d.join("a.png"), d.join("2024/2024-06")), (d.join("b.pdf"), d.join("Documents"))];
    let job = ops::spawn(1, OpKind::Sort { moves, label: "Tidying x".into() }, egui::Context::default());
    let t0 = Instant::now();
    while !job.progress.lock().unwrap().finished {
        assert!(t0.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(10));
    }
    let p = std::mem::take(&mut *job.progress.lock().unwrap());
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(std::fs::read_to_string(d.join("Music/song (1).mp3")).unwrap(), "song.mp3");
    assert_eq!(std::fs::read_to_string(d.join("Music/song.mp3")).unwrap(), "old");
    assert!(d.join("2024/2024-06/a.png").is_file() && d.join("Documents/b.pdf").is_file());
    assert_eq!(p.made, [d.join("2024"), d.join("2024/2024-06"), d.join("Documents")]);
    assert_eq!(p.produced.len(), 3);
}

/// A small EPUB made with zip as the format asks (the mimetype first, stored): a package
/// in a subfolder with a title, an author and a cover, two chapters in spine order
/// (the manifest lists them the other way round), headings, paragraphs, a list, a quote,
/// entities, a script and Markdown's own characters in the text. And a CBZ of ten
/// pages named so only a natural sort orders them, with a hidden macOS copy and a text
/// file beside them. None without zip and ImageMagick.
pub(crate) fn book_fixture(d: &Path) -> Option<()> {
    if !crate::media::have("zip") || !crate::media::have("magick") {
        return None;
    }
    let src = d.join("book-src");
    let w = |p: &str, s: &str| touch(&src.join(p), s);
    w("mimetype", "application/epub+zip");
    w("META-INF/container.xml", r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#);
    w("OEBPS/content.opf", r#"<?xml version="1.0" encoding="UTF-8"?>
<package xmlns="http://www.idpf.org/2007/opf" version="3.0" unique-identifier="id">
  <metadata xmlns:dc="http://purl.org/dc/elements/1.1/">
    <dc:title>The Lighthouse &amp; the Sea</dc:title>
    <dc:creator>Ada Keeper</dc:creator>
    <meta name="cover" content="cover-img"/>
  </metadata>
  <manifest>
    <item id="c2" href="text/chapter%202.xhtml" media-type="application/xhtml+xml"/>
    <item id="c1" href="text/chapter1.xhtml" media-type="application/xhtml+xml"/>
    <item id="cover-img" href="images/cover.png" media-type="image/png"/>
    <item id="css" href="style.css" media-type="text/css"/>
  </manifest>
  <spine><itemref idref="c1"/><itemref idref="c2"/></spine>
</package>"#);
    w("OEBPS/style.css", "p { margin: 0 }");
    w("OEBPS/text/chapter1.xhtml", r#"<?xml version="1.0" encoding="utf-8"?><!DOCTYPE html><html xmlns="http://www.w3.org/1999/xhtml"><head><title>One</title><style>p{}</style></head><body><section><h1>Chapter One</h1><p>The keeper climbed   the
stairs at dusk &hellip; every night, for <em>forty</em> years.</p><script>alert(1)</script><p>It cost $5 and *never* more.</p><ul><li>Oil</li><li>Wicks</li></ul><blockquote><p>Light the lamp.</p></blockquote><p>1. Not a list.</p></section></body></html>"#);
    w("OEBPS/text/chapter 2.xhtml", r#"<html><body><h2>Chapter Two</h2><p>A ship&#8217;s bell rang<br/>twice.</p><!-- a comment --></body></html>"#);
    std::fs::create_dir_all(src.join("OEBPS/images")).unwrap();
    assert!(std::process::Command::new("magick").args(["-size", "60x90", "gradient:#123-#9ab"]).arg(src.join("OEBPS/images/cover.png")).status().unwrap().success());
    let book = d.join("Lighthouse.epub");
    let _ = std::fs::remove_file(&book);
    let zip = |args: &[&str]| assert!(std::process::Command::new("zip").args(args).current_dir(&src).stdout(std::process::Stdio::null()).status().unwrap().success());
    zip(&["-X0", book.to_str().unwrap(), "mimetype"]);
    zip(&["-rX9", book.to_str().unwrap(), "META-INF", "OEBPS"]);
    let pages = d.join("comic-src");
    std::fs::create_dir_all(pages.join("__MACOSX")).unwrap();
    for k in 1..=10 {
        let colour = format!("rgb({},{},{})", k * 20, 255 - k * 20, 120);
        assert!(std::process::Command::new("magick").args(["-size", &format!("{}x{}", 100 + k, 150), &format!("xc:{colour}")]).arg(pages.join(format!("page{k}.png"))).status().unwrap().success());
    }
    std::fs::write(pages.join("__MACOSX/._page1.png"), "junk").unwrap();
    std::fs::write(pages.join("credits.txt"), "drawn by hand").unwrap();
    let comic = d.join("Harbour.cbz");
    let _ = std::fs::remove_file(&comic);
    assert!(std::process::Command::new("zip").args(["-rqX"]).arg(&comic).arg(".").current_dir(&pages).status().unwrap().success());
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&pages);
    Some(())
}

/// A real font to look at: DejaVu Sans when installed, else Maia's own fallback.
pub(crate) fn sample_font() -> PathBuf {
    let dejavu = PathBuf::from("/usr/share/fonts/TTF/DejaVuSans.ttf");
    if dejavu.is_file() { dejavu } else { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("assets/fallback-sans.ttf") }
}

/// An EPUB read through the archive code: title, author, cover and chapters in spine
/// order as Markdown that keeps headings, paragraphs, lists and quotes, leaves scripts
/// and comments out and shows Markdown's own characters as typed; a zip that isn't a
/// book says so. A comic's pages in natural order without the junk beside them, each
/// decoded at the size asked.
#[test]
fn books_and_comics_read_through_the_archive_code() {
    let d = tmpdir("books");
    if !crate::archive::available() || book_fixture(&d).is_none() {
        return;
    }
    let md = crate::books::read_book(&d.join("Lighthouse.epub")).unwrap();
    let lines: Vec<String> = md.lines().map(String::from).collect();
    let doc = crate::markdown::parse(&lines, None);
    use crate::markdown::Block;
    let text = |b: &Block| match b {
        Block::Heading(_, r) | Block::Para(r) => crate::markdown::plain(r),
        _ => String::new(),
    };
    let heads: Vec<(u8, String)> = doc.blocks.iter().filter_map(|b| if let Block::Heading(n, r) = b { Some((*n, crate::markdown::plain(r))) } else { None }).collect();
    assert_eq!(heads, [(1, "The Lighthouse & the Sea".to_string()), (1, "Chapter One".into()), (2, "Chapter Two".into())]);
    let paras: Vec<String> = doc.blocks.iter().filter(|b| matches!(b, Block::Para(_))).map(text).collect();
    assert!(paras.contains(&"Ada Keeper".to_string()), "{paras:?}");
    assert!(paras.contains(&"The keeper climbed the stairs at dusk … every night, for forty years.".to_string()), "{paras:?}");
    assert!(paras.contains(&"It cost $5 and *never* more.".to_string()), "as typed, no formula, no emphasis: {paras:?}");
    assert!(paras.contains(&"1. Not a list.".to_string()), "{paras:?}");
    assert!(paras.contains(&"A ship’s bell rang twice.".to_string()), "{paras:?}");
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::List { items, .. } if items.len() == 2)));
    assert!(doc.blocks.iter().any(|b| matches!(b, Block::Quote(_))));
    assert!(!md.contains("alert") && !md.contains("comment") && !md.contains("p{}"));
    assert!(md.find("Chapter One").unwrap() < md.find("Chapter Two").unwrap(), "spine order");
    assert_eq!(doc.images.len(), 1, "the cover");
    let cover = PathBuf::from(&doc.images[0].src);
    assert!(cover.is_absolute() && cover.is_file(), "the cover extracted: {cover:?}");
    assert_eq!(crate::imgdec::load(&cover, 128).unwrap().1, [60, 90]);
    // Not a book.
    let zip = d.join("plain.epub");
    assert!(std::process::Command::new("zip").args(["-qj"]).arg(&zip).arg(&cover).status().unwrap().success());
    assert_eq!(crate::books::read_book(&zip), Err("Not an EPUB book".into()));
    // The comic.
    let comic = d.join("Harbour.cbz");
    let pages = crate::books::comic_pages(&comic).unwrap();
    let names: Vec<String> = pages.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    assert_eq!(names, (1..=10).map(|k| format!("page{k}.png")).collect::<Vec<_>>());
    let p10 = crate::books::comic_page(&comic, &pages[9], 4096).unwrap();
    assert_eq!((p10.w, p10.h), (110, 150));
    let small = crate::books::comic_page(&comic, &pages[0], 75).unwrap();
    assert_eq!((small.w, small.h), (51, 75), "101 × 150 fitted to the size asked");
    // bsdtar reads by content: the same pages under a CBR's name.
    std::fs::copy(&comic, d.join("Harbour.cbr")).unwrap();
    assert_eq!(crate::books::comic_pages(&d.join("Harbour.cbr")).unwrap().len(), 10);
    assert!(crate::books::comic_page(&comic, Path::new("missing.png"), 100).is_none());
}

/// XML just well enough read: tags with namespaces, attributes in either quotes,
/// comments, CDATA and declarations.
#[test]
fn xml_tokens_and_attributes() {
    use crate::books::{Tok, attr, tokens};
    let t = tokens(r#"<?xml version="1.0"?><!-- c --><opf:item id='a' href="x y.html"/><p>Hi<![CDATA[ <b> ]]></p>"#);
    assert_eq!(t.len(), 5, "{t:?}");
    assert!(matches!(&t[0], Tok::Tag { name, empty: true, close: false, .. } if name == "item"));
    if let Tok::Tag { attrs, .. } = &t[0] {
        assert_eq!(attr(attrs, "id").as_deref(), Some("a"));
        assert_eq!(attr(attrs, "href").as_deref(), Some("x y.html"));
        assert_eq!(attr(attrs, "media-type"), None);
    }
    assert_eq!(t[2], Tok::Text("Hi"));
    assert_eq!(t[3], Tok::Text(" <b> "));
    assert!(matches!(&t[4], Tok::Tag { name, close: true, .. } if name == "p"));
    assert_eq!(attr(r#" xlink:href="&amp;x""#, "href").as_deref(), Some("&x"));
    assert_eq!(crate::books::package_path("<container><rootfiles><rootfile full-path=\"a/b.opf\"/></rootfiles></container>"), Some(PathBuf::from("a/b.opf")));
    assert!(tokens("<p unclosed").is_empty());
}

/// A real font's names as fontconfig reads them, sample lines drawn with it, the same
/// font packed as WOFF read back, WOFF2 and a non-font refused; Install copying it into
/// a fonts folder and calling fc-cache, a second Install finding it there.
#[test]
fn fonts_previewed_and_installed() {
    use crate::fontview::{info, install, read, samples, sfnt};
    let font = sample_font();
    let raw = std::fs::read(&font).unwrap();
    let i = info(&raw);
    if crate::media::have("fc-scan") {
        let o = std::process::Command::new("fc-scan").args(["--format", "%{family[0]}|%{style[0]}"]).arg(&font).output().unwrap();
        let fc = String::from_utf8_lossy(&o.stdout).into_owned();
        assert_eq!(fc, format!("{}|{}", i.family, i.style), "the names fontconfig gives");
    }
    assert!(i.glyphs > 10 && !i.version.is_empty() && !i.version.starts_with("Version"), "{i:?}");
    let d = tmpdir("fonts");
    let p = read(&font, 1.6, Some(&d.join("fonts"))).unwrap();
    assert_eq!(p.info, i);
    assert_eq!(p.lines.len(), samples().len());
    assert!(!p.installed);
    let (pt, img) = &p.lines[7];
    assert_eq!(*pt, 54.0);
    assert!(img.size[1] as f32 >= 54.0 * 1.6 * 0.9 && img.size[0] > img.size[1] * 10, "{:?}", img.size);
    assert!(img.pixels.iter().any(|c| c.a() == 255) && img.pixels.iter().any(|c| c.a() == 0), "ink and paper");
    // The same font as a WOFF: tables deflated behind WOFF's own directory.
    let n = u16::from_be_bytes([raw[4], raw[5]]) as usize;
    let mut tables = vec![];
    for k in 0..n {
        let e = 12 + k * 16;
        let be = |i: usize| u32::from_be_bytes([raw[i], raw[i + 1], raw[i + 2], raw[i + 3]]);
        let (tag, sum, off, len) = (be(e), be(e + 4), be(e + 8) as usize, be(e + 12) as usize);
        tables.push((tag, sum, raw[off..off + len].to_vec()));
    }
    let mut woff = b"wOFF".to_vec();
    woff.extend_from_slice(&raw[0..4]);
    woff.extend_from_slice(&[0; 4]);
    woff.extend_from_slice(&(n as u16).to_be_bytes());
    woff.extend_from_slice(&[0; 30]);
    let mut body = vec![];
    let start = 44 + n * 20;
    for (tag, sum, data) in &tables {
        // Compressed where that is smaller, as WOFF asks; stored as it is otherwise.
        let z = fdeflate::compress_to_vec(data);
        let stored = if z.len() < data.len() { z } else { data.clone() };
        let off = start + body.len();
        for v in [*tag, off as u32, stored.len() as u32, data.len() as u32, *sum] {
            woff.extend_from_slice(&v.to_be_bytes());
        }
        body.extend_from_slice(&stored);
        body.resize((body.len() + 3) & !3, 0);
    }
    woff.extend_from_slice(&body);
    let total = woff.len() as u32;
    woff[8..12].copy_from_slice(&total.to_be_bytes());
    let unpacked = sfnt(&woff).unwrap();
    assert_eq!(info(&unpacked), i, "the WOFF's names");
    std::fs::write(d.join("packed.woff"), &woff).unwrap();
    assert_eq!(read(&d.join("packed.woff"), 1.0, None).unwrap().lines.len(), samples().len());
    if crate::media::have("woff2_compress") {
        std::fs::copy(&font, d.join("w2.ttf")).unwrap();
        let _ = std::process::Command::new("woff2_compress").arg(d.join("w2.ttf")).stdout(std::process::Stdio::null()).status();
        if d.join("w2.woff2").exists() {
            assert_eq!(read(&d.join("w2.woff2"), 1.0, None).err().as_deref(), Some("WOFF2 fonts can't be shown here"));
        }
    }
    std::fs::write(d.join("fake.ttf"), "not a font at all").unwrap();
    assert_eq!(read(&d.join("fake.ttf"), 1.0, None).err().as_deref(), Some("Not a font"));
    let mut cut = raw.clone();
    cut.truncate(200);
    std::fs::write(d.join("cut.ttf"), &cut).unwrap();
    assert!(read(&d.join("cut.ttf"), 1.0, None).is_err(), "a damaged font is refused, not a crash");
    // Install, with a stand-in fc-cache.
    let tools = d.join("tools");
    fake_tool(&tools, "fc-cache", "exit 0");
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let fonts = d.join("fonts");
    let name = font.file_name().unwrap();
    let got = install(&font, &fonts, Some(&t)).unwrap();
    assert_eq!(got, Some(fonts.join(name)));
    assert_eq!(std::fs::read(fonts.join(name)).unwrap(), raw);
    assert_eq!(fake_calls(&tools), [vec!["fc-cache".to_string(), fonts.display().to_string()]]);
    assert_eq!(install(&font, &fonts, Some(&t)).unwrap(), None, "already there");
    assert!(read(&font, 1.0, Some(&fonts)).unwrap().installed);
    // Another font under the same name keeps both.
    std::fs::create_dir_all(d.join("x")).unwrap();
    std::fs::write(d.join("x").join(name), &woff).unwrap();
    let other = install(&d.join("x").join(name), &fonts, None).unwrap().unwrap();
    assert_ne!(other, fonts.join(name));
}

/// Where a photo was taken, written for people and as a map's address.
#[test]
fn photo_places_and_map_links() {
    use crate::exif::{map_url, place};
    assert_eq!(place(-33.8598, 151.2112), "33.85980° S, 151.21120° E");
    assert_eq!(place(48.85837, -2.294481), "48.85837° N, 2.29448° W");
    assert_eq!(map_url(48.85837, 2.294481), "https://www.openstreetmap.org/?mlat=48.858370&mlon=2.294481#map=16/48.858370/2.294481");
}

/// A home folder for the Timeline: files changed today, three days ago, three weeks
/// ago and two months ago, and recent ones where it never looks (hidden folders and
/// files, a cache, a git folder, installed packages, a Rust build).
pub(crate) fn timeline_fixture(h: &Path) {
    let now = std::time::SystemTime::now();
    let ago = |days: u64| now - Duration::from_secs(days * 86_400 + 60);
    let at = |p: &str, days: u64| {
        touch(&h.join(p), p);
        std::fs::File::options().write(true).open(h.join(p)).unwrap().set_modified(if days == 0 { now - Duration::from_secs(60) } else { ago(days) }).unwrap();
    };
    at("Documents/letter.odt", 0);
    at("Downloads/ticket.pdf", 3);
    at("Pictures/2026/beach.png", 21);
    at("Documents/old report.pdf", 60);
    at(".cache/thumb.png", 0);
    at(".notes.txt", 0);
    at("Code/app/.git/index", 0);
    at("Code/app/web/package.json", 40);
    at("Code/app/web/node_modules/left-pad/index.js", 0);
    at("Code/app/Cargo.toml", 40);
    at("Code/app/target/debug/app", 0);
    at("Code/app/src/main.rs", 1);
}

/// The Timeline: the last month's files under home, the ones where it never looks left
/// out; opened by its id, it shows newest first in the date groups.
#[test]
fn timeline_lists_the_last_month() {
    use crate::smart::{TIMELINE, by_id, search, timeline, timeline_view};
    let d = tmpdir("timeline");
    timeline_fixture(&d);
    let mut sf = timeline();
    assert_eq!((sf.id, sf.name.as_str(), sf.days), (TIMELINE, "Timeline", Some(31)));
    assert_eq!(by_id(&[], TIMELINE), Some(timeline()));
    assert_eq!(by_id(&[], 7), None);
    sf.root = d.clone();
    let (cancel, cut) = (std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false));
    let mut found: Vec<String> = vec![];
    search(&sf, &mut |b| {
        found.extend(b.iter().map(|e| e.path.strip_prefix(&d).unwrap().to_string_lossy().into_owned()));
        true
    }, &cancel, &cut);
    found.sort();
    assert_eq!(found, ["Code/app/src/main.rs", "Documents/letter.odt", "Downloads/ticket.pdf", "Pictures/2026/beach.png"]);
    let v = timeline_view(crate::config::ViewSettings::default());
    assert_eq!((v.sort, v.desc, v.group), (crate::config::SortKey::Modified, true, crate::config::GroupBy::Modified));
}

/// Compress With Password through the real bsdtar and 7z: a zip locked with AES-256 and
/// a 7z with its names locked too, the password never on any command line while they
/// are made (every process's argv watched); opened by Maia's own extraction only with
/// the password, a wrong one refused as such.
#[test]
fn locked_archives_open_only_with_their_password() {
    use crate::archive::{NEEDS_PASSWORD, WRONG_PASSWORD, create_locked, extract, forget_password, list, set_password};
    if !crate::archive::available() {
        return;
    }
    let d = tmpdir("locked");
    let src = d.join("src");
    touch(&src.join("plans/notes – é.txt"), "the secret plans");
    // Big and incompressible, so the tools run long enough to be watched.
    let mut noise = vec![0u8; 24 << 20];
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for b in noise.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    std::fs::write(src.join("noise.bin"), &noise).unwrap();
    let pw = "correct horse 9 battery";
    let names = vec![std::ffi::OsString::from("plans"), std::ffi::OsString::from("noise.bin")];
    let mut kinds = vec!["zip"];
    if crate::archive::can_lock_7z() {
        kinds.push("7z");
    }
    for ext in kinds {
        let out = d.join(format!("locked.{ext}"));
        let (o2, s2, n2) = (out.clone(), src.clone(), names.clone());
        let job = std::thread::spawn(move || create_locked(&o2, &s2, &n2, pw));
        // The archiver's command line while it runs (the process writing the part
        // file): the password is not in it.
        let (mut seen_tool, mut leaked) = (false, false);
        while !job.is_finished() {
            for e in std::fs::read_dir("/proc").unwrap().flatten() {
                let comm = std::fs::read_to_string(e.path().join("comm")).unwrap_or_default();
                if !matches!(comm.trim(), "bsdtar" | "7z" | "7za" | "7zz") {
                    continue;
                }
                let Ok(c) = std::fs::read(e.path().join("cmdline")) else { continue };
                let c = String::from_utf8_lossy(&c).replace('\0', " ");
                if c.contains(&format!(".locked.{ext}.part")) {
                    seen_tool = true;
                    leaked |= c.contains(pw);
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        job.join().unwrap().unwrap();
        assert!(!leaked, "the password on the {ext} command line");
        assert!(seen_tool, "the {ext} tool was watched while it ran");
        // The real 7z says how it is locked.
        if crate::media::have("7z") {
            let o = std::process::Command::new("7z").args(["l", "-slt", "-ba", &format!("-p{pw}")]).arg(&out).output().unwrap();
            let t = String::from_utf8_lossy(&o.stdout);
            assert!(t.contains("Encrypted = +"), "{t}");
            if ext == "zip" {
                assert!(t.contains("AES-256"), "{t}");
            }
        }
        // Without the password: the zip lists its names but won't give its contents;
        // the 7z won't even list.
        forget_password(&out);
        let stage = d.join(format!("x-{ext}"));
        if ext == "zip" {
            assert!(list(&out).unwrap().iter().any(|i| i.path == Path::new("plans/notes – é.txt")));
        } else {
            assert_eq!(list(&out).err().as_deref(), Some(NEEDS_PASSWORD));
        }
        let e = extract(&out, &[PathBuf::from("plans")], &stage);
        assert!(e.as_ref().is_err_and(|e| crate::archive::wants_password(e)), "{e:?}");
        set_password(&out, "wrong".into());
        let e = extract(&out, &[PathBuf::from("plans")], &d.join(format!("w-{ext}")));
        assert_eq!(e.err().as_deref(), Some(WRONG_PASSWORD));
        set_password(&out, pw.into());
        let _ = std::fs::remove_dir_all(&stage);
        extract(&out, &[PathBuf::from("plans"), PathBuf::from("noise.bin")], &stage).unwrap();
        assert_eq!(std::fs::read_to_string(stage.join("plans/notes – é.txt")).unwrap(), "the secret plans");
        assert!(std::fs::read(stage.join("noise.bin")).unwrap() == noise, "every byte back");
        assert!(!d.join(format!(".locked.{ext}.part")).exists());
    }
    assert!(create_locked(&d.join("bad.zip"), &src, &names, "two\nlines").is_err());
    assert!(create_locked(&d.join("bad.zip"), &src, &names, "").is_err());
    assert!(format!("{:?}", crate::archive::Secret(pw.into())) == "Secret(…)", "never printed");
    let _ = std::fs::remove_dir_all(&d);
}

/// ffprobe's own words for the tags of a FLAC (in the file) and an Opus (with its
/// stream), as captured from it; spellings and cases files use read as one.
#[test]
fn music_tags_parsed_from_ffprobe() {
    use crate::musictags::{parse, track_name};
    let flac = "format.tags.title=\"T\"\nformat.tags.artist=\"A\"\nformat.tags.album_artist=\"AA\"\nformat.tags.track=\"3/12\"\nformat.tags.date=\"2020\"\nformat.tags.genre=\"Jazz\"\nformat.tags.encoder=\"Lavf63.1.102\"\n";
    let t = parse(flac);
    assert_eq!(t.values, ["T", "A", "", "AA", "3/12", "2020", "Jazz"].map(String::from));
    assert!(!t.cover);
    let opus = "streams.stream.0.tags.encoder=\"Lavc63.1.102 libopus\"\nstreams.stream.0.tags.TITLE=\"Say \\\"hi\\\"\"\nstreams.stream.0.tags.ALBUMARTIST=\"Band\"\nstreams.stream.0.tags.TRACKNUMBER=\"7\"\nstreams.stream.1.disposition.attached_pic=1\n";
    let t = parse(opus);
    assert_eq!((t.get("title"), t.get("album_artist"), t.get("track")), ("Say \"hi\"", "Band", "7"));
    assert!(t.cover);
    assert_eq!(track_name("3/12"), "03");
    assert_eq!(track_name("12"), "12");
    assert_eq!(track_name("A1"), "A1");
}

/// Tags written through the real ffmpeg into MP3, FLAC, M4A and Opus: the fields
/// changed, the others and the sound kept as they were, a cover put in from a picture
/// and taken out again (Opus has nowhere to keep one); the original in the trash and
/// back by Undo's own way, byte for byte.
#[test]
fn music_tags_written_through_ffmpeg() {
    use crate::musictags::{Cover, read, write};
    let d = tmpdir("musictags");
    let Some(tools) = convert_tools(&d.join("tools")) else { return };
    let w = d.join("work");
    std::fs::create_dir_all(&w).unwrap();
    convert_inputs(&w, &tools);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let ff = |args: &[&str]| assert!(std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-y"]).args(args).current_dir(&w).status().unwrap().success());
    ff(&["-i", "tone.mp3", "-map", "0:a", "-c:a", "flac", "-metadata", "genre=Drone", "tone.flac"]);
    ff(&["-i", "tone.mp3", "-map", "0:a", "-c:a", "aac", "tone.m4a"]);
    ff(&["-i", "tone.mp3", "-map", "0:a", "-c:a", "libopus", "tone.opus"]);
    ff(&["-f", "lavfi", "-i", "testsrc=size=80x80", "-frames:v", "1", "art.png"]);
    let sound = |p: &Path| {
        let o = std::process::Command::new("ffmpeg").args(["-v", "error", "-i"]).arg(p).args(["-map", "0:a", "-f", "md5", "-"]).output().unwrap();
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    for f in ["tone.mp3", "tone.flac", "tone.m4a", "tone.opus"] {
        let p = w.join(f);
        let before = read(&t, &p).unwrap();
        let (bytes, audio) = (std::fs::read(&p).unwrap(), sound(&p));
        write(&t, &p, &[(0, "Evening Tone".into()), (2, "Sine Waves".into()), (4, "4/9".into())], &Cover::Set(w.join("art.png"))).unwrap();
        let after = read(&t, &p).unwrap();
        assert_eq!((after.get("title"), after.get("album"), after.get("track")), ("Evening Tone", "Sine Waves", "4/9"), "{f}");
        assert_eq!(after.get("artist"), before.get("artist"), "{f}: untouched fields kept");
        assert_eq!(after.get("genre"), before.get("genre"), "{f}");
        assert_eq!(after.cover, f != "tone.opus", "{f}: the cover");
        assert_eq!(sound(&p), audio, "{f}: the sound itself untouched");
        let trashed: Vec<_> = crate::trashcan::list().into_iter().filter(|it| it.original == p).collect();
        assert_eq!(trashed.len(), 1, "{f}: the original in the trash");
        assert_eq!(std::fs::read(&trashed[0].file).unwrap(), bytes);
        if f != "tone.opus" {
            write(&t, &p, &[], &Cover::Remove).unwrap();
            assert!(!read(&t, &p).unwrap().cover, "{f}: cover taken out");
            assert_eq!(read(&t, &p).unwrap().get("title"), "Evening Tone");
        }
        // Undo: the newest original back.
        assert_eq!(crate::picfix::undo_replaced(std::slice::from_ref(&p)), 0);
        if f == "tone.opus" {
            assert_eq!(std::fs::read(&p).unwrap(), bytes, "{f}: back as it was");
        }
    }
    assert!(std::fs::read_dir(&w).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().ends_with(".part")));
    std::fs::write(w.join("fake.mp3"), "ID3 not really").unwrap();
    assert!(write(&t, &w.join("fake.mp3"), &[(0, "x".into())], &Cover::Keep).is_err());
    assert_eq!(std::fs::read_to_string(w.join("fake.mp3")).unwrap(), "ID3 not really", "a failed write leaves the file");
    assert!(write(&t, &w.join("clip.avi"), &[(0, "x".into())], &Cover::Keep).is_err());
}

/// Batch Rename's tag words: {track} padded, {artist} and {title} filled in, a slash in a
/// tag never making a folder, a file without tags leaving the words empty.
#[test]
fn batch_rename_names_from_music_tags() {
    use crate::batchname::{Rules, names_with, uses_tags};
    use crate::musictags::Tags;
    assert!(uses_tags("{track} {title}") && !uses_tags("{name} ###"));
    let tag = |title: &str, artist: &str, track: &str| {
        let mut t = Tags::default();
        t.values[0] = title.into();
        t.values[1] = artist.into();
        t.values[4] = track.into();
        Some(t)
    };
    let items: Vec<(String, bool)> = ["a.mp3", "b.flac", "c.mp3"].iter().map(|n| (n.to_string(), false)).collect();
    let r = Rules { name: "{track} {artist} - {title}".into(), start: "1".into(), ..Default::default() };
    let got = names_with(&items, &r, &[tag("Intro", "Band", "1/9"), tag("AC/DC #1", "Them", "10"), None]).unwrap();
    assert_eq!(got, ["01 Band - Intro.mp3", "10 Them - AC-DC #1.flac", "-.mp3"], "no tags: no stray spaces");
}

/// Formatting through stand-ins for lsblk, udisksctl and gdbus (never the real ones):
/// a removable stick unmounted first, then udisks asked over D-Bus for a new partition
/// table and one partition formatted with the label; the system disk, a drive lsblk no
/// longer calls removable and a bad label refused before anything runs.
#[test]
fn format_a_removable_drive_through_udisks() {
    use crate::drives::{Fs, format, format_calls, label_for, list, object_path};
    let d = tmpdir("format");
    let rows = [
        r#"PATH="/dev/sda" PKNAME="" LABEL="" FSTYPE="" SIZE="500107862016" RM="0" HOTPLUG="0" TYPE="disk" MOUNTPOINTS="""#,
        r#"PATH="/dev/sda2" PKNAME="sda" LABEL="" FSTYPE="btrfs" SIZE="499570991104" RM="0" HOTPLUG="0" TYPE="part" MOUNTPOINTS="/""#,
        r#"PATH="/dev/sdz" PKNAME="" LABEL="" FSTYPE="" SIZE="16008609792" RM="1" HOTPLUG="1" TYPE="disk" MOUNTPOINTS="""#,
        r#"PATH="/dev/sdz1" PKNAME="sdz" LABEL="OLD" FSTYPE="vfat" SIZE="16007561216" RM="1" HOTPLUG="1" TYPE="part" MOUNTPOINTS="/run/media/me/OLD""#,
    ];
    std::fs::write(d.join("rows"), rows.join("\n") + "\n").unwrap();
    fake_tool(&d, "lsblk", "cat \"$D/rows\"");
    fake_tool(&d, "udisksctl", "exit 0");
    fake_tool(&d, "gdbus", "echo '()'");
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let all = list(&t);
    let stick = all.iter().find(|x| x.dev == "/dev/sdz1").unwrap().clone();
    let system = all.iter().find(|x| x.dev == "/dev/sda2").unwrap().clone();
    assert!(stick.removable && !system.removable);
    let _ = std::fs::remove_file(d.join("calls.log"));
    format(&t, &stick, Fs::Exfat, "Holiday 26").unwrap();
    let calls = fake_calls(&d);
    let progs: Vec<&str> = calls.iter().map(|c| c[0].as_str()).collect();
    assert_eq!(progs, ["lsblk", "udisksctl", "gdbus", "gdbus"], "unmounted, then the two udisks calls");
    assert_eq!(calls[1][1..], ["unmount", "-b", "/dev/sdz1"]);
    let want = format_calls("/dev/sdz", Fs::Exfat, "Holiday 26");
    assert_eq!(calls[2][1..], want[0][..]);
    assert_eq!(calls[3][1..], want[1][..]);
    assert!(want[0].contains(&"/org/freedesktop/UDisks2/block_devices/sdz".to_string()) && want[0].contains(&"org.freedesktop.UDisks2.Block.Format".to_string()) && want[0].contains(&"'dos'".to_string()));
    assert_eq!(want[1][want[1].len() - 2..], ["'exfat'".to_string(), "{'label': <'Holiday 26'>}".to_string()]);
    assert!(format_calls("/dev/sdz", Fs::Ext4, "x")[1].last().unwrap().contains("'take-ownership': <true>"));
    assert_eq!(format_calls("/dev/sdz", Fs::Fat32, "X")[1][format_calls("/dev/sdz", Fs::Fat32, "X")[1].len() - 5], "'0x0c'");
    // Refused before anything runs.
    let _ = std::fs::remove_file(d.join("calls.log"));
    assert!(format(&t, &system, Fs::Ext4, "x").is_err(), "the system disk");
    let mut lied = system.clone();
    lied.removable = true;
    assert!(format(&t, &lied, Fs::Ext4, "x").is_err(), "lsblk is asked again");
    assert!(format(&t, &stick, Fs::Fat32, "far too long a label").is_err());
    assert!(format(&t, &stick, Fs::Exfat, "it's").is_err(), "a quote would end the label");
    assert!(fake_calls(&d).iter().all(|c| c[0] != "gdbus" && c[0] != "udisksctl"), "{:?}", fake_calls(&d));
    let mut gone = stick.clone();
    gone.disk = "/dev/sdq".into();
    assert_eq!(format(&t, &gone, Fs::Exfat, "x").err().as_deref(), Some("The drive is no longer connected"));
    // Labels and object paths.
    assert_eq!(label_for(Fs::Fat32, " photos 26 ").unwrap(), "PHOTOS 26");
    assert_eq!(label_for(Fs::Exfat, "Photos").unwrap(), "Photos");
    assert!(label_for(Fs::Fat32, "TWELVE CHARS").is_err() && label_for(Fs::Exfat, "TWELVE CHARS").is_ok());
    assert!(label_for(Fs::Ext4, "a/b").is_err());
    assert_eq!(object_path("/dev/mmcblk0"), "/org/freedesktop/UDisks2/block_devices/mmcblk0");
    assert_eq!(object_path("/dev/dm-0"), "/org/freedesktop/UDisks2/block_devices/dm_2d0");
}

/// Quick Open's files: in a repository, git's (tracked and new, not what .gitignore
/// leaves out), in one order; outside one, a walk past hidden folders and build output.
/// A file's own name outranks its folders, and a query a newer one replaced gives up.
#[test]
fn quick_open_lists_and_ranks_a_projects_files() {
    use crate::quickopen::{list, rank};
    use std::sync::atomic::{AtomicBool, AtomicU64};
    let d = tmpdir("quickopen");
    let stop = AtomicBool::new(false);
    for (f, body) in [("src/main.rs", "fn main() {}\n"), ("src/view/list.rs", "\n"), ("docs/main notes.md", "\n"), ("README.md", "\n"), ("node_modules/pkg/index.js", "\n"), (".cache/x", "\n"), ("package.json", "{}\n")] {
        touch(&d.join(f), body);
    }
    // Not a repository: the walk, without node_modules (beside package.json) or .cache.
    let walked = list(&d, &stop);
    assert_eq!(walked, ["README.md", "docs/main notes.md", "package.json", "src/main.rs", "src/view/list.rs"]);
    if crate::media::have("git") {
        git(&d, &["init", "-q"]);
        touch(&d.join(".gitignore"), "node_modules/\nbuild.log\n");
        touch(&d.join("build.log"), "noise\n");
        git(&d, &["add", "src", "README.md", ".gitignore"]);
        git(&d, &["-c", "user.name=T", "-c", "user.email=t@example.invalid", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "first"]);
        touch(&d.join("src/new.rs"), "\n");
        let files = list(&d, &stop);
        assert_eq!(files, [".cache/x", ".gitignore", "README.md", "docs/main notes.md", "package.json", "src/main.rs", "src/new.rs", "src/view/list.rs"], "tracked and new (hidden too, as git sees them), in one order; ignored left out");
        let g = AtomicU64::new(1);
        let hits: Vec<&str> = rank(&files, "main", &g, 1).unwrap().iter().map(|&i| files[i as usize].as_str()).collect();
        assert_eq!(hits.first(), Some(&"src/main.rs"), "the name, ahead of a folder or a longer name");
        assert!(hits.contains(&"docs/main notes.md"));
        let hits: Vec<&str> = rank(&files, "vl", &g, 1).unwrap().iter().map(|&i| files[i as usize].as_str()).collect();
        assert_eq!(hits, ["src/view/list.rs"], "letters across folders");
        assert!(rank(&files, "zzz", &g, 1).unwrap().is_empty());
        assert_eq!(rank(&files, "main", &g, 0), None, "a newer query took over");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// A filter that hides a selected file takes it out of the selection: after renaming
/// one file (which stays selected) and typing another's name, F2 or Delete act on the
/// file found, never on the one out of sight.
#[test]
fn a_filter_drops_hidden_files_from_the_selection() {
    let d = tmpdir("filter-selection");
    for f in ["a.txt", "report.md", "notes.txt"] {
        touch(&d.join(f), "x\n");
    }
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let at = |t: &Tab, n: &str| t.entries.iter().position(|e| e.name == n).unwrap() as u32;
    let (a, report) = (at(&t, "a.txt"), at(&t, "report.md"));
    t.select_only(a);
    t.filter = "report".into();
    t.dirty = true;
    pump(&mut t, &stats);
    assert_eq!(t.cursor, Some(report), "the cursor on the match");
    assert!(t.selected.is_empty(), "a.txt hidden, no longer selected");
    assert_eq!(t.target_entries().iter().map(|e| e.name.clone()).collect::<Vec<_>>(), ["report.md"], "what F2 and Delete act on");
    // A selection the filter keeps in view stays.
    t.select_only(report);
    t.filter = "rep".into();
    t.dirty = true;
    pump(&mut t, &stats);
    assert!(t.selected.contains(&report));
    let _ = std::fs::remove_dir_all(&d);
}

/// Nothing a copy or move replaces is lost: a replacement is written beside what it
/// replaces and swapped in once whole, the old one going to the trash; one that fails
/// halfway (a file it can't read) leaves the old one as it was and nothing half made;
/// merging replaces a file in one step; a link to a folder deleted is only the link;
/// names swapped in one rename both survive, and a rename never lands on a name taken.
#[test]
fn file_operations_never_lose_what_they_replace() {
    use std::os::unix::fs::PermissionsExt;
    let ctx = egui::Context::default();
    let d = tmpdir("safety");
    let (src, dst) = (d.join("src"), d.join("dst"));
    let run = |kind: OpKind| {
        let j = ops::spawn(1, kind, ctx.clone());
        wait_job(&j);
        let p = j.progress.lock().unwrap();
        (p.errors.clone(), p.produced.clone())
    };
    let over = |p: &Path| HashMap::from([(p.to_path_buf(), Conflict::Overwrite)]);
    let parts = |dir: &Path| std::fs::read_dir(dir).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().contains("maia-part")).count();
    let trashed = |orig: &Path| crate::trashcan::list().into_iter().filter(|i| i.original == orig).count();
    // Replaced, with the old one in the trash.
    touch(&src.join("a.txt"), "new");
    touch(&dst.join("a.txt"), "old");
    let (errs, made) = run(OpKind::Copy { srcs: vec![src.join("a.txt")], dest: dst.clone(), decide: over(&src.join("a.txt")) });
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "new");
    assert_eq!(made, [(src.join("a.txt"), dst.join("a.txt"))], "recorded where it landed");
    assert_eq!(trashed(&dst.join("a.txt")), 1, "the old one can be had back");
    assert_eq!(parts(&dst), 0);
    // A replacement that fails: the old file stays, nothing half made is left.
    touch(&src.join("locked.txt"), "can't read me");
    std::fs::set_permissions(src.join("locked.txt"), std::fs::Permissions::from_mode(0o000)).unwrap();
    touch(&dst.join("locked.txt"), "precious");
    let unreadable = std::fs::File::open(src.join("locked.txt")).is_err();
    if unreadable {
        let (errs, _) = run(OpKind::Copy { srcs: vec![src.join("locked.txt")], dest: dst.clone(), decide: over(&src.join("locked.txt")) });
        assert_eq!(errs.len(), 1);
        assert_eq!(std::fs::read_to_string(dst.join("locked.txt")).unwrap(), "precious", "kept when the new one couldn't be read");
        assert_eq!(parts(&dst), 0, "no part left behind");
        // Merging a folder whose file can't be read: the file there is kept too.
        std::fs::create_dir_all(src.join("box")).unwrap();
        std::fs::rename(src.join("locked.txt"), src.join("box/locked.txt")).unwrap();
        touch(&dst.join("box/locked.txt"), "precious too");
        let (errs, _) = run(OpKind::Copy { srcs: vec![src.join("box")], dest: dst.clone(), decide: HashMap::from([(src.join("box"), Conflict::Merge)]) });
        assert_eq!(errs.len(), 1);
        assert_eq!(std::fs::read_to_string(dst.join("box/locked.txt")).unwrap(), "precious too");
        assert_eq!(parts(&dst.join("box")), 0);
        // A folder copy that fails halfway leaves no half folder.
        std::fs::create_dir_all(src.join("half")).unwrap();
        touch(&src.join("half/fine.txt"), "fine");
        std::fs::rename(src.join("box/locked.txt"), src.join("half/zz-locked.txt")).unwrap();
        let (errs, _) = run(OpKind::Copy { srcs: vec![src.join("half")], dest: dst.clone(), decide: HashMap::new() });
        assert_eq!(errs.len(), 1);
        assert!(!dst.join("half").exists(), "half a copy is no copy");
        std::fs::set_permissions(src.join("half/zz-locked.txt"), std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    // A move that replaces: the old one to the trash, the moved one in its place.
    touch(&src.join("m.txt"), "moved");
    touch(&dst.join("m.txt"), "was here");
    let (errs, _) = run(OpKind::Move { srcs: vec![src.join("m.txt")], dest: dst.clone(), decide: over(&src.join("m.txt")) });
    assert!(errs.is_empty(), "{errs:?}");
    assert!(!src.join("m.txt").exists() && std::fs::read_to_string(dst.join("m.txt")).unwrap() == "moved");
    assert_eq!(trashed(&dst.join("m.txt")), 1);
    // A link to a folder, deleted: the folder and its files stay.
    touch(&d.join("real/keep.txt"), "keep");
    std::os::unix::fs::symlink(d.join("real"), d.join("link")).unwrap();
    let (errs, _) = run(OpKind::Delete { paths: vec![d.join("link")] });
    assert!(errs.is_empty());
    assert!(std::fs::symlink_metadata(d.join("link")).is_err() && d.join("real/keep.txt").exists());
    // Two names swapped in one go; a rename onto a name taken is refused.
    touch(&d.join("x.txt"), "x");
    touch(&d.join("y.txt"), "y");
    let errs = crate::rename::apply(&[(d.join("x.txt"), "y.txt".into()), (d.join("y.txt"), "x.txt".into())]);
    assert!(errs.is_empty(), "{errs:?}");
    assert_eq!((std::fs::read_to_string(d.join("x.txt")).unwrap(), std::fs::read_to_string(d.join("y.txt")).unwrap()), ("y".into(), "x".into()));
    let errs = crate::rename::apply(&[(d.join("x.txt"), "y.txt".into())]);
    assert_eq!(errs.len(), 1, "taken");
    assert_eq!((std::fs::read_to_string(d.join("x.txt")).unwrap(), std::fs::read_to_string(d.join("y.txt")).unwrap()), ("y".into(), "x".into()), "both untouched");
    assert!(crate::fsx::rename_new(&d.join("x.txt"), &d.join("y.txt")).is_err());
    // Clean the test's trash entries.
    for it in crate::trashcan::list().into_iter().filter(|i| i.original.starts_with(&d)) {
        let _ = crate::trashcan::purge(&it);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Converting picks the encoder an ffmpeg build has: one without x264 (as some
/// distributions ship it) still makes an MP4, through OpenH264 or MPEG-4; one with no
/// MP3 encoder says so instead of failing halfway. The listing is ffmpeg's own; the
/// MP4 below is made by the real ffmpeg with what a build without x264 would get.
#[test]
fn convert_uses_the_encoders_ffmpeg_has() {
    use crate::convert::{Target, encoder_names, media_args};
    let listing = "Encoders:\n V..... = Video\n A..... = Audio\n ------\n V....D libx264              libx264 H.264\n V....D mpeg4                MPEG-4 part 2\n A....D aac                  AAC (Advanced Audio Coding)\n A....D libopus              libopus Opus\n";
    assert_eq!(encoder_names(listing), ["libx264", "mpeg4", "aac", "libopus"], "the legend's lines are not encoders");
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let full = media_args(Target::Mp4, false, &s(&["libx264", "aac"])).unwrap();
    assert!(full.windows(2).any(|w| w == ["-c:v", "libx264"]));
    let fedora = media_args(Target::Mp4, false, &s(&["libopenh264", "aac", "libopus"])).unwrap();
    assert!(fedora.windows(2).any(|w| w == ["-c:v", "libopenh264"]));
    let bare = media_args(Target::Mp4, false, &s(&["mpeg4", "aac"])).unwrap();
    assert!(bare.windows(2).any(|w| w == ["-c:v", "mpeg4"]));
    assert!(media_args(Target::Mp3, false, &s(&["aac", "flac"])).is_err(), "no LAME, said before starting");
    let own_opus = media_args(Target::Opus, false, &s(&["opus"])).unwrap();
    assert!(own_opus.windows(2).any(|w| w == ["-strict", "-2"]), "ffmpeg's own Opus is experimental");
    if !crate::media::have("ffmpeg") || !crate::media::have("ffprobe") {
        return;
    }
    let d = tmpdir("encoders");
    let src = d.join("clip.mkv");
    let ok = std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=97x61:rate=10:duration=1", "-f", "lavfi", "-i", "sine=duration=1", "-shortest", "-c:v", "ffv1", "-c:a", "flac"]).arg(&src).status().unwrap().success();
    assert!(ok);
    let out = d.join("clip.mp4");
    let st = std::process::Command::new("ffmpeg").args(["-hide_banner", "-loglevel", "error", "-nostdin", "-y", "-i"]).arg(&src).args(&bare).arg(&out).status().unwrap();
    assert!(st.success(), "an MP4 from MPEG-4 alone");
    let probe = std::process::Command::new("ffprobe").args(["-v", "error", "-show_entries", "stream=codec_name,width", "-of", "csv=p=0"]).arg(&out).output().unwrap();
    let said = String::from_utf8_lossy(&probe.stdout);
    assert!(said.contains("mpeg4,96") && said.contains("aac"), "even sides and both streams: {said}");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn tour_first_start_and_updates() {
    use crate::tour::{Kind, at_start_kind, version};
    assert_eq!(version("0.3.0"), [0, 3, 0]);
    assert_eq!(version("v0.10.2-rc1"), [0, 10, 2]);
    assert_eq!(version("1"), [1, 0, 0]);
    assert!(version("0.10.0") > version("0.9.9"), "numbers, not text");
    // Nothing on disk: the tour, whatever else.
    assert_eq!(at_start_kind(true, "", "0.3.0"), Some(Kind::Welcome));
    // A 0.2 home: settings, no version recorded.
    assert_eq!(at_start_kind(false, "", "0.3.0"), Some(Kind::News));
    assert_eq!(at_start_kind(false, "0.2.2", "0.3.0"), Some(Kind::News));
    // Seen already, or newer.
    assert_eq!(at_start_kind(false, "0.3.0", "0.3.0"), None);
    assert_eq!(at_start_kind(false, "0.3.1", "0.3.0"), None);
    // A userdata.json from 0.2.2 reads with no version seen; this one keeps it.
    let old: crate::config::UserData = serde_json::from_str(r#"{"bookmarks":["/tmp"],"recents":[],"quick":["new_tab"]}"#).unwrap();
    assert!(old.seen_version.is_empty() && old.bookmarks.len() == 1);
    let mut ud = crate::config::UserData::with_defaults();
    ud.seen_version = "0.3.0".into();
    let back: crate::config::UserData = serde_json::from_str(&serde_json::to_string(&ud).unwrap()).unwrap();
    assert_eq!(back.seen_version, "0.3.0");
    assert_eq!(env!("CARGO_PKG_VERSION"), crate::tour::NEWS_VERSION, "What's New describes this version");
}

/// Every film decodes as made: an animation of a few states of the same window, each
/// held a second or more, ending on a state other than its first; played through, the
/// player hands over every frame PLAYS times, the last held for good, and with
/// animations off only the last. Their bytes stay inside the binary's room for them.
#[test]
fn tour_films_decode_and_play() {
    use crate::tour::{Body, NEWS, PLAYS, WELCOME, play};
    let mut total = 0;
    let mut size = None;
    for p in WELCOME.iter().chain(NEWS) {
        let Body::Film(bytes) = p.body else { continue };
        total += bytes.len();
        let mut d = image_webp::WebPDecoder::new(std::io::Cursor::new(bytes)).unwrap();
        assert!(d.is_animated(), "{} is a film", p.title);
        let n = d.num_frames();
        assert!((2..=4).contains(&n), "{}: {n} states", p.title);
        let dims = d.dimensions();
        assert_eq!(*size.get_or_insert(dims), dims, "{}: every film the same size", p.title);
        let mut buf = vec![0u8; d.output_buffer_size().unwrap()];
        let mut frames = vec![];
        let mut ms = 0;
        for _ in 0..n {
            let t = d.read_frame(&mut buf).unwrap();
            assert!(t >= 900, "{}: a state held {t} ms", p.title);
            ms += t;
            frames.push(buf.clone());
        }
        assert!((3000..=6000).contains(&ms), "{}: {ms} ms in all", p.title);
        assert!(frames[0] != frames[n as usize - 1], "{}: something happens", p.title);
        let ctx = egui::Context::default();
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let w = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let t = std::thread::spawn(move || play(bytes, false, tx, w, ctx));
        let got: Vec<crate::tour::Frame> = rx.iter().collect();
        t.join().unwrap();
        assert_eq!(got.len(), (n * PLAYS) as usize, "{}: every frame, {PLAYS} times", p.title);
        assert_eq!(got.last().unwrap().ms, 0, "the last state stays");
        assert!(got[..got.len() - 1].iter().all(|f| f.ms >= 900));
        assert_eq!(got[0].img.size, [dims.0 as usize, dims.1 as usize]);
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let w = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ctx = egui::Context::default();
        std::thread::spawn(move || play(bytes, true, tx, w, ctx));
        let still: Vec<crate::tour::Frame> = rx.iter().collect();
        assert_eq!(still.len(), 1, "{}: animations off, one picture", p.title);
        let last = &got[got.len() - 1].img;
        assert!(still[0].img.pixels == last.pixels, "{}: the last state", p.title);
    }
    assert!(total < 370_000, "the films take {total} bytes");
    // Drawn one pixel for one at 1.6: 540 points wide.
    assert_eq!(size.map(|s| s.0), Some(864));
}

/// The pages name commands as the palette does, show keys that exist, and are written as
/// the README is: no dashes for commas, no words of praise.
#[test]
fn tour_words_match_the_app() {
    use crate::tour::{ALSO, Body, NEWS, WELCOME};
    let keymap = Keymap::build(&Default::default());
    let titles: Vec<&str> = commands::ALL.iter().map(|c| c.info().title.trim_end_matches('…')).collect();
    for p in WELCOME.iter().chain(NEWS) {
        assert!(!p.title.is_empty());
        for &c in p.keys {
            // Shown by its keys, or by its name where it has none.
            assert!(keymap.label(c).is_some() || titles.contains(&c.info().title.trim_end_matches('…')));
        }
        if let Body::Film(_) = p.body {
            assert!(!p.keys.is_empty(), "{}: how to reach it", p.title);
        }
    }
    // Commands the pages name by name are real ones.
    for name in ["Go To", "View Pictures", "Show Space Used", "Batch Rename", "Quick Open", "Recent Projects", "Find Duplicates", "Tidy Folder", "Undo History", "Timeline", "Previous Versions", "What's Using This", "Clean Build Folders In Home", "Choose Theme"] {
        assert!(titles.contains(&name), "{name:?} is a command");
    }
    let mut all: Vec<&str> = WELCOME.iter().chain(NEWS).flat_map(|p| [p.title, p.text]).collect();
    for (g, items) in ALSO {
        all.push(g);
        for (icon, a, b) in *items {
            assert!(crate::solar_map::SOLAR.iter().any(|(n, _)| n == icon), "{a}: no icon {icon:?}");
            all.push(a);
            all.push(b);
        }
    }
    for t in all {
        assert!(!t.contains('—') && !t.contains('–'), "{t:?}: a dash");
        for w in ["seamless", "powerful", "robust", "leverage", "utilize", "delve", "elevate", "streamline", "comprehensive", "!"] {
            assert!(!t.to_lowercase().contains(w), "{t:?}: {w:?}");
        }
    }
    // The choices on the first page are Options' own rows.
    assert!(crate::popups::OPTS.iter().any(|o| o.0 == crate::popups::Opt::Tour && o.1 == crate::popups::Section::System), "found again in Startup & system");
}

/// "Show in folder" over D-Bus, against a private bus (never the user's): Maia takes
/// org.freedesktop.FileManager1 (a second connection can't), and calls made with the
/// real gdbus reach the window as requests: items to show selected, folders to open,
/// properties; Introspect describes it; an unknown method is refused. A message
/// written and read back is the same message.
#[test]
fn file_manager1_answers_show_in_folder() {
    use crate::fm1::{Bus, NAME, answer, build, read, strings};
    use crate::single::Request;
    // The wire format alone first: a call with an `as` body, round trip.
    let mut body = vec![];
    // `as`: the array's byte length, then each string's length, bytes and nul.
    body.extend_from_slice(&13u32.to_le_bytes());
    body.extend_from_slice(&8u32.to_le_bytes());
    body.extend_from_slice(b"file:///\0");
    let m = build(1, 9, &[(1, 'o', "/org/freedesktop/FileManager1"), (2, 's', NAME), (3, 's', "ShowItems")], None, "as", &body);
    let back = read(&mut std::io::Cursor::new(m)).unwrap();
    assert_eq!((back.serial, back.path.as_str(), back.member.as_str(), back.signature.as_str()), (9, "/org/freedesktop/FileManager1", "ShowItems", "as"));
    assert_eq!(strings(&back.body), ["file:///"]);
    if !crate::media::have("dbus-daemon") || !crate::media::have("gdbus") {
        return;
    }
    let d = tmpdir("fm1");
    let sock = d.join("bus");
    let conf = d.join("bus.conf");
    std::fs::write(&conf, format!("<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth><policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>", sock.display())).unwrap();
    let mut daemon = std::process::Command::new("dbus-daemon").arg(format!("--config-file={}", conf.display())).arg("--nofork").stdout(std::process::Stdio::null()).spawn().unwrap();
    let start = Instant::now();
    while !sock.exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let addr = format!("unix:path={}", sock.display());
    let mut bus = Bus::connect(&addr).expect("connected and authenticated");
    assert!(bus.own(NAME), "the name is Maia's");
    assert!(!Bus::connect(&addr).unwrap().own(NAME), "and no one else's while it holds it");
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || answer(bus, tx, || {}));
    touch(&d.join("Some folder/a b.txt"), "x");
    let call = |method: &str, uris: &str| {
        std::process::Command::new("gdbus").args(["call", "--address", &addr, "--dest", NAME, "--object-path", "/org/freedesktop/FileManager1", "--method"]).arg(format!("org.freedesktop.FileManager1.{method}")).arg(uris).arg("").output().unwrap()
    };
    let file = d.join("Some folder/a b.txt");
    let uri = format!("['file://{}']", file.display().to_string().replace(' ', "%20"));
    let out = call("ShowItems", &uri);
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "()");
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(Request::Show(p)) if p == [file.clone()]), "the file, to show selected");
    assert!(call("ShowFolders", &format!("['file://{}']", d.display())).status.success());
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(Request::Open(p)) if p == [d.clone()]));
    assert!(call("ShowItemProperties", &uri).status.success());
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(Request::Properties(p)) if p == [file.clone()]));
    let intro = std::process::Command::new("gdbus").args(["introspect", "--address", &addr, "--dest", NAME, "--object-path", "/org/freedesktop/FileManager1"]).output().unwrap();
    assert!(String::from_utf8_lossy(&intro.stdout).contains("ShowItems"));
    let bad = call("Explode", "['file:///']");
    assert!(!bad.status.success() && String::from_utf8_lossy(&bad.stderr).contains("UnknownMethod"));
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&d);
}

/// Started by the bus with no window open, Maia opens none for what needs none: a dock
/// asking for the service's properties at login, an introspection. "Show in folder" goes
/// to the open window, or starts one when there is none. A window that opens takes the
/// name over, and the windowless one goes. Against a private bus, with the real gdbus.
#[test]
fn file_manager1_without_a_window() {
    use crate::fm1::{Bus, NAME, headless};
    use crate::single::Request;
    // What the bus is told to start: never a window by itself.
    assert_eq!(crate::fm1::service_file(Path::new("/opt/maia/bin/maia")), "[D-BUS Service]\nName=org.freedesktop.FileManager1\nExec=/opt/maia/bin/maia --fm1\n");
    if !crate::media::have("dbus-daemon") || !crate::media::have("gdbus") {
        return;
    }
    let d = tmpdir("fm1-headless");
    let sock = d.join("bus");
    let conf = d.join("bus.conf");
    std::fs::write(&conf, format!("<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth><policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>", sock.display())).unwrap();
    // Stopped however the test ends: a bus left running would outlive it.
    struct Daemon(std::process::Child);
    impl Drop for Daemon {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let _daemon = Daemon(std::process::Command::new("dbus-daemon").arg(format!("--config-file={}", conf.display())).arg("--nofork").stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).spawn().unwrap());
    let start = Instant::now();
    while !sock.exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let addr = format!("unix:path={}", sock.display());
    let window = d.join("window.sock");
    let (opened_tx, opened) = std::sync::mpsc::channel::<String>();
    let (done_tx, done) = std::sync::mpsc::channel();
    let (a, w) = (addr.clone(), window.clone());
    std::thread::spawn(move || {
        let ok = headless(&a, &w, |r| opened_tx.send(format!("{r:?}")).unwrap());
        let _ = done_tx.send(ok);
    });
    let gdbus = |args: &[&str]| std::process::Command::new("gdbus").args(["call", "--address", &addr, "--dest", NAME, "--object-path", "/org/freedesktop/FileManager1", "--method"]).args(args).output().unwrap();
    wait_for("the name taken", 5, || gdbus(&["org.freedesktop.DBus.Peer.Ping"]).status.success());
    let props = gdbus(&["org.freedesktop.DBus.Properties.GetAll", NAME]);
    assert!(props.status.success(), "{}", String::from_utf8_lossy(&props.stderr));
    assert_eq!(String::from_utf8_lossy(&props.stdout).trim(), "(@a{sv} {},)");
    let intro = std::process::Command::new("gdbus").args(["introspect", "--address", &addr, "--dest", NAME, "--object-path", "/org/freedesktop/FileManager1"]).output().unwrap();
    assert!(String::from_utf8_lossy(&intro.stdout).contains("ShowItems"));
    assert!(opened.recv_timeout(Duration::from_millis(300)).is_err(), "a window opened for nothing to show");
    // No window: one is started for what is asked.
    touch(&d.join("Some folder/a b.txt"), "x");
    let file = d.join("Some folder/a b.txt");
    let uri = format!("['file://{}']", file.display().to_string().replace(' ', "%20"));
    assert!(gdbus(&["org.freedesktop.FileManager1.ShowItems", &uri, ""]).status.success());
    assert_eq!(opened.recv_timeout(Duration::from_secs(5)).unwrap(), format!("{:?}", Request::Show(vec![file.clone()])));
    // A window listening: what is asked goes to it, and nothing else opens.
    let (tx, rx) = std::sync::mpsc::channel();
    crate::single::serve(crate::single::claim_at(&window).unwrap(), tx, || {});
    assert!(gdbus(&["org.freedesktop.FileManager1.ShowItemProperties", &uri, ""]).status.success());
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)), Ok(Request::Properties(p)) if p == [file.clone()]));
    assert!(opened.recv_timeout(Duration::from_millis(300)).is_err());
    // The window takes the name, and the windowless service goes.
    assert!(Bus::connect(&addr).unwrap().own(NAME), "the window gets the name");
    assert_eq!(done.recv_timeout(Duration::from_secs(5)), Ok(true), "the windowless service stayed");
    let _ = std::fs::remove_dir_all(&d);
}

/// The call the bus started Maia for can come before the answer to Maia's own request
/// for the name: it is kept and answered once the name is Maia's, not dropped. And the
/// single-instance socket carries every kind of request whole.
#[test]
fn file_manager1_keeps_the_call_it_was_started_for() {
    use crate::fm1::{Bus, NAME, answer, build, put_str, read};
    use crate::single::{Request, encode, parse};
    let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut uris = vec![];
    put_str(&mut uris, "file:///srv/photos");
    let mut body = (uris.len() as u32).to_le_bytes().to_vec();
    body.extend_from_slice(&uris);
    let early = build(1, 5, &[(1, 'o', "/org/freedesktop/FileManager1"), (2, 's', NAME), (3, 's', "ShowFolders"), (7, 's', ":1.9")], None, "as", &body);
    let answer_to_ours = build(2, 6, &[(7, 's', "org.freedesktop.DBus")], Some(1), "", &[]);
    use std::io::Write as _;
    b.write_all(&early).unwrap();
    b.write_all(&answer_to_ours).unwrap();
    let mut bus = Bus::over(a);
    assert!(bus.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", "", &[]).is_some());
    let (tx, rx) = std::sync::mpsc::channel();
    let t = std::thread::spawn(move || answer(bus, tx, || {}));
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)), Ok(Request::Open(vec![PathBuf::from("/srv/photos")])));
    let ours = read(&mut b).unwrap();
    assert_eq!(ours.member, "RequestName");
    let reply = read(&mut b).unwrap();
    assert_eq!((reply.kind, reply.reply_to), (2, Some(5)), "the early call was answered");
    drop(b);
    t.join().unwrap();
    for r in [Request::Focus, Request::Open(vec!["/a b".into()]), Request::Show(vec!["/x/1.jpg".into(), "/x/2.jpg".into()]), Request::Properties(vec!["/y".into()])] {
        assert_eq!(parse(&encode(&r)), Some(r));
    }
}

/// A big file is counted and read a window at a time: lines anywhere in it, the last one
/// without a line break, long lines cut, and every match found, across chunk seams too.
#[test]
fn big_files_page_and_search() {
    use crate::bigfile::Pager;
    let d = tmpdir("bigfile");
    let p = d.join("big.log");
    let mut text = String::new();
    for i in 0..300_000 {
        text.push_str(&format!("line {i} {}\n", if i % 100_000 == 7 { "Needle" } else { "hay" }));
    }
    text.push_str(&"x".repeat(9000));
    std::fs::write(&p, &text).unwrap();
    let ctx = egui::Context::default();
    let mut v = Pager::open(&p, text.len() as u64, false, &ctx).unwrap();
    let start = Instant::now();
    while !v.rows().1 {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(v.rows().0, 300_001, "the unterminated last line counts");
    for (a, b) in [(0u64, 5u64), (123_456, 123_460), (299_998, 300_001)] {
        v.want(a, b);
        let start = Instant::now();
        while v.line(a).is_none() || v.line(b - 1).is_none() {
            assert!(start.elapsed() < Duration::from_secs(5), "lines {a}..{b}");
            std::thread::sleep(Duration::from_millis(2));
            v.want(a, b);
        }
        if a == 123_456 {
            assert_eq!(v.line(123_456), Some("line 123456 hay"));
        }
    }
    assert!(v.line(300_000).unwrap().ends_with('…') && v.line(300_000).unwrap().len() < 4100);
    v.search("needle", &ctx);
    let f = v.find.as_ref().unwrap();
    let start = Instant::now();
    while !f.done() {
        assert!(start.elapsed() < Duration::from_secs(10));
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*f.hits.lock().unwrap(), vec![7, 100_007, 200_007], "lines, ignoring case");
    // Hex: offsets of the bytes, given as text or as pairs.
    let bin = d.join("blob.bin");
    let mut bytes = vec![0u8; 3 << 20];
    bytes[(1 << 20) - 2..(1 << 20) + 2].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    std::fs::write(&bin, &bytes).unwrap();
    assert_eq!(crate::bigfile::hex_query("de ad be ef"), vec![0xde, 0xad, 0xbe, 0xef]);
    assert_eq!(crate::bigfile::hex_query("0xDEAD"), vec![0xde, 0xad]);
    assert_eq!(crate::bigfile::hex_query("cafe"), b"cafe".to_vec());
    let mut h = Pager::open(&bin, bytes.len() as u64, true, &ctx).unwrap();
    assert_eq!(h.rows(), (3 << 16, true));
    h.search("de ad be ef", &ctx);
    let f = h.find.as_ref().unwrap();
    while !f.done() {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(*f.hits.lock().unwrap(), vec![(1 << 20) - 2], "found across the seam of two chunks");
    let row = ((1 << 20) - 2) / 16;
    h.want(row, row + 1);
    let start = Instant::now();
    while h.row_bytes(row).is_none() {
        assert!(start.elapsed() < Duration::from_secs(5));
        std::thread::sleep(Duration::from_millis(2));
        h.want(row, row + 1);
    }
    assert_eq!(&h.row_bytes(row).unwrap()[14..], &[0xde, 0xad]);
    let _ = std::fs::remove_dir_all(&d);
}

/// The find bar stays inside the body it is given. A terminal pane squeezed by a small
/// window has a body shorter than the bar, which once reached up over the panel's
/// status bar.
#[test]
fn find_bar_stays_inside_its_body() {
    use crate::findbar::{Finder, bar};
    let pal = crate::theme::palette(crate::theme::ThemeName::default());
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx, "", "");
    // Shapes painted beyond the panel's own background, and the room left below the bar.
    let mut run = |body: Option<egui::Rect>| {
        let mut f = Finder::new("main".into());
        let mut rest = egui::Rect::NOTHING;
        let out = ctx.run_pass(egui::RawInput::default(), |ctx| {
            ctx.root_ui(|root| {
                egui::CentralPanel::default().show(root, |ui| {
                    if let Some(body) = body {
                        rest = bar(ui, body, &mut f, "1 of 2", true, 7, false, &pal).1;
                    }
                });
            });
        });
        let n = out.shapes.len();
        out.drop_without_applying_deltas();
        (rest, n)
    };
    let (_, empty) = run(None);
    let tall = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, 200.0));
    let (rest, n) = run(Some(tall));
    assert!(rest.bottom() < tall.bottom() - 20.0 && n > empty, "a tall body gives the bar its room: {rest:?}");
    let short = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, 18.0));
    let (rest, n) = run(Some(short));
    assert_eq!(rest, short, "a short body keeps all of itself");
    assert_eq!(n, empty, "and nothing of the bar is drawn");
}

/// Find in a preview: case ignored unless the query has capitals, matches stepped through
/// in a ring, and the place kept when more text arrives.
#[test]
fn find_in_preview_matches() {
    use crate::findbar::{Finder, find_in};
    let mut at = vec![];
    find_in("aXa xa XA", "xa", false, &mut at);
    assert_eq!(at, vec![(1, 3), (4, 6), (7, 9)]);
    at.clear();
    find_in("aaaa", "aa", true, &mut at);
    assert_eq!(at, vec![(0, 2), (2, 4)], "matches don't overlap");
    let mut lines: Vec<String> = ["fn main() {", "    let Main = 1;", "}"].iter().map(|s| s.to_string()).collect();
    let mut f = Finder::new("main".into());
    f.update(&lines, 1);
    assert_eq!(f.hits, vec![(0, 3, 7), (1, 8, 12)]);
    assert!(f.reveal && f.count() == "1 of 2");
    f.step(1);
    f.step(1);
    assert_eq!(f.cur, 0, "past the last, the first");
    f.step(-1);
    assert_eq!(f.count(), "2 of 2");
    f.query = "Main".into();
    f.update(&lines, 1);
    assert_eq!(f.hits, vec![(1, 8, 12)], "a capital matches case");
    lines.push("main again, Main".into());
    f.update(&lines, 1);
    assert_eq!((f.hits.len(), f.cur), (2, 0), "new lines found in, the place kept");
    f.query = "nothing".into();
    f.update(&lines, 1);
    assert_eq!(f.count(), "No matches");
}

/// A unified diff side by side: context on both sides with each side's own numbers,
/// removals beside the additions that replaced them, an addition alone beside a gap,
/// and the changed part of a changed line.
#[test]
fn diffs_side_by_side() {
    use crate::sidediff::{Kind, Row, changed_span, rows};
    let lines: Vec<String> = ["@@ -10,4 +10,5 @@ fn f", " same", "-old a", "-old b", "+new a", " tail", "+added", "\\ No newline at end of file"].iter().map(|s| s.to_string()).collect();
    let r = rows(&lines);
    let row = |old, new, kind| Row { old, new, kind };
    assert_eq!(r, vec![row(Some((0, 0)), None, Kind::Head), row(Some((10, 1)), Some((10, 1)), Kind::Same), row(Some((11, 2)), Some((11, 4)), Kind::Changed), row(Some((12, 3)), None, Kind::Changed), row(Some((13, 5)), Some((12, 5)), Kind::Same), row(None, Some((13, 6)), Kind::Changed),]);
    assert_eq!(changed_span("let x = score(a);", "let x = rank(a);"), ((8, 13), (8, 12)));
    assert_eq!(changed_span("same", "same"), ((4, 4), (4, 4)));
    assert_eq!(changed_span("é1", "é2"), ((2, 3), (2, 3)), "cut at character boundaries");
}

/// A folder the sync can't list is left out, on either side, and named. Read as empty, an
/// unreadable folder here would make all of its twin "only there" for Mirror to trash,
/// and an unreadable one there would have everything in it copied again.
#[test]
fn sync_plans_nothing_inside_folders_it_cannot_list() {
    use std::os::unix::fs::PermissionsExt;
    let d = tmpdir("sync-unlisted");
    let (a, b) = (d.join("a"), d.join("b"));
    touch(&a.join("music/x.flac"), "x");
    touch(&b.join("music/x.flac"), "x");
    touch(&b.join("music/y.flac"), "y");
    touch(&a.join("docs/new.txt"), "n");
    touch(&b.join("docs/old.txt"), "o");
    touch(&a.join("top.txt"), "t");
    let mode = |p: &Path, m| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    mode(&a.join("music"), 0o000);
    mode(&b.join("docs"), 0o000);
    let shut = std::fs::read_dir(a.join("music")).is_err();
    let p = crate::compare::plan(&a, &b, &std::sync::atomic::AtomicBool::new(false));
    mode(&a.join("music"), 0o755);
    mode(&b.join("docs"), 0o755);
    // Root reads either way, and then there is nothing to test.
    if shut {
        assert_eq!(p.unlisted, [b.join("docs"), a.join("music")]);
        assert!(p.extra.is_empty() && p.update.is_empty(), "{p:?}");
        assert_eq!(p.copy, [(a.join("top.txt"), b.join("top.txt"))]);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Sync refuses two folders when one holds the other, also when the way to one of them
/// goes through a link, and plans nothing for them. A folder whose name merely starts
/// like the other's is not inside it, and one folder on both sides isn't either.
#[test]
fn sync_refuses_a_folder_and_one_inside_it() {
    use crate::compare::{Plan, one_inside_other, plan};
    let d = tmpdir("sync-inside");
    touch(&d.join("photos/2026/a.jpg"), "a");
    touch(&d.join("photos-old/b.jpg"), "b");
    std::os::unix::fs::symlink(d.join("photos/2026"), d.join("this-year")).unwrap();
    assert!(one_inside_other(&d.join("photos"), &d.join("this-year")), "through the link");
    assert!(one_inside_other(&d.join("this-year"), &d.join("photos")));
    assert!(!one_inside_other(&d.join("photos"), &d.join("photos")));
    assert!(!one_inside_other(&d.join("photos"), &d.join("photos-old")));
    let stop = std::sync::atomic::AtomicBool::new(false);
    assert_eq!(plan(&d.join("this-year"), &d.join("photos"), &stop), Plan::default());
    assert_eq!(plan(&d.join("photos"), &d.join("photos/2026"), &stop), Plan::default());
    assert_ne!(plan(&d.join("photos"), &d.join("photos-old"), &stop), Plan::default());
    let _ = std::fs::remove_dir_all(&d);
}

/// A copy on a FAT stick can read up to two seconds older than its original, as FAT
/// keeps even seconds only. Within that a file isn't newer, so Sync and "If newer" leave
/// it be; past it, it is.
#[test]
fn newer_allows_for_fat_times() {
    use crate::compare::newer;
    let t = std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
    let ms = |n| Some(t + Duration::from_millis(n));
    assert!(!newer(ms(1999), ms(0)) && !newer(ms(2000), ms(0)));
    assert!(newer(ms(2001), ms(0)));
    assert!(newer(ms(0), None), "a time beats none");
    assert!(!newer(None, ms(0)) && !newer(None, None));
    let d = tmpdir("sync-fat2");
    let (a, b) = (d.join("a"), d.join("b"));
    for (name, ahead) in [("stick.txt", 1800), ("edited.txt", 4000)] {
        touch(&a.join(name), "x");
        touch(&b.join(name), "x");
        let at = |p: &Path, t| std::fs::File::options().write(true).open(p).unwrap().set_modified(t).unwrap();
        at(&a.join(name), t + Duration::from_millis(ahead));
        at(&b.join(name), t);
    }
    let p = crate::compare::plan(&a, &b, &std::sync::atomic::AtomicBool::new(false));
    assert_eq!(p.update, [(a.join("edited.txt"), b.join("edited.txt"))]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Sync all the way down: what the other side lacks copied, newer files over older ones
/// (the older to the trash), mirroring sends what only it has to the trash, folders that
/// differ inside are found, and Undo puts it all back as one step.
#[test]
fn sync_plans_copies_and_undoes() {
    use crate::compare::{differs, plan};
    let ctx = egui::Context::default();
    let d = tmpdir("sync");
    let (a, b) = (d.join("a"), d.join("b"));
    touch(&a.join("same.txt"), "same");
    touch(&b.join("same.txt"), "same");
    let t = std::fs::metadata(a.join("same.txt")).unwrap().modified().unwrap();
    std::fs::File::options().write(true).open(b.join("same.txt")).unwrap().set_modified(t).unwrap();
    touch(&a.join("sub/new.txt"), "new");
    touch(&a.join("sub/deep/edit.txt"), "newer");
    touch(&b.join("sub/deep/edit.txt"), "old");
    let old = std::time::SystemTime::now() - Duration::from_secs(3600);
    std::fs::File::options().write(true).open(b.join("sub/deep/edit.txt")).unwrap().set_modified(old).unwrap();
    touch(&a.join("fresh/inner.txt"), "x");
    touch(&b.join("sub/only-there.txt"), "extra");
    let stop = std::sync::atomic::AtomicBool::new(false);
    assert!(differs(&a.join("sub"), &b.join("sub"), &stop));
    assert!(!differs(&a.join("sub/deep"), &a.join("sub/deep"), &stop));
    let p = plan(&a, &b, &stop);
    assert_eq!(p.copy, vec![(a.join("fresh"), b.join("fresh")), (a.join("sub/new.txt"), b.join("sub/new.txt"))]);
    assert_eq!(p.update, vec![(a.join("sub/deep/edit.txt"), b.join("sub/deep/edit.txt"))]);
    assert_eq!(p.extra, vec![b.join("sub/only-there.txt")]);
    let copies = p.copy.iter().chain(&p.update).cloned().collect();
    let j = ops::spawn(1, OpKind::Sync { copies, remove: p.extra.clone() }, ctx);
    wait_job(&j);
    let g = j.progress.lock().unwrap();
    assert!(g.errors.is_empty(), "{:?}", g.errors);
    assert_eq!(std::fs::read_to_string(b.join("sub/deep/edit.txt")).unwrap(), "newer");
    assert!(b.join("fresh/inner.txt").exists() && b.join("sub/new.txt").exists() && !b.join("sub/only-there.txt").exists());
    assert!(!differs(&a, &b, &stop), "the same now");
    let kind = crate::app::UndoKind::Synced(g.produced.iter().map(|x| x.1.clone()).collect(), g.replaced.clone(), g.gone.clone());
    drop(g);
    let mut app = crate::app::App::new(&egui::Context::default(), vec![d.clone()]);
    app.undo.push(crate::app::UndoEntry { label: "Sync".into(), kind });
    assert!(app.undo_last());
    assert_eq!(std::fs::read_to_string(b.join("sub/deep/edit.txt")).unwrap(), "old");
    assert!(!b.join("fresh").exists() && !b.join("sub/new.txt").exists() && b.join("sub/only-there.txt").exists());
    let _ = std::fs::remove_dir_all(&d);
}

/// A terminal `cols` × `rows` after a program wrote `bytes`.
fn vt(cols: usize, rows: usize, bytes: &[u8]) -> crate::vt::Term {
    let mut t = crate::vt::Term::new(cols, rows);
    t.feed(bytes);
    t
}

fn vt_rows(t: &crate::vt::Term) -> Vec<String> {
    (0..t.screen.rows).map(|y| t.screen.row_text(y)).collect()
}

fn vt_cell(t: &crate::vt::Term, x: usize, y: usize) -> crate::vt::Cell {
    t.screen.line(t.screen.view_line(y)).unwrap().cells[x]
}

#[test]
fn terminal_writes_wraps_and_moves() {
    let t = vt(10, 3, b"hello world, wrap me");
    assert_eq!(vt_rows(&t), ["hello worl", "d, wrap me", ""]);
    assert!(t.screen.line(t.screen.view_line(0)).unwrap().wrapped);
    assert_eq!(t.screen.text((0, 0), (1, 9)), "hello world, wrap me", "a wrapped row copies as one line");
    // The last column holds the cursor until the next character wraps.
    assert_eq!(t.screen.cursor(), (9, 1));
    assert_eq!(vt_rows(&vt(20, 3, b"ab\x08c\r\nx\ty\x07")), ["ac", "x       y", ""]);
    assert_eq!(vt_rows(&vt(10, 3, b"\x1b[2;3Hx\x1b[Hy\x1b[3;9H\x1b[2Dz\x1b[A\x1b[Cw")), ["y", "  x     w", "      z"]);
    // Rows scrolled off the top are kept above.
    let t = vt(10, 2, b"1\r\n2\r\n3\r\n4");
    assert_eq!(vt_rows(&t), ["3", "4"]);
    assert_eq!((t.screen.history(), t.screen.text((0, 0), (1, 9))), (2, "1\n2".to_string()));
}

#[test]
fn terminal_erases_inserts_and_deletes() {
    assert_eq!(vt_rows(&vt(10, 1, b"abcdef\x1b[3D\x1b[K")), ["abc"]);
    assert_eq!(vt_rows(&vt(10, 1, b"abcdef\x1b[3D\x1b[1K")), ["    ef"]);
    assert_eq!(vt_rows(&vt(10, 1, b"abcdef\x1b[2K")), [""]);
    assert_eq!(vt_rows(&vt(10, 3, b"aa\r\nbbbb\r\ncc\x1b[2;3H\x1b[J")), ["aa", "bb", ""]);
    assert_eq!(vt_rows(&vt(10, 3, b"aa\r\nbbbb\r\ncc\x1b[2;3H\x1b[1J")), ["", "   b", "cc"]);
    assert_eq!(vt_rows(&vt(10, 1, b"abcdef\x1b[1;3H\x1b[2@")), ["ab  cdef"]);
    assert_eq!(vt_rows(&vt(10, 1, b"abcdef\x1b[1;2H\x1b[2P")), ["adef"]);
    assert_eq!(vt_rows(&vt(10, 1, b"abcdef\x1b[1;2H\x1b[2X")), ["a  def"]);
    let mut t = vt(10, 3, b"1\r\n2\r\n3\x1b[2;1H\x1b[L");
    assert_eq!(vt_rows(&t), ["1", "", "2"]);
    t.feed(b"\x1b[M");
    assert_eq!(vt_rows(&t), ["1", "2", ""]);
    // Erased cells keep the background colour set (xterm's bce).
    let t = vt(10, 1, b"\x1b[44m\x1b[K");
    assert_eq!(vt_cell(&t, 5, 0).bg, 4);
}

#[test]
fn terminal_scroll_region() {
    let mut t = vt(10, 5, b"1\r\n2\r\n3\r\n4\r\n5\x1b[2;4r\x1b[4;1H\n");
    assert_eq!(vt_rows(&t), ["1", "3", "4", "", "5"]);
    assert_eq!(t.screen.history(), 0, "a region's rows don't go to the scrollback");
    t.feed(b"\x1b[2;1H\x1bM");
    assert_eq!(vt_rows(&t), ["1", "", "3", "4", "5"]);
    t.feed(b"\x1b[r\x1b[5;1H\n");
    assert_eq!(vt_rows(&t), ["", "3", "4", "5", ""]);
}

#[test]
fn terminal_alternate_screen() {
    let mut t = vt(10, 2, b"main text");
    t.feed(b"\x1b[?1049h");
    assert!(t.screen.alt());
    assert_eq!(vt_rows(&t), ["", ""]);
    t.feed(b"vim\r\n\n\n\n");
    assert_eq!(t.screen.history(), 0);
    t.feed(b"\x1b[1;1Hx\x1b[?1049l");
    assert!(!t.screen.alt());
    assert_eq!(vt_rows(&t), ["main text", ""]);
    assert_eq!(t.screen.cursor(), (9, 0));
}

#[test]
fn terminal_colors_and_attributes() {
    use crate::vt::{BOLD, DEFAULT, INVERSE, ITALIC, UNDERLINE, rgb};
    let t = vt(20, 1, b"\x1b[1;31mA\x1b[0m\x1b[38;5;123mB\x1b[48;2;255;0;0mC\x1b[38:2::0:255:0mD\x1b[7;3;4mE\x1b[0;95;102mF\x1b[mG");
    let c = |x| vt_cell(&t, x, 0);
    assert_eq!((c(0).ch(), c(0).fg, c(0).flags() & BOLD), ('A', 1, BOLD));
    assert_eq!((c(1).fg, c(1).bg, c(1).flags()), (123, DEFAULT, 0));
    assert_eq!((c(2).fg, c(2).bg), (123, rgb(255, 0, 0)));
    assert_eq!(c(3).fg, rgb(0, 255, 0));
    assert_eq!(c(4).flags(), INVERSE | ITALIC | UNDERLINE);
    assert_eq!((c(5).fg, c(5).bg), (13, 10));
    assert_eq!(c(6), crate::vt::Cell { c: 'G' as u32, ..crate::vt::Cell::BLANK });
}

#[test]
fn terminal_wide_characters() {
    use crate::vt::{SPACER, WIDE};
    let mut t = vt(10, 2, "a漢b".as_bytes());
    assert_eq!(vt_rows(&t)[0], "a漢b");
    assert_eq!((vt_cell(&t, 1, 0).flags() & WIDE, vt_cell(&t, 2, 0).flags() & SPACER), (WIDE, SPACER));
    assert_eq!(t.screen.cursor(), (4, 0));
    // Writing over one half blanks the other.
    t.feed(b"\x1b[1;3Hx");
    assert_eq!(vt_rows(&t)[0], "a xb");
    // One that doesn't fit at the end of a row starts the next.
    let t = vt(5, 2, "abcd漢".as_bytes());
    assert_eq!(vt_rows(&t), ["abcd", "漢"]);
    assert_eq!(t.screen.cursor(), (2, 1));
    // Combining marks don't take a cell.
    assert_eq!(vt_rows(&vt(5, 1, "e\u{301}x".as_bytes())), ["ex"]);
}

#[test]
fn terminal_resizes() {
    let mut t = vt(10, 4, b"1\r\n2\r\n3\r\n4");
    t.screen.resize(10, 2);
    assert_eq!((vt_rows(&t), t.screen.history(), t.screen.cursor()), (vec!["3".to_string(), "4".into()], 2, (1, 1)));
    t.screen.resize(10, 4);
    assert_eq!((vt_rows(&t), t.screen.cursor()), (vec!["1".to_string(), "2".into(), "3".into(), "4".into()], (1, 3)));
    t.screen.resize(3, 4);
    t.feed(b"\r\x1b[Kabcdef");
    assert_eq!(vt_rows(&t)[3], "def");
    // Rows below the cursor go before any above it do.
    let mut t = vt(10, 4, b"x");
    t.screen.resize(10, 2);
    assert_eq!((vt_rows(&t), t.screen.history()), (vec!["x".to_string(), String::new()], 0));
    // The alternate screen and the main one under it both fit the new size.
    let mut t = vt(10, 3, b"a\r\nb\r\nc\x1b[?1049h");
    t.screen.resize(4, 2);
    t.feed(b"\x1b[?1049l");
    assert_eq!(vt_rows(&t), ["b", "c"]);
}

/// A new width rewraps what was written, scrollback included, and the cursor keeps its
/// place in its line; going back to the old width gives the old rows back.
#[test]
fn terminal_rewraps_on_resize() {
    let mut t = vt(10, 4, b"hello world, it wraps\r\n$ ");
    assert_eq!((vt_rows(&t), t.screen.cursor()), (vec!["hello worl".to_string(), "d, it wrap".into(), "s".into(), "$".into()], (2, 3)));
    t.screen.resize(25, 4);
    assert_eq!((vt_rows(&t), t.screen.cursor()), (vec!["hello world, it wraps".to_string(), "$".into(), "".into(), "".into()], (2, 1)));
    t.screen.resize(10, 4);
    assert_eq!((vt_rows(&t), t.screen.cursor()), (vec!["hello worl".to_string(), "d, it wrap".into(), "s".into(), "$".into()], (2, 3)));
    // Narrower than the screen holds: the top rows go into the scrollback, and a row
    // kept there that ended in a space still has it when the line is joined again.
    t.screen.resize(6, 4);
    assert_eq!((vt_rows(&t), t.screen.history(), t.screen.cursor()), (vec!["world,".to_string(), " it wr".into(), "aps".into(), "$".into()], 1, (2, 3)));
    t.screen.resize(25, 4);
    assert_eq!((vt_rows(&t)[..2].to_vec(), t.screen.history(), t.screen.cursor()), (vec!["hello world, it wraps".to_string(), "$".into()], 0, (2, 1)));
    // Typing goes on where the cursor is.
    t.feed(b"ls");
    assert_eq!(vt_rows(&t)[1], "$ ls");
    // A wide character pushed to the next row by the edge rejoins its line.
    let mut t = vt(5, 2, "abcd漢".as_bytes());
    t.screen.resize(10, 2);
    assert_eq!((vt_rows(&t), t.screen.cursor()), (vec!["abcd漢".to_string(), "".into()], (6, 0)));
    t.screen.resize(5, 2);
    assert_eq!(vt_rows(&t), ["abcd", "漢"]);
    // The main screen rewraps under a full-screen program, which redraws its own.
    let mut t = vt(10, 3, b"hello world\x1b[?1049h");
    t.screen.resize(20, 3);
    t.feed(b"\x1b[?1049l");
    assert_eq!((vt_rows(&t)[0].as_str(), t.screen.cursor()), ("hello world", (11, 0)));
}

#[test]
fn terminal_answers_and_modes() {
    let mut t = vt(10, 5, b"\x1b[3;5H\x1b[6n\x1b[c");
    assert_eq!(std::mem::take(&mut t.screen.replies), b"\x1b[3;5R\x1b[?62;22c");
    t.feed(b"\x1b[?1h\x1b[?2004h\x1b[?25l\x1b[5 q");
    let s = &t.screen;
    assert!(s.app_cursor && s.bracketed_paste && !s.cursor_shown && s.shape == crate::vt::Shape::Bar);
    t.feed(b"\x1b]0;my title\x07\x1b]7;file://host/tmp/a%20b\x1b\\");
    assert_eq!((t.screen.title.as_str(), t.screen.cwd.as_deref()), ("my title", Some("/tmp/a b")));
    // Line drawing characters, as ncurses draws boxes.
    assert_eq!(vt_rows(&vt(10, 1, b"\x1b(0lqk\x1b(Bq")), ["┌─┐q"]);
}

#[test]
fn terminal_scrollback_is_bounded() {
    let mut t = vt(10, 2, b"");
    for i in 0..6000 {
        t.feed(format!("{i}\r\n").as_bytes());
    }
    let s = &t.screen;
    assert_eq!(s.history(), crate::vt::SCROLLBACK);
    assert_eq!(s.dropped, 6000 - crate::vt::SCROLLBACK as u64 - 1);
    let first = s.line(s.dropped).unwrap();
    assert_eq!((s.text((s.dropped, 0), (s.dropped, 9)), first.cells.len()), ("999".to_string(), 3), "kept rows hold only what shows");
    let mut t = vt(10, 2, b"1\r\n2\r\n3\x1b[3J");
    assert_eq!(t.screen.history(), 0);
    t.screen.scroll_view(5);
    assert_eq!(t.screen.scroll, 0);
}

#[test]
fn terminal_pty_round_trip() {
    use crate::terminal::Pty;
    use std::process::Command;
    let wait = |p: &Pty, ok: &dyn Fn(&crate::vt::Screen) -> bool| {
        let t0 = Instant::now();
        while !ok(&p.shared.term().screen) {
            assert!(t0.elapsed() < Duration::from_secs(5), "timed out: {:?}", vt_rows(&p.shared.term()));
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let ctx = egui::Context::default();
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "printf hi"]);
    let p = Pty::spawn(sh, 40, 5, ctx.clone()).unwrap();
    wait(&p, &|s| s.row_text(0) == "hi");
    let t0 = Instant::now();
    while !p.shared.ended() {
        assert!(t0.elapsed() < Duration::from_secs(5), "the reader sees the end");
        std::thread::sleep(Duration::from_millis(10));
    }
    // The terminal echoes what is typed, then cat writes it back.
    let mut p = Pty::spawn(Command::new("cat"), 40, 5, ctx.clone()).unwrap();
    p.write(b"abc\r");
    wait(&p, &|s| s.row_text(0) == "abc" && s.row_text(1) == "abc");
    let pid = p.pid().unwrap();
    drop(p);
    let t0 = Instant::now();
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(t0.elapsed() < Duration::from_secs(5), "hung up and reaped");
        std::thread::sleep(Duration::from_millis(10));
    }
    // A new size reaches the program.
    let mut sh = Command::new("/bin/sh");
    sh.args(["-c", "sleep 0.3; stty size"]);
    let p = Pty::spawn(sh, 40, 5, ctx).unwrap();
    p.resize(33, 7);
    wait(&p, &|s| s.row_text(0) == "7 33");
}

/// Hyperlinks as `ls --hyperlink=always` writes them: each name links to its file, the
/// spaces between don't; a link written with an id and a BEL, cut by what is written
/// over it, and kept on its text when the line is rewrapped.
#[test]
fn terminal_hyperlinks() {
    let d = tmpdir("vt-links");
    touch(&d.join("a b.txt"), "x");
    touch(&d.join("notes.md"), "y");
    let out = std::process::Command::new("ls").args(["--hyperlink=always", "--color=never", "-1"]).current_dir(&d).output().unwrap();
    assert!(out.status.success() && out.stdout.starts_with(b"\x1b]8;;file://"), "{:?}", String::from_utf8_lossy(&out.stdout));
    // The terminal's line discipline turns its line feeds into CR LF.
    let bytes: Vec<u8> = out.stdout.iter().flat_map(|&b| if b == b'\n' { vec![b'\r', b'\n'] } else { vec![b] }).collect();
    let t = vt(40, 4, &bytes);
    assert_eq!(vt_rows(&t)[..2], ["a b.txt", "notes.md"]);
    let row = |y| t.screen.line(t.screen.view_line(y)).unwrap();
    let path = |y, x| row(y).link(x).and_then(|u| crate::termlink::file_uri(u));
    assert_eq!(path(0, 0), Some(d.join("a b.txt").display().to_string()));
    assert_eq!(path(0, 6), path(0, 0), "the whole name");
    assert_eq!(path(1, 3), Some(d.join("notes.md").display().to_string()));
    assert!(row(0).link(7).is_none() && row(1).link(8).is_none(), "nothing after the name");
    let mut t = vt(10, 3, b"go \x1b]8;id=x1;https://example.com/a;b\x07here and\x1b]8;;\x07 on");
    let link = |t: &crate::vt::Term, n: u64, x: usize| t.screen.line(n).and_then(|r| r.link(x)).map(|u| u.to_string());
    assert_eq!(link(&t, 0, 3).as_deref(), Some("https://example.com/a;b"));
    // "go here an" + "d on": it runs on to the next row, and stops.
    assert_eq!((link(&t, 0, 2), link(&t, 1, 0).as_deref(), link(&t, 1, 1)), (None, Some("https://example.com/a;b"), None));
    // Wider, the line is one row and the link still on "here and".
    t.screen.resize(20, 3);
    assert_eq!(vt_rows(&t)[0], "go here and on");
    let n = t.screen.view_line(0);
    assert_eq!((3..11).filter(|&x| link(&t, n, x).is_some()).count(), 8);
    assert!(link(&t, n, 2).is_none() && link(&t, n, 11).is_none());
    // Plain text written over part of it unlinks that part only.
    t.feed(b"\x1b[1;4HHE");
    assert_eq!((link(&t, n, 3), link(&t, n, 4), link(&t, n, 5).is_some()), (None, None, true));
    let _ = std::fs::remove_dir_all(&d);
}

/// Mouse reports in each of xterm's modes and encodings, modifiers included.
#[test]
fn terminal_mouse_reports() {
    use crate::vt::Mouse::*;
    let none = (false, false, false);
    let mut t = vt(300, 5, b"");
    assert_eq!(t.screen.report(Press(0), 2, 3, none), None, "not asked for");
    // less --mouse and vim with mouse=a ask for these.
    t.feed(b"\x1b[?1006;1000h");
    let s = |t: &crate::vt::Term, e, x, y, m| t.screen.report(e, x, y, m).map(|b| String::from_utf8_lossy(&b).into_owned());
    assert_eq!(s(&t, Press(0), 2, 3, none).as_deref(), Some("\x1b[<0;3;4M"));
    assert_eq!(s(&t, Release(2), 2, 3, none).as_deref(), Some("\x1b[<2;3;4m"));
    assert_eq!(s(&t, Wheel(true), 0, 0, (false, false, true)).as_deref(), Some("\x1b[<80;1;1M"));
    assert_eq!(s(&t, Wheel(false), 0, 0, (true, true, false)).as_deref(), Some("\x1b[<77;1;1M"));
    assert_eq!(s(&t, Motion(Some(0)), 4, 0, none), None, "1000 is clicks only");
    t.feed(b"\x1b[?1002h");
    assert_eq!(s(&t, Motion(Some(0)), 4, 0, none).as_deref(), Some("\x1b[<32;5;1M"));
    assert_eq!(s(&t, Motion(None), 4, 0, none), None, "1002 is drags only");
    t.feed(b"\x1b[?1003h");
    assert_eq!(s(&t, Motion(None), 4, 0, none).as_deref(), Some("\x1b[<35;5;1M"));
    // xterm's own bytes, which can't say a column past 223.
    t.feed(b"\x1b[?1006l");
    assert_eq!(t.screen.report(Press(2), 2, 3, (true, false, false)), Some(vec![0x1b, b'[', b'M', 32 + 2 + 4, 32 + 3, 32 + 4]));
    assert_eq!(t.screen.report(Release(2), 2, 3, none), Some(vec![0x1b, b'[', b'M', 32 + 3, 32 + 3, 32 + 4]));
    assert_eq!(t.screen.report(Press(0), 250, 0, none), None);
    t.feed(b"\x1b[?1015h");
    assert_eq!(s(&t, Press(1), 2, 3, none).as_deref(), Some("\x1b[33;3;4M"));
    t.feed(b"\x1b[?1015l\x1b[?1005h");
    assert_eq!(s(&t, Press(0), 250, 0, none), Some(format!("\x1b[M {}!", char::from_u32(32 + 251).unwrap())));
    // X10's own mode: presses only, no modifiers.
    t.feed(b"\x1b[?1005l\x1b[?9h");
    assert_eq!(t.screen.report(Press(0), 0, 0, (false, false, true)), Some(b"\x1b[M !!".to_vec()));
    assert_eq!(t.screen.report(Release(0), 0, 0, none), None);
    t.feed(b"\x1b[?9l");
    assert_eq!(t.screen.report(Press(0), 0, 0, none), None);
}

/// Real programs asking for the mouse get it: less scrolls with the wheel, and a click
/// puts vim's cursor where it was.
#[test]
fn terminal_mouse_with_real_programs() {
    use crate::terminal::Pty;
    use crate::vt::Mouse::*;
    use std::process::Command;
    let wait = |p: &Pty, what: &str, ok: &dyn Fn(&crate::vt::Screen) -> bool| {
        let t0 = Instant::now();
        while !ok(&p.shared.term().screen) {
            assert!(t0.elapsed() < Duration::from_secs(10), "{what}: {:?}", vt_rows(&p.shared.term()));
            std::thread::sleep(Duration::from_millis(10));
        }
    };
    let d = tmpdir("vt-mouse");
    let file = d.join("lines.txt");
    touch(&file, &(1..=200).map(|i| format!("line {i}\n")).collect::<String>());
    let ctx = egui::Context::default();
    let none = (false, false, false);
    if crate::media::have("less") {
        let mut c = Command::new("less");
        c.arg("--mouse").arg(&file).env("LESSHISTFILE", "-");
        let mut p = Pty::spawn(c, 40, 10, ctx.clone()).unwrap();
        // less 710 asks for drags too (1002) after clicks (1000); older ones clicks alone.
        wait(&p, "less", &|s| matches!(s.mouse, crate::vt::Track::Click | crate::vt::Track::Drag) && s.mouse_enc == crate::vt::Enc::Sgr && s.row_text(0) == "line 1");
        let down = p.shared.term().screen.report(Wheel(false), 5, 5, none).unwrap();
        p.write(&down.repeat(3));
        wait(&p, "less scrolled down", &|s| s.row_text(0) != "line 1");
        let up = p.shared.term().screen.report(Wheel(true), 5, 5, none).unwrap();
        p.write(&up.repeat(10));
        wait(&p, "less scrolled back", &|s| s.row_text(0) == "line 1");
        p.write(b"q");
        wait(&p, "less let the mouse go", &|s| s.mouse == crate::vt::Track::Off && !s.alt());
    }
    if crate::media::have("vim") {
        let mut c = Command::new("vim");
        c.args(["-u", "NONE", "-i", "NONE", "-n", "-N", "-c", "set mouse=a"]).arg(&file);
        let mut p = Pty::spawn(c, 40, 10, ctx).unwrap();
        wait(&p, "vim", &|s| s.mouse == crate::vt::Track::Drag && s.row_text(0) == "line 1");
        let (press, release) = {
            let t = p.shared.term();
            (t.screen.report(Press(0), 2, 5, none).unwrap(), t.screen.report(Release(0), 2, 5, none).unwrap())
        };
        p.write(&press);
        p.write(&release);
        std::thread::sleep(Duration::from_millis(100));
        p.write(b"iX\x1b");
        wait(&p, "the X where vim was clicked", &|s| s.row_text(5) == "liXne 6");
        p.write(b":q!\r");
        wait(&p, "vim let the mouse go", &|s| s.mouse == crate::vt::Track::Off);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Find goes through the scrollback and the screen, a line that ran on as one; a
/// double click takes a word (a path with its line number whole), a triple the line.
#[test]
fn terminal_finds_and_selects() {
    let mut t = vt(10, 3, b"alpha one\r\n123456 Beta\r\nsee ~/src/main.rs:12, ok\r\n");
    for i in 0..20 {
        t.feed(format!("{i} beta\r\n").as_bytes());
    }
    let s = &t.screen;
    assert!(s.history() > 5);
    let mut hits = vec![];
    s.find("beta", false, 100, &mut hits);
    assert_eq!(hits.len(), 21);
    let first = s.dropped;
    // "123456 Bet" + "a": the match runs over the edge.
    assert_eq!(hits[0], ((first + 1, 7), (first + 2, 0)));
    assert_eq!(s.text(hits[0].0, hits[0].1), "Beta");
    s.find("Beta", true, 100, &mut hits);
    assert_eq!(hits.len(), 1, "upper case matches case");
    s.find("beta", false, 5, &mut hits);
    assert_eq!(hits.len(), 5);
    s.find("zzz", false, 100, &mut hits);
    assert!(hits.is_empty());
    let word = |p: (u64, usize)| s.word(p).map(|(a, b)| s.text(a, b));
    assert_eq!(word((first + 2, 0)).as_deref(), Some("Beta"), "a word over the edge");
    // "see ~/src/" "main.rs:12" ", ok"
    assert_eq!(word((first + 4, 2)).as_deref(), Some("~/src/main.rs:12"));
    assert_eq!(s.word((first + 1, 6)), Some(((first + 1, 6), (first + 1, 6))), "a blank, alone");
    assert_eq!(word((first + 5, 0)).as_deref(), Some(","));
    // A word ending a sentence leaves its full stop, unless the full stop was clicked.
    let t2 = vt(30, 2, b"read ~/notes.md. then a.b, c?");
    let s2 = &t2.screen;
    let n2 = s2.view_line(0);
    let word2 = |x| s2.word((n2, x)).map(|(a, b)| s2.text(a, b));
    assert_eq!((word2(6).as_deref(), word2(15).as_deref()), (Some("~/notes.md"), Some("~/notes.md.")));
    assert_eq!((word2(22).as_deref(), word2(27).as_deref()), (Some("a.b"), Some("c")));
    let (a, b) = s.line_span(first + 4);
    assert_eq!(s.text(a, b), "see ~/src/main.rs:12, ok");
}

/// Addresses and paths in a line, as compilers, grep and people write them.
#[test]
fn terminal_links_in_text() {
    use crate::termlink::{Target, at, resolve};
    let find = |l: &str, w: &str| {
        let i = l[..l.find(w).unwrap()].chars().count();
        at(l, i).map(|(a, b, t)| (l.chars().skip(a).take(b - a).collect::<String>(), t))
    };
    let url = |s: &str| Target::Url(s.into());
    let path = |s: &str, line, col| Target::Path { text: s.into(), line, col };
    assert_eq!(find("see https://example.com/a_(b). done", "example"), Some(("https://example.com/a_(b)".into(), url("https://example.com/a_(b)"))));
    assert_eq!(find("(at https://x.org/y),", "x.org"), Some(("https://x.org/y".into(), url("https://x.org/y"))));
    assert_eq!(find("<http://localhost:8080/>", "local"), Some(("http://localhost:8080/".into(), url("http://localhost:8080/"))));
    assert_eq!(find("error: src/main.rs:12:5: oops", "src"), Some(("src/main.rs:12:5".into(), path("src/main.rs", Some(12), Some(5)))));
    assert_eq!(find("  --> src/vt.rs:42", "vt"), Some(("src/vt.rs:42".into(), path("src/vt.rs", Some(42), None))));
    // grep -n and grep -n --column: the match after the numbers is not part of it.
    assert_eq!(find("notes.txt:7:some match", "notes"), Some(("notes.txt:7".into(), path("notes.txt", Some(7), None))));
    assert_eq!(find("notes.txt:7:3:some match", "notes"), Some(("notes.txt:7:3".into(), path("notes.txt", Some(7), Some(3)))));
    assert_eq!(find("notes.txt:7:some match", "some"), None);
    assert_eq!(find("see host:8080x", "host"), Some(("host:8080x".into(), path("host:8080x", None, None))));
    assert_eq!(find("cc --out=build/app.o", "build"), Some(("build/app.o".into(), path("build/app.o", None, None))));
    assert_eq!(find("open ~/notes.md.", "~"), Some(("~/notes.md".into(), path("~/notes.md", None, None))));
    assert_eq!(find("'/tmp/a b'", "/tmp"), Some(("/tmp/a".into(), path("/tmp/a", None, None))));
    assert_eq!(find("file:///tmp/a%20b", "file"), Some(("file:///tmp/a%20b".into(), path("/tmp/a b", None, None))));
    assert_eq!(at("a  b", 1), None);
    assert_eq!(at("...", 1), None);
    let d = tmpdir("vt-paths");
    touch(&d.join("sub/file.rs"), "fn main() {}\n");
    assert_eq!(resolve("sub/file.rs", Some(&d)), Some((d.join("sub/file.rs"), false)));
    assert_eq!(resolve("./sub/../sub", Some(&d)), Some((d.join("sub"), true)));
    assert_eq!(resolve(&d.join("sub").display().to_string(), None), Some((d.join("sub"), true)));
    assert_eq!(resolve("missing.rs", Some(&d)), None);
    assert_eq!(resolve("sub/file.rs", None), None, "relative without a folder to be in");
    assert_eq!(resolve("~", None), Some((crate::fsx::home(), true)));
    let _ = std::fs::remove_dir_all(&d);
}

/// Conflicts settled as "if newer" (replaced only by a newer file, else skipped) and by
/// a name typed for the one coming.
#[test]
fn conflicts_if_newer_and_renamed() {
    let ctx = egui::Context::default();
    let d = tmpdir("conflict-more");
    let (src, dst) = (d.join("src"), d.join("dst"));
    for (name, body) in [("new.txt", "newer"), ("old.txt", "older"), ("named.txt", "mine")] {
        touch(&src.join(name), body);
        touch(&dst.join(name), "there");
    }
    let past = std::time::SystemTime::now() - Duration::from_secs(3600);
    for p in [dst.join("new.txt"), src.join("old.txt")] {
        std::fs::File::options().write(true).open(p).unwrap().set_modified(past).unwrap();
    }
    let decide = HashMap::from([(src.join("new.txt"), Conflict::Newer), (src.join("old.txt"), Conflict::Newer), (src.join("named.txt"), Conflict::Rename("kept.txt".into()))]);
    let j = ops::spawn(1, OpKind::Copy { srcs: ["new.txt", "old.txt", "named.txt"].iter().map(|n| src.join(n)).collect(), dest: dst.clone(), decide }, ctx);
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty());
    assert_eq!(std::fs::read_to_string(dst.join("new.txt")).unwrap(), "newer", "newer: replaced");
    assert_eq!(std::fs::read_to_string(dst.join("old.txt")).unwrap(), "there", "older: skipped");
    assert_eq!(std::fs::read_to_string(dst.join("kept.txt")).unwrap(), "mine", "renamed");
    assert_eq!(std::fs::read_to_string(dst.join("named.txt")).unwrap(), "there");
    let _ = std::fs::remove_dir_all(&d);
}

/// An animated GIF over `pal`: frames of (left, top, width, height, palette indices,
/// delay in hundredths, disposal), index 0 transparent.
fn gif_bytes(w: u16, h: u16, pal: &[u8], repeat: gif::Repeat, frames: &[(u16, u16, u16, u16, Vec<u8>, u16, gif::DisposalMethod)]) -> Vec<u8> {
    let mut out = vec![];
    {
        let mut e = gif::Encoder::new(&mut out, w, h, pal).unwrap();
        e.set_repeat(repeat).unwrap();
        for (x, y, fw, fh, px, delay, dispose) in frames {
            let mut f = gif::Frame::from_indexed_pixels(*fw, *fh, px.clone(), Some(0));
            (f.left, f.top, f.delay, f.dispose) = (*x, *y, *delay, *dispose);
            e.write_frame(&f).unwrap();
        }
    }
    out
}

/// `n` whole-picture frames, frame k all of colour k + 1 of a palette where colour i
/// is (i, 255 - i, 0), each `delay` hundredths.
pub(crate) fn gif_run(w: u16, h: u16, n: u8, delay: u16, repeat: gif::Repeat) -> Vec<u8> {
    let pal: Vec<u8> = (0..=255u8).flat_map(|i| [i, 255 - i, 0]).collect();
    let frames: Vec<_> = (0..n).map(|k| (0, 0, w, h, vec![k + 1; w as usize * h as usize], delay, gif::DisposalMethod::Keep)).collect();
    gif_bytes(w, h, &pal, repeat, &frames)
}

/// The colour frame k of `gif_run` shows.
fn run_colour(k: u8) -> egui::Color32 {
    egui::Color32::from_rgb(k + 1, 254 - k, 0)
}

/// Animated GIF frames composited as browsers do: each drawn over the last where it
/// says, its transparent pixels showing what lies under them, an area cleared or put
/// back as the frame before asks; delays as written (the tiny ones shown for a tenth of
/// a second) and the loop count as times played. One frame is one frame; a still PNG
/// is no animation.
#[test]
fn animated_gif_frames() {
    use crate::anim::{Frames, delay_ms};
    use gif::DisposalMethod::{Background, Keep, Previous};
    let d = tmpdir("anim-gif");
    let path = d.join("a.gif");
    let pal = [0, 0, 0, 255, 0, 0, 0, 255, 0, 0, 0, 255];
    let frames = [(0, 0, 8, 6, vec![1; 48], 5, Keep), (2, 2, 4, 3, vec![2; 12], 0, Background), (0, 0, 2, 2, vec![3, 3, 3, 0], 1, Previous), (7, 5, 1, 1, vec![2], 30, Keep)];
    std::fs::write(&path, gif_bytes(8, 6, &pal, gif::Repeat::Finite(2), &frames)).unwrap();
    let mut f = Frames::open(&path).unwrap();
    let px = |f: &Frames, x: usize, y: usize| <[u8; 4]>::try_from(&f.canvas.px[(y * 8 + x) * 4..][..4]).unwrap();
    let (red, green, blue, clear) = ([255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255], [0; 4]);
    let mut delays = vec![f.next().unwrap()];
    assert_eq!(px(&f, 0, 0), red);
    delays.push(f.next().unwrap());
    assert_eq!((px(&f, 3, 3), px(&f, 0, 0)), (green, red));
    delays.push(f.next().unwrap());
    assert_eq!(px(&f, 3, 3), clear, "the frame before cleared its area");
    assert_eq!((px(&f, 0, 0), px(&f, 1, 1)), (blue, red), "a transparent pixel shows what is under it");
    delays.push(f.next().unwrap());
    assert_eq!((px(&f, 0, 0), px(&f, 7, 5)), (red, green), "the frame before put back what it covered");
    assert!(f.next().is_none());
    assert_eq!(delays, [50, 0, 10, 300]);
    assert_eq!(delays.iter().map(|&d| delay_ms(d)).collect::<Vec<_>>(), [50, 100, 100, 300]);
    assert_eq!(f.plays, Some(3), "twice more after the first time");
    std::fs::write(&path, gif_run(4, 4, 1, 10, gif::Repeat::Infinite)).unwrap();
    let mut f = Frames::open(&path).unwrap();
    assert!(f.next().is_some() && f.next().is_none() && f.plays.is_none(), "one frame, looping forever");
    std::fs::write(d.join("still.png"), png_bytes(4, 4, 1)).unwrap();
    assert!(Frames::open(&d.join("still.png")).is_none());
    assert!(Frames::open(&d.join("missing.gif")).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

/// APNG frames: a frame blended over what is there or replacing it, at its offset; an
/// area cleared after its frame; delays from their fractions; plays as written. A default
/// image outside the animation is not one of its frames.
#[test]
fn apng_frames() {
    use crate::anim::Frames;
    use png::{BlendOp, DisposeOp};
    let apng = |separate: bool| {
        let mut out = vec![];
        let mut e = png::Encoder::new(&mut out, 4, 4);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        e.set_animated(3, 2).unwrap();
        e.set_sep_def_img(separate).unwrap();
        let mut w = e.write_header().unwrap();
        if separate {
            w.write_image_data(&[255; 64]).unwrap();
        }
        let red: Vec<u8> = [255, 0, 0, 255].repeat(16);
        let half_blue: Vec<u8> = [0, 0, 255, 128].repeat(4);
        let green: Vec<u8> = [0, 255, 0, 255].repeat(4);
        for (x, y, fw, fh, px, (n, den), blend, dispose) in [(0, 0, 4, 4, red, (1, 10), BlendOp::Source, DisposeOp::None), (1, 1, 2, 2, half_blue, (3, 0), BlendOp::Over, DisposeOp::Background), (2, 2, 2, 2, green, (25, 1000), BlendOp::Source, DisposeOp::None)] {
            w.reset_frame_position().unwrap();
            w.set_frame_dimension(fw, fh).unwrap();
            w.set_frame_position(x, y).unwrap();
            w.set_frame_delay(n, den).unwrap();
            w.set_blend_op(blend).unwrap();
            w.set_dispose_op(dispose).unwrap();
            w.write_image_data(&px).unwrap();
        }
        w.finish().unwrap();
        out
    };
    let d = tmpdir("apng");
    for separate in [false, true] {
        let path = d.join("a.png");
        std::fs::write(&path, apng(separate)).unwrap();
        let mut f = Frames::open(&path).unwrap_or_else(|| panic!("animated (default image apart: {separate})"));
        let px = |f: &Frames, x: usize, y: usize| <[u8; 4]>::try_from(&f.canvas.px[(y * 4 + x) * 4..][..4]).unwrap();
        assert_eq!(f.next(), Some(100));
        assert_eq!(px(&f, 1, 1), [255, 0, 0, 255], "the first frame, not the default image");
        assert_eq!(f.next(), Some(30), "a zero denominator means hundredths");
        let [r, g, b, a] = px(&f, 1, 1);
        assert!(a == 255 && (126..=128).contains(&r) && g == 0 && (127..=129).contains(&b), "half blue over red: {:?}", [r, g, b, a]);
        assert_eq!(px(&f, 0, 0), [255, 0, 0, 255], "outside the frame untouched");
        assert_eq!(f.next(), Some(25));
        assert_eq!(px(&f, 1, 1), [0; 4], "cleared after its frame");
        assert_eq!(px(&f, 3, 3), [0, 255, 0, 255]);
        assert!(f.next().is_none());
        assert_eq!(f.plays, Some(2));
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The real ImageMagick (`magick`, else the older `convert`) on the PATH, as a stand-in
/// folder holding only it, and its name.
pub(crate) fn any_imagemagick_tools(dir: &Path) -> Option<(PathBuf, &'static str)> {
    let path = std::env::var_os("PATH")?;
    let (real, name) = ["magick", "convert"].into_iter().find_map(|n| std::env::split_paths(&path).map(|d| d.join(n)).find(|m| m.is_file()).map(|m| (m, n)))?;
    std::fs::create_dir_all(dir).unwrap();
    let _ = std::os::unix::fs::symlink(real, dir.join(name));
    Some((dir.to_path_buf(), name))
}

/// An animated WebP made by ImageMagick, when it is here: three lossless frames, the
/// decoder's composited canvas each time, their delays, looping forever.
#[test]
fn animated_webp_frames() {
    let d = tmpdir("anim-webp");
    let Some((tools, im)) = any_imagemagick_tools(&d.join("tools")) else {
        eprintln!("animated WebP: skipped (no ImageMagick)");
        return;
    };
    let path = d.join("a.webp");
    let made = std::process::Command::new(tools.join(im)).args(["-size", "40x30", "xc:red", "xc:lime", "xc:blue", "-set", "delay", "5", "-loop", "0", "-define", "webp:lossless=true"]).arg(&path).status().is_ok_and(|s| s.success());
    let Some(mut f) = made.then(|| crate::anim::Frames::open(&path)).flatten() else {
        eprintln!("animated WebP: skipped (ImageMagick wrote no animated WebP)");
        return;
    };
    let mut seen = vec![];
    while let Some(ms) = f.next() {
        seen.push((ms, <[u8; 4]>::try_from(&f.canvas.px[..4]).unwrap()));
    }
    // Lossless, but ImageMagick may round a level by one.
    let near = |a: [u8; 4], b: [u8; 4]| a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= 1);
    assert!(seen.len() == 3 && seen.iter().zip([[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]]).all(|(s, c)| s.0 == 50 && near(s.1, c)), "{seen:?}");
    assert_eq!(f.plays, None);
    let _ = std::fs::remove_dir_all(&d);
}

/// Drive `p` through `secs` of simulated time a frame (1/60 s) at a time, its worker
/// given a moment each frame; the colours shown go to `seen`, each once as it comes,
/// and the most frames it held to `held`.
fn play_for(p: &mut crate::anim::Player, ctx: &egui::Context, t: &mut f64, secs: f64, held: &mut usize, seen: &mut Vec<egui::Color32>) {
    let end = *t + secs;
    while *t < end {
        *t += 1.0 / 60.0;
        ctx.run_pass(egui::RawInput { time: Some(*t), ..Default::default() }, |_| {
            p.frame(true);
        }).drop_without_applying_deltas();
        let (_, n, _, _, c) = p.state();
        *held = (*held).max(n);
        if let Some(c) = c.filter(|c| seen.last() != Some(c)) {
            seen.push(c);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

/// Playing: frames on time, kept and looped from memory when they fit, as many times as
/// the GIF says, then still on the last; a pause holds the frame. One too long to keep
/// is decoded again each time round, a couple of frames held at once. Big frames are
/// scaled to the view; a still GIF never shows as moving.
#[test]
fn animations_play_within_their_memory() {
    use crate::anim::Player;
    let d = tmpdir("anim-play");
    let ctx = egui::Context::default();
    let mut t = 0.0;
    let path = d.join("loop.gif");
    std::fs::write(&path, gif_run(32, 32, 10, 5, gif::Repeat::Finite(1))).unwrap();
    let mut p = Player::start(&path, None, 200.0, 256 << 20, &ctx);
    let (mut held, mut seen) = (0, vec![]);
    play_for(&mut p, &ctx, &mut t, 2.0, &mut held, &mut seen);
    let want: Vec<_> = (0..10).chain(0..10).map(run_colour).collect();
    assert_eq!(seen, want, "twice through, in order");
    assert!(!p.playing && p.moving(), "then still on its last frame");
    assert_eq!((held, p.state().3), (10, false), "kept whole");
    p.toggle();
    assert!(p.playing, "played again from the top");
    play_for(&mut p, &ctx, &mut t, 0.12, &mut held, &mut seen);
    p.toggle();
    let at = p.state().0;
    assert!(at > 0, "moved on");
    play_for(&mut p, &ctx, &mut t, 0.5, &mut held, &mut seen);
    assert_eq!(p.state().0, at, "paused");
    // Too long to keep within 100 kB: forty 16 kB frames.
    std::fs::write(&path, gif_run(64, 64, 40, 5, gif::Repeat::Infinite)).unwrap();
    let mut p = Player::start(&path, None, 64.0, 400 << 10, &ctx);
    let (mut warm, mut held, mut seen) = (0, 0, vec![]);
    play_for(&mut p, &ctx, &mut t, 1.5, &mut warm, &mut seen);
    play_for(&mut p, &ctx, &mut t, 3.0, &mut held, &mut seen);
    assert!(p.state().3, "decoded again each time round");
    assert!(held <= 3, "the frame shown and two ahead, not {held}");
    let want: Vec<_> = (0..120u32).map(|k| run_colour((k % 40) as u8)).collect();
    assert!(seen.len() >= 80 && seen == want[..seen.len()], "round and round, in order: {} frames", seen.len());
    // Scaled to the view.
    let img = crate::anim::shown(&crate::imgdec::Rgba { w: 3000, h: 1000, px: vec![9; 12_000_000] }, 512);
    assert_eq!(img.size, [512, 171]);
    assert!(!p.is_for(&d.join("other.gif"), None, 64.0) && p.is_for(&path, None, 64.0));
    // A still GIF.
    std::fs::write(&path, gif_run(8, 8, 1, 5, gif::Repeat::Infinite)).unwrap();
    let mut p = Player::start(&path, None, 64.0, 256 << 20, &ctx);
    play_for(&mut p, &ctx, &mut t, 0.3, &mut held, &mut seen);
    assert!(!p.moving() && p.state().1 == 0, "nothing kept for a still");
    let _ = std::fs::remove_dir_all(&d);
}

/// HEIC, AVIF, TIFF and the like through the real ImageMagick: a TIFF stored turned
/// (orientation 6) comes upright, at its upright size, scaled to what was asked; an AVIF
/// (made by ffmpeg, when it is here) the same. Nothing here writes HEIC, so it is
/// skipped. PNM read with comments and alpha; what to install when nothing reads a
/// format.
#[test]
fn photos_through_external_tools() {
    use crate::extpic::{can, install_note, load, pnm};
    let (img, rest) = pnm(b"P6\n# by hand\n2 1\n255\n\xff\x00\x00\x00\xff\x00 400 300\n").unwrap();
    assert_eq!((img.w, img.h, &img.px[..]), (2, 1, &[255, 0, 0, 255, 0, 255, 0, 255][..]));
    assert_eq!(rest, b" 400 300\n");
    let (img, _) = pnm(b"P7\nWIDTH 1\nHEIGHT 1\nDEPTH 2\nMAXVAL 255\nTUPLTYPE GRAYSCALE_ALPHA\nENDHDR\n\x80\x40").unwrap();
    assert_eq!(img.px, [128, 128, 128, 64]);
    assert!(pnm(b"P6\n2 1\n65535\n\x00\x00").is_none(), "16 bits");
    assert!(pnm(b"P6\n2 1\n255\n\x00").is_none(), "cut short");
    assert_eq!(install_note("heic"), Some("Showing HEIC photos needs ImageMagick"));
    assert_eq!(install_note("png"), None);
    let d = tmpdir("extpic");
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        }
    }
    let _reset = Reset;
    std::fs::create_dir_all(d.join("none")).unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(d.join("none")));
    assert!(!can(&crate::drives::Tools::current().unwrap(), "heic"), "no tools, no HEIC");
    let Some((tools, im)) = any_imagemagick_tools(&d.join("tools")) else {
        eprintln!("photos: skipped (no ImageMagick)");
        return;
    };
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
    let t = crate::drives::Tools::current().unwrap();
    assert!(["heic", "avif", "tiff", "tif", "nef", "dng"].iter().all(|e| can(&t, e)) && !can(&t, "png"));
    // Red above blue, stored turned: upright, blue is on the left and red on the right.
    let tiff = d.join("turned.tiff");
    let made = std::process::Command::new(tools.join(im)).args(["-size", "300x100", "xc:red", "-size", "300x100", "xc:blue", "-append", "-orient", "RightTop"]).arg(&tiff).status().unwrap();
    assert!(made.success());
    let (img, dims) = load(&t, &tiff, 64).expect("a TIFF");
    assert_eq!((dims, img.w, img.h), ([200, 300], 43, 64));
    let at = |x: u32, y: u32| &img.px[((y * img.w + x) * 4) as usize..][..3];
    assert!(at(2, 32)[2] > 200 && at(40, 32)[0] > 200, "upright: {:?} {:?}", at(2, 32), at(40, 32));
    let avif = d.join("red.avif");
    let ffmpeg = std::process::Command::new("ffmpeg").args(["-loglevel", "error", "-f", "lavfi", "-i", "color=c=red:s=320x240", "-frames:v", "1"]).arg(&avif).status().is_ok_and(|s| s.success());
    if ffmpeg {
        let (img, dims) = load(&t, &avif, 64).expect("an AVIF");
        assert_eq!((dims, img.w, img.h), ([320, 240], 64, 48));
        assert!(img.px[0] > 200 && img.px[1] < 60, "red: {:?}", &img.px[..4]);
    } else {
        eprintln!("photos: AVIF skipped (ffmpeg can't write one)");
    }
    eprintln!("photos: HEIC skipped (nothing here writes one)");
    assert!(load(&t, &d.join("missing.heic"), 64).is_none());
    std::fs::write(d.join("junk.tiff"), "not a picture").unwrap();
    assert!(load(&t, &d.join("junk.tiff"), 64).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

/// Permissions changed for several items and all the way down: bits set and cleared,
/// files inside given Execute only where they had it, the owner changed, and the same
/// written for chmod and chown when it has to run as administrator.
#[test]
fn permissions_set_cleared_and_deep() {
    use crate::perms::Change;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let c = Change { set: 0o111 | 0o4000, clear: 0o022, owner: None, group: None };
    assert_eq!(c.mode(0o664, false), 0o4755);
    assert_eq!(c.mode(0o664, true), 0o4644, "a document inside stays a document");
    assert_eq!(c.mode(0o700, true), 0o4711, "a program inside stays one");
    assert_eq!(c.symbolic(false).as_deref(), Some("u+xs,g+x,g-w,o+x,o-w"));
    assert_eq!(c.symbolic(true).as_deref(), Some("u+Xs,g+X,g-w,o+X,o-w"));
    let d = tmpdir("perms");
    touch(&d.join("top/doc.txt"), "x");
    touch(&d.join("top/sub/run.sh"), "x");
    let set = |p: &Path, m: u32| std::fs::set_permissions(p, std::fs::Permissions::from_mode(m)).unwrap();
    set(&d.join("top/doc.txt"), 0o666);
    set(&d.join("top/sub/run.sh"), 0o744);
    let gid = std::fs::metadata(&d).unwrap().gid();
    let c = Change { set: 0o011, clear: 0o002, owner: None, group: Some(gid) };
    let j = ops::spawn(1, OpKind::Perms { paths: vec![d.join("top")], change: c, deep: true }, egui::Context::default());
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty(), "{:?}", j.progress.lock().unwrap().errors);
    let mode = |p: &str| std::fs::metadata(d.join(p)).unwrap().mode() & 0o7777;
    assert_eq!((mode("top"), mode("top/sub"), mode("top/doc.txt"), mode("top/sub/run.sh")), (0o755, 0o755, 0o664, 0o755));
    let t = Change { set: 0o100, clear: 0, owner: Some(0), group: None }.admin(vec![d.join("top")], true).unwrap();
    let argv: Vec<String> = t.argv().iter().map(|a| a.to_string_lossy().into_owned()).collect();
    assert!(argv[0].ends_with("chown") && argv[1..4] == ["-R", "--", "root"], "{argv:?}");
    assert!(matches!(t, crate::admin::Task::Perms { mode: Some(ref m), .. } if m == "u+X"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn theme_follows_the_desktop_and_comes_back() {
    use crate::theme::ThemeName;
    let mut app = crate::app::App::new(&egui::Context::default(), vec![]);
    app.cfg.theme = ThemeName::Nord;
    app.follow_system(true);
    assert_eq!(app.cfg.theme, ThemeName::Nord, "already dark");
    app.follow_system(false);
    assert_eq!(app.cfg.theme, ThemeName::Paper);
    app.cfg.theme = ThemeName::Latte;
    app.follow_system(true);
    assert_eq!(app.cfg.theme, ThemeName::Nord, "back to the dark theme left");
    app.follow_system(false);
    assert_eq!(app.cfg.theme, ThemeName::Latte, "and to the light one");
}

#[test]
fn a_dropped_watch_lets_go_of_its_descriptor() {
    let fds = || std::fs::read_dir("/proc/self/fd").unwrap().filter_map(|e| std::fs::read_link(e.ok()?.path()).ok()).filter(|l| l.to_string_lossy().contains("inotify")).count();
    let before = fds();
    let d = std::env::temp_dir().join(format!("maia-watch-drop-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    for watched in [false, true] {
        let mut w = crate::watch::Watch::new(egui::Context::default());
        if watched {
            w.sync(&[d.clone()].into_iter().collect());
        }
        drop(w);
    }
    let t0 = std::time::Instant::now();
    while fds() > before && t0.elapsed() < std::time::Duration::from_secs(5) {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(fds(), before, "both closed, with or without a folder watched");
    let _ = std::fs::remove_dir_all(&d);
}


// ---------------------------------------------------------------- queue, check, redo

/// A second folder on another file system than the temp folder (/dev/shm, a tmpfs), for
/// moves across disks; None where there is none.
fn other_fs(name: &str) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let shm = Path::new("/dev/shm");
    let t = std::fs::metadata(std::env::temp_dir()).ok()?.dev();
    if std::fs::metadata(shm).ok()?.dev() == t {
        return None;
    }
    let d = shm.join(format!("maia-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// What was put in a trash from under `dir`, removed for good.
fn purge_trashed_under(dir: &Path) {
    for it in crate::trashcan::list().into_iter().filter(|it| it.original.starts_with(dir)) {
        let _ = crate::trashcan::purge(&it);
    }
}

fn app_frame(app: &mut crate::app::App, ctx: &egui::Context) {
    let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), ..Default::default() };
    ctx.run_pass(raw, |c| app.frame(c)).drop_without_applying_deltas();
}

fn app_until(app: &mut crate::app::App, ctx: &egui::Context, what: &str, ok: impl Fn(&crate::app::App) -> bool) {
    let t0 = Instant::now();
    while !ok(app) {
        assert!(t0.elapsed() < Duration::from_secs(20), "timed out waiting for {what}");
        app_frame(app, ctx);
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Partitions of one drive, a btrfs subvolume (a device of its own, 0:N, named only by
/// its mount's source), LUKS on a partition and LVM on that are all the one disk; a stick
/// is another; tmpfs and a FUSE mount are each their own. The sysfs is laid out as the
/// kernel lays it out, the mount table as /proc/self/mountinfo reads on such a laptop.
#[test]
fn disks_are_named_by_the_drive_under_them() {
    use std::os::unix::fs::symlink;
    let d = tmpdir("disks");
    let sys = d.join("sys");
    let nvme = sys.join("devices/pci0000:00/0000:00:06.0/nvme/nvme0/nvme0n1");
    let sda = sys.join("devices/pci0000:00/0000:00:14.0/usb2/2-1/host0/block/sda");
    for (dir, part) in [(&nvme, "nvme0n1p1"), (&nvme, "nvme0n1p2"), (&nvme, "nvme0n1p3"), (&sda, "sda1")] {
        touch(&dir.join(part).join("partition"), "1\n");
    }
    let dm0 = sys.join("devices/virtual/block/dm-0");
    let dm1 = sys.join("devices/virtual/block/dm-1");
    std::fs::create_dir_all(dm0.join("slaves")).unwrap();
    std::fs::create_dir_all(dm1.join("slaves")).unwrap();
    std::fs::create_dir_all(sys.join("class/block")).unwrap();
    std::fs::create_dir_all(sys.join("dev/block")).unwrap();
    let class = |n: &str, to: &Path| symlink(to, sys.join("class/block").join(n)).unwrap();
    class("nvme0n1", &nvme);
    class("nvme0n1p1", &nvme.join("nvme0n1p1"));
    class("nvme0n1p2", &nvme.join("nvme0n1p2"));
    class("nvme0n1p3", &nvme.join("nvme0n1p3"));
    class("sda", &sda);
    class("sda1", &sda.join("sda1"));
    class("dm-0", &dm0);
    class("dm-1", &dm1);
    // LUKS on the third partition, LVM inside it.
    symlink(nvme.join("nvme0n1p3"), dm0.join("slaves/nvme0n1p3")).unwrap();
    symlink(&dm0, dm1.join("slaves/dm-0")).unwrap();
    for (dev, to) in [("259:1", nvme.join("nvme0n1p1")), ("259:3", nvme.join("nvme0n1p3")), ("254:0", dm0.clone()), ("254:1", dm1.clone()), ("8:1", sda.join("sda1"))] {
        symlink(to, sys.join("dev/block").join(dev)).unwrap();
    }
    let mountinfo = "\
26 1 0:33 /@ / rw,relatime shared:1 - btrfs /dev/nvme0n1p2 rw,ssd,space_cache=v2,subvolid=256,subvol=/@
45 26 0:34 /@home /home rw,relatime shared:28 - btrfs /dev/nvme0n1p2 rw,ssd,space_cache=v2,subvolid=257,subvol=/@home
48 26 259:1 / /boot rw,relatime shared:31 - vfat /dev/nvme0n1p1 rw,fmask=0137,dmask=0027
50 26 254:1 / /srv rw,relatime shared:33 - ext4 /dev/mapper/vg-srv rw
27 26 0:25 / /tmp rw,nosuid,nodev shared:14 - tmpfs tmpfs rw,size=16195776k
98 30 0:50 / /run/user/1000/gvfs rw,nosuid,nodev,relatime shared:244 - fuse.gvfsd-fuse gvfsd-fuse rw,user_id=1000,group_id=1000
101 26 8:1 / /run/media/me/STICK rw,nosuid,nodev,relatime shared:250 - vfat /dev/sda1 rw,uid=1000
";
    let key = |dev| ops::disk_key(dev, &sys, mountinfo);
    for dev in [(0, 33), (0, 34), (259, 1), (259, 3), (254, 0), (254, 1)] {
        assert_eq!(key(dev), "nvme0n1", "{dev:?} is on the laptop's drive");
    }
    assert_eq!(key((8, 1)), "sda", "the stick");
    assert_eq!(key((0, 25)), "0:25", "tmpfs is no disk");
    assert_eq!(key((0, 50)), "0:50", "nor is a FUSE mount");
    assert_eq!(key((0, 99)), "0:99", "a device in no mount");
    // This machine, as it is: one file system is one disk, another file system can be.
    let a = ops::disk_of(&d).unwrap();
    assert_eq!(ops::disk_of(&d.join("not yet/made")).as_deref(), Some(a.as_str()), "a path still to be made is on its folder's disk");
    assert_eq!(ops::disk_of(&std::env::temp_dir()).as_deref(), Some(a.as_str()));
    if let Some(shm) = other_fs("disks") {
        assert_ne!(ops::disk_of(&shm).unwrap(), a);
        let _ = std::fs::remove_dir_all(&shm);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Copies onto one disk wait for each other, first come first served, each card saying
/// what it waits for; a job told to start now and one on another disk don't wait; a
/// move within one file system (a rename) never queues; a waiting job cancelled ends
/// without copying anything.
#[test]
fn jobs_on_one_disk_take_turns() {
    let ctx = egui::Context::default();
    let q = std::sync::Arc::new(ops::Queue::default());
    let d = tmpdir("queue");
    let big: Vec<u8> = (0..24_000_000u32).map(|i| (i * 13 % 251) as u8).collect();
    std::fs::write(d.join("big.bin"), &big).unwrap();
    touch(&d.join("small.txt"), "small");
    touch(&d.join("moved.txt"), "moved");
    for o in ["out1", "out2", "out4", "out5", "elsewhere"] {
        std::fs::create_dir_all(d.join(o)).unwrap();
    }
    let copy = |id: u64, what: &str, to: &str| ops::spawn_in(id, OpKind::Copy { srcs: vec![d.join(what)], dest: d.join(to), decide: HashMap::new() }, ctx.clone(), &q, false);
    let first = copy(1, "big.bin", "out1");
    first.pause.store(true, std::sync::atomic::Ordering::Relaxed);
    let t0 = Instant::now();
    while q.running() != [1] {
        assert!(t0.elapsed() < Duration::from_secs(5), "the first job holds the disk");
        std::thread::sleep(Duration::from_millis(5));
    }
    let second = copy(2, "small.txt", "out2");
    let waits_for = |j: &ops::Job, label: &str| {
        let t0 = Instant::now();
        while j.progress.lock().unwrap().waiting.as_deref() != Some(label) {
            assert!(t0.elapsed() < Duration::from_secs(5), "{} waiting for {label}: {:?}", j.label, j.progress.lock().unwrap().waiting);
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    waits_for(&second, "Copying big.bin");
    // A rename on the same disk goes ahead.
    let mv = ops::spawn_in(3, OpKind::Move { srcs: vec![d.join("moved.txt")], dest: d.join("elsewhere"), decide: HashMap::new() }, ctx.clone(), &q, false);
    wait_job(&mv);
    assert!(d.join("elsewhere/moved.txt").exists());
    // Told to start now, it does, while the first still holds the disk.
    let third = copy(4, "small.txt", "out4");
    waits_for(&third, "Copying big.bin");
    third.progress.lock().unwrap().go = true;
    q.poke();
    wait_job(&third);
    assert!(d.join("out4/small.txt").exists() && third.progress.lock().unwrap().since.is_some());
    // Cancelled while it waits: done, nothing copied.
    let fourth = copy(5, "small.txt", "out5");
    waits_for(&fourth, "Copying big.bin");
    fourth.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    q.poke();
    wait_job(&fourth);
    assert!(!d.join("out5/small.txt").exists(), "a cancelled job in the queue copies nothing");
    // Another disk is another queue.
    if let Some(shm) = other_fs("queue") {
        touch(&shm.join("there.txt"), "there");
        std::fs::create_dir_all(shm.join("out")).unwrap();
        let other = ops::spawn_in(6, OpKind::Copy { srcs: vec![shm.join("there.txt")], dest: shm.join("out"), decide: HashMap::new() }, ctx.clone(), &q, false);
        wait_job(&other);
        assert!(shm.join("out/there.txt").exists(), "a copy on another disk runs beside the first");
        let _ = std::fs::remove_dir_all(&shm);
    }
    assert!(!d.join("out2/small.txt").exists() && !second.progress.lock().unwrap().finished, "the second still waits");
    first.pause.store(false, std::sync::atomic::Ordering::Relaxed);
    wait_job(&first);
    wait_job(&second);
    assert_eq!(std::fs::read(d.join("out1/big.bin")).unwrap(), big);
    assert!(d.join("out2/small.txt").exists(), "its turn came once the first was done");
    assert!(q.running().is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

/// With Check Copies on, every copied file is read back from the disk and compared with
/// what went in: a good copy is read twice over (its bytes count twice), and a file that
/// comes back different (changed on its way, as a failing drive would) fails the copy
/// and is removed. A move across disks then keeps its original. With the check off, the
/// same bad copy goes unnoticed.
#[test]
fn checked_copies_are_read_back() {
    let ctx = egui::Context::default();
    let q = std::sync::Arc::new(ops::Queue::default());
    let d = tmpdir("check");
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 7 % 253) as u8).collect();
    std::fs::write(d.join("big.bin"), &big).unwrap();
    touch(&d.join("box/a.txt"), "a");
    touch(&d.join("box/deep/b.txt"), "b");
    let bad = format!("{} photo.jpg", ops::SPOILED);
    touch(&d.join("bad/keep.txt"), "keep");
    std::fs::write(d.join("bad").join(&bad), &big[..100_000]).unwrap();
    std::fs::create_dir_all(d.join("out")).unwrap();
    let job = |id, kind, check| {
        let j = ops::spawn_in(id, kind, ctx.clone(), &q, check);
        wait_job(&j);
        j
    };
    let j = job(1, OpKind::Copy { srcs: vec![d.join("big.bin"), d.join("box")], dest: d.join("out"), decide: HashMap::new() }, true);
    let g = j.progress.lock().unwrap();
    assert!(g.errors.is_empty(), "{:?}", g.errors);
    assert_eq!((g.total_bytes, g.done_bytes), (2 * (big.len() as u64 + 2), 2 * (big.len() as u64 + 2)), "copied, then read back");
    drop(g);
    assert_eq!(std::fs::read(d.join("out/big.bin")).unwrap(), big);
    assert_eq!(std::fs::read_to_string(d.join("out/box/deep/b.txt")).unwrap(), "b");
    // Shared extents (btrfs, XFS) write no bytes that could come back different.
    if !reflinks(&d) {
        let j = job(2, OpKind::Copy { srcs: vec![d.join("bad")], dest: d.join("out"), decide: HashMap::new() }, true);
        let errs = j.progress.lock().unwrap().errors.clone();
        assert!(errs.len() == 1 && errs[0].contains("read back from the disk differently"), "{errs:?}");
        assert!(!d.join("out/bad").exists(), "no copy that is known to be wrong");
        let j = job(3, OpKind::Copy { srcs: vec![d.join("bad")], dest: d.join("out"), decide: HashMap::new() }, false);
        assert!(j.progress.lock().unwrap().errors.is_empty());
        assert_ne!(std::fs::read(d.join("out/bad").join(&bad)).unwrap(), std::fs::read(d.join("bad").join(&bad)).unwrap(), "unchecked, the changed copy stays");
    }
    if let Some(shm) = other_fs("check") {
        let j = job(4, OpKind::Move { srcs: vec![d.join("bad")], dest: shm.clone(), decide: HashMap::new() }, true);
        assert_eq!(j.progress.lock().unwrap().errors.len(), 1);
        assert!(d.join("bad").join(&bad).exists() && !shm.join("bad").exists(), "the original stays, the copy goes");
        let j = job(5, OpKind::Move { srcs: vec![d.join("box")], dest: shm.clone(), decide: HashMap::new() }, true);
        assert!(j.progress.lock().unwrap().errors.is_empty());
        assert!(!d.join("box").exists() && std::fs::read_to_string(shm.join("box/a.txt")).unwrap() == "a", "a good copy moves");
        let _ = std::fs::remove_dir_all(&shm);
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Can files in `dir` share extents (a reflink copy)?
fn reflinks(dir: &Path) -> bool {
    use std::os::fd::AsRawFd;
    let (a, b) = (dir.join(".reflink-a"), dir.join(".reflink-b"));
    std::fs::write(&a, b"x").unwrap();
    let (r, w) = (std::fs::File::open(&a).unwrap(), std::fs::File::create(&b).unwrap());
    let ok = unsafe { libc::ioctl(w.as_raw_fd(), 0x4004_9409, r.as_raw_fd()) } == 0;
    let _ = (std::fs::remove_file(&a), std::fs::remove_file(&b));
    ok
}

/// Undo of a move to another disk moves it back through a job (a rename can't cross
/// disks), and Redo moves it there again; a move over something brings that back from
/// the trash on Undo, and Redo puts it in the trash again.
#[test]
fn a_move_to_another_disk_is_undone_and_redone() {
    let Some(far) = other_fs("xundo") else { return };
    let ctx = egui::Context::default();
    let d = tmpdir("xundo");
    touch(&d.join("a.txt"), "new a");
    touch(&d.join("box/inner.txt"), "inner");
    touch(&far.join("a.txt"), "old a");
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    let mut decide = HashMap::new();
    decide.insert(d.join("a.txt"), Conflict::Overwrite);
    app.start_job(OpKind::Move { srcs: vec![d.join("a.txt"), d.join("box")], dest: far.clone(), decide });
    app_until(&mut app, &ctx, "the move", |a| a.undo.last().is_some_and(|u| matches!(u.kind, crate::app::UndoKind::MovedOver(..))));
    assert_eq!(std::fs::read_to_string(far.join("a.txt")).unwrap(), "new a");
    assert!(app.undo_last());
    assert!(app.jobs.iter().any(|j| j.label == "Moving 2 items back"), "a job with its card");
    app_until(&mut app, &ctx, "the move back", |a| a.job_steps.is_empty() && a.undo.can_redo());
    assert_eq!(std::fs::read_to_string(d.join("a.txt")).unwrap(), "new a");
    assert_eq!(std::fs::read_to_string(d.join("box/inner.txt")).unwrap(), "inner");
    assert_eq!(std::fs::read_to_string(far.join("a.txt")).unwrap(), "old a", "what it replaced is back");
    assert!(!far.join("box").exists() && app.undo.is_empty());
    assert!(app.redo_last());
    app_until(&mut app, &ctx, "the move again", |a| a.job_steps.is_empty() && !a.undo.is_empty());
    assert_eq!(std::fs::read_to_string(far.join("a.txt")).unwrap(), "new a");
    assert!(far.join("box/inner.txt").exists() && !d.join("a.txt").exists() && !d.join("box").exists());
    assert!(!app.undo.can_redo());
    // And back once more, by Undo again.
    assert!(app.undo_last());
    app_until(&mut app, &ctx, "back again", |a| a.job_steps.is_empty() && a.undo.can_redo());
    assert_eq!(std::fs::read_to_string(far.join("a.txt")).unwrap(), "old a");
    assert!(d.join("a.txt").exists() && d.join("box/inner.txt").exists());
    purge_trashed_under(&far);
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&far);
    let _ = std::fs::remove_dir_all(&d);
}

/// Each kind of change Undo takes back, Redo does again, after checking things are where
/// Undo left them; a new change leaves nothing to redo.
#[test]
fn redo_does_each_change_again() {
    use crate::app::{UndoEntry, UndoKind};
    let ctx = egui::Context::default();
    let d = tmpdir("redo");
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    let mut push = |app: &mut crate::app::App, label: &str, kind| app.undo.push(UndoEntry { label: label.into(), kind });
    let read = |p: &str| std::fs::read_to_string(d.join(p)).ok();
    // Renamed.
    touch(&d.join("b.txt"), "a");
    push(&mut app, "Rename", UndoKind::Rename(vec![(d.join("a.txt"), d.join("b.txt"))]));
    assert!(app.undo_last() && read("a.txt").is_some() && read("b.txt").is_none());
    assert!(app.undo.can_redo());
    assert!(app.redo_last() && read("b.txt").is_some() && read("a.txt").is_none());
    assert!(matches!(&app.undo.last().unwrap().kind, UndoKind::Rename(v) if v.len() == 1), "and Undo has it again");
    // Moved within a disk.
    touch(&d.join("to/m.txt"), "m");
    push(&mut app, "Move", UndoKind::Move(vec![(d.join("m.txt"), d.join("to/m.txt"))]));
    assert!(app.undo_last() && read("m.txt").is_some());
    assert!(app.redo_last() && read("to/m.txt").is_some() && read("m.txt").is_none());
    // Made (a copy, a new file): Undo trashes it, Redo brings it back from the trash.
    touch(&d.join("made.txt"), "made");
    push(&mut app, "Copy", UndoKind::Created(vec![d.join("made.txt")]));
    assert!(app.undo_last() && read("made.txt").is_none());
    assert!(app.redo_last() && read("made.txt").as_deref() == Some("made"));
    // Trashed: Undo restores, Redo trashes again.
    touch(&d.join("t.txt"), "t");
    crate::trashcan::delete(&d.join("t.txt")).unwrap();
    push(&mut app, "Trash", UndoKind::Trashed(vec![d.join("t.txt")]));
    assert!(app.undo_last() && read("t.txt").is_some());
    assert!(app.redo_last() && read("t.txt").is_none());
    assert!(app.undo_last() && read("t.txt").is_some(), "and back once more");
    assert!(app.redo_last());
    // Changed in place: the original in the trash; Redo swaps the changed one back in.
    touch(&d.join("pic.txt"), "original");
    crate::trashcan::delete(&d.join("pic.txt")).unwrap();
    touch(&d.join("pic.txt"), "turned");
    push(&mut app, "Rotate", UndoKind::Replaced(vec![d.join("pic.txt")]));
    assert!(app.undo_last() && read("pic.txt").as_deref() == Some("original"));
    assert!(app.redo_last() && read("pic.txt").as_deref() == Some("turned"));
    assert!(app.undo_last() && read("pic.txt").as_deref() == Some("original"));
    // Sorted into folders it made: Redo makes them again.
    touch(&d.join("Pictures/s.png"), "s");
    push(&mut app, "Tidy", UndoKind::Sorted(vec![(d.join("s.png"), d.join("Pictures/s.png"))], vec![d.join("Pictures")]));
    assert!(app.undo_last() && read("s.png").is_some() && !d.join("Pictures").exists());
    assert!(app.redo_last() && read("Pictures/s.png").is_some());
    // The world changed meanwhile: Redo refuses, and says so.
    assert!(app.undo_last());
    touch(&d.join("Pictures/s.png"), "someone else's");
    assert!(!app.redo_last(), "a file in the way");
    assert_eq!(read("s.png").as_deref(), Some("s"));
    assert_eq!(read("Pictures/s.png").as_deref(), Some("someone else's"));
    // A new change: nothing to redo.
    assert!(app.undo_last() && app.undo.can_redo());
    push(&mut app, "New", UndoKind::Created(vec![d.join("x")]));
    assert!(!app.undo.can_redo());
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&d);
}

/// Undo and Redo outlast a restart: written to the state folder when they change, read
/// back at start. What has nothing left to act on (its files gone) and what is past its
/// age is left out; the lists keep at most their number of changes, the newest.
#[test]
fn undo_and_redo_survive_a_restart() {
    use crate::app::{UndoEntry, UndoKind};
    let ctx = egui::Context::default();
    let d = tmpdir("undokeep");
    let file = d.join("state/undo.json");
    let start = |ctx: &egui::Context| {
        let mut app = crate::app::App::new(ctx, vec![d.clone()]);
        app.undo_keep = crate::undofile::Keeper::new(Some(file.clone()), ctx);
        app_until(&mut app, ctx, "the lists read", |a| !a.undo_keep.loading());
        app
    };
    let mut app = start(&ctx);
    assert!(app.undo.is_empty() && !app.undo.can_redo());
    touch(&d.join("b.txt"), "b");
    touch(&d.join("made.txt"), "m");
    touch(&d.join("moved/c.txt"), "c");
    app.undo.push(UndoEntry { label: "Gone".into(), kind: UndoKind::Move(vec![(d.join("x"), d.join("nowhere/x"))]) });
    app.undo.push(UndoEntry { label: "Rename a.txt to b.txt".into(), kind: UndoKind::Rename(vec![(d.join("a.txt"), d.join("b.txt"))]) });
    app.undo.push(UndoEntry { label: "Copy made.txt".into(), kind: UndoKind::Created(vec![d.join("made.txt")]) });
    app.undo.push(UndoEntry { label: "Move c.txt".into(), kind: UndoKind::Move(vec![(d.join("c.txt"), d.join("moved/c.txt"))]) });
    assert!(app.undo_last() && d.join("c.txt").exists(), "one undone, for Redo");
    app_until(&mut app, &ctx, "the write", |_| std::fs::read_to_string(&file).is_ok_and(|s| s.contains("Copy made.txt") && s.contains("Move c.txt")));
    drop(app);
    let mut app = start(&ctx);
    let labels: Vec<String> = (0..app.undo.len()).map(|i| app.undo[i].label.clone()).collect();
    assert_eq!(labels, ["Rename a.txt to b.txt", "Copy made.txt"], "the move whose files are gone is left out");
    assert!(app.undo.can_redo(), "Redo kept too");
    assert!(app.redo_last() && d.join("moved/c.txt").exists() && !d.join("c.txt").exists());
    assert!(app.undo_last() && app.undo_last() && app.undo_last());
    assert!(d.join("a.txt").exists() && !d.join("made.txt").exists(), "taken back after the restart");
    // Past its age, and over the number kept.
    for i in 0..(crate::undofile::MOST + 20) {
        touch(&d.join(format!("n{i}")), "n");
        app.undo.push(UndoEntry { label: format!("Make n{i}"), kind: UndoKind::Created(vec![d.join(format!("n{i}"))]) });
    }
    assert_eq!(app.undo.len(), crate::undofile::MOST);
    let last = format!("Make n{}", crate::undofile::MOST + 19);
    app_until(&mut app, &ctx, "the write", |_| std::fs::read_to_string(&file).is_ok_and(|s| s.contains(&last)));
    drop(app);
    let app = start(&ctx);
    assert_eq!(app.undo.len(), crate::undofile::MOST);
    assert_eq!(app.undo[0].label, "Make n20", "the oldest went");
    drop(app);
    std::fs::write(&file, format!(r#"{{"undo":[{{"at":1000,"label":"Old","kind":{{"Created":["{}"]}}}}],"redo":[]}}"#, d.join("n1").display())).unwrap();
    let app = start(&ctx);
    assert!(app.undo.is_empty(), "a change months old is left out");
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&d);
}

/// Selections in the previews, on real files: the part of each line a selection takes
/// and its line breaks, words by double-click, the text copied out of lines; a big file's
/// selected lines read back from disk through its line index (across the index's steps,
/// tabs shown as spaces, to its end with Ctrl+A, cut at the most a copy takes) and its
/// bytes as hex and as text; table cells as tab-separated lines; a data tree's rows and a
/// picked value; a .env copied as shown.
#[test]
fn preview_selections_copy_what_is_shown() {
    use crate::textsel::{END, Sel, lines_text, word};
    use std::borrow::Cow;
    let d = tmpdir("textsel");
    // Lines and words.
    let notes = d.join("notes.txt");
    std::fs::write(&notes, "naïve café\tbar\r\nsecond_line here\n\nlast").unwrap();
    let lines = crate::inspector::text_lines(&std::fs::read(&notes).unwrap()).unwrap();
    assert_eq!(lines, ["naïve café    bar", "second_line here", "", "last"]);
    let s = Sel::new((1, 7), (0, 7));
    assert_eq!(s.span(), ((0, 7), (1, 7)), "in order whichever way it was dragged");
    assert_eq!(s.on_line(0, lines[0].len()), Some((7, lines[0].len(), true)));
    assert_eq!(s.on_line(1, lines[1].len()), Some((0, 7, false)));
    assert_eq!(s.on_line(2, 0), None);
    assert_eq!(Sel::new((2, 0), (3, 0)).on_line(2, 0), Some((0, 0, true)), "an empty line's break");
    let get = |i: usize| lines.get(i).map(|l| Cow::Borrowed(l.as_str()));
    assert_eq!(lines_text(&s, lines.len(), get), "café    bar\nsecond_");
    assert_eq!(lines_text(&Sel::new((0, 0), END), lines.len(), get), "naïve café    bar\nsecond_line here\n\nlast", "all of it, no break after the last line");
    assert_eq!(lines_text(&Sel::at((1, 3)), lines.len(), get), "");
    assert_eq!(word(&lines[0], 2), (0, 6), "a word with a letter of two bytes");
    assert_eq!(word(&lines[1], 3), (0, 11), "underscores are part of a word");
    assert_eq!(word(&lines[0], 13), (12, 16), "spaces as a run");
    assert_eq!(word(&lines[1], lines[1].len()), (12, 16), "at the end of a line, the word before it");
    assert_eq!(word("a.b", 1), (1, 2), "anything else on its own");
    assert_eq!(crate::textsel::byte_at_col("日本語", 2.4), 3, "wide characters take two columns");
    // A big file: lines read back from the disk through the index.
    let big = d.join("big.log");
    let text: String = (0..3000).map(|i| if i % 500 == 7 { format!("{i}\twith a tab\n") } else { format!("line {i:05}\n") }).collect();
    std::fs::write(&big, &text).unwrap();
    let ctx = egui::Context::default();
    let v = crate::bigfile::Pager::open(&big, text.len() as u64, false, &ctx).unwrap();
    let t0 = Instant::now();
    while !v.rows().1 {
        assert!(t0.elapsed() < Duration::from_secs(20), "indexed");
        std::thread::sleep(Duration::from_millis(5));
    }
    let shown: Vec<String> = text.lines().map(|l| l.replace('\t', "    ")).collect();
    let read = |v: &crate::bigfile::Pager, s: Sel| v.copy_lines(s, &ctx).recv_timeout(Duration::from_secs(20)).unwrap();
    let (got, cut) = read(&v, Sel::new((1022, 5), (1025, 3)));
    assert_eq!(got, format!("{}\n{}\n{}\n{}", &shown[1022][5..], shown[1023], shown[1024], &shown[1025][..3]), "across a step of the index");
    assert!(!cut);
    let (got, _) = read(&v, Sel::new((2507, 0), (2507, 9)));
    assert_eq!(got, "2507    w", "a tab copied as the spaces shown");
    let (got, _) = read(&v, Sel::new((0, 0), END));
    assert_eq!(got, shown.join("\n"), "all of it");
    let (got, cut) = crate::bigfile::selected_lines(&std::fs::File::open(&big).unwrap(), 0, 0, &Sel::new((10, 0), END), 100);
    assert!(cut && got.len() == 100 && got.starts_with("line 00010\n"), "cut at the most a copy takes: {got:?}");
    // Its bytes.
    let blob = d.join("blob.bin");
    let bytes: Vec<u8> = (0..=255u8).cycle().take(700_000).collect();
    std::fs::write(&blob, &bytes).unwrap();
    let h = crate::bigfile::Pager::open(&blob, bytes.len() as u64, true, &ctx).unwrap();
    let (got, _) = h.copy_bytes(Sel::new(300_000, 299_997), false, &ctx).recv_timeout(Duration::from_secs(20)).unwrap();
    let want: Vec<String> = bytes[299_997..=300_000].iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(got, want.join(" "), "both ends taken");
    let (got, _) = h.copy_bytes(Sel::new(65, 70), true, &ctx).recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(got, "ABCDEF");
    let (got, _) = h.copy_bytes(Sel::new(0, 3), true, &ctx).recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(got, "....", "what isn't printable as dots");
    let (got, _) = h.copy_bytes(Sel::new(262_140, 262_150), false, &ctx).recv_timeout(Duration::from_secs(20)).unwrap();
    assert_eq!(got.split(' ').count(), 11, "read in pieces, one space between every byte");
    // Tables, sheets and trees from the fixtures the previews show.
    let dev = d.join("dev");
    dev_fixture(&dev);
    let csv = std::fs::read_to_string(dev.join("people.csv")).unwrap();
    let mut tv = crate::table::TableView::new(crate::table::Table::from_text(&csv, "people.csv", false));
    tv.sel = Some(Sel::new((2, 2), (1, 1)));
    assert_eq!(tv.copy_text(), "\"Grace \"\"Amazing\"\" Hopper\"\tgrace@example.invalid\n\"Line\nbreak\"\tx@example.invalid", "quoted where a spreadsheet needs it");
    assert!(tv.select_all());
    assert!(tv.copy_text().starts_with("id\tname\temail\tjoined\tscore\n1\tAda Lovelace\t"), "all of it, with the titles");
    assert_eq!(tv.copy_text().lines().count(), 6, "titles, four rows, one with a line break in quotes");
    let json = std::fs::read_to_string(dev.join("package.json")).unwrap();
    let mut t = crate::datatree::flatten(&crate::datatree::parse_json(&json).unwrap(), None);
    let _ = t.rows();
    let at = |t: &crate::datatree::Tree, key: &str| t.nodes.iter().position(|n| n.key == key).unwrap();
    let (name, build, test) = (at(&t, "name"), at(&t, "build"), at(&t, "test"));
    assert_eq!(t.row_text(build), ("build: \"vite build\"".to_string(), Some(7)));
    t.tsel = Some(Sel::new((name, 6), (build, 5)));
    let rows = t.copy_text();
    assert!(rows.starts_with("\"maia-web\"\nversion: \"0.2.0\"\n") && rows.ends_with("scripts\n  build"), "rows as drawn, indented: {rows:?}");
    t.tsel = None;
    t.focus = true;
    t.sel = test;
    assert_eq!(t.copy_text(), "vitest", "the picked value, without quotes");
    t.sel = at(&t, "scripts");
    assert_eq!(t.copy_text(), "scripts\n  build: \"vite build\"\n  test: \"vitest\"", "a map as its rows");
    t.toggle(t.sel);
    let _ = t.rows();
    assert_eq!(t.copy_text(), "scripts  {…} 2 keys", "folded, as it shows");
    // A .env as shown: the mask until it is revealed.
    let env = crate::inspector::text_lines(&std::fs::read(dev.join(".env")).unwrap()).unwrap();
    let kinds = crate::envfile::classify(&env);
    let (env, kinds) = (&env, &kinds);
    let shown = |reveal: bool| move |i: usize| env.get(i).map(|l| Cow::Owned(crate::envfile::shown(l, kinds[i], reveal)));
    let s = Sel::new((1, 0), (2, 100));
    assert_eq!(lines_text(&s, env.len(), shown(false)), format!("export API_KEY={m}\nDATABASE_URL={m}", m = crate::envfile::MASK));
    assert_eq!(lines_text(&s, env.len(), shown(true)), "export API_KEY=sk-live-0123456789\nDATABASE_URL=\"postgres://u:pw@localhost/db\"");
    let _ = std::fs::remove_dir_all(&d);
}

/// What the picture viewer does to files, on real ones: turns through the ImageMagick
/// here (`magick`, or version 6's `convert`), three in a row keeping one original in the
/// trash and Undo bringing it back byte for byte; a tag toggled on and off by what the
/// file holds, even when the tag cache read it before the change.
#[test]
fn viewer_turns_and_tags_real_pictures() {
    let d = tmpdir("viewer-sort");
    let pic = d.join("shot.png");
    std::fs::write(&pic, png_bytes(40, 20, 4)).unwrap();
    let shown = |p: &Path| crate::imgdec::load(p, 64).map(|x| x.1);
    // Tags: the cache asked first, then changed under it.
    assert!(crate::tags::get(&pic, |t| t.to_vec()).is_none(), "not read yet");
    crate::viewer::toggle_tag(&pic, "Red").unwrap();
    assert_eq!(crate::tags::read(&pic), ["Red"]);
    crate::viewer::toggle_tag(&pic, "red").unwrap();
    assert!(crate::tags::read(&pic).is_empty(), "off again, whatever its case");
    crate::viewer::toggle_tag(&pic, "keep").unwrap();
    crate::viewer::toggle_tag(&pic, "Blue").unwrap();
    assert_eq!(crate::tags::read(&pic), ["keep", "Blue"]);
    // Turns.
    let tools = d.join("tools");
    std::fs::create_dir_all(&tools).unwrap();
    let path = std::env::var_os("PATH").unwrap_or_default();
    for name in ["magick", "convert"] {
        if let Some(real) = std::env::split_paths(&path).map(|p| p.join(name)).find(|m| m.is_file()) {
            let _ = std::os::unix::fs::symlink(real, tools.join(name));
        }
    }
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
    let t = crate::drives::Tools::current().unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let Some(prog) = crate::picfix::magick(&t) else {
        eprintln!("viewer turns: no ImageMagick, skipped");
        return;
    };
    let original = std::fs::read(&pic).unwrap();
    let kept = std::sync::Mutex::new(HashSet::new());
    let trashed = || crate::trashcan::list().into_iter().filter(|it| it.original == pic).count();
    let before = trashed();
    let (r, again) = crate::viewer::turn_once(&t, prog, &pic, true, &kept);
    assert!(r.is_ok() && !again, "{r:?}");
    assert_eq!(shown(&pic), Some([20, 40]));
    let (r, again) = crate::viewer::turn_once(&t, prog, &pic, true, &kept);
    assert!(r.is_ok() && again);
    assert_eq!(shown(&pic), Some([40, 20]));
    let (r, _) = crate::viewer::turn_once(&t, prog, &pic, false, &kept);
    assert!(r.is_ok());
    assert_eq!(shown(&pic), Some([20, 40]), "right, right, left: one turn right");
    assert_eq!(trashed(), before + 1, "one original kept for three turns");
    assert_eq!(crate::picfix::undo_replaced(std::slice::from_ref(&pic)), 0);
    assert_eq!(std::fs::read(&pic).unwrap(), original, "the original back, byte for byte");
    // Not a picture ImageMagick writes back: refused, the file as it was.
    let svg = d.join("drawing.svg");
    std::fs::write(&svg, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
    let (r, _) = crate::viewer::turn_once(&t, prog, &svg, true, &kept);
    assert!(r.is_err());
    assert_eq!(std::fs::read_to_string(&svg).unwrap(), "<svg xmlns='http://www.w3.org/2000/svg'/>");
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------ the Open and Save dialog

fn unhex(parts: &[&str]) -> Vec<u8> {
    let s: String = parts.concat();
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// The OpenFile call xdg-desktop-portal 1.18.4 sent Maia's backend for this request,
/// made with gdbus through the real portal and taken off a private bus with
/// `dbus-monitor --binary`: parent `x11:3a00007`, title "Upload a photo", several files,
/// an accept label with a mnemonic, two filters (globs with classes, a MIME type), the
/// first as current, a choice with options and a yes-or-no one, and /tmp to start in.
const PORTAL_OPEN_CALL: &[&str] = &[
    "6c01000159020000220000009d00000001016f001f0000002f6f72672f667265656465736b746f702f706f7274616c2f",
    "6465736b746f700002017300270000006f72672e667265656465736b746f702e696d706c2e706f7274616c2e46696c65",
    "43686f6f7365720006017300040000003a312e320000000008016700096f737373617b73767d00000301730008000000",
    "4f70656e46696c65000000000000000007017300040000003a312e3100000000310000002f6f72672f66726565646573",
    "6b746f702f706f7274616c2f6465736b746f702f726571756573742f315f332f6d616961310000000000000000000000",
    "0b0000007831313a33613030303037000e00000055706c6f616420612070686f746f0000f10100000c00000061636365",
    "70745f6c6162656c00017300070000005f55706c6f616400080000006d756c7469706c65000162000100000000000000",
    "0700000066696c74657273000961287361287573292900005a0000000000000006000000496d6167657300002b000000",
    "000000000e0000002a2e5b70505d5b6e4e5d5b67475d0000010000000a000000696d6167652f6a706567000000000000",
    "03000000416c6c000a0000000000000000000000010000002a000000000000000e00000063757272656e745f66696c74",
    "6572000828736128757329290000000006000000496d6167657300002b000000000000000e0000002a2e5b70505d5b6e",
    "4e5d5b67475d0000010000000a000000696d6167652f6a7065670000000000000700000063686f69636573000b612873",
    "7361287373297329000000009200000008000000656e636f64696e670000000008000000456e636f64696e6700000000",
    "300000000000000004000000757466380000000007000000556e69636f646500060000006c6174696e31000007000000",
    "5765737465726e000400000075746638000000000000000002000000726f00000900000052656164206f6e6c79000000",
    "00000000000000000500000066616c7365000000000000000e00000063757272656e745f666f6c646572000261790000",
    "050000002f746d7000",
];

/// The Close the portal sent for it when the app (gdbus) went away.
const PORTAL_CLOSE_CALL: &[&str] = &[
    "6c0100010000000024000000a500000001016f00310000002f6f72672f667265656465736b746f702f706f7274616c2f",
    "6465736b746f702f726571756573742f315f332f6d616961310000000000000002017300230000006f72672e66726565",
    "6465736b746f702e696d706c2e706f7274616c2e52657175657374000000000006017300040000003a312e3200000000",
    "08016700000000000301730005000000436c6f736500000007017300040000003a312e3100000000",
];

/// The portal's own messages read as Maia reads them: the request with every option, the
/// Close on its handle, and the body written again byte for byte the same. Answers go
/// back as `ua{sv}`; between the backend and a picker window, requests and answers pass
/// whole; a message cut short or nested without end is refused, not followed.
#[test]
fn portal_requests_read_and_answers_written() {
    use crate::portal::{Answer, Filter, Mode, Pick, Val, parse, put, reply_body};
    let m = crate::fm1::read(&mut std::io::Cursor::new(unhex(PORTAL_OPEN_CALL))).expect("a whole message");
    assert_eq!((m.kind, m.path.as_str(), m.interface.as_str(), m.member.as_str(), m.signature.as_str()), (1, "/org/freedesktop/portal/desktop", crate::portal::IFACE, "OpenFile", "osssa{sv}"));
    let (handle, p) = Pick::from_call(&m.member, &m.signature, &m.body).expect("a request");
    assert_eq!(handle, "/org/freedesktop/portal/desktop/request/1_3/maia1");
    assert_eq!((p.mode, p.parent.as_str(), p.title.as_str(), p.accept_label.as_str(), p.multiple, p.directory), (Mode::Open, "x11:3a00007", "Upload a photo", "Upload", true, false));
    let images = Filter { name: "Images".into(), rules: vec![(0, "*.[pP][nN][gG]".into()), (1, "image/jpeg".into())] };
    assert_eq!(p.filters, vec![images.clone(), Filter { name: "All".into(), rules: vec![(0, "*".into())] }]);
    assert_eq!(p.current_filter, Some(0), "the current filter is one of the list");
    assert_eq!(p.choices.len(), 2);
    assert_eq!((p.choices[0].id.as_str(), p.choices[0].label.as_str(), p.choices[0].chosen.as_str()), ("encoding", "Encoding", "utf8"));
    assert_eq!(p.choices[0].options, vec![("utf8".to_string(), "Unicode".to_string()), ("latin1".into(), "Western".into())]);
    assert!(p.choices[1].options.is_empty() && p.choices[1].chosen == "false", "a yes or no");
    assert_eq!(p.current_folder.as_deref(), Some(Path::new("/tmp")), "the nul-terminated path");
    // Read and written again, the body is the portal's to the byte (alignment of the
    // empty `a(ss)` included).
    let vals = parse(&m.signature, &m.body).unwrap();
    let mut again = vec![];
    for (sig, v) in ["o", "s", "s", "s", "a{sv}"].iter().zip(&vals) {
        put(&mut again, sig, v);
    }
    assert_eq!(again, m.body);
    let c = crate::fm1::read(&mut std::io::Cursor::new(unhex(PORTAL_CLOSE_CALL))).unwrap();
    assert_eq!((c.path.as_str(), c.interface.as_str(), c.member.as_str()), (handle.as_str(), "org.freedesktop.impl.portal.Request", "Close"));
    // The answer: picked files, the questions' answers and the filter in use, by name.
    let a = Answer { response: 0, uris: vec!["file:///tmp/a%20b.png".into(), "file:///tmp/c.jpg".into()], choices: vec![("encoding".into(), "latin1".into()), ("ro".into(), "true".into())], filter: Some(0) };
    let v = parse("ua{sv}", &reply_body(&p, &a)).unwrap();
    assert_eq!(v[0], Val::U32(0));
    let Val::List(results) = &v[1] else { panic!("{v:?}") };
    let get = |k: &str| results.iter().find_map(|e| match e {
        Val::Tuple(t) if t[0] == Val::Str(k.into()) => match &t[1] {
            Val::Var(_, x) => Some(x.as_ref().clone()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(get("uris"), Some(Val::List(a.uris.iter().map(|u| Val::Str(u.clone())).collect())));
    assert_eq!(get("choices"), Some(Val::List(vec![Val::Tuple(vec![Val::Str("encoding".into()), Val::Str("latin1".into())]), Val::Tuple(vec![Val::Str("ro".into()), Val::Str("true".into())])])));
    let Some(Val::Tuple(f)) = get("current_filter") else { panic!("no filter") };
    assert_eq!(f[0], Val::Str("Images".into()));
    // Cancelled: response 1 and nothing else.
    let v = parse("ua{sv}", &reply_body(&p, &Answer::cancelled())).unwrap();
    assert_eq!(v, vec![Val::U32(1), Val::List(vec![])]);
    // Between the backend and the window: the request and the answer whole.
    let mut p2 = p.clone();
    p2.current_name = "naïve name.txt".into();
    p2.files = vec!["one.txt".into(), "two\nlines.txt".into()];
    p2.current_file = Some("/tmp/x y/z".into());
    assert_eq!(Pick::decode(&p2.encode()), p2);
    assert_eq!(Answer::decode(&a.encode()), a);
    assert_eq!(Answer::decode(b""), Answer::ended(), "a picker that said nothing ended some other way");
    assert_eq!(Answer::decode(b"uri\0file:///x\0"), Answer::ended(), "an answer without its response is no answer");
    // A current filter that is not in the list (or no list) is applied by itself.
    let mut b = vec![];
    for s in [Val::Str("/r/1".into()), Val::Str("app".into()), Val::Str("".into()), Val::Str("Save".into())] {
        put(&mut b, "s", &s);
    }
    let docs = Val::Tuple(vec![Val::Str("Docs".into()), Val::List(vec![Val::Tuple(vec![Val::U32(0), Val::Str("*.odt".into())])])]);
    let opts = Val::List(vec![
        Val::Tuple(vec![Val::Str("current_filter".into()), Val::Var("(sa(us))".into(), Box::new(docs))]),
        Val::Tuple(vec![Val::Str("current_name".into()), Val::Var("s".into(), Box::new(Val::Str("Untitled.odt".into())))]),
        Val::Tuple(vec![Val::Str("current_file".into()), Val::Var("ay".into(), Box::new(Val::Bytes(b"/tmp/old.odt\0".to_vec())))]),
        Val::Tuple(vec![Val::Str("a number".into()), Val::Var("x".into(), Box::new(Val::Other))]),
    ]);
    put(&mut b, "a{sv}", &opts);
    // An int64 nobody asked for is read past: its 8 bytes written as zeroes here.
    let (_, s) = Pick::from_call("SaveFile", "osssa{sv}", &b).unwrap();
    assert_eq!((s.mode, s.current_filter, s.filters.len(), s.current_name.as_str()), (Mode::Save, Some(0), 1, "Untitled.odt"));
    assert_eq!(s.current_file.as_deref(), Some(Path::new("/tmp/old.odt")));
    assert!(Pick::from_call("Explode", "osssa{sv}", &b).is_none() && Pick::from_call("OpenFile", "s", &b).is_none());
    for cut in [3, 17, 40, b.len() - 3] {
        assert!(Pick::from_call("SaveFile", "osssa{sv}", &b[..cut]).is_none(), "cut at {cut}");
    }
    let mut deep = vec![];
    for _ in 0..200 {
        deep.extend_from_slice(&[1, b'v', 0]);
    }
    assert!(parse("v", &deep).is_none(), "variants in variants without end");
    // A backend whose program an update replaced opens pickers with the new one.
    let d = tmpdir("portal-exe");
    let exe = d.join("maia");
    touch(&exe, "new build");
    assert_eq!(crate::portal::picker_exe(&exe), Some(exe.clone()));
    assert_eq!(crate::portal::picker_exe(Path::new(&format!("{} (deleted)", exe.display()))), Some(exe.clone()));
    assert_eq!(crate::portal::picker_exe(&d.join("gone (deleted)")), None, "nothing at the path: no picker");
    let _ = std::fs::remove_dir_all(&d);
    assert_eq!(crate::portal::plain_label("_Open"), "Open");
    assert_eq!(crate::portal::plain_label("Save __as"), "Save _as");
}

/// Globs as the portal's filters write them (classes, ranges, negation) and MIME types
/// with the shared database's subclasses: a C file is plain text, `image/*` takes a PNG.
/// Folders always pass, so the list can still be walked; folder mode lists only folders.
#[test]
fn picker_filters_let_through_what_the_app_asked() {
    use crate::picker::{Accept, glob, mime_is};
    let g = |p: &str, n: &str| glob(&p.chars().collect::<Vec<_>>(), &n.chars().collect::<Vec<_>>());
    assert!(g("*.[pP][nN][gG]", "Shot.PnG") && g("*.[pP][nN][gG]", "a.png") && !g("*.[pP][nN][gG]", "a.jpg"));
    assert!(g("*", "anything") && g("?.txt", "a.txt") && !g("?.txt", "ab.txt"));
    assert!(g("report-[0-9][0-9].pdf", "report-07.pdf") && !g("report-[0-9][0-9].pdf", "report-7a.pdf"));
    assert!(g("[!.]*", "visible") && !g("[!.]*", ".hidden"));
    assert!(g("*.tar.gz", "x.tar.gz") && !g("*.tar.gz", "x.tar.gz.part"));
    assert!(g("[", "[") && !g("*.PNG", "a.png"), "a lone bracket is itself; patterns are case-sensitive");
    assert!(mime_is("image/png", "image/*") && !mime_is("text/plain", "image/*") && mime_is("text/plain", "*"));
    assert!(mime_is("text/x-csrc", "text/plain") && mime_is("text/markdown", "text/plain"), "every text is plain text");
    if std::path::Path::new("/usr/share/mime/subclasses").is_file() {
        assert!(mime_is("application/x-shellscript", "text/plain"), "a subclass of the shared database");
        assert!(mime_is("image/svg+xml", "application/xml"));
    }
    let d = tmpdir("picker-filters");
    for f in ["a.png", "b.JPG", "c.jpeg", "notes.txt", "sub/x"] {
        touch(&d.join(f), "x");
    }
    let e = |n: &str| crate::fsx::Entry::from_path(&d.join(n)).unwrap();
    let mut p = crate::portal::Pick::default();
    p.filters = vec![crate::portal::Filter { name: "Images".into(), rules: vec![(0, "*.[pP][nN][gG]".into()), (1, "image/jpeg".into())] }, crate::portal::Filter { name: "Nothing known".into(), rules: vec![(7, "?".into())] }];
    let a = Accept::new(&p, Some(0)).unwrap();
    assert!(a.ok(&e("a.png")) && a.ok(&e("b.JPG")) && a.ok(&e("c.jpeg")) && !a.ok(&e("notes.txt")));
    assert!(a.ok(&e("sub")), "folders pass, to walk into");
    assert!(Accept::new(&p, Some(1)).is_none() && Accept::new(&p, None).is_none(), "nothing Maia can read filters nothing");
    p.directory = true;
    let a = Accept::new(&p, Some(0)).unwrap();
    assert!(a.ok(&e("sub")) && !a.ok(&e("a.png")), "folder mode: folders only");
    let _ = std::fs::remove_dir_all(&d);
}

/// Turning it on puts one line in the portals.conf the portal reads, keeps every other
/// line, and remembers what was there; off puts it back. With no file of the user's, a
/// copy of the desktop's own is made (portals before 1.20.1 read only the first file
/// found) and taken away again. All in temporary folders, never the real home.
#[test]
fn picker_routing_merges_and_restores_portals_conf() {
    use crate::portal::{IFACE, Places, conf_set, is_on, turn_off, turn_on};
    // The text alone.
    let (t, prev) = conf_set("# mine\n[preferred]\ndefault=hyprland;gtk\norg.freedesktop.impl.portal.ScreenCast=hyprland\n\n[other]\nx=1\n", IFACE, Some("maia"));
    assert_eq!(t, "# mine\n[preferred]\ndefault=hyprland;gtk\norg.freedesktop.impl.portal.ScreenCast=hyprland\norg.freedesktop.impl.portal.FileChooser=maia\n\n[other]\nx=1\n");
    assert_eq!(prev, None);
    let (t2, prev) = conf_set(&t, IFACE, Some("kde"));
    assert_eq!((prev.as_deref(), t2.matches("FileChooser").count()), (Some("maia"), 1), "replaced in place");
    let (t3, _) = conf_set(&t2, IFACE, None);
    assert!(!t3.contains("FileChooser") && t3.contains("ScreenCast=hyprland") && t3.contains("[other]\nx=1"));
    assert_eq!(conf_set("", IFACE, Some("maia")).0, "[preferred]\norg.freedesktop.impl.portal.FileChooser=maia\n");
    assert_eq!(conf_set("[other]\na=b\n", IFACE, Some("maia")).0, "[other]\na=b\n\n[preferred]\norg.freedesktop.impl.portal.FileChooser=maia\n");
    assert_eq!(conf_set("[preferred]\n org.freedesktop.impl.portal.FileChooser = gtk \n", IFACE, None).1.as_deref(), Some("gtk"), "spaces around =");
    let d = tmpdir("picker-routing");
    let sys = d.join("usr/share/xdg-desktop-portal");
    touch(&sys.join("hyprland-portals.conf"), "[preferred]\ndefault=hyprland;gtk\n");
    let pl = Places { data: d.join("data"), config: d.join("config"), state: d.join("state/previous-file-chooser.json"), desktops: vec!["hyprland".into()], system: vec![d.join("etc/xdg-desktop-portal"), sys.clone()], merges: false };
    let exe = Path::new("/opt/maia/bin/maia");
    // No file of the user's: one for this desktop, copied from the desktop's.
    assert!(!is_on(&pl));
    turn_on(&pl, exe).unwrap();
    let mine = d.join("config/xdg-desktop-portal/hyprland-portals.conf");
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "[preferred]\ndefault=hyprland;gtk\norg.freedesktop.impl.portal.FileChooser=maia\n");
    assert_eq!(std::fs::read_to_string(pl.portal_file()).unwrap(), crate::portal::PORTAL_FILE);
    assert_eq!(std::fs::read_to_string(pl.service_file()).unwrap(), "[D-BUS Service]\nName=org.freedesktop.impl.portal.desktop.maia\nExec=/opt/maia/bin/maia --portal\n");
    assert!(is_on(&pl));
    turn_on(&pl, exe).unwrap();
    turn_off(&pl).unwrap();
    assert!(!mine.exists() && !pl.portal_file().exists() && !pl.service_file().exists() && !pl.state.exists(), "everything Maia made is gone");
    assert!(!is_on(&pl));
    // A file of the user's, routed to KDE's dialog, with their own lines: kept, and KDE
    // again after.
    let generic = d.join("config/xdg-desktop-portal/portals.conf");
    let theirs = "[preferred]\n# my choice\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=kde\n";
    touch(&generic, theirs);
    turn_on(&pl, exe).unwrap();
    assert!(!mine.exists(), "the file read now is the one changed");
    assert_eq!(std::fs::read_to_string(&generic).unwrap(), "[preferred]\n# my choice\ndefault=gtk\norg.freedesktop.impl.portal.FileChooser=maia\n");
    // Turned on twice, the first previous value is the one kept.
    turn_on(&pl, exe).unwrap();
    // A line the user added meanwhile stays.
    let edited = std::fs::read_to_string(&generic).unwrap() + "org.freedesktop.impl.portal.Print=gtk\n";
    std::fs::write(&generic, &edited).unwrap();
    turn_off(&pl).unwrap();
    assert_eq!(std::fs::read_to_string(&generic).unwrap(), format!("{theirs}org.freedesktop.impl.portal.Print=gtk\n"));
    // A copy Maia made and the user changed since is theirs now: kept, without the line.
    std::fs::remove_file(&generic).unwrap();
    turn_on(&pl, exe).unwrap();
    std::fs::write(&mine, std::fs::read_to_string(&mine).unwrap() + "org.freedesktop.impl.portal.Screenshot=hyprland\n").unwrap();
    turn_off(&pl).unwrap();
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "[preferred]\ndefault=hyprland;gtk\norg.freedesktop.impl.portal.Screenshot=hyprland\n");
    // A portal that falls back from file to file (1.21.1 on) gets the line alone, so the
    // desktop's own file still decides the rest.
    std::fs::remove_file(&mine).unwrap();
    let pl = Places { merges: true, ..pl };
    turn_on(&pl, exe).unwrap();
    assert_eq!(std::fs::read_to_string(&mine).unwrap(), "[preferred]\norg.freedesktop.impl.portal.FileChooser=maia\n");
    assert!(is_on(&pl));
    turn_off(&pl).unwrap();
    assert!(!mine.exists() && !is_on(&pl));
    // What the Options row says is missing: no portal, or one before 1.20.1 without the
    // packaged file.
    use crate::portal::{missing, version_of};
    assert_eq!(version_of("xdg-desktop-portal 1.18.4\n"), Some((1, 18, 4)));
    assert_eq!(version_of("xdg-desktop-portal 1.21"), Some((1, 21, 0)));
    assert!(missing(None, false).is_some() && missing(Some((1, 18, 4)), false).is_some());
    assert!(missing(Some((1, 18, 4)), true).is_none() && missing(Some((1, 20, 1)), false).is_none() && missing(Some((1, 22, 0)), false).is_none());
    let _ = std::fs::remove_dir_all(&d);
}

/// The backend on a private bus (never the user's): the portal's calls, made by the real
/// xdg-desktop-portal when it is installed, reach the picker and its answer comes back to
/// the app as the Response signal; a Close ends the picker and no answer is sent. Without
/// the portal the backend is called directly. Introspection and the portal's GetAll work.
#[test]
fn portal_backend_answers_through_the_real_portal() {
    use crate::fm1::{Bus, put_str};
    use crate::portal::{Answer, NAME, Pick, Stop, Val, parse, put};
    if !crate::media::have("dbus-daemon") {
        return;
    }
    let d = tmpdir("portal-bus");
    let sock = d.join("bus");
    let conf = d.join("bus.conf");
    std::fs::write(&conf, format!("<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth><policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/><allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>", sock.display())).unwrap();
    let mut daemon = std::process::Command::new("dbus-daemon").arg(format!("--config-file={}", conf.display())).arg("--nofork").stdout(std::process::Stdio::null()).spawn().unwrap();
    let start = Instant::now();
    while !sock.exists() && start.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let addr = format!("unix:path={}", sock.display());
    let mut backend = Bus::connect(&addr).unwrap();
    assert!(backend.own(NAME));
    // A stand-in for the window: picks by the title, or waits to be closed.
    let (closed_tx, closed_rx) = std::sync::mpsc::channel::<String>();
    let closed_tx = std::sync::Mutex::new(closed_tx);
    let run = move |p: &Pick, stop: &Stop| -> Answer {
        if p.title == "wait" {
            let t0 = Instant::now();
            while !stop.stopped() && t0.elapsed() < Duration::from_secs(20) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = closed_tx.lock().unwrap().send(p.title.clone());
            return Answer::ended();
        }
        let name = if p.current_name.is_empty() { "picked.png" } else { &p.current_name };
        Answer { response: 0, uris: vec![format!("file:///tmp/{}", name.replace(' ', "%20"))], choices: vec![], filter: p.current_filter }
    };
    std::thread::spawn(move || crate::portal::serve(backend, std::sync::Arc::new(run)));
    let mut app = Bus::connect(&addr).unwrap();
    let opts = |items: Vec<(&str, &str, Val)>| Val::List(items.into_iter().map(|(k, s, v)| Val::Tuple(vec![Val::Str(k.into()), Val::Var(s.into(), Box::new(v))])).collect());
    // Directly, as the portal calls it.
    let mut body = vec![];
    for (sig, v) in [("o", Val::Str("/org/freedesktop/portal/desktop/request/1_9/direct".into())), ("s", Val::Str("test".into())), ("s", Val::Str(String::new())), ("s", Val::Str("Save as".into()))] {
        put(&mut body, sig, &v);
    }
    put(&mut body, "a{sv}", &opts(vec![("current_name", "s", Val::Str("a b.txt".into()))]));
    let r = app.call(NAME, "/org/freedesktop/portal/desktop", crate::portal::IFACE, "SaveFile", "osssa{sv}", &body).expect("an answer");
    let v = parse(&r.signature, &r.body).unwrap();
    assert_eq!(v[0], Val::U32(0));
    assert!(format!("{v:?}").contains("file:///tmp/a%20b.txt"));
    let intro = app.call(NAME, "/org/freedesktop/portal/desktop", "org.freedesktop.DBus.Introspectable", "Introspect", "", &[]).unwrap();
    assert!(String::from_utf8_lossy(&intro.body).contains("SaveFiles"));
    let mut b = vec![];
    put_str(&mut b, crate::portal::IFACE);
    let all = app.call(NAME, "/org/freedesktop/portal/desktop", "org.freedesktop.DBus.Properties", "GetAll", "s", &b).unwrap();
    assert_eq!(parse(&all.signature, &all.body).unwrap(), vec![Val::List(vec![])]);
    // Through the real portal, when it is here.
    let frontend = ["/usr/libexec/xdg-desktop-portal", "/usr/lib/xdg-desktop-portal"].into_iter().find(|p| Path::new(p).is_file());
    if let Some(exe) = frontend {
        let dir = d.join("portals");
        touch(&dir.join("maia.portal"), crate::portal::PORTAL_FILE);
        touch(&dir.join("portals.conf"), "[preferred]\ndefault=none\norg.freedesktop.impl.portal.FileChooser=maia\n");
        let mut xdp = std::process::Command::new(exe)
            .env_clear()
            .env("DBUS_SESSION_BUS_ADDRESS", &addr)
            .env("XDG_DESKTOP_PORTAL_DIR", &dir)
            .env("XDG_CURRENT_DESKTOP", "test")
            .env("HOME", &d)
            .env("XDG_RUNTIME_DIR", &d)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        // The portal is up once it owns its name.
        let t0 = Instant::now();
        loop {
            let mut b = vec![];
            put_str(&mut b, "org.freedesktop.portal.Desktop");
            if app.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "GetNameOwner", "s", &b).is_some() {
                break;
            }
            assert!(t0.elapsed() < Duration::from_secs(10), "the portal started");
            std::thread::sleep(Duration::from_millis(50));
        }
        let mut b = vec![];
        put_str(&mut b, "type='signal',interface='org.freedesktop.portal.Request',member='Response'");
        app.call("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "AddMatch", "s", &b).unwrap();
        let ask = |app: &mut Bus, method: &str, title: &str, o: Val| -> String {
            let mut body = vec![];
            put(&mut body, "s", &Val::Str("x11:1c00003".into()));
            put(&mut body, "s", &Val::Str(title.into()));
            put(&mut body, "a{sv}", &o);
            let r = app.call("org.freedesktop.portal.Desktop", "/org/freedesktop/portal/desktop", "org.freedesktop.portal.FileChooser", method, "ssa{sv}", &body).expect("the portal took it");
            match parse(&r.signature, &r.body).unwrap().remove(0) {
                Val::Str(h) => h,
                v => panic!("{v:?}"),
            }
        };
        let response = |app: &mut Bus, handle: &str| -> Vec<Val> {
            loop {
                let m = app.recv().expect("a signal");
                if m.member == "Response" && m.path == handle {
                    return parse(&m.signature, &m.body).unwrap();
                }
            }
        };
        let images = Val::Tuple(vec![Val::Str("Images".into()), Val::List(vec![Val::Tuple(vec![Val::U32(1), Val::Str("image/*".into())])])]);
        let h = ask(&mut app, "OpenFile", "Open a picture", opts(vec![("handle_token", "s", Val::Str("t1".into())), ("filters", "a(sa(us))", Val::List(vec![images.clone()])), ("current_filter", "(sa(us))", images)]));
        assert!(h.ends_with("/t1"), "{h}");
        let v = response(&mut app, &h);
        assert_eq!(v[0], Val::U32(0));
        let s = format!("{v:?}");
        assert!(s.contains("file:///tmp/picked.png") && s.contains("Images"), "{s}");
        let h = ask(&mut app, "SaveFile", "Save the page", opts(vec![("handle_token", "s", Val::Str("t2".into())), ("current_name", "s", Val::Str("page one.html".into()))]));
        assert!(format!("{:?}", response(&mut app, &h)).contains("file:///tmp/page%20one.html"));
        // Closed by the app: the picker goes, and nothing answers.
        let h = ask(&mut app, "OpenFile", "wait", opts(vec![("handle_token", "s", Val::Str("t3".into()))]));
        std::thread::sleep(Duration::from_millis(200));
        assert!(app.call("org.freedesktop.portal.Desktop", &h, "org.freedesktop.portal.Request", "Close", "", &[]).is_some());
        assert_eq!(closed_rx.recv_timeout(Duration::from_secs(10)).ok().as_deref(), Some("wait"), "the picker was told to go");
        let _ = xdp.kill();
        let _ = xdp.wait();
    }
    let _ = daemon.kill();
    let _ = daemon.wait();
    let _ = std::fs::remove_dir_all(&d);
}

/// A picker window's answer in each mode, from the real app over a real folder: one file,
/// several (in the order listed), a folder (the one shown or the one selected), a new
/// name, a name already taken (asked first), a folder's name (gone into), several names
/// to save in a folder, Cancel and Escape. The app's filter decides what is listed.
#[test]
fn picker_answers_each_mode() {
    use crate::picker::{Picker, accept, cancel};
    use crate::portal::{Answer, Filter, Mode, Pick};
    let d = tmpdir("picker-modes");
    for f in ["a.png", "b.txt", "c d.png", "keep.txt", "sub/inner.txt"] {
        touch(&d.join(f), "x");
    }
    let uri = |p: &Path| crate::clipboard::path_to_uri(p);
    let ctx = egui::Context::default();
    let frame = |app: &mut crate::app::App, events: Vec<egui::Event>| {
        let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 640.0))), events, ..Default::default() };
        ctx.run_pass(raw, |c| app.frame(c)).drop_without_applying_deltas();
    };
    let open = |pick: Pick| -> crate::app::App {
        let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
        app.picker = Some(Box::new(Picker::new(pick)));
        let t0 = Instant::now();
        loop {
            frame(&mut app, vec![]);
            if !app.tab().is_loading() && !app.tab().dirty || t0.elapsed() > Duration::from_secs(5) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        frame(&mut app, vec![]);
        app
    };
    let select = |app: &mut crate::app::App, names: &[&str]| {
        let t = app.tab();
        t.selected.clear();
        for n in names {
            let i = t.entries.iter().position(|e| e.name == *n).unwrap_or_else(|| panic!("{n} listed"));
            t.selected.insert(i as u32);
            t.cursor = Some(i as u32);
        }
    };
    let done = |app: &crate::app::App| app.picker.as_ref().and_then(|p| p.done.clone());
    let listed = |app: &mut crate::app::App| -> Vec<String> {
        let t = app.tab();
        let mut v: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
        v.sort();
        v
    };
    // One file, with the filter the app gave: only pictures (and folders) are listed.
    let pngs = Filter { name: "Pictures".into(), rules: vec![(0, "*.[pP][nN][gG]".into())] };
    let mut app = open(Pick { filters: vec![Filter { name: "Text".into(), rules: vec![(1, "text/plain".into())] }, pngs.clone()], current_filter: Some(1), ..Default::default() });
    assert_eq!(listed(&mut app), ["a.png", "c d.png", "sub"]);
    select(&mut app, &["a.png"]);
    accept(&mut app);
    assert_eq!(done(&app), Some(Answer { response: 0, uris: vec![uri(&d.join("a.png"))], choices: vec![], filter: Some(1) }));
    assert!(app.quit, "the window closes with its answer");
    // Several, in the order listed; without `multiple` only the first.
    for multiple in [true, false] {
        let mut app = open(Pick { multiple, ..Default::default() });
        assert_eq!(listed(&mut app).len(), 5, "no filter: everything");
        select(&mut app, &["c d.png", "a.png", "sub"]);
        accept(&mut app);
        let want: Vec<String> = if multiple { vec![uri(&d.join("a.png")), uri(&d.join("c d.png"))] } else { vec![uri(&d.join("a.png"))] };
        assert_eq!(done(&app).map(|a| a.uris), Some(want), "multiple {multiple}");
    }
    // A folder alone is gone into, not answered.
    let mut app = open(Pick::default());
    select(&mut app, &["sub"]);
    accept(&mut app);
    assert!(done(&app).is_none() && app.tab().path == d.join("sub"));
    // Folders: the one shown, or the one selected; files are not listed.
    let mut app = open(Pick { directory: true, ..Default::default() });
    assert_eq!(listed(&mut app), ["sub"]);
    accept(&mut app);
    assert_eq!(done(&app).map(|a| a.uris), Some(vec![uri(&d)]));
    let mut app = open(Pick { directory: true, ..Default::default() });
    select(&mut app, &["sub"]);
    assert!(crate::picker::enter(&mut app), "Enter answers in folder mode");
    assert_eq!(done(&app).map(|a| a.uris), Some(vec![uri(&d.join("sub"))]));
    // Save: the name given, in the folder shown, with the app's question answered.
    let q = crate::portal::Choice { id: "ro".into(), label: "Compress".into(), options: vec![], chosen: "true".into() };
    let mut app = open(Pick { mode: Mode::Save, current_name: "new file.txt".into(), choices: vec![q], ..Default::default() });
    assert_eq!(app.picker.as_ref().unwrap().name, "new file.txt");
    accept(&mut app);
    assert_eq!(done(&app), Some(Answer { response: 0, uris: vec![uri(&d.join("new file.txt"))], choices: vec![("ro".into(), "true".into())], filter: None }));
    // A name already there asks first; the app does the replacing.
    let mut app = open(Pick { mode: Mode::Save, current_name: "keep.txt".into(), ..Default::default() });
    accept(&mut app);
    assert!(done(&app).is_none());
    let crate::popups::Popup::Choose { choices, msg, .. } = std::mem::replace(&mut app.popup, crate::popups::Popup::None) else { panic!("asked to replace") };
    assert!(msg.contains("keep.txt"));
    let (label, risky, f) = choices.into_iter().next().unwrap();
    assert_eq!((label.as_str(), risky), ("Replace", true));
    (f.unwrap())(&mut app);
    assert_eq!(done(&app).map(|a| a.uris), Some(vec![uri(&d.join("keep.txt"))]));
    assert_eq!(std::fs::read_to_string(d.join("keep.txt")).unwrap(), "x", "Maia itself writes nothing");
    // A folder's name goes into it; a file clicked in the list names the file.
    let mut app = open(Pick { mode: Mode::Save, current_name: "sub".into(), ..Default::default() });
    accept(&mut app);
    assert!(done(&app).is_none() && app.tab().path == d.join("sub") && app.picker.as_ref().unwrap().name.is_empty());
    let mut app = open(Pick { mode: Mode::Save, current_name: "x".into(), ..Default::default() });
    select(&mut app, &["b.txt"]);
    frame(&mut app, vec![]);
    assert_eq!(app.picker.as_ref().unwrap().name, "b.txt");
    // Several names for one folder; one of them taken.
    let mut app = open(Pick { mode: Mode::SaveFiles, files: vec!["x.txt".into(), "keep.txt".into()], ..Default::default() });
    assert_eq!(listed(&mut app), ["sub"], "a folder is chosen");
    accept(&mut app);
    let crate::popups::Popup::Choose { choices, .. } = std::mem::replace(&mut app.popup, crate::popups::Popup::None) else { panic!("asked to replace") };
    (choices.into_iter().next().unwrap().2.unwrap())(&mut app);
    assert_eq!(done(&app).map(|a| a.uris), Some(vec![uri(&d.join("x.txt")), uri(&d.join("keep.txt"))]));
    // Cancel, and Escape.
    let mut app = open(Pick::default());
    cancel(&mut app);
    assert_eq!(done(&app), Some(Answer::cancelled()));
    let mut app = open(Pick::default());
    frame(&mut app, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert_eq!(done(&app).map(|a| a.response), Some(1), "Escape cancels");
    // Escape clears a filter first.
    let mut app = open(Pick::default());
    app.tab().filter = "zz".into();
    frame(&mut app, vec![egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }]);
    assert!(done(&app).is_none() && app.tab().filter.is_empty());
    let _ = crate::picker::ANSWER.lock().unwrap().take();
    let _ = std::fs::remove_dir_all(&d);
}

/// A tag's view finds what carries it under a folder, in any case, folders too, and
/// skips what smart folders skip; the list sorts and groups by each item's first tag,
/// colour labels in their order before the words, the untagged last either way round.
#[test]
fn tag_views_find_and_sort_by_the_first_tag() {
    use crate::config::GroupBy;
    use crate::tags;
    let d = tmpdir("tagview");
    for f in ["a.txt", "b.txt", "c.txt", "d.txt", "sub/e.txt", ".hidden/g.txt", "node_modules/h.txt"] {
        touch(&d.join(f), "x");
    }
    std::fs::create_dir_all(d.join("sub/f")).unwrap();
    if tags::write(&d.join("a.txt"), &["work".into()]).is_err() {
        eprintln!("no user xattrs here: skipped");
        return;
    }
    let set = |f: &str, t: &[&str]| tags::write(&d.join(f), &t.iter().map(|s| s.to_string()).collect::<Vec<_>>()).unwrap();
    set("b.txt", &["archive", "Blue"]);
    set("c.txt", &["Red"]);
    set("sub/e.txt", &["Work"]);
    set("sub/f", &["work", "Gray"]);
    set(".hidden/g.txt", &["work"]);
    set("node_modules/h.txt", &["work"]);
    let found = std::sync::Mutex::new(vec![]);
    let (cancel, cut) = (std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false));
    tags::search(&d, "WORK", &|b| {
        found.lock().unwrap().extend(b.into_iter().map(|e| e.path.strip_prefix(&d).unwrap().to_path_buf()));
        true
    }, &cancel, &cut);
    let mut found = found.into_inner().unwrap();
    found.sort();
    assert_eq!(found, [PathBuf::from("a.txt"), PathBuf::from("sub/e.txt"), PathBuf::from("sub/f")]);
    // A file's first tag in the Tags order: Blue before the word "archive".
    assert_eq!(tags::leading(&tags::read(&d.join("b.txt"))).map(String::as_str), Some("Blue"));
    assert_eq!(tags::ordered(&["work".into(), "Gray".into(), "Red".into(), "archive".into()]), ["Red", "Gray", "archive", "work"]);
    assert_eq!(tags::edited(&["a".into(), "b".into()], &tags::How::Rename("A".into(), "b".into())), ["b"], "renamed onto one it had");
    // The listing, once the reader has every file's tags.
    let stats = HashMap::new();
    let ctx = egui::Context::default();
    let mut t = Tab::new(92, d.clone(), ViewSettings::default(), ctx.clone());
    let order = |t: &mut Tab, sort: SortKey, desc: bool, group: GroupBy| -> (Vec<String>, Vec<String>) {
        t.settings.sort = sort;
        t.settings.desc = desc;
        t.settings.group = group;
        let start = Instant::now();
        loop {
            t.dirty = true;
            pump(t, &stats);
            tags::flush(&ctx);
            if t.entries.iter().all(|e| tags::get(&e.path, |_| ()).is_some()) || start.elapsed() > Duration::from_secs(5) {
                t.dirty = true;
                pump(t, &stats);
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        (t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect(), t.header_labels())
    };
    assert_eq!(order(&mut t, SortKey::Tags, false, GroupBy::None).0, ["node_modules", "sub", "c.txt", "b.txt", "a.txt", "d.txt"]);
    assert_eq!(order(&mut t, SortKey::Tags, true, GroupBy::None).0, ["sub", "node_modules", "a.txt", "b.txt", "c.txt", "d.txt"], "untagged still last");
    let (names, groups) = order(&mut t, SortKey::Name, false, GroupBy::Tag);
    assert_eq!(groups, ["Red", "Blue", "work", "No tags"], "an item under its first tag only");
    assert_eq!(names, ["c.txt", "b.txt", "a.txt", "node_modules", "sub", "d.txt"]);
    // Every tag read so far, colour labels too, for the sidebar.
    let mut known = vec!["work".to_string()];
    assert!(tags::remember(&mut known));
    for t in ["Red", "Blue", "archive", "Work"] {
        assert!(known.iter().any(|k| k.eq_ignore_ascii_case(t)), "{t} known");
    }
    assert_eq!(known.iter().filter(|k| k.eq_ignore_ascii_case("work")).count(), 1, "one entry whatever the case");
    let _ = std::fs::remove_dir_all(&d);
}

/// A tag change is one change for Undo, however many files it touched: taken back and
/// done again, only where each file's tags are still what the change left. Past a few
/// hundred files it runs on a worker and is recorded when it ends.
#[test]
fn tag_changes_undo_and_redo_as_one() {
    use crate::tags::{self, How, Job, Record};
    let ctx = egui::Context::default();
    let d = tmpdir("tagundo");
    let files: Vec<PathBuf> = (0..3).map(|i| d.join(format!("f{i}.txt"))).collect();
    for f in &files {
        touch(f, "x");
    }
    if tags::write(&files[0], &["Red".into()]).is_err() {
        eprintln!("no user xattrs here: skipped");
        return;
    }
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    app.undo_keep = crate::undofile::Keeper::new(None, &ctx);
    let read = |f: &PathBuf| tags::read(f);
    tags::start(&mut app, Job::Edit(files.clone(), How::Add("work".into())), Record::New("Tag 3 items work".into()));
    assert_eq!(read(&files[0]), ["Red", "work"]);
    assert_eq!(read(&files[2]), ["work"]);
    assert_eq!((app.undo.len(), app.undo.last().map(|u| u.label.as_str())), (1, Some("Tag 3 items work")));
    assert!(app.undo_last());
    assert_eq!(read(&files[0]), ["Red"]);
    assert!(read(&files[2]).is_empty() && app.undo.can_redo());
    assert!(app.redo_last());
    assert_eq!(read(&files[1]), ["work"]);
    // Renamed everywhere, then one file changed by someone else before Undo: that one
    // is left as it is.
    tags::start(&mut app, Job::Edit(files.clone(), How::Rename("work".into(), "job".into())), Record::New("Rename tag “work” to “job”".into()));
    assert_eq!(read(&files[0]), ["Red", "job"]);
    tags::write(&files[1], &["job".into(), "mine".into()]).unwrap();
    app.undo_last();
    assert_eq!(read(&files[0]), ["Red", "work"]);
    assert_eq!(read(&files[1]), ["job", "mine"], "changed since: left alone");
    assert_eq!(app.undo.redos().last().map(|u| u.kind.count()), Some(2), "redo holds what was taken back");
    // Many files: on a worker, one change when it ends.
    let many: Vec<PathBuf> = (0..260).map(|i| d.join(format!("m/{i}.txt"))).collect();
    for f in &many {
        touch(f, "x");
    }
    let before = app.undo.len();
    tags::start(&mut app, Job::Edit(many.clone(), How::Add("Green".into())), Record::New("Tag 260 items Green".into()));
    app_until(&mut app, &ctx, "the tagging", |a| !tags::busy() && a.undo.len() == before + 1);
    assert!(many.iter().all(|f| read(f) == ["Green"]));
    assert_eq!(app.undo.last().map(|u| u.kind.count()), Some(260));
    app.undo_last();
    app_until(&mut app, &ctx, "the untagging", |_| !tags::busy() && read(&many[259]).is_empty());
    assert!(many.iter().all(|f| read(f).is_empty()));
    let _ = std::fs::remove_dir_all(&d);
}

/// A JPEG with EXIF written the way a camera writes it: Orientation in IFD0, then
/// DateTimeOriginal in the EXIF IFD it points to.
/// The real ffmpeg and ffprobe on the PATH, linked into a stand-in folder; None when
/// one is missing.
pub(crate) fn media_tools(dir: &Path) -> Option<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    for t in ["ffmpeg", "ffprobe"] {
        let real = std::env::var_os("PATH").and_then(|p| std::env::split_paths(&p).map(|d| d.join(t)).find(|m| m.is_file()))?;
        let _ = std::os::unix::fs::symlink(real, dir.join(t));
    }
    Some(dir.to_path_buf())
}

pub(crate) fn with_exif_taken(jpeg: &[u8], orient: u16, taken: &str) -> Vec<u8> {
    let mut t = b"MM\x00\x2a\x00\x00\x00\x08".to_vec();
    let entry = |t: &mut Vec<u8>, tag: u16, kind: u16, n: u32, v: [u8; 4]| {
        t.extend_from_slice(&tag.to_be_bytes());
        t.extend_from_slice(&kind.to_be_bytes());
        t.extend_from_slice(&n.to_be_bytes());
        t.extend_from_slice(&v);
    };
    // IFD0 at 8: two entries (2 + 24 + 4 bytes), the EXIF IFD at 38.
    t.extend_from_slice(&2u16.to_be_bytes());
    let o = orient.to_be_bytes();
    entry(&mut t, 0x0112, 3, 1, [o[0], o[1], 0, 0]);
    entry(&mut t, 0x8769, 4, 1, 38u32.to_be_bytes());
    t.extend_from_slice(&[0; 4]);
    // EXIF IFD: one entry, its 20-byte date right after (38 + 2 + 12 + 4 = 56).
    t.extend_from_slice(&1u16.to_be_bytes());
    entry(&mut t, 0x9003, 2, 20, 56u32.to_be_bytes());
    t.extend_from_slice(&[0; 4]);
    t.extend_from_slice(taken.as_bytes());
    t.push(0);
    let mut app1 = b"Exif\x00\x00".to_vec();
    app1.extend_from_slice(&t);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xff, 0xe1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// Real pictures, films and recordings made by ImageMagick and ffmpeg: the date a photo
/// was taken from its EXIF (a film's from its container), sizes from every header kind
/// (a photo stored turned counted turned), lengths from ffprobe, read off the frame and
/// kept on disk for the next time; the list sorts by each with the rest last either way
/// round, and groups photos by month.
#[test]
fn media_facts_from_real_files_sort_and_group() {
    use crate::config::GroupBy;
    use crate::facts;
    let _one = FACTS_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    facts::focus(vec![]);
    let d = tmpdir("facts");
    let Some(tools) = media_tools(&d.join("tools")) else {
        eprintln!("ffmpeg missing: skipped");
        return;
    };
    let w = d.join("media");
    std::fs::create_dir_all(&w).unwrap();
    let run = |prog: &str, args: &[&str]| assert!(std::process::Command::new(tools.join(prog)).args(args).current_dir(&w).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success()), "{prog} {args:?}");
    let ff = |args: &[&str]| run("ffmpeg", &[&["-hide_banner", "-loglevel", "error", "-y"], args].concat());
    ff(&["-f", "lavfi", "-i", "testsrc=size=400x240", "-frames:v", "1", "raw.jpg"]);
    let raw = std::fs::read(w.join("raw.jpg")).unwrap();
    std::fs::write(w.join("turned.jpg"), with_exif_taken(&raw, 6, "2023:07:14 18:30:05")).unwrap();
    std::fs::write(w.join("later.jpg"), with_exif_taken(&raw, 1, "2024:01:02 09:00:00")).unwrap();
    std::fs::remove_file(w.join("raw.jpg")).unwrap();
    // Both kinds of WebP: lossy (VP8) and lossless (VP8L).
    for (f, size, extra) in [("wide.png", "300x20", ""), ("anim.gif", "64x48", ""), ("small.webp", "90x60", ""), ("flat.webp", "70x50", "-lossless 1"), ("old.bmp", "33x17", "")] {
        let src = format!("testsrc=size={size}");
        let mut a = vec!["-f", "lavfi", "-i", src.as_str(), "-frames:v", "1"];
        a.extend(extra.split_whitespace());
        a.push(f);
        ff(&a);
    }
    ff(&["-f", "lavfi", "-i", "testsrc=duration=2:size=320x240:rate=10", "-pix_fmt", "yuv420p", "-metadata", "creation_time=2024-05-01T10:00:00Z", "clip.mp4"]);
    ff(&["-f", "lavfi", "-i", "sine=frequency=440:duration=3", "tone.flac"]);
    touch(&w.join("notes.txt"), "x");
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(tools.clone()));
    let ctx = egui::Context::default();
    let stats = HashMap::new();
    let mut t = Tab::new(93, w.clone(), ViewSettings::default(), ctx.clone());
    let settle = |t: &mut Tab| {
        let start = Instant::now();
        loop {
            t.dirty = true;
            pump(t, &stats);
            facts::flush(&ctx);
            if t.entries.iter().all(|e| facts::get(e).is_some()) {
                t.dirty = true;
                pump(t, &stats);
                return;
            }
            assert!(start.elapsed() < Duration::from_secs(20), "facts read");
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    t.settings.columns.push((crate::config::Column::Duration, 80.0));
    settle(&mut t);
    let of = |t: &Tab, n: &str| facts::get(t.entries.iter().find(|e| e.name == n).unwrap()).unwrap();
    let local = |s: &str| crate::timefmt::parse_local(s).unwrap();
    assert_eq!(of(&t, "turned.jpg"), facts::Facts { taken: Some(local("2023-07-14T18:30:05")), dims: Some((240, 400)), duration: None }, "stored turned, shown upright");
    assert_eq!(of(&t, "later.jpg").dims, Some((400, 240)));
    for (n, dims) in [("wide.png", (300, 20)), ("anim.gif", (64, 48)), ("small.webp", (90, 60)), ("flat.webp", (70, 50)), ("old.bmp", (33, 17))] {
        assert_eq!(of(&t, n).dims, Some(dims), "{n}");
    }
    let clip = of(&t, "clip.mp4");
    assert_eq!((clip.dims, clip.taken), (Some((320, 240)), Some(std::time::UNIX_EPOCH + Duration::from_secs(1_714_557_600))));
    assert!((clip.duration.unwrap() - 2.0).abs() < 0.1 && (of(&t, "tone.flac").duration.unwrap() - 3.0).abs() < 0.1);
    assert_eq!((of(&t, "tone.flac").dims, of(&t, "tone.flac").taken), (None, None), "a recording has no size nor date taken");
    assert_eq!(of(&t, "notes.txt"), facts::Facts::default());
    // Sorted by each, the rest after, whichever way round.
    let names = |t: &mut Tab, k: SortKey, desc: bool| -> Vec<String> {
        t.settings.sort = k;
        t.settings.desc = desc;
        settle(t);
        t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect()
    };
    assert_eq!(&names(&mut t, SortKey::Duration, true)[..2], ["tone.flac", "clip.mp4"]);
    assert_eq!(&names(&mut t, SortKey::Duration, false)[..2], ["clip.mp4", "tone.flac"]);
    assert_eq!(&names(&mut t, SortKey::Taken, true)[..3], ["clip.mp4", "later.jpg", "turned.jpg"]);
    assert_eq!(names(&mut t, SortKey::Dimensions, false), ["old.bmp", "anim.gif", "flat.webp", "small.webp", "wide.png", "clip.mp4", "later.jpg", "turned.jpg", "notes.txt", "tone.flac"]);
    t.settings.group = GroupBy::Taken;
    names(&mut t, SortKey::Name, false);
    assert_eq!(t.header_labels(), ["May 2024", "January 2024", "July 2023", "No date"]);
    // What ffprobe found is kept, by path, size and time changed.
    let kept = std::fs::read_to_string(facts::kept_path()).unwrap();
    assert!(kept.lines().any(|l| l.ends_with(&*w.join("clip.mp4").to_string_lossy()) && l.contains("\t320\t240\t")), "{kept}");
    // A film changed is read again.
    facts::invalidate(&w.join("clip.mp4"));
    assert!(facts::get(t.entries.iter().find(|e| e.name == "clip.mp4").unwrap()).is_none(), "asked again");
    settle(&mut t);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&d);
}

/// The folders shown are one setting for the whole process: one test at a time.
static FACTS_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A probe for a folder no longer shown is killed, and the file is asked for again
/// when its folder is shown again. A stand-in ffprobe that hangs, writing its pid.
#[test]
fn media_probes_stop_when_their_folder_is_left() {
    use crate::facts;
    let _one = FACTS_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let d = tmpdir("factsgone");
    let fake = d.join("tools");
    std::fs::create_dir_all(&fake).unwrap();
    let pidf = d.join("pid");
    std::fs::write(fake.join("ffprobe"), format!("#!/bin/sh\necho $$ > {}\nexec sleep 30\n", pidf.display())).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(fake.join("ffprobe"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let here = d.join("films");
    touch(&here.join("long.mkv"), "not really");
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
    facts::focus(vec![here.clone()]);
    let e = crate::fsx::Entry::from_path(&here.join("long.mkv")).unwrap();
    assert!(facts::get(&e).is_none());
    facts::flush(&egui::Context::default());
    let t0 = Instant::now();
    while !pidf.exists() {
        assert!(t0.elapsed() < Duration::from_secs(10), "probe started");
        std::thread::sleep(Duration::from_millis(10));
    }
    let pid = std::fs::read_to_string(&pidf).unwrap().trim().parse::<i32>().unwrap();
    facts::focus(vec![d.join("elsewhere")]);
    let t0 = Instant::now();
    while unsafe { libc::kill(pid, 0) } == 0 && std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|s| !s.contains(") Z ")) {
        assert!(t0.elapsed() < Duration::from_secs(5), "probe killed");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Not taken for an answer: shown again, it is asked for again.
    std::thread::sleep(Duration::from_millis(100));
    facts::focus(vec![here.clone()]);
    assert!(facts::get(&e).is_none());
    facts::focus(vec![]);
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let _ = std::fs::remove_dir_all(&d);
}

/// git in `d` as one fixed person at one fixed time of day `day` of September 2026, so
/// the commits (and their hashes) come out the same every run.
fn git_on(d: &Path, day: u32, args: &[&str]) {
    let date = format!("2026-09-{day:02}T10:00:00+00:00");
    let ok = std::process::Command::new("git")
        .args(["-c", "commit.gpgsign=false", "-c", "init.defaultBranch=main", "-c", "core.hooksPath=/dev/null"])
        .args(args)
        .current_dir(d)
        .env("GIT_AUTHOR_NAME", "Ada Lovelace")
        .env("GIT_AUTHOR_EMAIL", "ada@example.invalid")
        .env("GIT_COMMITTER_NAME", "Ada Lovelace")
        .env("GIT_COMMITTER_EMAIL", "ada@example.invalid")
        .env("GIT_AUTHOR_DATE", &date)
        .env("GIT_COMMITTER_DATE", &date)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    assert!(ok, "git {args:?}");
}

/// What the history's find bar makes of its text: plain words, "quoted words", the
/// `author:`, `path:`, `since:` and `until:` terms (half typed ones ignored, a date form
/// that isn't one kept as a word), case only when a word has capitals, git's flags, what
/// a row marks, and the text a search starts with in a subfolder.
#[test]
fn git_search_text_read() {
    use crate::gitfind::{hash_hit, parse, spans, start_text};
    let now = 1_790_000_000;
    let q = parse("fix  \"two words\" author:ada path:./src/ui/ since:7d until:2026-09 author: path:", now);
    assert_eq!(q.words, ["fix", "two words"]);
    assert_eq!((q.authors.as_slice(), q.paths.as_slice()), (&["ada".to_string()][..], &["src/ui".to_string()][..]));
    assert_eq!(q.since, Some(now - 7 * 86_400));
    let oct = crate::timefmt::parse_local("2026-10-01T00:00:00").map(|t| t.duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64).unwrap();
    assert_eq!(q.until, Some(oct - 1), "the whole of September");
    assert!(!q.case, "a capital in a path or a date says nothing");
    assert_eq!(q.flags(), ["--fixed-strings", "--regexp-ignore-case", "--author=ada", &format!("--since=@{}", now - 7 * 86_400), &format!("--until=@{}", oct - 1)]);
    assert!(parse("Fix", now).case && parse("author:Ada", now).case && !parse("path:README.md fix", now).case);
    assert_eq!(parse("until:3d", now).until, Some(now - 3 * 86_400), "an age's moment itself");
    assert_eq!(parse("since:soon", now).words, ["since:soon"], "not a date: a word");
    assert_eq!(parse("Author:\"Grace Hopper\"", now).authors, ["Grace Hopper"]);
    assert!(parse("  author: path:/ ", now).is_empty(), "nothing to look for yet");
    assert!(parse("", now).flags().is_empty());
    let q = parse("pan panel lay", now);
    assert_eq!(spans(&q, "Lay out the panels", false), [(0, 3), (12, 17)], "overlaps joined");
    let q = parse("grace author:hop", now);
    assert_eq!((spans(&q, "Grace Hopper", true), spans(&q, "Grace Hopper", false)), (vec![(0, 5), (6, 9)], vec![(0, 5)]));
    let q = parse("1a2B", now);
    assert_eq!((hash_hit(&q, "1a2b3c4d"), hash_hit(&q, "ffff1a2b"), hash_hit(&parse("1a2", now), "1a2b")), (4, 0, 0), "four hex digits at least, from the start");
    assert_eq!((start_text("src/ui"), start_text(""), start_text("my notes/"), start_text(".")), ("path:src/ui ".to_string(), String::new(), "path:\"my notes\" ".to_string(), String::new()));
}

/// Commits found as the history's search finds them: git run with the flags the view
/// gives it, its output read as the view reads it. Words are looked for in the subject,
/// the rest of the message, the author and their email and the hash; the terms narrow in
/// git; a file's history keeps to its file.
#[test]
fn git_history_search_finds_commits() {
    use std::io::BufRead;
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitsearch");
    commit_view_fixture(&d);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let read = |path: Option<&str>, text: &str, n: usize| -> (Vec<String>, bool) {
        let q = crate::gitfind::parse(text, now);
        let mut child = std::process::Command::new("git").arg("-C").arg(&d).args(crate::gitlog::list_args(path, Some(&q))).stdout(std::process::Stdio::piped()).spawn().unwrap();
        let mut r = std::io::BufReader::new(child.stdout.take().unwrap());
        let mut got = vec![];
        let end = crate::gitlog::read_matching(&mut r, n, &q, |v| got.extend(v));
        drop(r.fill_buf().map(|_| ()));
        let _ = child.kill();
        let _ = child.wait();
        (got.into_iter().map(|c| c.subject).collect(), end)
    };
    let found = |text: &str| read(None, text, usize::MAX).0;
    let all = ["Tune the panel", "Remove the logo", "Write the long notes", "Lay out the panels", "Start the project"];
    assert_eq!(found("panels"), ["Lay out the panels"]);
    assert_eq!(found("Panels"), Vec::<String>::new(), "a capital matches case");
    assert_eq!(found("handbook"), ["Lay out the panels"], "in the message under the subject");
    assert_eq!(found("grace"), ["Tune the panel", "Lay out the panels"], "the author's name");
    assert_eq!(found("linus@example"), ["Write the long notes"], "their email");
    assert_eq!(found("panel grace"), ["Tune the panel", "Lay out the panels"], "every word, each anywhere");
    assert_eq!(found("\"long notes\""), ["Write the long notes"]);
    assert_eq!(found("zzz"), Vec::<String>::new());
    let logo = git_said(&d, &["log", "-1", "--format=%H", "--grep=Remove the logo"]);
    assert_eq!(found(&logo[..6]), ["Remove the logo"], "the start of its hash");
    assert_eq!(found("author:ada"), ["Remove the logo", "Start the project"]);
    assert_eq!(found("author:ADA"), Vec::<String>::new());
    assert_eq!(found("author:\"Grace Hopper\" author:linus"), ["Tune the panel", "Write the long notes", "Lay out the panels"], "any of several");
    assert_eq!(found("path:notes.md"), ["Write the long notes", "Start the project"]);
    assert_eq!(found("path:assets"), ["Remove the logo", "Lay out the panels", "Start the project"], "a folder");
    assert_eq!(found("path:./src/ui/"), ["Tune the panel", "Lay out the panels", "Start the project"]);
    assert_eq!(found("author:grace path:src"), ["Tune the panel", "Lay out the panels"]);
    assert_eq!(found("since:2026-09-04"), ["Tune the panel", "Remove the logo"]);
    assert_eq!(found("until:2026-09-02"), ["Lay out the panels", "Start the project"]);
    assert_eq!(found("since:2026-09-02 until:2026-09-03 the"), ["Write the long notes", "Lay out the panels"]);
    assert_eq!((found("since:1d"), found("until:1d")), (vec![], all.map(String::from).to_vec()), "ages count back from now");
    // A file's history searched: its own commits only, other paths left out.
    assert_eq!(read(Some("src/ui/panel.rs"), "tune", usize::MAX).0, ["Tune the panel"]);
    assert_eq!(read(Some("src/ui/panel.rs"), "path:assets", usize::MAX).0, ["Tune the panel", "Lay out the panels"]);
    // A page of one: the reader stops there, the log not ended.
    assert_eq!(read(None, "the", 1), (vec!["Tune the panel".to_string()], false));
    assert_eq!(read(None, "the", 10), (all.map(String::from).to_vec(), true));
    let _ = std::fs::remove_dir_all(&d);
}

/// A repository for comparing: main and topic split after "Start"; topic changes the
/// notes, adds a file and changes the picture, main adds a readme.
pub(crate) fn branches_fixture(d: &Path) {
    std::fs::create_dir_all(d).unwrap();
    git_on(d, 1, &["init", "-q"]);
    touch(&d.join("notes.txt"), "one\ntwo\nthree\n");
    std::fs::write(d.join("pic.png"), png_bytes(20, 10, 1)).unwrap();
    git_on(d, 1, &["add", "-A"]);
    git_on(d, 1, &["commit", "-qm", "Start"]);
    git_on(d, 2, &["switch", "-qc", "topic"]);
    touch(&d.join("notes.txt"), "one\nTWO on topic\nthree\n");
    git_on(d, 2, &["commit", "-qam", "Topic edits the notes"]);
    touch(&d.join("extra.txt"), "more\n");
    git_on(d, 3, &["add", "-A"]);
    git_on(d, 3, &["commit", "-qm", "Topic adds a file"]);
    std::fs::write(d.join("pic.png"), png_bytes(30, 12, 2)).unwrap();
    git_on(d, 4, &["commit", "-qam", "Topic redraws the picture"]);
    git_on(d, 5, &["switch", "-q", "main"]);
    touch(&d.join("readme.md"), "# Read me\n");
    git_on(d, 5, &["add", "-A"]);
    git_on(d, 5, &["commit", "-qm", "Main gets a readme"]);
}

/// Two branches compared: the commits only each has, where they split, the files topic
/// changes since then (and every difference, main's readme included), a file's diff and
/// the picture's two sides as the commit view reads them; two commits on one line put
/// older first; a name git doesn't know, and two histories with nothing in common.
#[test]
fn git_compare_branches_and_commits() {
    use crate::gitcompare::{heading, sides};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitcompare");
    branches_fixture(&d);
    let rev = |r: &str| git_said(&d, &["rev-parse", r]);
    let subjects = |v: &[crate::gitlog::Commit]| v.iter().map(|c| c.subject.clone()).collect::<Vec<_>>();
    let s = sides(&d, "main", "topic", false).unwrap();
    assert_eq!((s.hashes.clone(), s.base.clone(), s.counts, s.swapped), ([rev("main"), rev("topic")], Some(rev("main~1")), [1, 3], false));
    assert_eq!(subjects(&s.only[0]), ["Main gets a readme"]);
    assert_eq!(subjects(&s.only[1]), ["Topic redraws the picture", "Topic adds a file", "Topic edits the notes"]);
    let names = |v: &[crate::gitlog::Change]| v.iter().map(|c| (c.status as char, c.path.clone(), c.lines)).collect::<Vec<_>>();
    let base = s.base.clone().unwrap();
    let since = crate::gitlog::files(&d, &base, &s.hashes[1]);
    assert_eq!(names(&since), [('A', "extra.txt".into(), Some((1, 0))), ('M', "notes.txt".into(), Some((1, 1))), ('M', "pic.png".into(), None)]);
    let every = crate::gitlog::files(&d, &s.hashes[0], &s.hashes[1]);
    assert_eq!(names(&every).iter().map(|x| (x.0, x.1.as_str())).collect::<Vec<_>>(), [('A', "extra.txt"), ('M', "notes.txt"), ('M', "pic.png"), ('D', "readme.md")]);
    let parents = [base.clone()];
    let notes = since.iter().find(|c| c.path == "notes.txt").unwrap();
    assert_eq!(crate::gitlog::diff(&d, &s.hashes[1], &parents, notes).0, ["@@ -1,3 +1,3 @@", " one", "-two", "+TWO on topic", " three"]);
    let pic = since.iter().find(|c| c.path == "pic.png").unwrap();
    let side = |old| crate::gitlog::picture_side(&d, &s.hashes[1], &parents, pic, old, 64).map(|p| p.1);
    assert_eq!((side(true), side(false)), (Some([20, 10]), Some([30, 12])), "before and after");
    // Commits on one line: the older is the old side, whichever was marked first.
    let s = sides(&d, &rev("topic"), &rev("main~1"), true).unwrap();
    assert_eq!((s.hashes.clone(), s.counts, s.swapped, s.base.clone()), ([rev("main~1"), rev("topic")], [0, 3], true, Some(rev("main~1"))));
    let s = sides(&d, &rev("topic"), &rev("main~1"), false).unwrap();
    assert_eq!((s.counts, s.swapped), ([3, 0], false), "a branch compared as asked");
    assert_eq!(sides(&d, "main", "nope", false).err().as_deref(), Some("git doesn't know nope"));
    git_on(&d, 6, &["switch", "-q", "--orphan", "lone"]);
    touch(&d.join("lone.txt"), "alone\n");
    git_on(&d, 6, &["add", "lone.txt"]);
    git_on(&d, 6, &["commit", "-qm", "Alone"]);
    let s = sides(&d, "main", "lone", false).unwrap();
    assert_eq!((s.base.clone(), s.counts), (None, [2, 1]), "no history in common");
    assert_eq!((heading(0, "x"), heading(1, "main"), heading(1200, "topic")), ("Nothing only on x".to_string(), "1 commit only on main".to_string(), "1,200 commits only on topic".to_string()));
    let _ = std::fs::remove_dir_all(&d);
}

/// Merging a branch into the one HEAD is on: a fast-forward, nothing to merge, a merge
/// commit with git's own message, git's refusals in plain words (changes or untracked
/// files in the way, a name it doesn't know, no history in common, a merge still going),
/// conflicts given up on and then settled and finished, `merge.ff` honoured, and the
/// merge taken back by a reset to the commit it says it was at.
#[test]
fn git_merge_into_the_current_branch() {
    use crate::gitsync::{CONFLICTS, Doing, Side, doing};
    use crate::gitwork::{Op, run};
    if !crate::media::have("git") {
        return;
    }
    let d = tmpdir("gitmerge");
    work_repo(&d);
    git(&d, &["config", "merge.ff", "true"]);
    let commit = |day: u32, file: &str, text: &str, msg: &str| {
        touch(&d.join(file), text);
        git_on(&d, day, &["add", "-A"]);
        git_on(&d, day, &["commit", "-qm", msg]);
    };
    let rev = |r: &str| git_said(&d, &["rev-parse", r]);
    let short = |h: &str| h[..7].to_string();
    let merge = |b: &str| run(&d, &Op::Merge(b.into()));
    commit(1, "notes.txt", "one\ntwo\nthree\n", "Start");
    let start = rev("HEAD");
    git(&d, &["switch", "-qc", "topic"]);
    commit(2, "a.txt", "a\n", "Add a");
    git(&d, &["switch", "-q", "main"]);
    assert_eq!(merge("topic").unwrap(), format!("Fast-forwarded main to topic · it was at {}", short(&start)));
    assert_eq!(rev("HEAD"), rev("topic"));
    assert_eq!(merge("topic").unwrap(), "main already has everything on topic");
    // Both moved on: a merge commit, git's message, no editor.
    git(&d, &["switch", "-q", "topic"]);
    commit(3, "b.txt", "b\n", "Add b");
    git(&d, &["switch", "-q", "main"]);
    commit(3, "c.txt", "c\n", "Add c");
    let before = rev("HEAD");
    assert_eq!(merge("topic").unwrap(), format!("Merged topic into main · it was at {}", short(&before)));
    assert_eq!(git_said(&d, &["log", "-1", "--format=%P"]).split_whitespace().count(), 2);
    assert_eq!(git_said(&d, &["log", "-1", "--format=%s"]), "Merge branch 'topic'");
    assert!(d.join("b.txt").exists() && d.join("c.txt").exists());
    // Taken back: the reset the commit menu offers, to the commit it was at.
    run(&d, &Op::Reset { to: before.clone(), hard: true }).unwrap();
    assert_eq!((rev("HEAD"), d.join("b.txt").exists()), (before.clone(), false));
    // Refused: a change of its own in the way, then a file git doesn't track.
    git(&d, &["switch", "-q", "topic"]);
    commit(4, "notes.txt", "one\ntwo from topic\nthree\n", "Topic's two");
    commit(4, "new.txt", "new\n", "Topic's new file");
    git(&d, &["switch", "-q", "main"]);
    touch(&d.join("notes.txt"), "one\ntwo, not committed\nthree\n");
    assert_eq!(merge("topic").unwrap_err(), "Your uncommitted changes to notes.txt would be overwritten by the merge. Commit or stash them first.");
    assert_eq!((doing(&d), std::fs::read_to_string(d.join("notes.txt")).unwrap(), rev("HEAD")), (None, "one\ntwo, not committed\nthree\n".to_string(), before.clone()));
    git(&d, &["checkout", "--", "notes.txt"]);
    touch(&d.join("new.txt"), "mine\n");
    assert_eq!(merge("topic").unwrap_err(), "The merge would overwrite new.txt, which git doesn't track. Move them away or commit them first.");
    std::fs::remove_file(d.join("new.txt")).unwrap();
    assert_eq!(merge("nope").unwrap_err(), "git doesn't know nope.");
    // Conflicts: given up on, everything as before.
    commit(5, "notes.txt", "one\ntwo from main\nthree\n", "Main's two");
    let ours = rev("HEAD");
    assert_eq!(merge("topic").unwrap_err(), CONFLICTS);
    assert_eq!((doing(&d), work_of(&d).doing), (Some(Doing::Merge), Some(Doing::Merge)));
    assert!(Op::Merge("topic".into()).moved(&Err(CONFLICTS.into())), "the history reads its new state");
    assert_eq!(merge("topic").unwrap_err(), "Merging is still in progress: continue or abort it on the Changes page first.");
    assert_eq!(run(&d, &Op::Continue).unwrap_err(), "Some files are still in conflict.");
    assert_eq!(run(&d, &Op::Abort).unwrap(), "Stopped merging and went back");
    assert_eq!((doing(&d), rev("HEAD"), std::fs::read_to_string(d.join("notes.txt")).unwrap()), (None, ours.clone(), "one\ntwo from main\nthree\n".to_string()));
    // Again, settled with theirs and finished: the commit has git's message.
    assert_eq!(merge("topic").unwrap_err(), CONFLICTS);
    assert!(crate::gitsync::conflicts(&std::fs::read_to_string(d.join("notes.txt")).unwrap()).len() == 1);
    run(&d, &Op::Settle { path: "notes.txt".into(), at: 0, side: Side::Theirs }).unwrap();
    assert_eq!(run(&d, &Op::Continue).unwrap(), "Finished the merge");
    assert_eq!((doing(&d), git_said(&d, &["log", "-1", "--format=%s"])), (None, "Merge branch 'topic'".to_string()));
    assert_eq!(std::fs::read_to_string(d.join("notes.txt")).unwrap(), "one\ntwo from topic\nthree\n");
    assert!(d.join("new.txt").exists());
    // git's own setting asks for a merge commit even when a fast-forward would do.
    git(&d, &["switch", "-qc", "later"]);
    commit(6, "d.txt", "d\n", "Add d");
    git(&d, &["switch", "-q", "main"]);
    git(&d, &["config", "merge.ff", "false"]);
    let at = rev("HEAD");
    assert_eq!(merge("later").unwrap(), format!("Merged later into main · it was at {}", short(&at)));
    assert_eq!(git_said(&d, &["log", "-1", "--format=%P"]), format!("{at} {}", rev("later")));
    // Nothing in common.
    git(&d, &["switch", "-q", "--orphan", "lone"]);
    commit(7, "lone.txt", "alone\n", "Alone");
    git(&d, &["switch", "-q", "main"]);
    assert_eq!(merge("lone").unwrap_err(), "lone and main have no history in common, so git won't merge them.");
    let _ = std::fs::remove_dir_all(&d);
}

/// A remote branch merged in: fetched from a real bare repository, fast-forwarded to,
/// then merged with a commit once both sides have moved on.
#[test]
fn git_merge_a_remote_branch() {
    use crate::gitwork::{Op, run};
    if !crate::media::have("git") {
        return;
    }
    let (d, a, b) = sync_fixture("gitmergeremote");
    git(&a, &["switch", "-qc", "topic"]);
    touch(&a.join("t.txt"), "topic\n");
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-qm", "Topic work"]);
    git(&a, &["push", "-q", "-u", "origin", "topic"]);
    git(&b, &["config", "merge.ff", "true"]);
    run(&b, &Op::Fetch).unwrap();
    let was = git_said(&b, &["rev-parse", "--short=7", "HEAD"]);
    assert_eq!(run(&b, &Op::Merge("origin/topic".into())).unwrap(), format!("Fast-forwarded main to origin/topic · it was at {was}"));
    assert!(b.join("t.txt").exists());
    touch(&a.join("t2.txt"), "more\n");
    git(&a, &["add", "-A"]);
    git(&a, &["commit", "-qm", "More topic work"]);
    git(&a, &["push", "-q"]);
    touch(&b.join("b.txt"), "b\n");
    git(&b, &["add", "-A"]);
    git(&b, &["commit", "-qm", "B's own"]);
    run(&b, &Op::Fetch).unwrap();
    let was = git_said(&b, &["rev-parse", "--short=7", "HEAD"]);
    assert_eq!(run(&b, &Op::Merge("origin/topic".into())).unwrap(), format!("Merged origin/topic into main · it was at {was}"));
    assert_eq!(git_said(&b, &["log", "-1", "--format=%s"]), "Merge remote-tracking branch 'origin/topic'");
    let _ = std::fs::remove_dir_all(&d);
}

/// Let a Columns tab and its strip's listings land, sorting as the view does.
fn strip_settle(t: &mut Tab, hidden: bool) {
    let (stats, recents) = (HashMap::new(), HashSet::new());
    let c = ViewCtx { show_hidden: hidden, show_system: false, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
    let start = Instant::now();
    loop {
        t.poll();
        if t.dirty {
            t.rebuild(&c);
        }
        crate::columns::sync(t, &c);
        let busy = t.is_loading() || t.strip.as_ref().is_some_and(|s| s.cols.iter().any(|c| c.tab.is_loading()) || s.preview.as_ref().is_some_and(|p| s.get(p).is_none()));
        if !busy {
            crate::columns::sync(t, &c);
            return;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "the strip's listings never landed");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn col_names(t: &Tab, dir: &Path) -> Vec<String> {
    let st = t.strip.as_ref().expect("a strip");
    let col = &st.get(dir).unwrap_or_else(|| panic!("no column for {}", dir.display())).tab;
    col.view.iter().map(|&i| col.entries[i as usize].name.clone()).collect()
}

#[test]
fn columns_start_at_home_or_the_root() {
    let home = crate::fsx::home();
    assert_eq!(crate::columns::root_for(&home.join("Documents/Taxes")), home);
    assert_eq!(crate::columns::root_for(&home), home);
    if !Path::new("/proc").starts_with(&home) {
        assert_eq!(crate::columns::root_for(Path::new("/proc/self")), PathBuf::from("/"));
    }
    let chain = crate::columns::chain(Path::new("/h"), Path::new("/h/a/b"));
    assert_eq!(chain, vec![PathBuf::from("/h"), PathBuf::from("/h/a"), PathBuf::from("/h/a/b")]);
    // Inside an archive the folders go on past the archive's name.
    let chain = crate::columns::chain(Path::new("/h"), Path::new("/h/x.zip/docs"));
    assert_eq!(chain.last(), Some(&PathBuf::from("/h/x.zip/docs")));
    assert_eq!(chain.len(), 3);
}

#[test]
fn columns_strip_lists_the_way_down() {
    let d = tmpdir("columns");
    touch(&d.join("a/b/c/deep.txt"), "x");
    touch(&d.join("a/b/note.txt"), "x");
    touch(&d.join("a/zz.txt"), "x");
    touch(&d.join("a/y/keep.txt"), "x");
    touch(&d.join("a/.hid/inner/x.txt"), "x");
    let v = ViewSettings { column_view: true, ..Default::default() };
    let mut t = Tab::new(1, d.join("a/b"), v, egui::Context::default());
    strip_settle(&mut t, false);
    let st = t.strip.as_ref().unwrap();
    assert!(st.chain.ends_with(&[d.clone(), d.join("a")]), "the folders above, the root first: {:?}", st.chain);
    // The column above lists its folder like the tab would, the way down marked.
    assert_eq!(col_names(&t, &d.join("a")), vec!["b", "y", "zz.txt"], "hidden left out, folders first, by name");
    let a = &st.get(&d.join("a")).unwrap().tab;
    assert_eq!(st.get(&d.join("a")).unwrap().child.map(|i| a.entries[i as usize].name.as_str()), Some("b"));
    // The cursor rests on c: its listing is the next column.
    assert_eq!(t.cursor_path(), Some(d.join("a/b/c")));
    assert_eq!(st.preview, Some(d.join("a/b/c")));
    assert_eq!(col_names(&t, &d.join("a/b/c")), vec!["deep.txt"]);
    // A file under the cursor has nothing after it, nor has a selection of several.
    t.move_cursor(1, false, false);
    strip_settle(&mut t, false);
    assert_eq!(t.cursor_path(), Some(d.join("a/b/note.txt")));
    assert_eq!(t.strip.as_ref().unwrap().preview, None);
    t.move_cursor(-1, true, false);
    strip_settle(&mut t, false);
    assert_eq!(t.selected.len(), 2);
    assert_eq!(t.strip.as_ref().unwrap().preview, None, "two picked: no next column");
    // The columns follow the tab's sort, never its grouping.
    t.settings.desc = true;
    t.settings.group = crate::config::GroupBy::Type;
    strip_settle(&mut t, false);
    assert_eq!(col_names(&t, &d.join("a")), vec!["y", "b", "zz.txt"], "descending, folders still first");
    assert!(t.strip.as_ref().unwrap().cols.iter().all(|c| c.tab.settings.group == crate::config::GroupBy::None));
    t.settings.desc = false;
    // Going up, the folder left keeps its column filled and the one above shows at once:
    // the tab has its listing before its own read lands.
    t.up(|_| None);
    assert!(t.entries.is_empty(), "a fresh read is under way");
    let (stats, recents) = (HashMap::new(), HashSet::new());
    let c = ViewCtx { show_hidden: false, show_system: false, folders_first: true, recents_on_top: false, recents: &recents, pinned: &Default::default(), stats: &stats };
    crate::columns::sync(&mut t, &c);
    assert_eq!(t.path, d.join("a"));
    assert!(!t.view.is_empty(), "the column's listing stands in");
    assert_eq!(t.cursor_path(), Some(d.join("a/b")), "on the folder come from");
    assert_eq!(t.strip.as_ref().unwrap().preview, Some(d.join("a/b")));
    assert!(col_names(&t, &d.join("a/b")).contains(&"note.txt".to_string()), "the folder left is still listed beside");
    strip_settle(&mut t, false);
    // A hidden folder on the way down shows in its column even with hidden files off.
    t.navigate(d.join("a/.hid/inner"), None);
    strip_settle(&mut t, false);
    let names = col_names(&t, &d.join("a"));
    assert!(names.contains(&".hid".to_string()) && !names.contains(&".hidden".to_string()), "{names:?}");
    assert_eq!(names.iter().position(|n| n == ".hid"), Some(0), "among the folders by name");
    // A change Maia made reads the column again.
    std::fs::write(d.join("a/new.txt"), "x").unwrap();
    t.changed(&crate::ops::Changed { arrived: vec![d.join("a")], left: vec![], gone: vec![] });
    strip_settle(&mut t, false);
    assert!(col_names(&t, &d.join("a")).contains(&"new.txt".to_string()), "the column above caught the new file");
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn columns_stay_through_a_saved_view() {
    let d = tmpdir("colsaved");
    for n in ["a", "b"] {
        std::fs::create_dir_all(d.join(n)).unwrap();
    }
    let ctx = egui::Context::default();
    let mut t = Tab::new(1, d.clone(), ViewSettings { column_view: true, ..Default::default() }, ctx.clone());
    let saved = ViewSettings { sort: SortKey::Modified, zoom: 0.65, ..Default::default() };
    t.navigate(d.join("a"), Some((saved, Some(crate::config::SaveScope::ThisFolder))));
    assert!(t.settings.column_view, "walked into, a folder's saved view keeps the columns");
    assert_eq!(t.settings.sort, SortKey::Modified, "and lends them its sort");
    t.navigate(d.join("b"), None);
    assert!(t.settings.column_view);
    assert_eq!(t.settings.sort, SortKey::Name, "the view of before, back");
    // Columns saved for a folder: shown there, and the view of before after it.
    let mut t = Tab::new(2, d.clone(), ViewSettings::default(), ctx);
    t.navigate(d.join("a"), Some((ViewSettings { column_view: true, ..Default::default() }, Some(crate::config::SaveScope::ThisFolder))));
    assert!(t.settings.column_view);
    assert!(!t.tree_active(), "no tree under the columns");
    t.navigate(d.join("b"), None);
    assert!(!t.settings.column_view, "out of the folder, Details again");
    // Settings written before Columns existed read as not Columns.
    let old: ViewSettings = serde_json::from_str(r#"{"zoom":0.1,"sort":"Name"}"#).unwrap();
    assert!(!old.column_view && crate::listing::mode_of(&old) == crate::listing::Mode::Tree);
    let new: ViewSettings = serde_json::from_str(r#"{"zoom":0.1,"column_view":true}"#).unwrap();
    assert_eq!(crate::listing::mode_of(&new), crate::listing::Mode::Columns);
    assert_eq!(crate::listing::view_label(&new), "Columns");
    // Its key, the next free one after the six views on the zoom axis.
    let km = Keymap::build(&Default::default());
    assert_eq!(km.lookup(&commands::parse_binding("Ctrl+Shift+7").unwrap()).0, Some(Cmd::ViewColumns));
    let _ = std::fs::remove_dir_all(&d);
}

/// One frame with these input events (keys, typed text).
fn app_input(app: &mut crate::app::App, ctx: &egui::Context, events: Vec<egui::Event>) {
    let raw = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))), events, focused: true, ..Default::default() };
    ctx.run_pass(raw, |c| app.frame(c)).drop_without_applying_deltas();
}

fn press(key: egui::Key) -> egui::Event {
    egui::Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE }
}

/// The Go To picker of Copy To… / Move To…, and what it holds.
fn dest_picker(app: &crate::app::App) -> Option<&crate::popups::GoToState> {
    match &app.popup {
        crate::popups::Popup::GoTo(st) if st.dest.is_some() => Some(st),
        _ => None,
    }
}

/// The cursor on `name` and nothing else selected, once the folder is listed.
fn pick_entry(app: &mut crate::app::App, ctx: &egui::Context, name: &str) {
    app_until(app, ctx, "the listing", |a| a.panels[&a.active].tab().entries.iter().any(|e| e.name == name));
    let t = app.tab();
    let i = t.entries.iter().position(|e| e.name == name).unwrap() as u32;
    t.select_only(i);
}

/// Copy To… and Move To… through the picker as it is typed into: a path typed down to
/// its folder lands there, the folder is remembered and comes first next time, Enter
/// on it moves, and Undo takes each back. Real files, real jobs.
#[test]
fn copy_to_and_move_to_through_the_picker() {
    let ctx = egui::Context::default();
    let d = tmpdir("copyto");
    touch(&d.join("a.txt"), "a");
    touch(&d.join("b.txt"), "b");
    touch(&d.join("box/old.txt"), "old");
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    app.ud.destinations.clear();
    pick_entry(&mut app, &ctx, "a.txt");
    app.run(Cmd::CopyTo);
    app_frame(&mut app, &ctx);
    let st = dest_picker(&app).expect("Go To opens to pick the destination");
    assert_eq!(st.dest.as_ref().unwrap().title(), "Copy “a.txt” to…");
    assert!(st.items.iter().all(|i| i.header || i.is_dir), "only folders are offered");
    assert!(!st.items.iter().any(|i| i.header && i.label == "Recent destinations"), "nothing sent anywhere yet");
    // Typed down to the folder: the folder itself is the first row.
    app_input(&mut app, &ctx, vec![egui::Event::Text(format!("{}/box/", d.display()))]);
    app_until(&mut app, &ctx, "the folder read", |a| dest_picker(a).is_some_and(|st| st.items.first().is_some_and(|i| i.path == d.join("box"))));
    app_input(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    assert!(dest_picker(&app).is_none(), "Enter starts it and closes the picker");
    app_until(&mut app, &ctx, "the copy", |a| d.join("box/a.txt").exists() && a.jobs.iter().all(|j| j.finished_at.is_some()) && !a.undo.is_empty());
    assert!(d.join("a.txt").exists(), "a copy leaves the original");
    assert_eq!(app.ud.destinations, vec![d.join("box")], "remembered");
    // Move To…: the folder used last heads the list, already highlighted.
    pick_entry(&mut app, &ctx, "b.txt");
    app.run(Cmd::MoveTo);
    app_frame(&mut app, &ctx);
    app_frame(&mut app, &ctx);
    let st = dest_picker(&app).unwrap();
    assert_eq!(st.dest.as_ref().unwrap().title(), "Move “b.txt” to…");
    assert!(st.items[0].header && st.items[0].label == "Recent destinations", "recent destinations first");
    assert_eq!(st.items[1].path, d.join("box"));
    assert_eq!(st.sel, 1, "the newest one highlighted");
    app_input(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    app_until(&mut app, &ctx, "the move", |a| d.join("box/b.txt").exists() && !d.join("b.txt").exists() && a.jobs.iter().all(|j| j.finished_at.is_some()));
    assert!(app.undo_last());
    app_until(&mut app, &ctx, "the move undone", |_| d.join("b.txt").exists() && !d.join("box/b.txt").exists());
    // Esc leaves everything as it was.
    pick_entry(&mut app, &ctx, "b.txt");
    app.run(Cmd::MoveTo);
    app_frame(&mut app, &ctx);
    app_input(&mut app, &ctx, vec![press(egui::Key::Escape)]);
    assert!(dest_picker(&app).is_none());
    for _ in 0..5 {
        app_frame(&mut app, &ctx);
    }
    assert!(d.join("b.txt").exists() && !d.join("box/b.txt").exists(), "cancelled");
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&d);
}

/// What the picker refuses, each with its reason, and the picker stays open to choose
/// again: a folder into itself or below it, the trash, inside an archive, a folder that
/// isn't there, a move to where things already are. A link to a folder may go into
/// the folder it points at; a copy beside the original is a duplicate.
#[test]
fn copy_to_refuses_what_makes_no_sense() {
    use crate::copyto::refusal;
    let d = tmpdir("copytoref");
    touch(&d.join("box/inner/x.txt"), "x");
    touch(&d.join("f.txt"), "f");
    touch(&d.join("bundle.zip"), "PK");
    std::os::unix::fs::symlink(d.join("box"), d.join("box-link")).unwrap();
    let boxed = vec![d.join("box")];
    assert_eq!(refusal(&boxed, &d.join("box"), false).as_deref(), Some("A folder can't go inside itself"));
    assert_eq!(refusal(&boxed, &d.join("box/inner"), true).as_deref(), Some("A folder can't go inside itself"));
    assert_eq!(refusal(&boxed, &d.join("box-link/inner"), false).as_deref(), Some("A folder can't go inside itself"), "through a link too");
    assert_eq!(refusal(&[d.join("box-link")], &d.join("box"), true), None, "the link moves, not the folder");
    assert!(refusal(&boxed, &d.join("bundle.zip/docs"), false).is_some_and(|w| w.starts_with("Archives are read-only")));
    assert!(refusal(&boxed, &d.join("nowhere"), false).is_some_and(|w| w.starts_with("No folder at ")));
    assert!(refusal(&boxed, &d.join("f.txt"), false).is_some_and(|w| w.starts_with("No folder at ")), "a file is no folder");
    let trash = crate::sidebar::trash_dir();
    assert!(refusal(&[d.join("f.txt")], &trash, true).is_some_and(|w| w.contains("Move To Trash")));
    assert!(refusal(&[d.join("f.txt")], trash.parent().unwrap(), false).is_some());
    assert!(crate::copyto::in_trash(Path::new("/run/media/stick/.Trash-1000/files")));
    assert!(!crate::copyto::in_trash(&d));
    assert!(refusal(&[d.join("f.txt")], Path::new(&format!("{}:computer", "maia")), false).is_some());
    assert_eq!(refusal(&[d.join("f.txt")], &d, true).as_deref(), Some("Already in that folder"));
    assert_eq!(refusal(&[d.join("f.txt"), d.join("box/inner/x.txt")], &d, true), None, "one of them still has to move");
    assert_eq!(refusal(&[d.join("f.txt")], &d, false), None, "a copy beside it is a duplicate");
    assert_eq!(refusal(&[d.join("f.txt")], &d.join("box"), false), None);
    // In the picker: refused, said, still open, nothing copied.
    let ctx = egui::Context::default();
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    pick_entry(&mut app, &ctx, "box");
    app.run(Cmd::CopyTo);
    app_frame(&mut app, &ctx);
    app_input(&mut app, &ctx, vec![egui::Event::Text(format!("{}/box/inner/", d.display()))]);
    app_until(&mut app, &ctx, "the folder read", |a| dest_picker(a).is_some_and(|st| st.items.first().is_some_and(|i| i.path == d.join("box/inner"))));
    app_input(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    let st = dest_picker(&app).expect("the picker stays to choose again");
    assert_eq!(st.dest.as_ref().unwrap().refused.as_deref(), Some("A folder can't go inside itself"));
    // Typing again takes the reason away.
    app_input(&mut app, &ctx, vec![press(egui::Key::Backspace)]);
    app_frame(&mut app, &ctx);
    assert_eq!(dest_picker(&app).unwrap().dest.as_ref().unwrap().refused, None);
    app_input(&mut app, &ctx, vec![press(egui::Key::Escape)]);
    assert!(!d.join("box/inner/box").exists());
    // Nothing to move inside an archive, and nothing picked without a selection.
    app.tab().selected.clear();
    app.tab().cursor = None;
    app.run(Cmd::CopyTo);
    assert!(dest_picker(&app).is_none() && app.toast.as_ref().is_some_and(|t| t.0 == "Select what to copy"));
    let _ = std::fs::remove_dir_all(&d);
}

/// The remembered destinations: newest first, each once, at most `KEEP`; written with
/// the user data and read back (an older file without them reads as none).
#[test]
fn copy_to_remembers_a_few_destinations() {
    let mut ud = crate::config::UserData::default();
    for i in 0..12 {
        crate::copyto::remember(&mut ud, Path::new(&format!("/d/{i}")));
    }
    crate::copyto::remember(&mut ud, Path::new("/d/5"));
    assert_eq!(ud.destinations.len(), crate::copyto::KEEP);
    assert_eq!(ud.destinations[0], PathBuf::from("/d/5"));
    assert_eq!(ud.destinations[1], PathBuf::from("/d/11"));
    assert_eq!(ud.destinations.iter().filter(|p| **p == Path::new("/d/5")).count(), 1);
    let json = serde_json::to_string(&ud).unwrap();
    let back: crate::config::UserData = serde_json::from_str(&json).unwrap();
    assert_eq!(back.destinations, ud.destinations);
    let old: crate::config::UserData = serde_json::from_str(r#"{"bookmarks":["/x"]}"#).unwrap();
    assert!(old.destinations.is_empty());
    // Their default keys, beside F5 and F6.
    let km = Keymap::build(&Default::default());
    assert_eq!(km.lookup(&commands::parse_binding("Shift+F5").unwrap()).0, Some(Cmd::CopyTo));
    assert_eq!(km.lookup(&commands::parse_binding("Shift+F6").unwrap()).0, Some(Cmd::MoveTo));
}

/// Select Matching… on a real folder: globs with `*`, `?`, classes and `{a,b}`, case
/// ignored, added to what is selected; only what the view shows (hidden files and the
/// filter respected); paths below the folder in a flattened listing.
#[test]
fn select_matching_adds_what_the_pattern_names() {
    use crate::selmatch::{Pattern, select};
    let d = tmpdir("selmatch");
    for f in ["IMG_0001.JPG", "img_0002.jpg", "IMG_0003.png", "holiday.jpeg", "notes.txt", "a.b.tar.gz", ".hidden.jpg", "raw/IMG_0004.jpg", "src/main.rs", "src/deep/lib.rs"] {
        touch(&d.join(f), "x");
    }
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &stats);
    let selected = |t: &Tab| {
        let mut v: Vec<String> = t.selected.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
        v.sort();
        v
    };
    let pat = |s: &str| Pattern::new(s).unwrap();
    assert_eq!(select(&mut t, &pat("*.jpg")), 2);
    assert_eq!(selected(&t), ["IMG_0001.JPG", "img_0002.jpg"], "case ignored, the hidden one not shown so not chosen");
    assert!(t.cursor.is_some_and(|c| t.selected.contains(&c)), "the cursor on a match");
    // Added to, not replaced.
    assert_eq!(select(&mut t, &pat("*.{png,jpeg}")), 2);
    assert_eq!(selected(&t), ["IMG_0001.JPG", "IMG_0003.png", "holiday.jpeg", "img_0002.jpg"]);
    t.selected.clear();
    assert_eq!(select(&mut t, &pat("img_000[13].*")), 2);
    assert_eq!(selected(&t), ["IMG_0001.JPG", "IMG_0003.png"]);
    t.selected.clear();
    assert_eq!(select(&mut t, &pat("  ?????.txt ")), 1, "spaces around are trimmed");
    assert_eq!(select(&mut t, &pat("*.tar.gz")), 1);
    assert_eq!(select(&mut t, &pat("raw")), 1, "a folder matches by its name");
    assert_eq!(select(&mut t, &pat("{a,b}")), 0, "a whole name or nothing");
    assert_eq!(select(&mut t, &pat("{notes}.txt")), 0, "a brace with no comma is a character");
    assert!(Pattern::new("   ").is_none());
    // The filter narrows what can be chosen.
    t.selected.clear();
    t.filter = "IMG".into();
    pump(&mut t, &stats);
    assert_eq!(select(&mut t, &pat("*")), 3);
    assert_eq!(selected(&t), ["IMG_0001.JPG", "IMG_0003.png", "img_0002.jpg"]);
    // Flattened: names anywhere below, or a path below the folder.
    t.filter.clear();
    t.selected.clear();
    t.set_flatten(true);
    pump(&mut t, &stats);
    assert_eq!(select(&mut t, &pat("img_*.jpg")), 3, "raw/IMG_0004.jpg too");
    t.selected.clear();
    assert_eq!(select(&mut t, &pat("src/*.rs")), 2, "`*` crosses folders as the picker's globs do");
    assert_eq!(selected(&t), ["lib.rs", "main.rs"]);
    t.selected.clear();
    assert_eq!(select(&mut t, &pat("src/main.rs")), 1);
    let _ = std::fs::remove_dir_all(&d);
}

/// Through the prompt: what it says, and the selection of the active tab.
#[test]
fn select_matching_prompt_says_how_many() {
    let ctx = egui::Context::default();
    let d = tmpdir("selprompt");
    for f in ["a.jpg", "b.JPG", "c.png"] {
        touch(&d.join(f), "x");
    }
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    pick_entry(&mut app, &ctx, "c.png");
    app.run(Cmd::SelectMatching);
    assert!(matches!(app.popup, crate::popups::Popup::Prompt { .. }));
    app_frame(&mut app, &ctx);
    app_input(&mut app, &ctx, vec![egui::Event::Text("*.jpg".into())]);
    app_input(&mut app, &ctx, vec![press(egui::Key::Enter)]);
    assert!(matches!(app.popup, crate::popups::Popup::None));
    assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("2 items match *.jpg"));
    let names: HashSet<String> = app.tab().target_entries().iter().map(|e| e.name.clone()).collect();
    assert_eq!(names, ["a.jpg", "b.JPG", "c.png"].map(String::from).into(), "added to the one selected");
    crate::selmatch::run(&mut app, "*.gif");
    assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some("Nothing here matches *.gif"));
    let _ = std::fs::remove_dir_all(&d);
}

/// Run one archive change on this thread, as its job would: Ok, or the error it ends with.
fn archive_edit(a: &Path, e: crate::archwrite::Edit) -> Result<(), String> {
    let (cancel, pause) = (std::sync::atomic::AtomicBool::new(false), std::sync::atomic::AtomicBool::new(false));
    let p = std::sync::Mutex::new(ops::Progress::default());
    let r = crate::archwrite::edit(a, &e, &p, &ops::Gate::new(&cancel, &pause), &egui::Context::default());
    if r.is_ok() {
        let g = p.lock().unwrap();
        assert!(g.total_bytes > 0 && g.done_bytes == g.total_bytes, "{}: {} of {} bytes counted", a.display(), g.done_bytes, g.total_bytes);
    }
    r
}

/// An archive's names and, for files, contents, as the real bsdtar (or 7z, for a locked
/// one) gives them back.
fn archive_contents(a: &Path) -> std::collections::BTreeMap<String, String> {
    let all = crate::archive::list(a).unwrap_or_else(|e| panic!("{}: {e}", a.display()));
    let x = a.with_file_name(format!("x-{}", a.file_name().unwrap().to_string_lossy()));
    let _ = std::fs::remove_dir_all(&x);
    crate::archive::extract(a, &[], &x).unwrap_or_else(|e| panic!("{}: {e}", a.display()));
    let m = all.iter().map(|i| (i.path.display().to_string(), if i.is_dir { "/".to_string() } else { std::fs::read_to_string(x.join(&i.path)).unwrap_or_else(|e| format!("unreadable: {e}")) })).collect();
    let _ = std::fs::remove_dir_all(&x);
    m
}

/// The versions of `a` an edit sent to the trash, gone from it again.
fn purge_trashed(a: &Path) {
    for it in crate::trashcan::list().into_iter().filter(|it| it.original == a) {
        let _ = crate::trashcan::purge(&it);
    }
}

/// Files added, members deleted and renamed, new folders: in zips (one written by bsdtar,
/// with data descriptors 7z can't rename in place, and one by 7z), 7z and every tarball
/// kind, each made by the real tools; with the 7z command and, on a thread that can't
/// see it, without (every change a rebuild). Each change leaves the old archive in the
/// trash, where Undo's swap finds it, and nothing of its work beside the archive.
#[test]
fn archives_change_in_place() {
    use crate::archwrite::Edit;
    if !crate::archive::available() {
        return;
    }
    let d = tmpdir("archedit");
    let src = d.join("src");
    touch(&src.join("notes.txt"), "the notes");
    touch(&src.join("docs/a.txt"), "first doc");
    touch(&src.join("Trip – é/b.txt"), "sand");
    let add = d.join("add");
    touch(&add.join("new.txt"), "brand new");
    touch(&add.join("more/c.txt"), "more inside");
    touch(&add.join("notes.txt"), "other notes");
    let have7 = crate::archive::seven().is_ok();
    let mut kinds = vec!["zip", "7z", "tar", "tar.gz", "tgz", "tar.xz", "tar.zst", "tar.bz2", "cbz"];
    if have7 {
        kinds.push("7zip.zip");
    }
    for without7 in [false, true] {
        if without7 && !have7 {
            continue;
        }
        crate::archive::NO_SEVEN.with(|n| n.set(without7));
        for ext in &kinds {
            let a = d.join(format!("pack-{without7}.{ext}"));
            let names = ["notes.txt", "docs", "Trip – é"];
            let made = if *ext == "7zip.zip" {
                std::process::Command::new("7z").env("LC_ALL", "C.UTF-8").args(["a", "-bso0", "-bsp0", "-tzip"]).arg(&a).args(names).current_dir(&src).status().is_ok_and(|s| s.success())
            } else {
                let fmt = match *ext {
                    "zip" | "cbz" => "zip",
                    "7z" => "7zip",
                    _ => "pax",
                };
                let filter: &[&str] = match *ext {
                    "tar.gz" | "tgz" => &["-z"],
                    "tar.xz" => &["-J"],
                    "tar.zst" => &["--zstd"],
                    "tar.bz2" => &["-j"],
                    _ => &[],
                };
                std::process::Command::new("bsdtar").env("LC_ALL", "C.UTF-8").arg("-cf").arg(&a).args(["--format", fmt]).args(filter).arg("-C").arg(&src).args(names).status().is_ok_and(|s| s.success())
            };
            assert!(made, "{ext}: made by the real tool");
            let before = archive_contents(&a);
            // When each change was made, as its Undo entry keeps it.
            let made = std::cell::RefCell::new(vec![]);
            let step = |what: &str, e: Edit| {
                archive_edit(&a, e).unwrap_or_else(|e| panic!("{ext} ({}): {what}: {e}", if without7 { "no 7z" } else { "7z" }));
                made.borrow_mut().push(std::time::SystemTime::now());
                assert!(crate::picfix::newest_trashed(&a).is_some(), "{ext}: {what}: the old one in the trash");
                let left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.')).collect();
                assert!(left.is_empty(), "{ext}: {what}: left beside it: {left:?}");
                archive_contents(&a)
            };
            // Added into a folder inside, a folder with it; a name taken kept beside.
            let c = step("add", Edit::Add { srcs: vec![add.join("new.txt"), add.join("more"), add.join("notes.txt")], into: PathBuf::from("docs"), replace: false });
            assert_eq!(c.get("docs/new.txt").map(String::as_str), Some("brand new"), "{ext}: {c:?}");
            assert_eq!(c.get("docs/more/c.txt").map(String::as_str), Some("more inside"), "{ext}");
            assert_eq!(c.get("docs/notes.txt").map(String::as_str), Some("other notes"), "{ext}: not taken there");
            let after_add = c.clone();
            let c = step("add, keep both", Edit::Add { srcs: vec![add.join("notes.txt")], into: PathBuf::new(), replace: false });
            assert_eq!((c.get("notes.txt").map(String::as_str), c.get("notes (1).txt").map(String::as_str)), (Some("the notes"), Some("other notes")), "{ext}");
            let c = step("add, replace", Edit::Add { srcs: vec![add.join("notes.txt")], into: PathBuf::new(), replace: true });
            assert_eq!(c.get("notes.txt").map(String::as_str), Some("other notes"), "{ext}: replaced");
            // Deleted: a file, and a folder with what is in it.
            let c = step("delete", Edit::Delete { members: vec![PathBuf::from("docs/a.txt"), PathBuf::from("Trip – é")] });
            assert!(!c.contains_key("docs/a.txt") && !c.keys().any(|k| k.starts_with("Trip")), "{ext}: {c:?}");
            assert_eq!(c.get("docs/new.txt").map(String::as_str), Some("brand new"), "{ext}: the rest stays");
            // Renamed: a file, and a folder, which takes what is in it along.
            let c = step("rename", Edit::Rename { pairs: vec![(PathBuf::from("notes (1).txt"), "kept.txt".into()), (PathBuf::from("docs"), "papers".into())] });
            assert_eq!(c.get("kept.txt").map(String::as_str), Some("other notes"), "{ext}: {c:?}");
            assert_eq!(c.get("papers/more/c.txt").map(String::as_str), Some("more inside"), "{ext}: {c:?}");
            assert!(!c.keys().any(|k| k.starts_with("docs")), "{ext}: {c:?}");
            // A new folder and a new file.
            step("new folder", Edit::Make { at: PathBuf::from("papers/New folder"), folder: true });
            let c = step("new file", Edit::Make { at: PathBuf::from("papers/New folder/empty.txt"), folder: false });
            assert_eq!(c.get("papers/New folder").map(String::as_str), Some("/"), "{ext}: {c:?}");
            assert_eq!(c.get("papers/New folder/empty.txt").map(String::as_str), Some(""), "{ext}");
            // Refused before anything is done: a name taken, a member that isn't there.
            assert!(archive_edit(&a, Edit::Rename { pairs: vec![(PathBuf::from("kept.txt"), "notes.txt".into())] }).is_err_and(|e| e.contains("already")), "{ext}");
            assert!(archive_edit(&a, Edit::Delete { members: vec![PathBuf::from("nope.txt")] }).is_err(), "{ext}");
            // Undo, change by change, is the archive as it was made; Redo of the first
            // change is the archive after it.
            let mut undone = vec![];
            for t in made.borrow().iter().rev() {
                assert_eq!(crate::picfix::undo_replaced_by(&[a.clone()], Some(*t)), 0, "{ext}: undone");
                undone.push(std::time::SystemTime::now());
            }
            assert_eq!(archive_contents(&a), before, "{ext}: undone back to the start");
            assert_eq!(crate::picfix::undo_replaced_by(&[a.clone()], undone.last().copied()), 0, "{ext}: redone");
            assert_eq!(archive_contents(&a), after_add, "{ext}: the first change again");
            purge_trashed(&a);
        }
    }
    crate::archive::NO_SEVEN.with(|n| n.set(false));
    // What can only be read says so.
    for (n, why) in [("x.rar", "RAR archives can only be read here"), ("x.iso", "ISO archives"), ("x.deb", "DEB archives"), ("notes.txt.gz", "one compressed file"), ("x.tar.lz", "LZ archives")] {
        assert!(crate::archwrite::kind(Path::new(n)).is_err_and(|e| e.contains(why)), "{n}");
    }
    assert!(crate::archwrite::kind(Path::new("x.tar.zst")).is_ok() && crate::archwrite::kind(Path::new("X.ZIP")).is_ok());
    let _ = std::fs::remove_dir_all(&d);
}

/// Locked archives changed in place, made by the real tools (Info-ZIP's ZipCrypto, 7z's
/// AES zip, 7z with and without its names locked): asked for the password, a wrong one
/// refused with the archive untouched, and with the right one what is added is locked
/// the same way, as the real 7z reports it; names locked stay locked. Without the 7z
/// command the zips are rebuilt, locked again by bsdtar the way they were.
#[test]
fn locked_archives_change_and_stay_locked() {
    use crate::archive::{NEEDS_PASSWORD, WRONG_PASSWORD, forget_password, set_password};
    use crate::archwrite::Edit;
    if !crate::archive::available() {
        return;
    }
    let d = tmpdir("lockedit");
    let src = d.join("src");
    touch(&src.join("notes.txt"), "the secret plan");
    touch(&src.join("Trip photos/beach.txt"), "sand");
    touch(&d.join("add/late.txt"), "added later");
    let Some(made) = encrypted_archives(&d, &src, "hunter2") else {
        eprintln!("zip or 7z missing: skipped");
        return;
    };
    let slt = |a: &Path| String::from_utf8_lossy(&std::process::Command::new("7z").args(["l", "-slt", "-ba", "-phunter2"]).arg(a).output().unwrap().stdout).into_owned();
    let block = |t: &str, path: &str| t.split("\n\n").find(|b| b.lines().any(|l| l == format!("Path = {path}"))).unwrap_or("").to_string();
    for without7 in [false, true] {
        crate::archive::NO_SEVEN.with(|n| n.set(without7));
        for m in &made {
            let a = d.join(format!("{without7}-{}", m.file_name().unwrap().to_string_lossy()));
            std::fs::copy(m, &a).unwrap();
            let name = a.file_name().unwrap().to_string_lossy().into_owned();
            let add = Edit::Add { srcs: vec![d.join("add/late.txt")], into: PathBuf::new(), replace: false };
            if without7 && name.ends_with(".7z") {
                set_password(&a, "hunter2".into());
                let r = archive_edit(&a, add);
                assert!(r.as_ref().is_err_and(|e| e.ends_with("need 7z")), "{name}: {r:?}");
                continue;
            }
            let bytes = std::fs::read(&a).unwrap();
            forget_password(&a);
            assert_eq!(archive_edit(&a, add.clone()).err().as_deref(), Some(NEEDS_PASSWORD), "{name}");
            set_password(&a, "wrong".into());
            assert_eq!(archive_edit(&a, add.clone()).err().as_deref(), Some(WRONG_PASSWORD), "{name}");
            assert!(std::fs::read(&a).unwrap() == bytes && crate::picfix::newest_trashed(&a).is_none(), "{name}: untouched");
            set_password(&a, "hunter2".into());
            archive_edit(&a, add).unwrap_or_else(|e| panic!("{name}: {e}"));
            archive_edit(&a, Edit::Rename { pairs: vec![(PathBuf::from("Trip photos"), "Holiday".into())] }).unwrap_or_else(|e| panic!("{name}: {e}"));
            archive_edit(&a, Edit::Delete { members: vec![PathBuf::from("notes.txt")] }).unwrap_or_else(|e| panic!("{name}: {e}"));
            let c = archive_contents(&a);
            assert_eq!(c.get("late.txt").map(String::as_str), Some("added later"), "{name}: {c:?}");
            assert_eq!(c.get("Holiday/beach.txt").map(String::as_str), Some("sand"), "{name}: {c:?}");
            assert!(!c.contains_key("notes.txt"), "{name}");
            // Locked as it was: what was added too.
            let t = slt(&a);
            for f in ["late.txt", "Holiday/beach.txt"] {
                let b = block(&t, f);
                assert!(b.contains("Encrypted = +"), "{name}: {f} locked: {b}");
                if name.ends_with("aes.zip") {
                    assert!(b.contains("AES-256"), "{name}: {b}");
                }
                if name.ends_with("crypto.zip") {
                    assert!(b.contains("ZipCrypto"), "{name}: {b}");
                }
            }
            if name.contains("hidden") {
                let o = std::process::Command::new("7z").args(["l", "-ba", "-pnot-it"]).arg(&a).output().unwrap();
                assert!(!o.status.success(), "{name}: names still locked");
            }
            purge_trashed(&a);
            let _ = std::fs::remove_dir_all(crate::archive::open_cache(&a));
        }
    }
    crate::archive::NO_SEVEN.with(|n| n.set(false));
    let _ = std::fs::remove_dir_all(&d);
}

/// Compressing as a job, through the real bsdtar and 7z: every format at each level
/// (the smallest smaller than the fastest, on text that compresses), sources from two
/// folders in one archive, a locked one; and a long one watched as it goes: its bytes
/// count up between the ends, Pause holds them still (the tool is stopped), and Cancel
/// ends it with nothing left behind, not even an error.
#[test]
fn compress_jobs_report_progress_and_cancel() {
    use crate::archwrite::Level;
    use std::sync::atomic::Ordering;
    if !crate::archive::available() {
        return;
    }
    let ctx = egui::Context::default();
    let d = tmpdir("compressjob");
    // Text as text goes: words picked at random from a small vocabulary.
    let vocab = ["the", "archive", "keeps", "every", "file", "under", "one", "name", "and", "a", "smaller", "size", "when", "its", "words", "repeat", "often", "enough"];
    let mut x = 0x2545_F491u64;
    let words: String = (0..400_000)
        .map(|i| {
            x = x.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            format!("{}{}", vocab[(x >> 33) as usize % vocab.len()], if i % 13 == 12 { "\n" } else { " " })
        })
        .collect();
    touch(&d.join("a/log.txt"), &words);
    touch(&d.join("b/deep/other.txt"), "from another folder");
    let run = |srcs: Vec<PathBuf>, out: &Path, level: Level, pw: Option<&str>| {
        let j = ops::spawn(1, OpKind::Compress { srcs, out: out.to_path_buf(), password: pw.map(|p| crate::archive::Secret(p.into())), level }, ctx.clone());
        let start = Instant::now();
        while !j.progress.lock().unwrap().finished {
            assert!(start.elapsed() < Duration::from_secs(60), "compress timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
        let p = j.progress.lock().unwrap();
        assert!(p.errors.is_empty(), "{}: {:?}", out.display(), p.errors);
        assert!(p.total_bytes > 0 && p.done_bytes == p.total_bytes, "{}: {} of {}", out.display(), p.done_bytes, p.total_bytes);
    };
    for ext in ["zip", "tar.gz", "tar.xz", "7z"] {
        let (fast, small) = (d.join(format!("fast.{ext}")), d.join(format!("small.{ext}")));
        run(vec![d.join("a/log.txt")], &fast, Level::Fast, None);
        run(vec![d.join("a/log.txt")], &small, Level::Smallest, None);
        let (f, s) = (std::fs::metadata(&fast).unwrap().len(), std::fs::metadata(&small).unwrap().len());
        assert!(s < f, "{ext}: smallest {s} not below fast {f}");
        assert_eq!(archive_contents(&small).get("log.txt").map(String::as_str), Some(words.as_str()), "{ext}");
        // Two folders, one archive: each item at its top.
        let two = d.join(format!("two.{ext}"));
        run(vec![d.join("a/log.txt"), d.join("b/deep")], &two, Level::Normal, None);
        let c = archive_contents(&two);
        assert_eq!(c.get("deep/other.txt").map(String::as_str), Some("from another folder"), "{ext}: {:?}", c.keys().collect::<Vec<_>>());
        assert!(c.contains_key("log.txt"), "{ext}");
    }
    // Locked, from two folders: a 7z gathers them for the 7z command.
    if crate::archive::can_lock_7z() {
        let l = d.join("locked.7z");
        run(vec![d.join("a/log.txt"), d.join("b/deep")], &l, Level::Fast, Some("pa ss"));
        crate::archive::forget_password(&l);
        assert_eq!(crate::archive::list(&l).err().as_deref(), Some(crate::archive::NEEDS_PASSWORD));
        crate::archive::set_password(&l, "pa ss".into());
        assert_eq!(archive_contents(&l).get("deep/other.txt").map(String::as_str), Some("from another folder"));
    }
    assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with('.')), "no part or gathering folder left");
    // A long one: 48 MB that won't compress, at xz's smallest.
    let mut noise = vec![0u8; 48 << 20];
    let mut x = 0x2545_F491_4F6C_DD1Du64;
    for b in noise.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    std::fs::write(d.join("noise.bin"), &noise).unwrap();
    let out = d.join("noise.tar.xz");
    let j = ops::spawn(2, OpKind::Compress { srcs: vec![d.join("noise.bin")], out: out.clone(), password: None, level: Level::Smallest }, ctx.clone());
    let done = || j.progress.lock().unwrap().done_bytes;
    let start = Instant::now();
    while done() < 4 << 20 {
        assert!(start.elapsed() < Duration::from_secs(60) && !j.progress.lock().unwrap().finished, "no progress seen");
        std::thread::sleep(Duration::from_millis(20));
    }
    let total = j.progress.lock().unwrap().total_bytes;
    assert_eq!(total, 48 << 20);
    j.pause.store(true, Ordering::Relaxed);
    std::thread::sleep(Duration::from_millis(300));
    let held = done();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(done(), held, "paused: the tool is stopped");
    assert!(held > 0 && held < total, "between the ends: {held} of {total}");
    j.pause.store(false, Ordering::Relaxed);
    let start = Instant::now();
    while done() <= held {
        assert!(start.elapsed() < Duration::from_secs(30), "resumed");
        std::thread::sleep(Duration::from_millis(20));
    }
    j.cancel.store(true, Ordering::Relaxed);
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty(), "a cancel is not an error");
    assert!(!out.exists(), "nothing made");
    assert!(std::fs::read_dir(&d).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with('.')), "no part file left");
    let _ = std::fs::remove_dir_all(&d);
}

/// A lone compressed file (gzip, bzip2, xz, zstd, each written by bsdtar's raw format) is
/// an archive of the one file its name says, sized and read whole; a .deb (an ar
/// archive, as bsdtar writes one) and an .rpm (an empty signature and header over a cpio
/// payload, the layout libarchive reads) list and extract as archives. Extract Here on
/// the .gz gives the file, through the job.
#[test]
fn lone_compressed_files_and_packages_open_as_archives() {
    use crate::archive::{entries, extract, extract_for_open, is_archive, read_member, single, stem};
    if !crate::archive::available() {
        return;
    }
    let d = tmpdir("lonepack");
    let text: String = (0..5000).map(|i| format!("{i} – é\n")).collect();
    touch(&d.join("src/server.log"), &text);
    let bsdtar = |args: &[&str]| assert!(std::process::Command::new("bsdtar").env("LC_ALL", "C.UTF-8").args(args).current_dir(&d).status().unwrap().success(), "{args:?}");
    for (ext, filter) in [("gz", "-z"), ("bz2", "-j"), ("xz", "-J"), ("zst", "--zstd")] {
        let a = d.join(format!("server.log.{ext}"));
        bsdtar(&["-cf", a.to_str().unwrap(), "--format", "raw", filter, "-C", "src", "server.log"]);
        assert!(is_archive(&format!("server.log.{ext}")) && single(&a) && stem(&format!("server.log.{ext}")) == "server.log", "{ext}");
        let e = entries(&a, Path::new("")).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert_eq!(e.len(), 1, "{ext}");
        assert_eq!((e[0].name.as_str(), e[0].size, e[0].is_dir), ("server.log", text.len() as u64, false), "{ext}");
        assert_eq!(e[0].path, a.join("server.log"));
        assert_eq!(read_member(&a, Path::new("server.log"), 1 << 20).unwrap(), text.as_bytes(), "{ext}");
        let f = extract_for_open(&a, Path::new("server.log")).unwrap_or_else(|e| panic!("{ext}: {e}"));
        assert_eq!(std::fs::read_to_string(&f).unwrap(), text, "{ext}");
        let x = d.join(format!("x-{ext}"));
        extract(&a, &[], &x).unwrap();
        assert_eq!(std::fs::read_to_string(x.join("server.log")).unwrap(), text, "{ext}");
        let _ = std::fs::remove_dir_all(crate::archive::open_cache(&a));
    }
    // Not a tarball's: those stay tarballs.
    assert!(!single(Path::new("x.tar.gz")) && !single(Path::new("x.tar.zst")) && single(Path::new("x.json.zst")));
    // Extract Here on one: the file beside it, counted against the archive's size.
    let ctx = egui::Context::default();
    let out = d.join("out");
    std::fs::create_dir_all(&out).unwrap();
    let j = ops::spawn(1, OpKind::Extract { archive: d.join("server.log.xz"), members: vec![], dest: out.clone() }, ctx.clone());
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty(), "{:?}", j.progress.lock().unwrap().errors);
    assert_eq!(std::fs::read_to_string(out.join("server.log")).unwrap(), text);
    assert!(j.progress.lock().unwrap().done_bytes == std::fs::metadata(d.join("server.log.xz")).unwrap().len());
    // A .deb: debian-binary, control and data, as dpkg lays them out.
    touch(&d.join("pkg/data/usr/share/doc/hello/README"), "hello from the package");
    touch(&d.join("pkg/control/control"), "Package: hello\nVersion: 1.0\n");
    touch(&d.join("pkg/deb/debian-binary"), "2.0\n");
    bsdtar(&["-czf", "pkg/deb/control.tar.gz", "-C", "pkg/control", "control"]);
    bsdtar(&["-cJf", "pkg/deb/data.tar.xz", "-C", "pkg/data", "usr"]);
    bsdtar(&["-cf", "hello_1.0_all.deb", "--format", "ar", "-C", "pkg/deb", "debian-binary", "control.tar.gz", "data.tar.xz"]);
    let deb = d.join("hello_1.0_all.deb");
    assert!(is_archive("hello_1.0_all.deb"));
    let mut names: Vec<String> = entries(&deb, Path::new("")).unwrap().iter().map(|e| e.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["control.tar.gz", "data.tar.xz", "debian-binary"]);
    let f = extract_for_open(&deb, Path::new("debian-binary")).unwrap();
    assert_eq!(std::fs::read_to_string(f).unwrap(), "2.0\n");
    // An .rpm: the lead, two empty headers, a gzipped cpio of the files.
    bsdtar(&["-czf", "payload.cpio.gz", "--format", "newc", "-C", "pkg/data", "./usr"]);
    let mut rpm = vec![0xed, 0xab, 0xee, 0xdb, 3, 0, 0, 0, 0, 1];
    rpm.extend(b"hello-1.0-1".iter().copied().chain(std::iter::repeat(0)).take(66));
    rpm.extend([0, 1, 0, 5]);
    rpm.extend([0u8; 16]);
    for _ in 0..2 {
        rpm.extend([0x8e, 0xad, 0xe8, 0x01, 0, 0, 0, 0]);
        rpm.extend([0u8; 8]);
    }
    rpm.extend(std::fs::read(d.join("payload.cpio.gz")).unwrap());
    let r = d.join("hello-1.0-1.noarch.rpm");
    std::fs::write(&r, rpm).unwrap();
    assert!(is_archive("hello-1.0-1.noarch.rpm"));
    let doc = entries(&r, Path::new("usr/share/doc/hello")).unwrap_or_else(|e| panic!("rpm: {e}"));
    assert_eq!(doc.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(), ["README"]);
    let x = d.join("x-rpm");
    extract(&r, &[PathBuf::from("usr/share/doc/hello/README")], &x).unwrap();
    assert_eq!(std::fs::read_to_string(x.join("usr/share/doc/hello/README")).unwrap(), "hello from the package");
    for c in [&deb, &r] {
        let _ = std::fs::remove_dir_all(crate::archive::open_cache(c));
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// An archive change cancelled halfway (a big tarball being rebuilt) leaves the archive
/// as it was and nothing beside it; refused kinds and a missing member change nothing.
#[test]
fn archive_change_cancelled_leaves_it_alone() {
    use crate::archwrite::Edit;
    use std::sync::atomic::Ordering;
    if !crate::archive::available() {
        return;
    }
    let d = tmpdir("editcancel");
    let mut noise = vec![0u8; 40 << 20];
    let mut x = 0x9E37_79B9_7F4A_7C15u64;
    for b in noise.iter_mut() {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = x as u8;
    }
    std::fs::create_dir_all(d.join("src")).unwrap();
    std::fs::write(d.join("src/noise.bin"), &noise).unwrap();
    touch(&d.join("src/small.txt"), "small");
    let a = d.join("big.tar.xz");
    assert!(std::process::Command::new("bsdtar").args(["-cJf"]).arg(&a).arg("-C").arg(d.join("src")).args(["noise.bin", "small.txt"]).status().unwrap().success());
    let bytes = std::fs::read(&a).unwrap();
    let j = ops::spawn(1, OpKind::ArchiveEdit { archive: a.clone(), edit: Edit::Delete { members: vec![PathBuf::from("small.txt")] } }, egui::Context::default());
    let start = Instant::now();
    // Into the packing, past the extraction.
    while j.progress.lock().unwrap().done_bytes <= bytes.len() as u64 + (4 << 20) {
        assert!(start.elapsed() < Duration::from_secs(60) && !j.progress.lock().unwrap().finished, "no progress");
        std::thread::sleep(Duration::from_millis(20));
    }
    j.cancel.store(true, Ordering::Relaxed);
    wait_job(&j);
    let p = j.progress.lock().unwrap();
    assert!(p.errors.is_empty() && p.replaced.is_empty(), "{:?}", p.errors);
    assert!(std::fs::read(&a).unwrap() == bytes, "untouched");
    assert!(crate::picfix::newest_trashed(&a).is_none(), "nothing trashed");
    let left: Vec<String> = std::fs::read_dir(&d).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('.')).collect();
    assert!(left.is_empty(), "left beside it: {left:?}");
    drop(p);
    // A rar is only read; a member not there is not deleted.
    let rar = d.join("x.rar");
    std::fs::write(&rar, b"Rar!").unwrap();
    assert!(archive_edit(&rar, Edit::Delete { members: vec![PathBuf::from("a")] }).is_err_and(|e| e.contains("can only be read")));
    assert!(archive_edit(&a, Edit::Delete { members: vec![PathBuf::from("gone.txt")] }).is_err_and(|e| e.contains("isn't in it")));
    assert!(std::fs::read(&a).unwrap() == bytes);
    let _ = std::fs::remove_dir_all(&d);
}

/// Where SELinux runs, a copy is labelled for where it lands, as with `cp`, and a move to
/// another disk keeps the label it had, as with `mv`. Without SELinux, or without the
/// right to relabel, there is nothing to check.
#[test]
fn copies_take_their_new_places_label_and_moves_keep_theirs() {
    let cpath = |p: &Path| std::ffi::CString::new(p.as_os_str().as_encoded_bytes()).unwrap();
    let get = |p: &Path| -> Option<String> {
        let mut b = vec![0u8; 512];
        let n = unsafe { libc::lgetxattr(cpath(p).as_ptr(), c"security.selinux".as_ptr(), b.as_mut_ptr().cast(), b.len()) };
        (n > 0).then(|| String::from_utf8_lossy(&b[..n as usize]).trim_end_matches('\0').to_string())
    };
    let put = |p: &Path, v: &str| unsafe { libc::lsetxattr(cpath(p).as_ptr(), c"security.selinux".as_ptr(), v.as_ptr().cast(), v.len(), 0) } == 0;
    let ctx = egui::Context::default();
    let d = tmpdir("labels");
    let key = d.join("from/id_ed25519.pub");
    touch(&key, "k");
    touch(&d.join("to/probe"), "p");
    let done = |d: &Path| {
        let _ = std::fs::remove_dir_all(d);
    };
    let Some(native) = get(&d.join("to/probe")).filter(|_| get(&key).is_some()) else { return done(&d) };
    // A type the folder wouldn't give it, as a file from ~/.ssh carries.
    let mut parts: Vec<&str> = native.split(':').collect();
    if parts.len() < 4 {
        return done(&d);
    }
    parts[2] = if parts[2] == "ssh_home_t" { "user_home_t" } else { "ssh_home_t" };
    let foreign = parts.join(":");
    if !put(&key, &foreign) {
        return done(&d);
    }
    let j = ops::spawn(1, OpKind::Copy { srcs: vec![key.clone()], dest: d.join("to"), decide: HashMap::new() }, ctx.clone());
    wait_job(&j);
    assert_eq!(get(&d.join("to/id_ed25519.pub")), Some(native), "the copy kept the label it came with");
    if let Some(far) = other_fs("labels") {
        let j = ops::spawn(2, OpKind::Move { srcs: vec![key.clone()], dest: far.clone(), decide: HashMap::new() }, ctx);
        wait_job(&j);
        if far.join("id_ed25519.pub").exists() {
            assert_eq!(get(&far.join("id_ed25519.pub")), Some(foreign), "the move was relabelled");
        }
        done(&far);
    }
    done(&d);
}

/// Merging into a folder replaces a same-named file by trashing the old one first. When
/// the check then finds the new copy wrong, the old text can still be had from the trash.
#[test]
fn merge_keeps_the_old_file_when_its_replacement_fails_the_check() {
    let ctx = egui::Context::default();
    let q = std::sync::Arc::new(ops::Queue::default());
    let d = tmpdir("merge-spoil");
    if reflinks(&d) {
        let _ = std::fs::remove_dir_all(&d);
        return; // a reflinked copy shares the old blocks, so nothing is spoiled to find
    }
    let ledger = format!("{} ledger.csv", ops::SPOILED);
    for (side, year) in [("src", "2026"), ("out", "2025")] {
        touch(&d.join(side).join("books").join(&ledger), &format!("{year} ledger"));
    }
    let decide = HashMap::from([(d.join("src/books"), Conflict::Merge)]);
    let j = ops::spawn_in(1, OpKind::Copy { srcs: vec![d.join("src/books")], dest: d.join("out"), decide }, ctx, &q, true);
    wait_job(&j);
    let errors = j.progress.lock().unwrap().errors.clone();
    assert!(errors.iter().any(|e| e.contains("read back")), "{errors:?}");
    let old = crate::picfix::newest_trashed(&d.join("out/books").join(&ledger)).expect("the old ledger is gone, not in the trash");
    assert_eq!(std::fs::read_to_string(&old.file).unwrap(), "2025 ledger");
    purge_trashed_under(&d);
    let _ = std::fs::remove_dir_all(&d);
}

/// A name an app hands the Save dialog keeps only its last plain part, so a sandboxed app
/// can't reach past the folder the user picked with "../" or a full path of its own.
#[test]
fn save_dialog_names_land_in_the_picked_folder() {
    use crate::picker::save_path;
    let dir = Path::new("/home/u/Pictures");
    assert_eq!(save_path(dir, "scan 1.png"), Some(dir.join("scan 1.png")));
    assert_eq!(save_path(dir, "../../.config/autostart/x.desktop"), Some(dir.join("x.desktop")));
    assert_eq!(save_path(dir, "/etc/cron.d/job"), Some(dir.join("job")));
    assert_eq!(save_path(dir, "shots/./"), Some(dir.join("shots")));
    for nothing in ["../..", "/", ""] {
        assert_eq!(save_path(dir, nothing), None, "{nothing:?}");
    }
}

/// Members whose names would point out of the archive once joined to its path, rooted
/// ones and ones going up, are left out of its listing, from bsdtar and from 7z alike.
#[test]
fn archive_listings_leave_out_names_that_point_outside() {
    let now = std::time::SystemTime::now();
    let row = |name: &str| format!("-rw-r--r--  0 0      0           3 Mar 12 09:15 {name}\n");
    let tar: String = ["/etc/cron.daily/x", "docs/../../up.txt", "docs/a.txt", "./b.txt"].map(row).concat();
    let names = |v: Vec<crate::archive::Item>| {
        let mut v: Vec<PathBuf> = v.into_iter().map(|i| i.path).collect();
        v.sort();
        v
    };
    assert_eq!(names(crate::archive::parse_list(&tar, now)), ["b.txt", "docs", "docs/a.txt"].map(PathBuf::from));
    let slt = "Path = /root/.ssh/authorized_keys\nSize = 3\n\nPath = a/../../b\nSize = 3\n\nPath = c.txt\nSize = 3\n";
    assert_eq!(names(crate::archive::parse_slt(slt)), [PathBuf::from("c.txt")]);
}

/// A .7z member whose name holds a wildcard character is renamed on its own: "a*.txt" is
/// that one name to 7z, not every member starting with "a".
#[test]
fn seven_zip_rename_takes_names_literally() {
    if !crate::media::have("7z") {
        return;
    }
    let d = tmpdir("7z-literal");
    touch(&d.join("in/a*.txt"), "star");
    touch(&d.join("in/ab.txt"), "ab");
    let a = d.join("x.7z");
    let made = std::process::Command::new("7z").args(["a", "-bso0", "-bsp0", "-spd"]).arg(&a).args(["a*.txt", "ab.txt"]).current_dir(d.join("in")).status().unwrap();
    assert!(made.success());
    archive_edit(&a, crate::archwrite::Edit::Rename { pairs: vec![(PathBuf::from("a*.txt"), "star.txt".into())] }).unwrap();
    let listed = std::process::Command::new("7z").args(["l", "-slt", "-ba"]).arg(&a).output().unwrap().stdout;
    let listed = String::from_utf8_lossy(&listed);
    let mut names: Vec<&str> = listed.lines().filter_map(|l| l.strip_prefix("Path = ")).collect();
    names.sort();
    assert_eq!(names, ["ab.txt", "star.txt"]);
    let _ = std::fs::remove_dir_all(&d);
}

/// Without bsdtar, Compress and Extract Here say what to install instead of doing nothing;
/// with it, Compress asks its questions as before.
#[test]
fn compress_and_extract_say_what_they_need() {
    let ctx = egui::Context::default();
    let d = tmpdir("needs-bsdtar");
    touch(&d.join("notes.txt"), "n");
    touch(&d.join("pack.zip"), "not really a zip");
    let mut app = crate::app::App::new(&ctx, vec![d.clone()]);
    app_until(&mut app, &ctx, "the listing", |a| a.panels.get(&a.active).is_some_and(|p| p.tabs[p.active].entries.len() == 2));
    let pick = |app: &mut crate::app::App, name: &str| {
        let t = app.tab();
        t.selected = t.view.iter().copied().filter(|&i| t.entries[i as usize].name == name).collect();
        t.cursor = t.selected.iter().next().copied();
    };
    crate::archive::GONE.with(|g| g.set(true));
    pick(&mut app, "notes.txt");
    app.run(Cmd::CompressZip);
    assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some(crate::archive::COMPRESS_NEEDS));
    assert!(matches!(app.popup, crate::popups::Popup::None), "no dialog that could only fail");
    pick(&mut app, "pack.zip");
    app.run(Cmd::ExtractHere);
    assert_eq!(app.toast.as_ref().map(|t| t.0.as_str()), Some(crate::archive::EXTRACT_NEEDS));
    assert!(app.actions.is_empty(), "no extract job started");
    crate::archive::GONE.with(|g| g.set(false));
    if crate::archive::available() {
        pick(&mut app, "notes.txt");
        app.run(Cmd::CompressZip);
        assert!(matches!(app.popup, crate::popups::Popup::Compress(_)), "with bsdtar, the dialog");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// A home folder with everything Free up looks for, under `d`: a Rust project's build, a
/// web project's packages, the thumbnail cache, pip's and npm's downloads, a trashed
/// project (whose node_modules is the trash's, not another project's), a folder this user
/// can't change, and a plain file.
fn free_up_home(d: &Path) -> crate::freeup::Places {
    let home = d.join("home");
    touch(&home.join("proj/Cargo.toml"), "[package]");
    touch(&home.join("proj/target/debug/maia"), &"x".repeat(40_000));
    touch(&home.join("web/package.json"), "{}");
    touch(&home.join("web/node_modules/left-pad/index.js"), &"y".repeat(20_000));
    touch(&home.join(".cache/thumbnails/normal/a.png"), &"t".repeat(8_000));
    touch(&home.join(".cache/pip/http/wheel"), &"p".repeat(12_000));
    touch(&home.join(".npm/_cacache/index"), &"n".repeat(6_000));
    let trash = home.join(".local/share/Trash");
    touch(&trash.join("files/old/package.json"), "{}");
    touch(&trash.join("files/old/node_modules/x.js"), &"o".repeat(3_000));
    touch(&trash.join("info/old.trashinfo"), "[Trash Info]\nPath=/home/x/old\nDeletionDate=2026-10-01T10:00:00\n");
    touch(&home.join("notes.txt"), "keep");
    // Outside the home folder: a disk's other folders hold no one's projects.
    touch(&d.join("srv/app/Cargo.toml"), "[package]");
    touch(&d.join("srv/app/target/x"), "x");
    crate::freeup::Places { cache: home.join(".cache"), trashes: vec![trash], home }
}

fn free_up_done(v: &mut crate::freeup::View) {
    let start = Instant::now();
    while !(v.done && v.counting() == 0) {
        assert!(start.elapsed() < Duration::from_secs(10), "the scan never finished");
        std::thread::sleep(Duration::from_millis(10));
        v.pump();
    }
}

/// What Free up finds and where: everything that comes back by itself inside the folder,
/// in a folder this user may change, grouped and sized; nothing inside the trash or the
/// caches found twice by the walk; the walk kept to the home folder when the folder holds
/// it, and to the folder itself otherwise.
#[test]
fn free_up_finds_what_comes_back() {
    use crate::freeup::{Group, find};
    let d = tmpdir("freeup-find");
    let places = free_up_home(&d);
    let home = places.home.clone();
    let stop = std::sync::atomic::AtomicBool::new(false);
    let run = |root: &Path| {
        let mut v = vec![];
        find(root, &places, &stop, &mut |f| v.push((f.group, f.path.strip_prefix(&home).map(Path::to_path_buf).unwrap_or(f.path.clone()), f.note)));
        v.sort_by(|a, b| a.1.cmp(&b.1));
        v
    };
    let got = run(&home);
    let want: Vec<(Group, PathBuf)> = vec![
        (Group::Caches, ".cache/pip".into()),
        (Group::Thumbs, ".cache/thumbnails".into()),
        (Group::Trash, ".local/share/Trash".into()),
        (Group::Caches, ".npm/_cacache".into()),
        (Group::Builds, "proj/target".into()),
        (Group::Packages, "web/node_modules".into()),
    ];
    assert_eq!(got.iter().map(|g| (g.0, g.1.clone())).collect::<Vec<_>>(), want);
    assert_eq!(got[2].2, "1 item");
    assert_eq!(got[4].2, "Rust build");
    // The disk the home folder is on, opened at its top: only the home folder is walked.
    assert_eq!(run(&d), got, "/srv's build is no one's project");
    // A project folder: its own build and nothing from elsewhere in the home folder.
    assert_eq!(run(&home.join("proj")).iter().map(|g| g.1.clone()).collect::<Vec<_>>(), [PathBuf::from("proj/target")]);
    // Elsewhere on the disk, that folder is what is walked.
    assert_eq!(run(&d.join("srv")).iter().map(|g| (g.0, g.1.clone())).collect::<Vec<_>>(), [(Group::Builds, d.join("srv/app/target"))]);
    // A folder this user may not change keeps its build output.
    if unsafe { libc::geteuid() } != 0 {
        touch(&home.join("shared/package.json"), "{}");
        touch(&home.join("shared/node_modules/a.js"), "a");
        std::fs::set_permissions(home.join("shared"), std::os::unix::fs::PermissionsExt::from_mode(0o555)).unwrap();
        assert!(!run(&home).iter().any(|g| g.1.starts_with("shared")), "not this user's to delete");
        std::fs::set_permissions(home.join("shared"), std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    }
    for (kind, group) in [("Node packages", Group::Packages), ("Python environment", Group::Packages), ("Python test environments", Group::Packages), ("Rust build", Group::Builds), ("Python bytecode", Group::Builds), ("Cache", Group::Builds), ("Gradle build", Group::Builds)] {
        assert_eq!(crate::freeup::group_of(kind), group, "{kind}");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Free up's list: what comes back by itself checked and the rest not, sizes added up, a
/// group checked whole or folder by folder, and the delete given the trash's entries with
/// their .trashinfo (never the trash folder). Run as the dialog runs it: what was not
/// chosen stays.
#[test]
fn free_up_deletes_what_was_chosen() {
    use crate::freeup::{Group, Row, View};
    let ctx = egui::Context::default();
    let d = tmpdir("freeup-view");
    let places = free_up_home(&d);
    let home = places.home.clone();
    let mut v = View::new_in(home.clone(), Some(places), d.join("record"), &ctx);
    free_up_done(&mut v);
    let groups: Vec<Group> = v.groups.iter().map(|g| g.group).collect();
    assert_eq!(groups, [Group::Builds, Group::Packages, Group::Caches, Group::Thumbs, Group::Trash], "in one order, whatever turned up first");
    let checked: Vec<bool> = v.groups.iter().map(|g| g.parts.iter().all(|p| p.checked)).collect();
    assert_eq!(checked, [true, false, true, true, false], "what comes back by itself is checked");
    let size = |p: &str| crate::stats::compute(&home.join(p), &std::sync::atomic::AtomicU64::new(0), 0).size;
    assert_eq!(v.chosen(), size("proj/target") + size(".cache/pip") + size(".npm/_cacache") + size(".cache/thumbnails"));
    assert!(v.ready());
    // The caches are two folders: listed under their group once it is opened.
    let caches = v.rows().iter().position(|r| *r == Row::Group(2)).unwrap();
    assert_eq!(v.rows().len(), 6, "five groups and Find Duplicates");
    v.open(Row::Group(2), true);
    assert_eq!(v.rows().len(), 8);
    assert_eq!(v.rows()[caches + 1], Row::Part(2, 0));
    // npm's alone left out; the trash put in.
    let npm = v.groups[2].parts.iter().position(|p| p.path.ends_with("_cacache")).unwrap();
    v.flip(Row::Part(2, npm));
    v.flip(Row::Group(4));
    let (paths, freed) = v.take_chosen();
    let mut rel: Vec<PathBuf> = paths.iter().map(|p| p.strip_prefix(&home).unwrap().to_path_buf()).collect();
    rel.sort();
    assert_eq!(rel, [".cache/pip", ".cache/thumbnails", ".local/share/Trash/files/old", ".local/share/Trash/info/old.trashinfo", "proj/target"].map(PathBuf::from));
    assert_eq!(freed, size("proj/target") + size(".cache/pip") + size(".cache/thumbnails") + size(".local/share/Trash"));
    assert_eq!(v.groups.iter().map(|g| g.group).collect::<Vec<_>>(), [Group::Packages, Group::Caches], "what went is out of the list");
    let j = ops::spawn(1, OpKind::Delete { paths }, ctx);
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty());
    for gone in ["proj/target", ".cache/pip", ".cache/thumbnails", ".local/share/Trash/files/old", ".local/share/Trash/info/old.trashinfo"] {
        assert!(!home.join(gone).exists(), "{gone} deleted");
    }
    for kept in ["proj/Cargo.toml", "web/node_modules/left-pad/index.js", ".npm/_cacache/index", "notes.txt", ".local/share/Trash"] {
        assert!(home.join(kept).exists(), "{kept} kept");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// Something trashed while Free up is open was never listed or counted, so emptying the
/// trash from Free up leaves it: only the entries the scan saw go, with their .trashinfo.
#[test]
fn free_up_leaves_what_was_trashed_after_its_scan() {
    use crate::freeup::{Group, View};
    let ctx = egui::Context::default();
    let d = tmpdir("freeup-late");
    let places = free_up_home(&d);
    let (home, trash) = (places.home.clone(), places.trashes[0].clone());
    let mut v = View::new_in(home.clone(), Some(places), d.join("record"), &ctx);
    free_up_done(&mut v);
    let t = v.groups.iter().position(|g| g.group == Group::Trash).unwrap();
    assert_eq!(v.groups[t].parts[0].note, "1 item");
    // Trashed after the scan: a big file, and one without its .trashinfo yet.
    touch(&trash.join("files/movie.mkv"), &"m".repeat(50_000));
    touch(&trash.join("info/movie.mkv.trashinfo"), "[Trash Info]\nPath=/home/x/movie.mkv\nDeletionDate=2026-10-27T10:00:00\n");
    touch(&trash.join("files/half"), "h");
    for g in &mut v.groups {
        for p in &mut g.parts {
            p.checked = g.group == Group::Trash;
        }
    }
    let (paths, freed) = v.take_chosen();
    assert_eq!(paths, [trash.join("files/old"), trash.join("info/old.trashinfo")]);
    assert!(freed < 50_000, "counted only what was there: {freed}");
    let j = ops::spawn(1, OpKind::Delete { paths }, ctx);
    wait_job(&j);
    assert!(j.progress.lock().unwrap().errors.is_empty());
    assert!(!trash.join("files/old").exists() && !trash.join("info/old.trashinfo").exists());
    for kept in ["files/movie.mkv", "info/movie.mkv.trashinfo", "files/half"] {
        assert!(trash.join(kept).exists(), "{kept} was deleted without being shown");
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The record of how full a disk is: a reading a day, the day's latest standing only when
/// it moved (a start that changes nothing writes nothing), a clock gone back ignored, each
/// disk its own, a year kept. And when it is drawn: three readings reaching a week back,
/// with the week's change from the reading a week ago or the first after it.
#[test]
fn disk_record_keeps_a_reading_a_day() {
    use crate::freeup::{charted, days_in, note_in, week_change};
    let d = tmpdir("freeup-record");
    let f = d.join("maia/space-history");
    let (root, data) = (Path::new("/"), Path::new("/data"));
    const GB: u64 = 1 << 30;
    note_in(&f, root, 10 * GB, 100 * GB, 100);
    assert_eq!(days_in(&f, root), [(100, 10 * GB, 100 * GB)]);
    let written = std::fs::metadata(&f).unwrap().modified().unwrap();
    std::thread::sleep(Duration::from_millis(20));
    note_in(&f, root, 10 * GB + (1 << 20), 100 * GB, 100);
    assert_eq!(std::fs::metadata(&f).unwrap().modified().unwrap(), written, "a megabyte's change writes nothing");
    assert_eq!(days_in(&f, root), [(100, 10 * GB, 100 * GB)]);
    note_in(&f, root, 11 * GB, 100 * GB, 100);
    assert_eq!(days_in(&f, root), [(100, 11 * GB, 100 * GB)], "the day's latest reading stands");
    note_in(&f, data, GB, 2000 * GB, 100);
    note_in(&f, root, 12 * GB, 100 * GB, 101);
    note_in(&f, root, 5 * GB, 100 * GB, 99);
    assert_eq!(days_in(&f, root), [(100, 11 * GB, 100 * GB), (101, 12 * GB, 100 * GB)], "a clock gone back changes nothing");
    assert_eq!(days_in(&f, data), [(100, GB, 2000 * GB)]);
    for day in 102..520 {
        note_in(&f, root, day as u64 * GB / 10, 100 * GB, day);
    }
    let kept = days_in(&f, root);
    assert_eq!(kept.len(), 400, "a year or so");
    assert_eq!((kept[0].0, kept[399].0), (120, 519));
    assert_eq!(days_in(&f, data).len(), 1, "another disk keeps its own");
    // Drawn from a week back.
    let r = |v: &[(i64, u64)]| v.iter().map(|&(d, u)| (d, u * GB, 100 * GB)).collect::<Vec<_>>();
    assert!(!charted(&r(&[(195, 10), (199, 11)]), 200), "two readings");
    assert!(!charted(&r(&[(195, 10), (197, 11), (200, 12)]), 200), "five days back");
    assert!(charted(&r(&[(194, 10), (197, 11), (200, 12)]), 200));
    assert_eq!(week_change(&r(&[(191, 10), (193, 12), (200, 15)]), 200), Some((7, 3 * GB as i64)), "from the reading a week back");
    assert_eq!(week_change(&r(&[(195, 20), (200, 18)]), 200), Some((5, -2 * (GB as i64))), "or the first after it");
    assert_eq!(week_change(&r(&[(200, 18)]), 200), None);
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------ transfers

fn wait_for(what: &str, secs: u64, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(start.elapsed() < Duration::from_secs(secs), "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Jobs waiting for one disk go in the order they are put in: by id at first, then as
/// moved (before another, or last), the running one left where it is. Their copies land
/// in that order once the disk is free.
#[test]
fn waiting_transfers_go_in_the_order_they_are_put() {
    use std::sync::atomic::Ordering::Relaxed;
    let ctx = egui::Context::default();
    let q = std::sync::Arc::new(ops::Queue::default());
    let d = tmpdir("reorder");
    let big: Vec<u8> = (0..6_000_000u32).map(|i| (i % 251) as u8).collect();
    std::fs::write(d.join("big.bin"), &big).unwrap();
    for n in ["a", "b", "c", "e"] {
        touch(&d.join(format!("{n}.txt")), n);
    }
    std::fs::create_dir_all(d.join("out")).unwrap();
    let copy = |id: u64, what: &str| ops::spawn_in(id, OpKind::Copy { srcs: vec![d.join(what)], dest: d.join("out"), decide: HashMap::new() }, ctx.clone(), &q, false);
    let first = copy(1, "big.bin");
    first.pause.store(true, Relaxed);
    wait_for("the first to hold the disk", 5, || q.running() == [1]);
    let jobs: Vec<ops::Job> = [(2, "a.txt"), (3, "b.txt"), (4, "c.txt"), (5, "e.txt")].into_iter().map(|(id, n)| copy(id, n)).collect();
    wait_for("four waiting", 5, || q.waiting().len() == 4);
    assert_eq!(q.waiting(), [2, 3, 4, 5]);
    q.move_to(5, Some(2));
    assert_eq!(q.waiting(), [5, 2, 3, 4], "put first");
    q.move_to(2, None);
    assert_eq!(q.waiting(), [5, 3, 4, 2], "put last");
    q.move_to(4, Some(3));
    assert_eq!(q.waiting(), [5, 4, 3, 2]);
    q.move_to(1, Some(5));
    q.move_to(99, Some(5));
    assert_eq!(q.waiting(), [5, 4, 3, 2], "the running job and an unknown one stay out of it");
    assert_eq!(q.running(), [1]);
    // Paused, the first holds its bytes where they are.
    let held = first.progress.lock().unwrap().done_bytes;
    std::thread::sleep(Duration::from_millis(120));
    assert_eq!(first.progress.lock().unwrap().done_bytes, held, "paused");
    assert!(jobs.iter().all(|j| j.progress.lock().unwrap().waiting.as_deref() == Some("Copying big.bin")));
    // The chip's sum: five jobs, the waiting ones counting for nothing yet.
    let all: Vec<ops::Job> = std::iter::once(first).chain(jobs).collect();
    let (text, frac) = crate::transfers::summary(&all).unwrap();
    assert!(text.starts_with("5 jobs · "), "{text}");
    assert!((0.0..1.0).contains(&frac));
    // Let go: each starts in the order it was put.
    all[0].pause.store(false, Relaxed);
    for j in &all {
        wait_job(j);
    }
    let mut order: Vec<(Instant, u64)> = all[1..].iter().map(|j| (j.progress.lock().unwrap().since.expect("started after waiting"), j.id)).collect();
    order.sort();
    assert_eq!(order.iter().map(|x| x.1).collect::<Vec<_>>(), [5, 4, 3, 2]);
    assert_eq!(std::fs::read(d.join("out/big.bin")).unwrap(), big);
    assert!(crate::transfers::summary(&all).is_none(), "nothing left: no chip");
    assert!(q.waiting().is_empty() && q.running().is_empty());
    let _ = std::fs::remove_dir_all(&d);
}

/// A transfer queued behind others asks about a taken name inside its job: the clash
/// says which is newer and how many more of its names are taken; Replace puts the old one
/// in the trash, Skip with "the same for the others" answers the rest at once, a name not
/// taken is copied without asking, and two folders can be merged. Cancelled while it
/// asks, the job ends and copies nothing more. The time spent asking is left out of the
/// speed.
#[test]
fn a_queued_transfer_asks_about_taken_names_inside_its_job() {
    let ctx = egui::Context::default();
    let d = tmpdir("clash");
    let (src, out) = (d.join("src"), d.join("out"));
    for n in ["a", "b", "c", "d"] {
        touch(&src.join(format!("{n}.txt")), &format!("new {n}"));
    }
    for n in ["a", "b", "c"] {
        touch(&out.join(format!("{n}.txt")), &format!("old {n}"));
    }
    touch(&src.join("box/new.txt"), "new");
    touch(&out.join("box/old.txt"), "old");
    // The one there is two days older than the one coming.
    let two_days_ago = std::time::SystemTime::now() - Duration::from_secs(2 * 86_400 + 60);
    std::fs::File::options().write(true).open(out.join("a.txt")).unwrap().set_modified(two_days_ago).unwrap();
    let ask = |names: &[&str]| -> HashMap<PathBuf, Conflict> { names.iter().map(|n| (src.join(n), Conflict::Ask)).collect() };
    let srcs: Vec<PathBuf> = ["a.txt", "b.txt", "c.txt", "d.txt"].iter().map(|n| src.join(n)).collect();
    let j = ops::spawn(1, OpKind::Copy { srcs: srcs.clone(), dest: out.clone(), decide: ask(&["a.txt", "b.txt", "c.txt", "d.txt"]) }, ctx.clone());
    let clash = |j: &ops::Job| j.progress.lock().unwrap().clash.clone();
    wait_for("the first clash", 5, || clash(&j).is_some());
    let c = clash(&j).unwrap();
    assert_eq!((c.name.as_str(), c.note.as_str(), c.dirs, c.others), ("a.txt", "older by 2 days", false, 2));
    std::thread::sleep(Duration::from_millis(150));
    j.progress.lock().unwrap().answer = Some((Conflict::Overwrite, false));
    wait_for("the second clash", 5, || clash(&j).is_some_and(|c| c.name == "b.txt"));
    assert_eq!(clash(&j).unwrap().others, 1, "c.txt still to come");
    assert_eq!(std::fs::read_to_string(out.join("a.txt")).unwrap(), "new a", "replaced");
    assert!(crate::trashcan::list().iter().any(|i| i.original == out.join("a.txt")), "the old one is in the trash");
    j.progress.lock().unwrap().answer = Some((Conflict::Skip, true));
    wait_job(&j);
    let g = j.progress.lock().unwrap();
    assert!(g.clash.is_none() && g.errors.is_empty(), "{:?}", g.errors);
    assert!(g.held >= Duration::from_millis(150), "the wait for answers is counted apart: {:?}", g.held);
    drop(g);
    assert_eq!(std::fs::read_to_string(out.join("b.txt")).unwrap(), "old b", "skipped");
    assert_eq!(std::fs::read_to_string(out.join("c.txt")).unwrap(), "old c", "skipped by the same answer");
    assert_eq!(std::fs::read_to_string(out.join("d.txt")).unwrap(), "new d", "not taken: copied without asking");
    // Two folders: merged.
    let m = ops::spawn(2, OpKind::Copy { srcs: vec![src.join("box")], dest: out.clone(), decide: ask(&["box"]) }, ctx.clone());
    wait_for("the folder clash", 5, || clash(&m).is_some());
    assert!(clash(&m).unwrap().dirs);
    m.progress.lock().unwrap().answer = Some((Conflict::Merge, false));
    wait_job(&m);
    assert!(out.join("box/old.txt").exists() && out.join("box/new.txt").exists(), "merged");
    // A move kept beside the one there.
    let mv = ops::spawn(3, OpKind::Move { srcs: vec![src.join("b.txt")], dest: out.clone(), decide: ask(&["b.txt"]) }, ctx.clone());
    wait_for("the move's clash", 5, || clash(&mv).is_some());
    mv.progress.lock().unwrap().answer = Some((Conflict::KeepBoth, false));
    wait_job(&mv);
    assert!(!src.join("b.txt").exists());
    let kept: Vec<String> = std::fs::read_dir(&out).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).filter(|n| n.starts_with('b') && n != "box").collect();
    assert_eq!(kept.len(), 2, "both kept: {kept:?}");
    // Cancelled while it asks: nothing after it is copied.
    touch(&src.join("z.txt"), "z");
    let cj = ops::spawn(4, OpKind::Copy { srcs: vec![src.join("c.txt"), src.join("z.txt")], dest: out.clone(), decide: ask(&["c.txt", "z.txt"]) }, ctx.clone());
    wait_for("the clash before the cancel", 5, || clash(&cj).is_some());
    cj.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    wait_job(&cj);
    assert!(clash(&cj).is_none(), "the question goes with the job");
    assert!(!out.join("z.txt").exists(), "nothing after it");
    assert_eq!(std::fs::read_to_string(out.join("c.txt")).unwrap(), "old c");
    let _ = std::fs::remove_dir_all(&d);
}

/// How the one there compares with the one coming, in the clash's words; and the sizes
/// in the transfers list, the unit said once when both share it.
#[test]
fn clash_notes_and_sizes_read_as_said() {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = |s: u64| Some(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + s));
    assert_eq!(ops::clash_note(t(0), t(0)), "same date");
    assert_eq!(ops::clash_note(t(1), t(0)), "same date");
    assert_eq!(ops::clash_note(t(45), t(0)), "newer by 45 seconds");
    assert_eq!(ops::clash_note(t(0), t(60)), "older by 1 minute");
    assert_eq!(ops::clash_note(t(7200), t(0)), "newer by 2 hours");
    assert_eq!(ops::clash_note(t(0), t(86_400 * 3 + 5)), "older by 3 days");
    assert_eq!(ops::clash_note(t(86_400 * 65), t(0)), "newer by 2 months");
    assert_eq!(ops::clash_note(t(0), t(86_400 * 800)), "older by 2 years");
    assert_eq!(ops::clash_note(None, t(0)), "");
    assert_eq!(ops::clash_note(Some(SystemTime::now()), None), "");
    assert_eq!(crate::transfers::of_size(2_100_000_000, 3_300_000_000), format!("{} of {}", crate::fsx::fmt_size(2_100_000_000).rsplit_once(' ').unwrap().0, crate::fsx::fmt_size(3_300_000_000)));
    let mixed = crate::transfers::of_size(512_000, 3_300_000_000);
    assert_eq!(mixed, format!("{} of {}", crate::fsx::fmt_size(512_000), crate::fsx::fmt_size(3_300_000_000)), "two units, both said");
}

// ------------------------------------------------------------------ download origin

fn set_xattr(p: &Path, name: &str, value: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
    let n = std::ffi::CString::new(name).unwrap();
    unsafe { libc::setxattr(c.as_ptr(), n.as_ptr(), value.as_ptr().cast(), value.len(), 0) == 0 }
}

/// Where a download came from, as Chromium writes it: the address in
/// user.xdg.origin.url and the page in user.xdg.referrer.url. Read off the file, cached
/// once read (a file without one too), read again when it changed; grouped by its site,
/// folders first and the unmarked last.
#[test]
fn download_origin_read_cached_and_grouped() {
    use crate::origin::{self, Origin};
    assert_eq!(origin::host("https://www.Example.com:8080/files/a.pdf?x=1#y").as_deref(), Some("example.com"));
    assert_eq!(origin::host("https://user:pw@cdn.example.org/a").as_deref(), Some("cdn.example.org"));
    assert_eq!(origin::host("http://[2001:db8::1]:80/a").as_deref(), Some("[2001:db8::1]"));
    assert_eq!(origin::host("blob:https://web.whatsapp.com/1b2c").as_deref(), Some("web.whatsapp.com"));
    assert_eq!(origin::host("data:text/plain,hi"), None);
    assert_eq!(origin::host("https:///nothing"), None);
    assert_eq!(origin::short("https://www.example.com/files/report.pdf?token=abc#p2"), "example.com/files/report.pdf");
    assert_eq!(origin::short("https://example.com/"), "example.com");
    assert!(origin::openable("HTTPS://example.com/a") && origin::openable("http://x/"));
    for bad in ["file:///etc/passwd", "javascript:alert(1)", "data:text/html,x", "ftp://x/y", ""] {
        assert!(!origin::openable(bad), "{bad}");
    }
    assert_eq!(Origin::new(None, Some("https://news.example.net/story".into())).site.as_deref(), Some("news.example.net"));
    assert_eq!(Origin::new(Some("https://www.example.com/a".into()), Some("https://github.com/b".into())).site.as_deref(), Some("example.com"), "the address before the page");
    let d = tmpdir("origin");
    touch(&d.join("report.pdf"), "pdf");
    touch(&d.join("plain.txt"), "txt");
    touch(&d.join("linked.zip"), "zip");
    std::fs::create_dir_all(d.join("Folder")).unwrap();
    if !set_xattr(&d.join("report.pdf"), "user.xdg.origin.url", "https://www.example.com/files/report.pdf") {
        eprintln!("no user attributes on {}: skipped", d.display());
        return;
    }
    assert!(set_xattr(&d.join("report.pdf"), "user.xdg.referrer.url", "https://example.com/downloads"));
    assert!(set_xattr(&d.join("linked.zip"), "user.xdg.referrer.url", "https://github.com/x/y/releases"));
    assert_eq!(origin::read(&d.join("report.pdf")), Some(Origin::new(Some("https://www.example.com/files/report.pdf".into()), Some("https://example.com/downloads".into()))));
    assert_eq!(origin::read(&d.join("plain.txt")), None);
    let ctx = egui::Context::default();
    let before = origin::generation();
    for f in ["report.pdf", "plain.txt", "linked.zip"] {
        assert!(origin::get(&d.join(f)).is_none(), "{f} is read on the worker, not here");
        assert!(origin::get(&d.join(f)).is_none(), "and asked for once");
    }
    origin::flush(&ctx);
    wait_for("the worker", 5, || origin::get(&d.join("linked.zip")).is_some() && origin::get(&d.join("plain.txt")).is_some() && origin::get(&d.join("report.pdf")).is_some());
    assert!(origin::generation() > before);
    assert_eq!(origin::get(&d.join("plain.txt")), Some(None));
    let entry = |n: &str| crate::fsx::Entry::from_path(&d.join(n)).unwrap();
    assert_eq!(origin::group(&entry("report.pdf")), "example.com");
    assert_eq!(origin::group(&entry("linked.zip")), "github.com", "by the page when the address is missing");
    assert_eq!(origin::group(&entry("plain.txt")), origin::UNKNOWN);
    assert_eq!(origin::group(&entry("Folder")), origin::FOLDERS);
    let mut groups = vec![origin::UNKNOWN, "github.com", origin::FOLDERS];
    groups.sort_by_key(|g| origin::rank(g));
    assert_eq!(groups, [origin::FOLDERS, "github.com", origin::UNKNOWN]);
    // Marked later (downloaded again over it): read again once told.
    assert!(set_xattr(&d.join("plain.txt"), "user.xdg.origin.url", "https://files.example.org/plain.txt"));
    assert_eq!(origin::get(&d.join("plain.txt")), Some(None), "cached until told");
    origin::invalidate(&d.join("plain.txt"));
    assert!(origin::get(&d.join("plain.txt")).is_none());
    origin::flush(&ctx);
    wait_for("read again", 5, || origin::get(&d.join("plain.txt")).is_some_and(|o| o.is_some()));
    assert_eq!(origin::group(&entry("plain.txt")), "files.example.org");
    // A file named oddly, and an attribute that is all blanks.
    touch(&d.join("blank.bin"), "b");
    assert!(set_xattr(&d.join("blank.bin"), "user.xdg.origin.url", "  \0"));
    assert_eq!(origin::read(&d.join("blank.bin")), None);
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------ jump

/// Frecency weighs visits as zoxide does; visits are counted and, past the most kept,
/// the least used are forgotten; zoxide's own list is read from its real output.
#[test]
fn jump_counts_visits_and_reads_zoxide() {
    use crate::jump::{Visit, frecency, note, parse_zoxide};
    let now = 10_000_000;
    let v = |n, ago| Visit { n, at: now - ago };
    assert_eq!(frecency(&v(3, 10), now), 12.0);
    assert_eq!(frecency(&v(3, 3600), now), 6.0);
    assert_eq!(frecency(&v(3, 86_400), now), 1.5);
    assert_eq!(frecency(&v(3, 604_800), now), 0.75);
    assert_eq!(frecency(&Visit { n: 2, at: now + 50 }, now), 8.0, "a clock gone back counts as now");
    let mut visits = HashMap::new();
    note(&mut visits, Path::new("/a"), now);
    note(&mut visits, Path::new("/a"), now + 5);
    assert_eq!((visits[Path::new("/a")].n, visits[Path::new("/a")].at), (2, now + 5));
    // A thousand folders, the first hundred used often: those stay once it is pruned.
    for i in 0..1001u64 {
        let p = PathBuf::from(format!("/f/{i}"));
        let times = if i < 100 { 5 } else { 1 };
        for _ in 0..times {
            note(&mut visits, &p, now - (1001 - i) * 10);
        }
    }
    assert!(visits.len() <= 1000, "pruned: {}", visits.len());
    assert!((0..100).all(|i| visits.contains_key(&PathBuf::from(format!("/f/{i}")))), "the most used stay");
    assert!(visits.contains_key(Path::new("/a")), "the most recent stays");
    // zoxide's own output, from a database of its own.
    let z = tmpdir("zoxide");
    let have = std::process::Command::new("zoxide").arg("--version").output().is_ok_and(|o| o.status.success());
    let sample = if have {
        for (dir, times) in [("/usr", 3), ("/usr/share", 1), ("/tmp", 2)] {
            for _ in 0..times {
                let ok = std::process::Command::new("zoxide").env("_ZO_DATA_DIR", &z).env("_ZO_EXCLUDE_DIRS", "").args(["add", dir]).status().is_ok_and(|s| s.success());
                assert!(ok);
            }
        }
        let out = std::process::Command::new("zoxide").env("_ZO_DATA_DIR", &z).args(["query", "--list", "--score"]).output().unwrap();
        String::from_utf8(out.stdout).unwrap()
    } else {
        "  12.0 /usr\n   8.0 /tmp\n   4.0 /usr/share\n".into()
    };
    let got = parse_zoxide(&sample);
    let paths: Vec<&Path> = got.iter().map(|x| x.0.as_path()).collect();
    assert_eq!(paths, [Path::new("/usr"), Path::new("/tmp"), Path::new("/usr/share")], "{sample:?}");
    assert!(got[0].1 > got[1].1 && got[1].1 > got[2].1);
    assert!(parse_zoxide("garbage\n  1.0 relative/path\n 2.5 /with space/dir\n").iter().map(|x| x.0.clone()).eq([PathBuf::from("/with space/dir")]));
    let _ = std::fs::remove_dir_all(&z);
}

// ------------------------------------------------------------------ earlier copies

/// The last nine sets copied or cut, newest first, the same set moved up rather than
/// kept twice; a cut set gone once pasted; each set said in a few words.
#[test]
fn earlier_copies_keep_the_newest_nine() {
    use crate::pastes::{Set, names, pasted, remember, title};
    let set = |names: &[&str], cut: bool, dirs: usize| Set { paths: names.iter().map(|n| PathBuf::from("/home/u/Pictures/Camera").join(n)).collect(), cut, dirs, from: PathBuf::from("/home/u/Pictures/Camera"), at: 0 };
    let mut list = vec![];
    for i in 0..12 {
        remember(&mut list, set(&[&format!("{i}.jpg")], false, 0));
    }
    assert_eq!(list.len(), 9);
    assert_eq!(list[0].paths[0].file_name().unwrap(), "11.jpg");
    assert_eq!(list[8].paths[0].file_name().unwrap(), "3.jpg");
    remember(&mut list, set(&["5.jpg"], false, 0));
    assert_eq!(list.len(), 9, "not kept twice");
    assert_eq!(list[0].paths[0].file_name().unwrap(), "5.jpg");
    assert_eq!(list[1].paths[0].file_name().unwrap(), "11.jpg");
    remember(&mut list, set(&["a.jpg", "b.jpg"], true, 0));
    pasted(&mut list, &[PathBuf::from("/home/u/Pictures/Camera/a.jpg"), PathBuf::from("/home/u/Pictures/Camera/b.jpg")], false);
    assert_eq!(list.len(), 9, "a copy pasted stays");
    pasted(&mut list, &[PathBuf::from("/home/u/Pictures/Camera/a.jpg"), PathBuf::from("/home/u/Pictures/Camera/b.jpg")], true);
    assert_eq!(list.len(), 8, "a cut pasted is gone");
    assert!(list.iter().all(|s| !s.cut));
    assert_eq!(title(&set(&["a"], false, 0)), "1 file from Pictures/Camera");
    assert_eq!(title(&set(&["a", "b", "c", "d"], false, 0)), "4 files from Pictures/Camera");
    assert_eq!(title(&set(&["a"], false, 1)), "1 folder from Pictures/Camera");
    assert_eq!(title(&set(&["a", "b"], false, 2)), "2 folders from Pictures/Camera");
    assert_eq!(title(&set(&["a", "b", "c"], false, 1)), "3 items from Pictures/Camera");
    assert_eq!(names(&set(&["IMG_2041.jpg", "IMG_2042.jpg", "x", "y"], false, 0)), "IMG_2041.jpg, IMG_2042.jpg and 2 more");
    assert_eq!(names(&set(&["one.txt", "two.txt"], false, 0)), "one.txt, two.txt");
}

// ------------------------------------------------------------------ what grew

/// Space used's readings: one a day for each folder, the day's latest standing, the four
/// newest days kept, and of many folders the most recently counted; the reading before
/// today found again. Names that would break the file are left out, and a change is said
/// as it is shown.
#[test]
fn space_readings_say_what_grew() {
    use crate::grew::{Reading, earlier_in, fmt_change, note_in};
    let d = tmpdir("grew");
    let f = d.join("maia/space-readings");
    let dir = PathBuf::from("/data/Projects");
    let reading = |day: i64, sizes: &[(&str, u64)]| Reading { day, at: day as u64 * 86_400 + 3600, dir: dir.clone(), sizes: sizes.iter().map(|(n, s)| (n.to_string(), *s)).collect() };
    assert!(earlier_in(&f, &dir, 100).is_none(), "nothing yet");
    note_in(&f, reading(100, &[("maia", 5_000_000), ("site", 2_000)]));
    assert!(earlier_in(&f, &dir, 100).is_none(), "today's own reading isn't earlier");
    let r = earlier_in(&f, &dir, 101).unwrap();
    assert_eq!((r.day, r.total(), r.sizes["maia"]), (100, 5_002_000, 5_000_000));
    note_in(&f, reading(100, &[("maia", 7_000_000)]));
    assert_eq!(earlier_in(&f, &dir, 101).unwrap().sizes.len(), 1, "the day's latest reading stands");
    for day in 101..110 {
        note_in(&f, reading(day, &[("maia", day as u64 * 1000)]));
    }
    let text = std::fs::read_to_string(&f).unwrap();
    assert_eq!(text.matches(&format!("\t{}\n", dir.display())).count(), 4, "four days kept:\n{text}");
    assert_eq!(earlier_in(&f, &dir, 109).unwrap().day, 108);
    assert_eq!(earlier_in(&f, &dir, 200).unwrap().day, 109);
    assert!(earlier_in(&f, &dir, 105).is_none(), "the older days are gone");
    // Names with a tab or a line break, and a folder with one, are left out.
    note_in(&f, Reading { day: 120, at: 120 * 86_400, dir: dir.clone(), sizes: [("ok".to_string(), 1), ("bad\tname".to_string(), 2), ("line\nbreak".to_string(), 3)].into() });
    assert_eq!(earlier_in(&f, &dir, 121).unwrap().sizes.keys().collect::<Vec<_>>(), ["ok"]);
    note_in(&f, Reading { day: 120, at: 1, dir: PathBuf::from("/weird\nfolder"), sizes: [("x".to_string(), 1)].into() });
    assert!(!std::fs::read_to_string(&f).unwrap().contains("weird"));
    // A name that starts like a reading's line is still a name.
    note_in(&f, Reading { day: 121, at: 121 * 86_400, dir: dir.clone(), sizes: [("@100\t5\t/x".to_string(), 9)].into() });
    assert_eq!(earlier_in(&f, &dir, 122).unwrap().sizes.len(), 0, "kept out: it holds a tab");
    // Of many folders, the most recently counted are kept.
    for k in 0..420u64 {
        note_in(&f, Reading { day: 130, at: 130 * 86_400 + k, dir: PathBuf::from(format!("/many/{k}")), sizes: [("x".to_string(), k)].into() });
    }
    assert!(earlier_in(&f, Path::new("/many/419"), 131).is_some());
    assert!(earlier_in(&f, Path::new("/many/0"), 131).is_none(), "the least recent went");
    assert!(earlier_in(&f, &dir, 131).is_none());
    assert_eq!(fmt_change(0), "same");
    assert_eq!(fmt_change(60_000), "same", "under 64 KB is the same");
    assert_eq!(fmt_change(9_100_000_000), format!("+{}", crate::fsx::fmt_size(9_100_000_000)));
    assert_eq!(fmt_change(-3_200_000_000), format!("−{}", crate::fsx::fmt_size(3_200_000_000)));
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------ calendar

/// The calendar's dates: days and dates convert both ways across centuries, weekdays and
/// month lengths come out as `date` says.
#[test]
fn calendar_dates_agree_with_date() {
    use crate::calendar::{civil, day_of, weekday};
    for z in (-800_000i64..800_000).step_by(7) {
        let (y, m, d) = civil(z);
        assert!((1..=12).contains(&m) && (1..=31).contains(&d));
        assert_eq!(day_of(y, m, d), z, "{y}-{m}-{d}");
    }
    assert_eq!(civil(0), (1970, 1, 1));
    assert_eq!(weekday(0), 3, "1970-01-01 was a Thursday");
    assert_eq!(civil(day_of(2024, 2, 29) + 1), (2024, 3, 1));
    assert_eq!(civil(day_of(1900, 2, 28) + 1), (1900, 3, 1), "1900 was no leap year");
    assert_eq!(civil(day_of(2000, 2, 28) + 1), (2000, 2, 29), "2000 was");
    // What `date` says of a few days, in UTC.
    for day in [-25_000i64, -1, 0, 11_016, 19_782, 20_757, 47_482] {
        let out = std::process::Command::new("date").env("TZ", "UTC").args(["-d", &format!("@{}", day * 86_400), "+%Y %m %d %u"]).output();
        let Ok(out) = out else { continue };
        let s = String::from_utf8_lossy(&out.stdout);
        let v: Vec<i64> = s.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        let (y, m, d) = civil(day);
        assert_eq!(v, [y, m as i64, d as i64, weekday(day) as i64 + 1], "day {day}");
    }
}

// ------------------------------------------------------------------ send to a phone

/// The QR codes Maia draws scan: every length an address can have, at each version,
/// read back by zbarimg from the modules drawn as a picture; past version 10 there is
/// no code.
#[test]
fn qr_codes_scan_back() {
    let have = std::process::Command::new("zbarimg").arg("--version").output().is_ok_and(|o| o.status.success());
    let d = tmpdir("qr");
    let mut lens: Vec<usize> = (1..60).step_by(3).collect();
    lens.extend([64, 77, 90, 106, 122, 134, 154, 174, 180, 213, 230, 271]);
    let mut versions = HashSet::new();
    for n in lens {
        let text: String = format!("http://192.168.1.23:47250/{}", "aZ09-_".repeat(60)).chars().take(n).collect();
        let code = crate::qr::encode(text.as_bytes()).unwrap_or_else(|| panic!("no code for {n} bytes"));
        assert_eq!((code.size - 17) % 4, 0, "a QR code's size: {}", code.size);
        versions.insert((code.size - 17) / 4);
        if !have {
            continue;
        }
        // Four pixels a module, four modules of quiet zone.
        let px = 4;
        let side = (code.size + 8) * px;
        let mut rgb = vec![255u8; side * side * 3];
        for y in 0..code.size {
            for x in 0..code.size {
                if code.get(x, y) {
                    for dy in 0..px {
                        for dx in 0..px {
                            let at = ((y + 4) * px + dy) * side + (x + 4) * px + dx;
                            rgb[at * 3..at * 3 + 3].fill(0);
                        }
                    }
                }
            }
        }
        let f = d.join(format!("{n}.png"));
        crate::sim::write_png(&f, side, side, &rgb);
        let out = std::process::Command::new("zbarimg").args(["--raw", "-q", "-Sbinary"]).arg(&f).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim_end_matches('\n'), text, "{n} bytes, version {}", (code.size - 17) / 4);
    }
    assert!(versions.len() >= 8, "most versions drawn: {versions:?}");
    assert!(crate::qr::encode(&[b'x'; 300]).is_none(), "too long for version 10");
    let _ = std::fs::remove_dir_all(&d);
}

fn ask_phone(req: &str, items: &[crate::phone::Item], clean: bool, zip: Option<&crate::drives::Tools>) -> (String, Vec<u8>, Vec<usize>) {
    let mut out = vec![];
    let mut got = vec![];
    let c = crate::phone::Clean { on: std::sync::atomic::AtomicBool::new(clean), tools: None };
    crate::phone::serve(&mut std::io::Cursor::new(req.as_bytes().to_vec()), &mut out, items, "T0KEN", zip, &c, &std::sync::atomic::AtomicBool::new(false), &mut |k| got.push(k));
    let split = out.windows(4).position(|w| w == b"\r\n\r\n").map_or(out.len(), |i| i + 4);
    (String::from_utf8_lossy(&out[..split]).into_owned(), out[split..].to_vec(), got)
}

/// The phone's server: only a path with the token is answered; one item comes at once,
/// several as a page of links (names escaped); a folder comes as a zip bsdtar writes; a
/// photo is sent without its place when that is on; HEAD gets the headers alone; other
/// methods are refused. Names with any characters reach the phone whole.
#[test]
fn phone_server_answers_only_with_its_token() {
    use crate::phone::Item;
    let d = tmpdir("phone");
    touch(&d.join("report final.pdf"), "%PDF-1.4 pdf body");
    touch(&d.join("box/inner.txt"), "inside");
    touch(&d.join("<b>&\"odd\".txt"), "odd");
    let item = |n: &str, dir: bool| Item { path: d.join(n), name: n.into(), dir, size: std::fs::metadata(d.join(n)).map_or(0, |m| m.len()) };
    let one = [item("report final.pdf", false)];
    let (h, body, got) = ask_phone("GET /T0KEN/ HTTP/1.1\r\nHost: x\r\n\r\n", &one, false, None);
    assert!(h.starts_with("HTTP/1.1 200 OK"), "{h}");
    assert!(h.contains("Content-Length: 17\r\n") && h.contains("filename=\"report final.pdf\""), "{h}");
    assert_eq!(body, b"%PDF-1.4 pdf body");
    assert_eq!(got, [0], "heard of as sent whole");
    for bad in ["GET / HTTP/1.1", "GET /T0KE HTTP/1.1", "GET /WRONG/0 HTTP/1.1", "GET /T0KEN0 HTTP/1.1", "GET /T0KEN/7/ HTTP/1.1", "GET /x/T0KEN/ HTTP/1.1"] {
        let (h, body, got) = ask_phone(&format!("{bad}\r\n\r\n"), &one, false, None);
        assert!(h.starts_with("HTTP/1.1 404"), "{bad}: {h}");
        assert!(body.is_empty() && got.is_empty(), "{bad}");
    }
    let (h, _, _) = ask_phone("POST /T0KEN/ HTTP/1.1\r\n\r\n", &one, false, None);
    assert!(h.starts_with("HTTP/1.1 405"));
    let (h, body, got) = ask_phone("HEAD /T0KEN/ HTTP/1.1\r\n\r\n", &one, false, None);
    assert!(h.starts_with("HTTP/1.1 200") && body.is_empty() && got.is_empty(), "{h}");
    // Several: a page.
    let many = [item("report final.pdf", false), item("<b>&\"odd\".txt", false), item("box", true)];
    let (h, body, _) = ask_phone("GET /T0KEN/?x=1 HTTP/1.1\r\n\r\n", &many, false, None);
    let page = String::from_utf8(body).unwrap();
    assert!(h.contains("text/html") && page.contains("href=\"/T0KEN/1/\""), "{page}");
    assert!(page.contains("&lt;b&gt;&amp;&quot;odd&quot;.txt") && !page.contains("&\"odd"), "escaped");
    assert!(page.contains("box.zip") && page.contains("folder, as a zip"));
    let (h, body, got) = ask_phone("GET /T0KEN/1/ HTTP/1.1\r\n\r\n", &many, false, None);
    assert_eq!((body.as_slice(), got.as_slice()), (b"odd".as_slice(), [1].as_slice()));
    assert!(h.contains("filename=\"<b>&_odd_.txt\"") && h.contains("filename*=UTF-8''%3Cb%3E%26%22odd%22.txt"), "{h}");
    // A folder, zipped as it is read.
    let fake = tmpdir("phone-tools");
    let bsdtar = ["/usr/bin/bsdtar", "/bin/bsdtar"].into_iter().find(|p| Path::new(p).exists());
    if let Some(b) = bsdtar {
        std::os::unix::fs::symlink(b, fake.join("bsdtar")).unwrap();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
        let tools = crate::drives::Tools::current();
        crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
        let (h, body, got) = ask_phone("GET /T0KEN/2/ HTTP/1.1\r\n\r\n", &many, false, tools.as_ref());
        assert!(h.contains("application/zip") && body.starts_with(b"PK"), "{h}");
        assert_eq!(got, [2]);
        std::fs::write(d.join("got.zip"), &body).unwrap();
        let list = std::process::Command::new(b).arg("-tf").arg(d.join("got.zip")).output().unwrap();
        let names = String::from_utf8_lossy(&list.stdout);
        assert!(names.lines().any(|l| l == "box/inner.txt"), "{names}");
    }
    // Without bsdtar a folder isn't sent.
    let (_, body, got) = ask_phone("GET /T0KEN/2/ HTTP/1.1\r\n\r\n", &many, false, None);
    assert!(body.is_empty() && got.is_empty());
    // A request line too long, or headers that never end, get nothing harmful.
    let long = format!("GET /T0KEN/{} HTTP/1.1\r\n\r\n", "9".repeat(10_000));
    let (h, _, got) = ask_phone(&long, &one, false, None);
    assert!(!h.starts_with("HTTP/1.1 200") && got.is_empty(), "{h}");
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&fake);
}

/// A folder sent to a phone with places left out has its photos cleaned inside the zip, at
/// any depth and under any name, and the rest as it is (a link stays a link); the cleaned
/// copies are gone once it's sent. With the option off it goes as it is, and a picture
/// that can't be cleaned keeps the whole folder home, as it keeps a single picture.
#[test]
fn phone_folder_goes_without_its_photos_places() {
    use crate::phone::Item;
    let Some(b) = ["/usr/bin/bsdtar", "/bin/bsdtar"].into_iter().find(|p| Path::new(p).exists()) else { return };
    let d = tmpdir("phone-clean");
    let trip = d.join("سفر trip");
    std::fs::create_dir_all(trip.join("day 2")).unwrap();
    let place = Some(["N", "52/1 30/1 1234/100", "E", "13/1 24/1 0/1"]);
    if exif_photo(&trip.join("day 2/IMG_0001.jpg"), "64x48", "2026:05:01 10:00:00", place).is_none() {
        let _ = std::fs::remove_dir_all(&d);
        return;
    }
    touch(&trip.join("notes.txt"), "pack light");
    std::os::unix::fs::symlink("notes.txt", trip.join("latest")).unwrap();
    let fake = tmpdir("phone-clean-tools");
    std::os::unix::fs::symlink(b, fake.join("bsdtar")).unwrap();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = Some(fake.clone()));
    let tools = crate::drives::Tools::current();
    crate::drives::FAKE.with(|f| *f.borrow_mut() = None);
    let items = [Item { path: trip.clone(), name: "سفر trip".into(), dir: true, size: 0 }];
    let unzip = |body: &[u8], to: &str| -> PathBuf {
        let zip = d.join(format!("{to}.zip"));
        std::fs::write(&zip, body).unwrap();
        std::fs::create_dir_all(d.join(to)).unwrap();
        assert!(std::process::Command::new(b).arg("-xf").arg(&zip).arg("-C").arg(d.join(to)).status().unwrap().success());
        d.join(to).join("سفر trip")
    };
    let gps = |p: &Path| String::from_utf8_lossy(&std::process::Command::new("exiv2").args(["-pa", "-q"]).arg(p).output().unwrap().stdout).contains("GPSLatitude");
    let (h, body, got) = ask_phone("GET /T0KEN/ HTTP/1.1\r\n\r\n", &items, true, tools.as_ref());
    assert!(h.contains("application/zip") && body.starts_with(b"PK"), "{h}");
    assert_eq!(got, [0]);
    let out = unzip(&body, "cleaned");
    let photo = std::fs::read(out.join("day 2/IMG_0001.jpg")).unwrap();
    assert!(photo.starts_with(&[0xff, 0xd8]), "still a JPEG");
    assert!(!gps(&out.join("day 2/IMG_0001.jpg")), "the place went to the phone");
    assert_eq!(std::fs::read_to_string(out.join("notes.txt")).unwrap(), "pack light");
    assert_eq!(std::fs::read_link(out.join("latest")).unwrap(), Path::new("notes.txt"));
    let mine = format!("phone-{}-", std::process::id());
    let left = std::fs::read_dir(std::env::temp_dir().join("maia")).map(|rd| rd.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(&mine)).count()).unwrap_or(0);
    assert_eq!(left, 0, "the cleaned copies stayed behind");
    // Off: the folder as it is, place and all.
    let (_, body, _) = ask_phone("GET /T0KEN/ HTTP/1.1\r\n\r\n", &items, false, tools.as_ref());
    assert!(gps(&unzip(&body, "as-is").join("day 2/IMG_0001.jpg")));
    // A picture that can't be cleaned (HEIC, with no exiftool): nothing is sent.
    touch(&trip.join("day 2/IMG_0002.heic"), "heic");
    let (h, body, got) = ask_phone("GET /T0KEN/ HTTP/1.1\r\n\r\n", &items, true, tools.as_ref());
    assert!(h.starts_with("HTTP/1.1 404") && body.is_empty() && got.is_empty(), "{h}");
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&fake);
}

/// ufw's own rules, as ufw writes them: a rule for the port, a range, a list or every
/// port lets phones in; one for another port, for UDP, or one that drops does not. Of
/// the addresses, the Wi-Fi or wired one on a home network, never a VPN or a container's.
#[test]
fn phone_firewall_rules_and_address() {
    use crate::phone::{pick, ufw_allows};
    use std::net::Ipv4Addr;
    let rules = "*filter\n:ufw-user-input - [0:0]\n### RULES ###\n\n### tuple ### allow tcp 22 0.0.0.0/0 any 0.0.0.0/0 in\n-A ufw-user-input -p tcp --dport 22 -j ACCEPT\n\n### tuple ### allow udp 47250 0.0.0.0/0 any 0.0.0.0/0 in\n-A ufw-user-input -p udp --dport 47250 -j ACCEPT\n\n### tuple ### deny tcp 47250 0.0.0.0/0 any 0.0.0.0/0 in\n-A ufw-user-input -p tcp --dport 47250 -j DROP\n\n### END RULES ###\nCOMMIT\n";
    assert!(!ufw_allows(rules, 47250));
    assert!(ufw_allows(rules, 22));
    assert!(ufw_allows("-A ufw-user-input -p tcp --dport 47250 -j ACCEPT\n", 47250));
    assert!(ufw_allows("-A ufw-user-input -p tcp -m multiport --dports 8000,47000:48000 -j ACCEPT\n", 47250));
    assert!(!ufw_allows("-A ufw-user-input -p tcp -m multiport --dports 8000,47000:47249 -j ACCEPT\n", 47250));
    assert!(ufw_allows("-A ufw-user-input -s 192.168.1.0/24 -j ACCEPT\n", 47250), "a whole network");
    assert!(!ufw_allows("-A ufw-user-output -p tcp --dport 47250 -j ACCEPT\n", 47250), "going out is not coming in");
    let found = |v: &[(&str, [u8; 4])]| v.iter().map(|(n, ip)| (n.to_string(), Ipv4Addr::from(*ip))).collect::<Vec<_>>();
    assert_eq!(pick(&found(&[("docker0", [172, 17, 0, 1]), ("tailscale0", [100, 64, 0, 3]), ("wlan0", [192, 168, 1, 23]), ("enp3s0", [10, 0, 0, 5])])), Some(Ipv4Addr::new(192, 168, 1, 23)));
    assert_eq!(pick(&found(&[("virbr0", [192, 168, 122, 1]), ("enp3s0", [10, 0, 0, 5])])), Some(Ipv4Addr::new(10, 0, 0, 5)));
    assert_eq!(pick(&found(&[("wg0", [10, 8, 0, 2]), ("eth0", [8, 8, 8, 8])])), None, "a public address or a VPN's is no use");
}

// ------------------------------------------------------------------ README card

/// A named pipe in a project, among its code or as its README, is passed over: the code is
/// counted and the README thread finishes, where opening the pipe would wait for a writer
/// for good and nothing would be counted again until Maia restarted.
#[test]
fn a_named_pipe_never_hangs_the_line_count_or_the_readme() {
    let d = tmpdir("fifo-project");
    touch(&d.join("Cargo.toml"), "[package]");
    touch(&d.join("src/main.rs"), "fn main() {\n    println!(\"hi\");\n}\n");
    for p in ["src/pipe.rs", "README.md"] {
        let c = std::ffi::CString::new(d.join(p).as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let dir = d.clone();
    std::thread::spawn(move || {
        let _ = tx.send(crate::linecount::count(&dir));
    });
    let count = rx.recv_timeout(Duration::from_secs(5)).expect("the count is waiting on the pipe").expect("a project");
    let rust = count.langs.iter().find(|l| l.name == "Rust").expect("the Rust counted");
    assert_eq!((rust.lines, rust.files), (3, 1), "main.rs alone, the pipe passed over");
    let ctx = egui::Context::default();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), ctx.clone());
    pump(&mut t, &HashMap::new());
    crate::readme::sync(&mut t, &ctx);
    let mut card = t.readme.take().unwrap();
    assert_eq!(card.file.as_deref(), Some(d.join("README.md").as_path()));
    wait_for("the README thread to finish", 5, || {
        crate::readme::height(&mut card, 14.0, 700.0, 0.0);
        !card.reading()
    });
    assert!(card.text().is_none(), "nothing read, no card");
    let _ = std::fs::remove_dir_all(&d);
}

/// The README card: found among a listing's names (README.md before readme.txt, never a
/// folder), read on a thread, an empty one no card; its heights: a heading and a few
/// lines at first, dragged between two lines and four fifths of the room, opened up to
/// most of it, nothing in a panel too short.
#[test]
fn readme_card_found_read_and_sized() {
    use crate::readme::{height, max_height, min_height, shut_height};
    let d = tmpdir("readme");
    touch(&d.join("readme.txt"), "plain");
    touch(&d.join("README.md"), &format!("# Title\n\n{}", "A line of the readme.\n\n".repeat(80)));
    std::fs::create_dir_all(d.join("readme")).unwrap();
    let ctx = egui::Context::default();
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), ctx.clone());
    pump(&mut t, &stats);
    crate::readme::sync(&mut t, &ctx);
    assert_eq!(t.readme.as_ref().unwrap().file.as_deref(), Some(d.join("README.md").as_path()));
    let size = 14.0;
    let room = 700.0;
    let mut card = t.readme.take().unwrap();
    wait_for("the readme read", 5, || height(&mut card, size, room, 0.0) > 0.0);
    assert_eq!(height(&mut card, size, room, 0.0), shut_height(size, 0.0, room), "its start");
    assert_eq!(height(&mut card, size, 200.0, 0.0), 0.0, "no card in a short panel");
    assert_eq!(shut_height(size, 1.0, room), min_height(size), "dragged too small");
    assert_eq!(shut_height(size, 10_000.0, room), max_height(room), "dragged too big");
    assert_eq!(shut_height(size, 300.0, room), 300.0);
    card.open = true;
    let open = height(&mut card, size, room, 0.0);
    // Its whole height is known once drawn; until then, opened, it keeps its start's.
    assert!(open >= shut_height(size, 0.0, room) && open <= (room * 0.6).round(), "opened: {open}");
    assert!(height(&mut card, size, room, 650.0) <= max_height(room));
    t.readme = Some(card);
    // An empty README is no card; none at all, no card either.
    let e = tmpdir("readme-empty");
    touch(&e.join("README"), "\n\n");
    let mut t = Tab::new(2, e.clone(), ViewSettings::default(), ctx.clone());
    pump(&mut t, &stats);
    crate::readme::sync(&mut t, &ctx);
    let mut card = t.readme.take().unwrap();
    assert!(card.file.is_some());
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(height(&mut card, size, room, 0.0), 0.0);
    std::fs::remove_file(e.join("README")).unwrap();
    let mut t = Tab::new(3, e.clone(), ViewSettings::default(), ctx.clone());
    pump(&mut t, &stats);
    crate::readme::sync(&mut t, &ctx);
    assert!(t.readme.as_ref().unwrap().file.is_none());
    let _ = std::fs::remove_dir_all(&d);
    let _ = std::fs::remove_dir_all(&e);
}

// ------------------------------------------------------------------ lines of code

/// Lines of code: blank lines and comments left out (a block comment over several lines,
/// one closed on its line with code after it), each language summed, a binary file with a
/// code name skipped, a folder that isn't a project's top not counted. On Maia's own
/// sources, the Rust is counted as it is.
#[test]
fn lines_of_code_counted_by_language() {
    use crate::linecount::{code_lines, count};
    let c = (&["//"][..], Some(("/*", "*/")));
    assert_eq!(code_lines("fn a() {}\n\n// note\n   \n/* one\n two */ let x = 1;\nlet s = \"src/*\";\nlet t = 2;\n", c.0, c.1), 4);
    assert_eq!(code_lines("/* a */ /* b */\n/*\n*/\ncode /* trailing\nmore\n", c.0, c.1), 2, "a comment opened after code isn't followed");
    assert_eq!(code_lines("# py\nx = 1  # note\n\n", &["#"], None), 1);
    assert_eq!(code_lines("", c.0, c.1), 0);
    let d = tmpdir("loc");
    touch(&d.join("Cargo.toml"), "[package]\nname = \"x\"\n");
    touch(&d.join("src/main.rs"), "// main\nfn main() {\n    println!(\"hi\");\n}\n\n/*\n block\n*/\n");
    touch(&d.join("src/lib.rs"), "pub fn a() {}\npub fn b() {}\n");
    touch(&d.join("tools/run.py"), "#!/usr/bin/env python3\n# a tool\nprint('x')\n");
    std::fs::write(d.join("src/blob.rs"), b"fn x() {}\n\0\0\0binary").unwrap();
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let got = count(&d).unwrap();
    let by: HashMap<&str, (u64, u64)> = got.langs.iter().map(|l| (l.name, (l.lines, l.files))).collect();
    assert_eq!(by.get("Rust"), Some(&(5, 2)), "{:?}", got.langs.iter().map(|l| (l.name, l.lines, l.files)).collect::<Vec<_>>());
    assert_eq!(by.get("Python"), Some(&(1, 1)));
    assert_eq!(got.langs[0].name, "Rust", "most lines first");
    assert_eq!(got.lines, got.langs.iter().map(|l| l.lines).sum::<u64>());
    assert!(!got.cut);
    assert!(count(&d.join("sub")).is_none(), "not the top of a project");
    assert!(count(&d.join("src")).is_none());
    // Maia itself.
    let me = Path::new(env!("CARGO_MANIFEST_DIR"));
    if me.join("src/main.rs").is_file() {
        let t = Instant::now();
        let mine = count(me).unwrap();
        let rust = mine.langs.iter().find(|l| l.name == "Rust").unwrap();
        assert!(rust.lines > 50_000 && rust.files > 100, "{} lines in {} files", rust.lines, rust.files);
        assert!(t.elapsed() < Duration::from_secs(10));
    }
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------ pins

/// Pinned files and folders come first in their folder whatever the sort, ahead of
/// folders first and recents on top, in the sort's order among themselves; pinned tabs
/// go to the left of their strip with the active tab kept active.
#[test]
fn pins_come_first_in_folders_and_tab_strips() {
    let d = tmpdir("pins");
    for (n, s) in [("a.txt", 5), ("b.txt", 50), ("c.txt", 500), ("z.txt", 1)] {
        touch(&d.join(n), &"x".repeat(s));
    }
    std::fs::create_dir_all(d.join("Folder")).unwrap();
    std::fs::create_dir_all(d.join("Other")).unwrap();
    let ctx = egui::Context::default();
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), ctx.clone());
    pump(&mut t, &stats);
    let names = |t: &Tab| t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&t), ["Folder", "Other", "a.txt", "b.txt", "c.txt", "z.txt"]);
    let pinned: HashSet<PathBuf> = [d.join("z.txt"), d.join("b.txt")].into();
    let recents: HashSet<PathBuf> = [d.join("c.txt")].into();
    let rebuild = |t: &mut Tab, recents_on_top: bool| {
        let c = ViewCtx { show_hidden: false, show_system: false, folders_first: true, recents_on_top, recents: &recents, pinned: &pinned, stats: &stats };
        t.dirty = true;
        t.rebuild(&c);
    };
    rebuild(&mut t, false);
    assert_eq!(names(&t), ["b.txt", "z.txt", "Folder", "Other", "a.txt", "c.txt"]);
    rebuild(&mut t, true);
    assert_eq!(names(&t), ["b.txt", "z.txt", "c.txt", "Folder", "Other", "a.txt"], "ahead of recents too");
    t.set_sort(SortKey::Size);
    rebuild(&mut t, false);
    assert_eq!(&names(&t)[..2], ["b.txt", "z.txt"], "by size among themselves");
    t.settings.desc = false;
    rebuild(&mut t, false);
    assert_eq!(&names(&t)[..2], ["z.txt", "b.txt"]);
    // Tabs.
    let mut p = crate::panel::Panel::new(1, Tab::new(10, d.clone(), ViewSettings::default(), ctx.clone()));
    for id in 11..14 {
        p.tabs.push(Tab::new(id, d.clone(), ViewSettings::default(), ctx.clone()));
    }
    p.active = 2;
    p.tabs[3].pinned = true;
    p.tabs[1].pinned = true;
    p.settle_pins();
    assert_eq!(p.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), [11, 13, 10, 12]);
    assert_eq!(p.tabs[p.active].id, 12, "the active tab stays active");
    let before: Vec<u64> = p.tabs.iter().map(|t| t.id).collect();
    p.settle_pins();
    assert_eq!(p.tabs.iter().map(|t| t.id).collect::<Vec<_>>(), before, "settled stays");
    let _ = std::fs::remove_dir_all(&d);
}

// ------------------------------------------------------------------ space map

/// Space used's map: every block inside the area, none over another, each as big as its
/// share, and none far from square; a single item fills it; nothing to lay out lays out
/// nothing.
#[test]
fn space_map_blocks_fill_their_share() {
    use egui::{Rect, pos2};
    let area = Rect::from_min_max(pos2(10.0, 20.0), pos2(810.0, 520.0));
    for sizes in [vec![500.0, 300.0, 120.0, 50.0, 20.0, 5.0, 4.0, 1.0], vec![1.0; 37], (1..300).rev().map(|x| (x * x) as f64).collect::<Vec<_>>(), vec![42.0]] {
        let input: Vec<(u32, f64)> = sizes.iter().enumerate().map(|(i, s)| (i as u32, *s)).collect();
        let mut out = vec![];
        crate::spaceuse::squarify(&input, area, &mut out);
        assert_eq!(out.len(), input.len());
        let total: f64 = sizes.iter().sum();
        let mut covered = 0.0;
        for (k, r) in &out {
            assert!(area.expand(0.05).contains_rect(*r), "{r:?} outside");
            let share = sizes[*k as usize] / total * area.area() as f64;
            assert!((r.area() as f64 - share).abs() < share * 0.01 + 0.5, "block {k}: {} for {share}", r.area());
            covered += r.area() as f64;
        }
        assert!((covered - area.area() as f64).abs() < area.area() as f64 * 0.001);
        for i in 0..out.len() {
            for j in i + 1..out.len() {
                let x = out[i].1.intersect(out[j].1);
                assert!(!x.is_positive() || x.area() < 0.05, "{:?} over {:?}", out[i], out[j]);
            }
        }
        if sizes.len() == 8 {
            let worst = out.iter().take(4).map(|(_, r)| (r.width() / r.height()).max(r.height() / r.width())).fold(0.0, f32::max);
            assert!(worst < 3.0, "the big blocks near square: {worst}");
        }
    }
    let mut out = vec![];
    crate::spaceuse::squarify(&[], area, &mut out);
    crate::spaceuse::squarify(&[(0, 0.0)], area, &mut out);
    crate::spaceuse::squarify(&[(0, 1.0)], Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(0.5, 100.0)), &mut out);
    assert!(out.is_empty());
}

// ------------------------------------------------------------------ share without the place

/// Photos shared without their place or camera, cleaned by Maia alone: real JPEG, PNG,
/// WebP and TIFF files written by ImageMagick with a location, camera, serial number,
/// owner and an XMP copy of the place (exiv2 writes them), read back by exiv2 after
/// cleaning. Nothing of the place or the camera is left, the date and orientation are,
/// the length is the same and the pixels are untouched.
#[test]
fn photos_shared_without_their_place_or_camera() {
    let tool = |n: &str| std::process::Command::new(n).arg("--version").output().is_ok_and(|o| o.status.success() || !o.stdout.is_empty());
    if !tool("magick") || !tool("exiv2") {
        eprintln!("needs ImageMagick and exiv2: skipped");
        return;
    }
    let d = tmpdir("scrub");
    let run = |args: &[&str]| {
        let o = std::process::Command::new(args[0]).args(&args[1..]).current_dir(&d).output().unwrap();
        assert!(o.status.success(), "{args:?}: {}", String::from_utf8_lossy(&o.stderr));
        String::from_utf8_lossy(&o.stdout).into_owned()
    };
    run(&["magick", "-size", "96x64", "gradient:red-blue", "-quality", "92", "photo.jpg"]);
    run(&[
        "exiv2",
        "-M", "set Exif.GPSInfo.GPSLatitude 52/1 30/1 1234/100",
        "-M", "set Exif.GPSInfo.GPSLatitudeRef N",
        "-M", "set Exif.GPSInfo.GPSLongitude 13/1 24/1 0/1",
        "-M", "set Exif.GPSInfo.GPSLongitudeRef E",
        "-M", "set Exif.Image.Make Canon",
        "-M", "set Exif.Image.Model EOS R6",
        "-M", "set Exif.Image.Artist Jane Doe",
        "-M", "set Exif.Image.Orientation 6",
        "-M", "set Exif.Photo.DateTimeOriginal 2026:10:18 14:22:05",
        "-M", "set Exif.Photo.BodySerialNumber 123456789",
        "-M", "set Exif.Photo.LensModel RF 24-105mm",
        "-M", "set Exif.Photo.CameraOwnerName Jane Doe",
        "-M", "set Xmp.exif.GPSLatitude 52,30.2N",
        "-M", "set Xmp.exif.GPSLongitude 13,24.0E",
        "-M", "set Xmp.tiff.Make Canon",
        "-M", "set Iptc.Application2.City Berlin",
        "-M", "set Iptc.Application2.CountryName Germany",
        "photo.jpg",
    ]);
    for ext in ["png", "webp", "tif"] {
        run(&["magick", "photo.jpg", &format!("photo.{ext}")]);
        // ImageMagick carries the Exif over; the XMP is written again by exiv2 where it
        // isn't, as a camera app's or an editor's would be.
        let _ = std::process::Command::new("exiv2").args(["-M", "set Xmp.exif.GPSLatitude 52,30.2N", "-M", "set Xmp.tiff.Make Canon"]).arg(format!("photo.{ext}")).current_dir(&d).output();
    }
    let leaks = |text: &str| -> Vec<String> {
        text.lines()
            .filter(|l| {
                let k = l.split_whitespace().next().unwrap_or("").to_ascii_lowercase();
                (k.contains("gps") && !k.ends_with("gpstag")) || k.ends_with(".make") || k.ends_with(".model") || k.contains("serial") || k.contains("owner") || k.contains("artist") || k.contains("lens") || k.contains("city") || k.contains("country")
            })
            .filter(|l| l.split_whitespace().nth(3).is_some_and(|v| !v.chars().all(|c| c == '0' || c == ' ' || c == '/')))
            .map(String::from)
            .collect()
    };
    let pixels = |f: &str| run(&["magick", f, "-depth", "8", "rgb:-"]);
    for ext in ["jpg", "png", "webp", "tif"] {
        let f = format!("photo.{ext}");
        let before = run(&["exiv2", "-pa", "-q", &f]);
        assert!(!leaks(&before).is_empty(), "{ext}: the place is there to begin with:\n{before}");
        let b = crate::scrub::clean(&d.join(&f), None).unwrap();
        assert_eq!(b.len() as u64, std::fs::metadata(d.join(&f)).unwrap().len(), "{ext}: nothing moved");
        let out = format!("clean.{ext}");
        std::fs::write(d.join(&out), &b).unwrap();
        let after = std::process::Command::new("exiv2").args(["-pa", "-q"]).arg(&out).current_dir(&d).output().unwrap();
        let after = String::from_utf8_lossy(&after.stdout).into_owned() + &String::from_utf8_lossy(&after.stderr);
        assert!(leaks(&after).is_empty(), "{ext}: left in the copy: {:?}\n{after}", leaks(&after));
        if ext == "jpg" {
            assert!(after.contains("DateTimeOriginal") && after.contains("2026:10:18 14:22:05"), "the date stays:\n{after}");
            assert!(after.contains("Exif.Image.Orientation"), "the orientation stays");
        }
        // The text ImageMagick writes into a PNG says it all again.
        let raw = String::from_utf8_lossy(&b).to_ascii_lowercase();
        for word in ["canon", "52/1", "jane doe", "123456789", "eos r6", "berlin", "germany"] {
            assert!(!raw.contains(word), "{ext}: {word:?} still in the bytes");
        }
        assert_eq!(pixels(&out), pixels(&f), "{ext}: the picture itself is the same");
        // Still a picture ImageMagick reads (info:, as ImageMagick 6 has no magick identify).
        let _ = run(&["magick", &out, "info:"]);
    }
    // Pictures without any of it come out as they went in; other kinds need exiftool.
    run(&["magick", "-size", "8x8", "xc:green", "plain.png"]);
    assert_eq!(crate::scrub::clean(&d.join("plain.png"), None).unwrap(), std::fs::read(d.join("plain.png")).unwrap());
    touch(&d.join("photo.heic"), "not really");
    assert_eq!(crate::scrub::clean(&d.join("photo.heic"), None).unwrap_err(), "needs exiftool");
    assert!(crate::scrub::applies("HEIC") && !crate::scrub::built_in("heic") && crate::scrub::built_in("JPEG") && !crate::scrub::applies("pdf"));
    // The copies to attach: pictures cleaned in a folder of their own, the rest as they are.
    touch(&d.join("notes.txt"), "notes");
    let copies = crate::scrub::copies(&[d.join("photo.jpg"), d.join("notes.txt")], None).unwrap();
    assert_eq!(copies[1], d.join("notes.txt"));
    assert_ne!(copies[0], d.join("photo.jpg"));
    assert_eq!(copies[0].file_name().unwrap(), "photo.jpg");
    assert!(!String::from_utf8_lossy(&std::fs::read(&copies[0]).unwrap()).contains("Canon"));
    let _ = std::fs::remove_dir_all(copies[0].parent().unwrap());
    let _ = std::fs::remove_dir_all(&d);
}

/// Two photos of one name attached together both go, each with its own picture and the
/// name it had: the second waits in a folder of its own rather than over the first.
#[test]
fn email_copies_keep_two_photos_of_one_name_apart() {
    let d = tmpdir("scrub-twins");
    for (dir, size) in [("Camera", "40x30"), ("Desktop", "64x48")] {
        std::fs::create_dir_all(d.join(dir)).unwrap();
        if exif_photo(&d.join(dir).join("IMG_0001.jpg"), size, "2026:05:01 10:00:00", None).is_none() {
            let _ = std::fs::remove_dir_all(&d);
            return;
        }
    }
    let copies = crate::scrub::copies(&[d.join("Camera/IMG_0001.jpg"), d.join("Desktop/IMG_0001.jpg")], None).unwrap();
    assert_ne!(copies[0], copies[1], "both attachments are one file");
    assert!(copies.iter().all(|c| c.file_name().unwrap() == "IMG_0001.jpg"), "{copies:?}");
    let size = |p: &Path| String::from_utf8_lossy(&std::process::Command::new("magick").args(["identify", "-format", "%wx%h"]).arg(p).output().unwrap().stdout).into_owned();
    assert_eq!((size(&copies[0]), size(&copies[1])), ("40x30".to_string(), "64x48".to_string()));
    let _ = std::fs::remove_dir_all(copies[0].parent().unwrap());
    let _ = std::fs::remove_dir_all(&d);
}

/// However many folders Space used counts, the readings file holds a bounded number of
/// names, the most recently counted folders' kept whole.
#[test]
fn space_readings_stay_small() {
    use crate::grew::{Reading, earlier_in, note_in};
    let d = tmpdir("grew-budget");
    let f = d.join("space-readings");
    for k in 0..60u64 {
        let sizes: HashMap<String, u64> = (0..4000).map(|i| (format!("item {i:04}"), i)).collect();
        note_in(&f, Reading { day: 10, at: 10 * 86_400 + k, dir: PathBuf::from(format!("/big/{k}")), sizes });
    }
    let lines = std::fs::read_to_string(&f).unwrap().lines().count();
    assert!(lines <= 104_000, "{lines} lines");
    assert_eq!(earlier_in(&f, Path::new("/big/59"), 11).unwrap().sizes.len(), 4000, "the newest kept whole");
    assert!(earlier_in(&f, Path::new("/big/0"), 11).is_none(), "the oldest gone");
    assert!(std::fs::metadata(&f).unwrap().len() < 4 << 20);
    let _ = std::fs::remove_dir_all(&d);
}

/// The README card's fade is solid from the step its text stops being drawn at.
#[test]
fn readme_fade_is_solid_where_the_text_stops() {
    assert!(crate::readme::fade_solid_from_ok());
}

// ------------------------------------------------------------------ robustness

/// What comes from outside never panics (a release build aborts on one): real photos cut
/// short and with bytes changed at random through the cleaner, the phone server's parser
/// fed garbage requests, addresses, code and QR texts of every shape.
#[test]
fn outside_input_never_panics() {
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut rnd = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // Photos: made by ImageMagick with metadata where it is there, else small hand-made
    // headers of each kind.
    let d = tmpdir("fuzz");
    let mut seeds: Vec<Vec<u8>> = vec![];
    for ext in ["jpg", "png", "webp", "tif"] {
        let f = d.join(format!("p.{ext}"));
        let ok = std::process::Command::new("magick").args(["-size", "16x12", "gradient:red-blue"]).arg(&f).status().is_ok_and(|s| s.success());
        if ok {
            let _ = std::process::Command::new("exiv2").args(["-M", "set Exif.GPSInfo.GPSLatitude 1/1 2/1 3/1", "-M", "set Exif.Image.Make X", "-M", "set Xmp.exif.GPSLatitude 1,2N", "-M", "set Iptc.Application2.City C"]).arg(&f).output();
            seeds.push(std::fs::read(&f).unwrap());
        }
    }
    seeds.push(b"\xFF\xD8\xFF\xE1\x00\x10Exif\0\0II*\0\x08\0\0\0\xFF\xD9".to_vec());
    seeds.push(b"\x89PNG\r\n\x1a\n\0\0\0\x0diTXtXML:com.adobe.xmp\0\0\0\0\0\0\0\0\0\0IEND".to_vec());
    seeds.push(b"RIFF\x20\0\0\0WEBPXMP \x08\0\0\0abcdefgh".to_vec());
    seeds.push(b"MM\0*\0\0\0\x08\0\x02\x88\x25\0\x04\0\0\0\x01\0\0\x01\0\x86\x49\0\x01\0\0\0\x20\0\0\0\x30".to_vec());
    for round in 0..4000 {
        let mut b = seeds[round % seeds.len()].clone();
        match rnd() % 4 {
            0 => b.truncate((rnd() as usize) % (b.len() + 1)),
            1 => {
                for _ in 0..1 + rnd() % 8 {
                    if !b.is_empty() {
                        let i = (rnd() as usize) % b.len();
                        b[i] = rnd() as u8;
                    }
                }
            }
            2 => {
                // A length or offset field made huge.
                if b.len() > 8 {
                    let i = (rnd() as usize) % (b.len() - 4);
                    b[i..i + 4].copy_from_slice(&[0xFF, 0xFF, 0xFF, (rnd() as u8) | 0x80]);
                }
            }
            _ => b.extend((0..rnd() % 64).map(|_| rnd() as u8)),
        }
        let len = b.len();
        crate::scrub::bytes(&mut b);
        assert_eq!(b.len(), len, "the cleaner keeps the length");
        let _ = crate::exif::block(&b);
    }
    // The phone server's request parser.
    let items = [crate::phone::Item { path: d.join("p.jpg"), name: "p.jpg".into(), dir: false, size: 3 }];
    let clean = crate::phone::Clean { on: std::sync::atomic::AtomicBool::new(true), tools: None };
    let stop = std::sync::atomic::AtomicBool::new(false);
    let pieces: [&[u8]; 9] = [b"GET ", b"HEAD ", b"/T0KEN", b"/", b"0", b" HTTP/1.1", b"\r\n", b"\n", b"\xff\xfe"];
    for _ in 0..3000 {
        let mut req = vec![];
        for _ in 0..rnd() % 12 {
            req.extend_from_slice(pieces[(rnd() as usize) % pieces.len()]);
            if rnd() % 5 == 0 {
                req.push(rnd() as u8);
            }
        }
        let mut out = vec![];
        crate::phone::serve(&mut std::io::Cursor::new(req), &mut out, &items, "T0KEN", None, &clean, &stop, &mut |_| {});
    }
    // Addresses, code and QR texts.
    let chars: Vec<char> = "ab:/@[]?#.%-_ 0é\u{1F600}\t*/\"\\".chars().collect();
    for _ in 0..3000 {
        let s: String = (0..rnd() % 40).map(|_| chars[(rnd() as usize) % chars.len()]).collect();
        let _ = (crate::origin::host(&s), crate::origin::short(&s), crate::origin::openable(&s));
        let _ = crate::linecount::code_lines(&s, &["//", "#"], Some(("/*", "*/")));
        let _ = crate::linecount::code_lines(&s, &["--"], Some(("{-", "-}")));
        let _ = crate::jump::parse_zoxide(&s);
        let _ = crate::phone::ufw_allows(&s, 47250);
        if let Some(c) = crate::qr::encode(s.as_bytes()) {
            assert_eq!((c.size - 17) % 4, 0);
        }
    }
    let _ = std::fs::remove_dir_all(&d);
}

/// The README card follows its file: edited, the card shows the new text once the folder
/// is read again; replaced by a better-named one, that one.
#[test]
fn readme_card_follows_the_file() {
    let d = tmpdir("readme-edit");
    touch(&d.join("README.md"), "# Old title\n\nold words\n");
    let ctx = egui::Context::default();
    let stats = HashMap::new();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), ctx.clone());
    pump(&mut t, &stats);
    crate::readme::sync(&mut t, &ctx);
    let text = |t: &mut Tab| {
        let c = t.readme.as_mut().unwrap();
        wait_for("the readme read", 5, || crate::readme::height(c, 14.0, 700.0, 0.0) > 0.0 && c.text().is_some());
        c.text().unwrap()
    };
    assert!(text(&mut t).contains("Old title"));
    std::thread::sleep(Duration::from_millis(20));
    touch(&d.join("README.md"), "# New title\n\nnew words, more of them\n");
    t.reload();
    pump(&mut t, &stats);
    crate::readme::sync(&mut t, &ctx);
    assert!(text(&mut t).contains("New title"), "read again after it changed");
    let _ = std::fs::remove_dir_all(&d);
}


// ------------------------------------------------------------------ GitHub profile

/// What GitHub's GraphQL said for a profile (the viewer query of `github.rs`), captured
/// and cut to four weeks and three repositories, the names changed.
pub(crate) const GH_VIEWER: &str = r##"{"data":{"viewer":{"login":"ana-lima","name":"Ana Lima","bio":"Writes small tools\nfor quiet machines.","pronouns":"she/her","location":"Porto","company":null,"websiteUrl":"https://ana.example","createdAt":"2019-04-02T10:00:00Z","avatarUrl":"https://avatars.githubusercontent.com/u/1?v=4","status":{"message":"On a train"},"followers":{"totalCount":1204,"nodes":[{"login":"f0","avatarUrl":"https://avatars.githubusercontent.com/u/10?v=4"},{"login":"f1","avatarUrl":"https://avatars.githubusercontent.com/u/11?v=4"},{"login":"f2","avatarUrl":"https://avatars.githubusercontent.com/u/12?v=4"}]},"following":{"totalCount":12},"organizations":{"nodes":[{"login":"tinyco","avatarUrl":"https://avatars.githubusercontent.com/u/99?v=4"}]},"contributionsCollection":{"contributionCalendar":{"totalContributions":1318,"weeks":[{"contributionDays":[{"date":"2025-10-26","contributionCount":0,"contributionLevel":"NONE","weekday":0},{"date":"2025-10-27","contributionCount":0,"contributionLevel":"NONE","weekday":1},{"date":"2025-10-28","contributionCount":0,"contributionLevel":"NONE","weekday":2},{"date":"2025-10-29","contributionCount":0,"contributionLevel":"NONE","weekday":3},{"date":"2025-10-30","contributionCount":0,"contributionLevel":"NONE","weekday":4},{"date":"2025-10-31","contributionCount":0,"contributionLevel":"NONE","weekday":5},{"date":"2025-11-01","contributionCount":0,"contributionLevel":"NONE","weekday":6}]},{"contributionDays":[{"date":"2025-11-02","contributionCount":0,"contributionLevel":"NONE","weekday":0},{"date":"2025-11-03","contributionCount":0,"contributionLevel":"NONE","weekday":1},{"date":"2025-11-04","contributionCount":0,"contributionLevel":"NONE","weekday":2},{"date":"2025-11-05","contributionCount":0,"contributionLevel":"NONE","weekday":3},{"date":"2025-11-06","contributionCount":0,"contributionLevel":"NONE","weekday":4},{"date":"2025-11-07","contributionCount":0,"contributionLevel":"NONE","weekday":5},{"date":"2025-11-08","contributionCount":0,"contributionLevel":"NONE","weekday":6}]},{"contributionDays":[{"date":"2026-10-18","contributionCount":2,"contributionLevel":"FIRST_QUARTILE","weekday":0},{"date":"2026-10-19","contributionCount":5,"contributionLevel":"FIRST_QUARTILE","weekday":1},{"date":"2026-10-20","contributionCount":9,"contributionLevel":"SECOND_QUARTILE","weekday":2},{"date":"2026-10-21","contributionCount":14,"contributionLevel":"THIRD_QUARTILE","weekday":3},{"date":"2026-10-22","contributionCount":1,"contributionLevel":"FIRST_QUARTILE","weekday":4},{"date":"2026-10-23","contributionCount":0,"contributionLevel":"NONE","weekday":5},{"date":"2026-10-24","contributionCount":0,"contributionLevel":"NONE","weekday":6}]},{"contributionDays":[{"date":"2026-10-25","contributionCount":4,"contributionLevel":"FIRST_QUARTILE","weekday":0},{"date":"2026-10-26","contributionCount":21,"contributionLevel":"FOURTH_QUARTILE","weekday":1},{"date":"2026-10-27","contributionCount":0,"contributionLevel":"NONE","weekday":2}]}]}},"repositories":{"totalCount":3,"nodes":[{"nameWithOwner":"ana-lima/lamp","createdAt":"2026-09-14T09:12:00Z","languages":{"edges":[{"size":4210000,"node":{"name":"Rust","color":"#dea584"}},{"size":18000,"node":{"name":"Shell","color":"#89e051"}},{"size":9000,"node":{"name":"Python","color":"#3572A5"}}]},"releases":{"nodes":[{"name":"Lamp 0.1.0","tagName":"v0.1.0","createdAt":"2026-09-30T10:00:00Z","url":"https://github.com/ana-lima/lamp/releases/v0.1.0"}]},"url":"https://github.com/ana-lima/lamp"},{"nameWithOwner":"ana-lima/notes","createdAt":"2026-05-02T18:40:00Z","languages":{"edges":[{"size":41000,"node":{"name":"Go","color":"#00ADD8"}},{"size":9000,"node":{"name":"HTML","color":"#e34c26"}},{"size":4000,"node":{"name":"CSS","color":"#663399"}}]},"releases":{"nodes":[]},"url":"https://github.com/ana-lima/notes"},{"nameWithOwner":"ana-lima/kettle","createdAt":"2026-08-21T12:05:00Z","languages":{"edges":[{"size":1200000,"node":{"name":"C++","color":"#f34b7d"}},{"size":300000,"node":{"name":"Lua","color":"#000080"}},{"size":90000,"node":{"name":"Python","color":"#3572A5"}},{"size":12000,"node":{"name":"CMake","color":"#DA3434"}},{"size":3000,"node":{"name":"Shell","color":"#89e051"}},{"size":1000,"node":{"name":"C","color":"#555555"}}]},"releases":{"nodes":[]},"url":"https://github.com/ana-lima/kettle"}]},"id":"U_kgDOexample"}}}"##;
/// Its recent commits query, cut to two repositories of three commits.
pub(crate) const GH_HISTORY: &str = r##"{"data":{"viewer":{"repositories":{"nodes":[{"defaultBranchRef":{"target":{"history":{"nodes":[{"oid":"abc0000000000000000000000000000000000000","messageHeadline":"Keep the lamp lit","committedDate":"2026-10-12T16:20:05Z","url":"https://github.com/ana-lima/lamp/commit/abc0000000000000000000000000000000000000"},{"oid":"abc0010000000000000000000000000000000000","messageHeadline":"Dim it at night","committedDate":"2026-10-12T16:02:44Z","url":"https://github.com/ana-lima/lamp/commit/abc0010000000000000000000000000000000000"},{"oid":"abc0020000000000000000000000000000000000","messageHeadline":"Start the lamp","committedDate":"2026-10-12T15:41:30Z","url":"https://github.com/ana-lima/lamp/commit/abc0020000000000000000000000000000000000"}]}}},"nameWithOwner":"ana-lima/lamp"},{"defaultBranchRef":{"target":{"history":{"nodes":[{"oid":"abc0100000000000000000000000000000000000","messageHeadline":"Write things down","committedDate":"2026-05-02T19:53:10Z","url":"https://github.com/ana-lima/notes/commit/abc0100000000000000000000000000000000000"},{"oid":"abc0110000000000000000000000000000000000","messageHeadline":"Find old notes","committedDate":"2026-05-02T19:31:27Z","url":"https://github.com/ana-lima/notes/commit/abc0110000000000000000000000000000000000"},{"oid":"abc0120000000000000000000000000000000000","messageHeadline":"Start the notes","committedDate":"2026-05-02T18:58:44Z","url":"https://github.com/ana-lima/notes/commit/abc0120000000000000000000000000000000000"}]}}},"nameWithOwner":"ana-lima/notes"}]}}}}"##;
/// The badges of a public profile page as GitHub writes them (the sidebar's links).
pub(crate) const GH_ACHIEVEMENTS: &str = r##"<a href="/ana-lima?achievement=starstruck&amp;tab=achievements" class="position-relative"><img src="https://github.githubassets.com/assets/starstruck-bronze-b71ec4d7766a.png" data-hovercard-type="achievement" data-hovercard-url="/users/ana-lima/achievements/starstruck/detail?hovercard=1" width="64" alt="Achievement: Starstruck" data-view-component="true" class="achievement-badge-sidebar" /><span data-view-component="true" class="Label achievement-tier-label achievement-tier-label--bronze text-small text-bold color-shadow-medium px-2 tmp-px-2 py-0 tmp-py-0 mb-1 tmp-mb-1 position-absolute right-0 bottom-0">x4</span></a><a href="/ana-lima?achievement=quickdraw&amp;tab=achievements" class="position-relative"><img src="https://github.githubassets.com/assets/quickdraw-default-39c6aec8ff89.png" data-hovercard-type="achievement" data-hovercard-url="/users/ana-lima/achievements/quickdraw/detail?hovercard=1" width="64" alt="Achievement: Quickdraw" data-view-component="true" class="achievement-badge-sidebar" /></a><a href="/ana-lima?achievement=pull-shark&amp;tab=achievements" class="position-relative"><img src="https://github.githubassets.com/assets/pull-shark-bronze-a37accb528d1.png" data-hovercard-type="achievement" data-hovercard-url="/users/ana-lima/achievements/pull-shark/detail?hovercard=1" width="64" alt="Achievement: Pull Shark" data-view-component="true" class="achievement-badge-sidebar" /><span data-view-component="true" class="Label achievement-tier-label achievement-tier-label--bronze text-small text-bold color-shadow-medium px-2 tmp-px-2 py-0 tmp-py-0 mb-1 tmp-mb-1 position-absolute right-0 bottom-0">x2</span></a><a href="/ana-lima?achievement=galaxy-brain&amp;tab=achievements" class="position-relative"><img src="https://github.githubassets.com/assets/galaxy-brain-default-0d3f1a7c9e52.png" data-hovercard-type="achievement" data-hovercard-url="/users/ana-lima/achievements/galaxy-brain/detail?hovercard=1" width="64" alt="Achievement: Galaxy Brain" data-view-component="true" class="achievement-badge-sidebar" /></a>"##;

#[test]
fn github_profile_read_from_its_answer() {
    use crate::github::{self, Kind};
    let v: serde_json::Value = serde_json::from_str(GH_VIEWER).unwrap();
    let p = github::parse_viewer(&v).expect("a profile");
    assert_eq!((p.login.as_str(), p.name.as_str(), p.pronouns.as_str(), p.location.as_str()), ("ana-lima", "Ana Lima", "she/her", "Porto"));
    assert_eq!(p.bio, "Writes small tools for quiet machines.", "lines joined, spaces one");
    assert_eq!((p.status.as_str(), p.website.as_str(), p.company.as_str()), ("On a train", "https://ana.example", ""));
    assert_eq!((p.followers, p.following, p.repos, p.total), (1204, 12, 3, 1318));
    assert_eq!(p.faces.len(), 3);
    assert_eq!(p.orgs, vec![("tinyco".to_string(), "https://avatars.githubusercontent.com/u/99?v=4".to_string())]);
    // Four weeks of days, the first week's place in it from GitHub's own weekday.
    let first = &v["data"]["viewer"]["contributionsCollection"]["contributionCalendar"]["weeks"][0]["contributionDays"][0];
    assert_eq!(p.first_weekday, first["weekday"].as_u64().unwrap() as usize);
    assert!(p.days.len() > 21 && p.days.len() <= 28, "{}", p.days.len());
    assert!(p.days.iter().all(|d| d.level <= 4 && d.date.len() == 10));
    assert!(p.days.iter().any(|d| d.count > 0 && d.level > 0));
    // Languages by bytes over the repositories, largest first, shares of the whole.
    assert!(!p.langs.is_empty() && p.langs.windows(2).all(|w| w[0].2 >= w[1].2));
    let sum: f32 = p.langs.iter().map(|l| l.2).sum();
    assert!(sum > 0.5 && sum <= 1.0001, "{sum}");
    // A repository made this year and a release this year are moments of their own.
    let created: Vec<&str> = p.items.iter().filter(|i| i.kind == Kind::Created).map(|i| i.title.as_str()).collect();
    assert_eq!(created, ["Created lamp", "Created notes", "Created kettle"]);
    let released: Vec<&str> = p.items.iter().filter(|i| i.kind == Kind::Release).map(|i| i.title.as_str()).collect();
    assert_eq!(released, ["Released Lamp 0.1.0"], "the name already says which");
    assert!(p.items.iter().all(|i| i.url.starts_with("https://github.com/ana-lima/")));
    // Not a profile: an error answer, or no login.
    assert!(github::parse_viewer(&serde_json::json!({"errors": [{"message": "Bad credentials"}]})).is_none());
    assert!(github::parse_viewer(&serde_json::json!({"data": {"viewer": {"login": ""}}})).is_none());
}

#[test]
fn github_recent_commits_read_from_their_answer() {
    let v: serde_json::Value = serde_json::from_str(GH_HISTORY).unwrap();
    let items = crate::github::parse_history(&v);
    assert_eq!(items.len(), 6);
    assert!(items.iter().all(|i| i.kind == crate::github::Kind::Commit && i.sha.as_ref().is_some_and(|s| s.len() == 40)));
    assert_eq!((items[0].title.as_str(), items[0].repo.as_str()), ("Keep the lamp lit", "ana-lima/lamp"));
    assert_eq!(items[0].when, crate::github::iso("2026-10-12T16:20:05Z").unwrap());
    assert!(items.iter().filter(|i| i.repo == "ana-lima/notes").count() == 3);
    // A repository without a default branch (empty) gives nothing, not an error.
    assert!(crate::github::parse_history(&serde_json::json!({"data": {"viewer": {"repositories": {"nodes": [{"nameWithOwner": "a/b", "defaultBranchRef": null}]}}}})).is_empty());
}

#[test]
fn github_achievements_read_from_the_profile_page() {
    let a = crate::github::parse_achievements(GH_ACHIEVEMENTS);
    let got: Vec<(&str, &str, u8, u8)> = a.iter().map(|x| (x.slug.as_str(), x.name.as_str(), x.tier, x.count)).collect();
    assert_eq!(got, [("starstruck", "Starstruck", 1, 4), ("quickdraw", "Quickdraw", 0, 1), ("pull-shark", "Pull Shark", 1, 2), ("galaxy-brain", "Galaxy Brain", 0, 1)]);
    // The page shows each badge more than once: the first counts.
    let twice = format!("{GH_ACHIEVEMENTS}{GH_ACHIEVEMENTS}");
    assert_eq!(crate::github::parse_achievements(&twice).len(), 4);
    // A page that changed shape gives none, quietly.
    assert!(crate::github::parse_achievements("<html><body>Profile</body></html>").is_empty());
    assert!(crate::github::parse_achievements(r#"alt="Achievement: Broken"#).is_empty());
}

#[test]
fn github_times_and_counts_read_plainly() {
    use crate::github::{iso, thousands};
    assert_eq!(iso("1970-01-01T00:00:00Z"), Some(0));
    assert_eq!(iso("2000-03-01T00:00:00Z"), Some(951_868_800), "after a leap day");
    assert_eq!(iso("2024-02-29T12:00:00Z"), Some(1_709_208_000));
    assert_eq!(iso("2026-10-27"), Some(1_793_059_200), "a date alone is its midnight");
    assert_eq!(iso("soon"), None);
    for (n, s) in [(0, "0"), (999, "999"), (1000, "1,000"), (1442, "1,442"), (1_234_567, "1,234,567")] {
        assert_eq!(thousands(n), s);
    }
}

/// The page's keys: the first arrow shows the cursor, later ones move it; Enter on a
/// commit whose repository has no clone here asks to open it on GitHub.
#[test]
fn github_page_keys_move_through_the_timeline() {
    use egui::Key;
    let v: serde_json::Value = serde_json::from_str(GH_VIEWER).unwrap();
    let mut p = crate::github::parse_viewer(&v).unwrap();
    p.items.extend(crate::github::parse_history(&serde_json::from_str(GH_HISTORY).unwrap()));
    let n = p.items.len();
    let mut page = crate::github::Page::default();
    page.data = Some(Box::new(p));
    assert!(page.takes(Key::ArrowDown) && page.takes(Key::O) && !page.takes(Key::A));
    let mut actions = vec![];
    page.key(Key::ArrowDown, 1, &mut actions);
    assert_eq!((page.sel, page.keyed), (0, true), "the first arrow only shows where it is");
    page.key(Key::ArrowDown, 1, &mut actions);
    page.key(Key::End, 1, &mut actions);
    assert_eq!(page.sel, n - 1);
    page.key(Key::Home, 1, &mut actions);
    page.key(Key::ArrowUp, 1, &mut actions);
    assert_eq!(page.sel, 0, "stops at the top");
    page.key(Key::Enter, 1, &mut actions);
    assert_eq!(actions.len(), 1, "no clone of it here: opened on GitHub");
    assert!(crate::github::Page::default().takes(Key::ArrowDown) == false, "nothing to move through");
}

#[test]
fn github_is_a_place_not_a_folder() {
    let ctx = egui::Context::default();
    let gh = PathBuf::from(tab::GITHUB);
    assert!(tab::is_github(&gh) && tab::is_place(&gh) && !tab::is_computer(&gh));
    assert!(tab::is_place(Path::new(tab::COMPUTER)) && !tab::is_place(Path::new("/tmp")));
    let mut t = Tab::new(1, gh.clone(), ViewSettings::default(), ctx);
    assert!(!t.is_loading() && t.entries.is_empty(), "nothing is read for it");
    assert_eq!(t.title(), "GitHub");
    t.up(|_| None);
    assert!(tab::is_computer(&t.path), "up from GitHub is Computer");
}

// ------------------------------------------------------------------ colours of pictures

fn flat_image(w: usize, h: usize, at: impl Fn(usize, usize) -> [u8; 3]) -> egui::ColorImage {
    let mut px = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        for x in 0..w {
            let c = at(x, y);
            px.extend_from_slice(&[c[0], c[1], c[2], 255]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([w, h], &px)
}

#[test]
fn a_pictures_colours_and_their_shares() {
    use crate::colours::{glow, hex, palette};
    let red = [210, 40, 40];
    let blue = [30, 70, 200];
    let img = flat_image(80, 60, |x, _| if x < 60 { red } else { blue });
    let p = palette(&img, 5);
    assert_eq!(p.len(), 2, "two colours, however many asked: {p:?}");
    assert_eq!(hex(p[0].0), "#D22828");
    assert!((p[0].1 - 0.75).abs() < 0.05 && (p[1].1 - 0.25).abs() < 0.05, "{p:?}");
    let g = glow(&img);
    assert!(g[0].r() > g[0].b() && g[3].b() > g[3].r(), "red on the left, blue on the right: {g:?}");
    assert!(g.iter().all(|c| c.a() == 255));
    assert!(palette(&flat_image(0, 0, |_, _| red), 5).is_empty());
    // A transparent picture has nothing to glow with.
    let clear = egui::ColorImage::from_rgba_unmultiplied([4, 4], &[0u8; 64]);
    assert!(palette(&clear, 5).is_empty() && glow(&clear).iter().all(|c| c.a() == 0));
}

#[test]
fn pictures_sort_red_through_violet_then_grey() {
    use crate::colours::{palette, sort_number};
    let n = |c: [u8; 3]| sort_number(&palette(&flat_image(16, 16, |_, _| c), 5));
    let order = [[220, 30, 40], [230, 140, 30], [220, 210, 40], [40, 180, 60], [40, 170, 200], [40, 60, 210], [140, 50, 200], [235, 235, 235], [120, 120, 120], [15, 15, 15]];
    let keys: Vec<u64> = order.iter().map(|c| n(*c)).collect();
    assert!(keys.windows(2).all(|w| w[0] < w[1]), "{keys:?}");
    // A deep red just short of 360 degrees counts with the reds, not after violet.
    assert!(n([210, 30, 60]) < n([230, 140, 30]));
    // A grey picture with a small bright patch sorts by the patch only when it is big
    // enough to see.
    let speck = flat_image(40, 40, |x, y| if x < 2 && y < 2 { [230, 30, 30] } else { [128, 128, 128] });
    assert!(sort_number(&palette(&speck, 5)) >= 1 << 32, "a speck doesn't colour it");
}

/// Sorted by colour, a folder's real pictures come in hue order as their colours are
/// read (off the frame, by the colour workers), the rest last.
#[test]
fn a_folder_sorted_by_colour() {
    let d = tmpdir("by-colour");
    let write = |name: &str, c: [u8; 3]| {
        let f = std::fs::File::create(d.join(name)).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), 24, 24);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        let px: Vec<u8> = (0..24 * 24).flat_map(|_| [c[0], c[1], c[2], 255]).collect();
        e.write_header().unwrap().write_image_data(&px).unwrap();
    };
    write("a-blue.png", [40, 60, 210]);
    write("b-grey.png", [128, 128, 128]);
    write("c-red.png", [220, 30, 40]);
    write("d-green.png", [40, 180, 60]);
    touch(&d.join("e-notes.txt"), "no colour");
    let ctx = egui::Context::default();
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), ctx.clone());
    t.settings.sort = SortKey::Colour;
    t.settings.desc = false;
    let stats = HashMap::new();
    let start = Instant::now();
    loop {
        pump(&mut t, &stats);
        crate::colours::focus(&[d.clone()]);
        crate::colours::flush(&ctx);
        if t.entries.iter().filter(|e| crate::colours::sortable(e)).all(|e| crate::colours::sort_key(e).is_some()) {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(20), "colours read");
        std::thread::sleep(Duration::from_millis(10));
    }
    pump(&mut t, &stats);
    let names: Vec<String> = t.view.iter().map(|&i| t.entries[i as usize].name.clone()).collect();
    assert_eq!(names, ["c-red.png", "d-green.png", "a-blue.png", "b-grey.png", "e-notes.txt"]);
}

// ------------------------------------------------------------------ the eye candy

#[test]
fn date_ribbon_years_agree_with_the_calendar() {
    // In UTC against the calendar's own day arithmetic, and moved by a zone's offset.
    let mut t = 0i64;
    while t < 4_000_000_000 {
        let st = std::time::UNIX_EPOCH + Duration::from_secs(t as u64);
        let (y, m, _) = crate::calendar::civil(t.div_euclid(86_400));
        assert_eq!(crate::ribbon::year_month(st, 0), Some((y as i32, m as u32)), "at {t}");
        let (y, m, _) = crate::calendar::civil((t + 5 * 3600).div_euclid(86_400));
        assert_eq!(crate::ribbon::year_month(st, 5 * 3600), Some((y as i32, m as u32)), "at {t} +5h");
        t += 86_400 * 17 + 3_607;
    }
    // New Year's Eve, an hour before midnight in UTC, is next year two hours east.
    let eve = std::time::UNIX_EPOCH + Duration::from_secs(1_798_761_600 - 3_601);
    assert_eq!(crate::ribbon::year_month(eve, 0), Some((2026, 12)));
    assert_eq!(crate::ribbon::year_month(eve, 7_200), Some((2027, 1)));
}

#[test]
fn rows_glide_only_when_their_order_changes() {
    use crate::glide::Glide;
    let (a, b, c) = (Path::new("/x/a"), Path::new("/x/b"), Path::new("/x/c"));
    let dir = Path::new("/x");
    let ctx = egui::Context::default();
    let mut g = Glide::default();
    let frame = |g: &mut Glide, generation: u64, t: f64, rows: &[&Path]| -> Vec<(egui::Vec2, f32)> {
        g.begin(dir, 0, generation, false, true, t);
        let out = rows.iter().enumerate().map(|(i, p)| g.row(p, egui::vec2(0.0, i as f32 * 20.0), 400.0)).collect();
        g.end(&ctx);
        out
    };
    frame(&mut g, 1, 0.0, &[a, b, c]);
    // Scrolling or redrawing the same order moves nothing.
    assert!(frame(&mut g, 1, 0.1, &[a, b, c]).iter().all(|(o, a)| *o == egui::Vec2::ZERO && *a == 1.0));
    // c rises to the top: it starts where it was (40 lower) and the others step down.
    let moved = frame(&mut g, 2, 1.0, &[c, a, b]);
    assert_eq!(moved[0].0, egui::vec2(0.0, 40.0));
    assert_eq!(moved[1].0, egui::vec2(0.0, -20.0));
    let half = frame(&mut g, 2, 1.1, &[c, a, b]);
    assert!(half[0].0.y > 0.0 && half[0].0.y < 40.0, "part way: {half:?}");
    let done = frame(&mut g, 2, 1.5, &[c, a, b]);
    assert!(done.iter().all(|(o, _)| *o == egui::Vec2::ZERO), "settled: {done:?}");
    // A newcomer fades in; another folder, another view, a folder still reading or
    // motion off move nothing.
    let d = Path::new("/x/d");
    let faded = frame(&mut g, 3, 2.0, &[d, c, a, b]);
    assert!(faded[0].1 < 0.1 && faded[1].0 != egui::Vec2::ZERO);
    for (path, view, loading, anim) in [(Path::new("/y"), 0, false, true), (dir, 1, false, true), (dir, 0, true, true), (dir, 0, false, false)] {
        let mut g = Glide::default();
        g.begin(path, view, 1, false, true, 0.0);
        g.row(a, egui::vec2(0.0, 0.0), 400.0);
        g.row(b, egui::vec2(0.0, 20.0), 400.0);
        g.begin(dir, 0, 2, loading, anim, 1.0);
        assert_eq!(g.row(b, egui::vec2(0.0, 0.0), 400.0).0, egui::Vec2::ZERO, "{path:?} {view} {loading} {anim}");
    }
    // The first frame after a folder finished reading: what was drawn while it read is
    // no start for a glide, so nothing slides in from the half-read list.
    let mut g = Glide::default();
    g.begin(dir, 0, 1, true, true, 0.0);
    g.row(a, egui::vec2(0.0, 0.0), 400.0);
    g.row(b, egui::vec2(0.0, 20.0), 400.0);
    g.begin(dir, 0, 2, false, true, 1.0);
    assert_eq!(g.row(b, egui::vec2(0.0, 0.0), 400.0), (egui::Vec2::ZERO, 1.0));
    assert_eq!(g.row(c, egui::vec2(0.0, 20.0), 400.0), (egui::Vec2::ZERO, 1.0), "no fade either");
}

#[test]
fn a_folder_at_a_glance_counts_what_fills_it() {
    let d = tmpdir("glance");
    for f in ["a.jpg", "b.png", "c.webp", "d.rs", "e.txt", "f.mp4", "sub/x"] {
        touch(&d.join(f), "x");
    }
    let mut t = Tab::new(1, d.clone(), ViewSettings::default(), egui::Context::default());
    pump(&mut t, &HashMap::new());
    let s = crate::glance::shares(&mut t);
    let pics = s.iter().find(|(c, _)| *c == crate::theme::FileCat::Image).map(|x| x.1).unwrap();
    assert!((pics - 3.0 / 7.0).abs() < 1e-4, "{}", crate::glance::describe(&s));
    assert!((s.iter().map(|x| x.1).sum::<f32>() - 1.0).abs() < 1e-4);
    assert!(s[0].0 == crate::theme::FileCat::Image, "largest first");
    let said = crate::glance::describe(&s);
    assert!(said.starts_with("Pictures 43%") && said.contains("Folders 14%"), "{said}");
}

#[test]
fn a_folder_peeks_with_its_first_pictures_by_name() {
    let d = tmpdir("peek");
    for f in ["img10.png", "img2.png", "img1.png", "notes.txt", ".hidden.png", "img3.jpg"] {
        touch(&d.join(f), "x");
    }
    std::fs::create_dir_all(d.join("dir.png")).unwrap();
    let got: Vec<String> = crate::peek::first_pictures(&d).iter().map(|(p, _)| p.file_name().unwrap().to_string_lossy().into_owned()).collect();
    assert_eq!(got, ["img1.png", "img2.png", "img3.jpg"], "natural order, no hidden ones, no folders");
    assert!(crate::peek::first_pictures(&d.join("missing")).is_empty());
}

#[test]
fn a_copys_speed_line_samples_twice_a_second() {
    let id = 9_000_001;
    assert!(crate::speedline::sample(id, 0, 10.0).is_empty());
    assert!(crate::speedline::sample(id, 100, 10.2).is_empty(), "too soon for a sample");
    assert_eq!(crate::speedline::sample(id, 1000, 10.5), vec![2000.0]);
    assert_eq!(crate::speedline::sample(id, 1000, 11.0), vec![2000.0, 0.0], "a stall shows as nothing copied");
    crate::speedline::keep_only(&[]);
    assert!(crate::speedline::sample(id, 5000, 12.0).is_empty(), "forgotten once its card went");
}

#[test]
fn a_theme_change_waits_a_frame_for_the_wash() {
    use crate::wash::Wash;
    let ctx = egui::Context::default();
    let mut w = Wash::default();
    ctx.run_pass(Default::default(), |_| {}).textures_delta.clear();
    assert!(!w.theme_changing(&ctx, true), "the old theme stays one more frame");
    assert!(w.wanted());
    w.copied(Some((egui::TextureId::User(7), [800, 600])));
    assert!(matches!(w, Wash::Ready(..)));
    assert!(w.theme_changing(&ctx, true), "then the new theme, under the copy");
    assert!(matches!(w, Wash::Running { .. }));
    // A copy that failed, or none made (no runner draws), lets the change through.
    let mut w = Wash::default();
    w.theme_changing(&ctx, true);
    w.copied(None);
    assert!(w.theme_changing(&ctx, true) && !w.wanted());
    let mut w = Wash::default();
    w.theme_changing(&ctx, true);
    assert!(w.theme_changing(&ctx, true), "asked and not answered");
    assert!(Wash::default().theme_changing(&ctx, false), "motion off: at once");
}

#[test]
fn popups_stay_solid_without_the_blur() {
    let c = egui::Color32::from_rgb(40, 41, 42);
    assert_eq!(crate::frost::fill(c), c, "no graphics card here: nothing to blur, no see-through");
}

#[test]
fn a_font_file_shows_itself() {
    let font = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/fallback-sans.ttf");
    let tile = crate::fontview::tile(&font, 96).expect("a tile");
    assert_eq!((tile.w, tile.h), (96, 96));
    let at = |x: u32, y: u32| &tile.px[((y * 96 + x) * 4) as usize..((y * 96 + x) * 4 + 4) as usize];
    assert_eq!(at(0, 0)[3], 0, "rounded corners");
    assert_eq!(at(48, 4), &[0xf4, 0xf1, 0xea, 255], "paper");
    // The letters, dark, in the middle of the card, none at its edges.
    let ink = |x0: u32, x1: u32, y0: u32, y1: u32| (y0..y1).flat_map(|y| (x0..x1).map(move |x| (x, y))).filter(|&(x, y)| at(x, y)[0] < 128).count();
    assert!(ink(16, 80, 16, 80) > 150, "the letters are drawn: {}", ink(16, 80, 16, 80));
    assert_eq!(ink(0, 96, 0, 8) + ink(0, 96, 88, 96), 0, "none at the edges");
    let entry = crate::fsx::Entry::from_path(&font).unwrap();
    assert!(crate::thumbs::can_thumb(&entry));
    let d = tmpdir("not-a-font");
    touch(&d.join("broken.ttf"), "not a font");
    assert!(crate::fontview::tile(&d.join("broken.ttf"), 96).is_none());
}

#[test]
fn a_refused_name_says_why() {
    let d = tmpdir("say-no");
    touch(&d.join("a.txt"), "a");
    touch(&d.join("b.txt"), "b");
    let mut r = Rename::new(vec![(0, d.join("a.txt"), "a.txt".into())], false);
    let mut say = |text: &str| {
        r.select_all();
        r.insert_all(text);
        r.reason()
    };
    assert_eq!(say("b.txt").as_deref(), Some("“b.txt” is already here"));
    assert_eq!(say("").as_deref(), Some("A name can't be empty"));
    assert_eq!(say("..").as_deref(), Some("“..” is the folder itself"));
    assert_eq!(say("c.txt"), None);
    assert_eq!(say("a.txt"), None, "its own name is not taken");
}

/// A picture decoded for the listing comes with its colours: the five it is made of and
/// the sixteen the glow is drawn from, worked out on the thumbnail workers.
#[test]
fn thumbnails_carry_their_colours() {
    let d = tmpdir("thumb-colours");
    let png = d.join("sunset.png");
    {
        let f = std::fs::File::create(&png).unwrap();
        let mut e = png::Encoder::new(std::io::BufWriter::new(f), 64, 48);
        e.set_color(png::ColorType::Rgba);
        e.set_depth(png::BitDepth::Eight);
        // Orange sky over a dark blue sea.
        let px: Vec<u8> = (0..64 * 48).flat_map(|i| if i / 64 < 30 { [240, 140, 40, 255] } else { [20, 40, 90, 255] }).collect();
        e.write_header().unwrap().write_image_data(&px).unwrap();
    }
    let ctx = egui::Context::default();
    let mut thumbs = crate::thumbs::Thumbs::new(ctx.clone(), 1);
    let mtime = std::fs::metadata(&png).unwrap().modified().ok();
    let t0 = Instant::now();
    let (palette, glow) = loop {
        ctx.run_pass(egui::RawInput::default(), |ctx| {
            thumbs.pump(ctx);
        }).drop_without_applying_deltas();
        if let Some(l) = thumbs.get(&png, mtime, 64.0) {
            break (l.palette.clone(), l.glow);
        }
        assert!(t0.elapsed() < Duration::from_secs(10) && !thumbs.failed(&png), "never decoded");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(palette.len(), 2, "{palette:?}");
    assert_eq!(crate::colours::hex(palette[0].0), "#F08C28", "the sky, the larger share");
    assert!((palette[0].1 - 30.0 / 48.0).abs() < 0.05);
    assert!(glow[0].r() > glow[0].b() && glow[15].b() > glow[15].r(), "sky above, sea below");
    let _ = std::fs::remove_dir_all(&d);
}

/// The glow behind a picture: one mesh of six by six points around it, clear at its
/// outer edge, nothing at all for a picture with no colour or no size.
#[test]
fn a_glow_fades_to_nothing_at_its_edge() {
    let ctx = egui::Context::default();
    let cells = [egui::Color32::from_rgb(200, 80, 40); 16];
    let pic = egui::Rect::from_min_size(egui::pos2(100.0, 100.0), egui::vec2(200.0, 100.0));
    let mut out = ctx.run_pass(egui::RawInput::default(), |ctx| {
        let p = ctx.layer_painter(egui::LayerId::background());
        crate::colours::paint_glow(&p, pic, &cells, 0.5);
        crate::colours::paint_glow(&p, pic, &[egui::Color32::TRANSPARENT; 16], 0.5);
        crate::colours::paint_glow(&p, egui::Rect::from_min_size(pic.min, egui::Vec2::ZERO), &cells, 0.5);
    });
    out.textures_delta.clear();
    let meshes: Vec<&egui::epaint::Mesh> = out.shapes.iter().filter_map(|s| if let egui::Shape::Mesh(m) = &s.shape { Some(&**m) } else { None }).collect();
    assert_eq!(meshes.len(), 1, "only the picture with colour and size glows");
    let m = meshes[0];
    assert_eq!((m.vertices.len(), m.indices.len()), (36, 5 * 5 * 6));
    let outer = m.vertices.iter().filter(|v| !pic.expand(1.0).contains(v.pos));
    assert!(outer.clone().count() == 20 && outer.clone().all(|v| v.color.a() == 0), "the ring outside the picture is clear");
    assert!(m.vertices.iter().filter(|v| pic.expand(1.0).contains(v.pos)).all(|v| v.color.a() > 100));
    let spread = m.vertices.iter().map(|v| v.pos.x).fold(f32::MAX, f32::min);
    assert!(spread < pic.left() - 20.0 && spread > pic.left() - 240.0, "a soft edge, not the whole window: {spread}");
}

#[test]
fn fonts_it_cannot_read_show_their_icon() {
    let d = tmpdir("fonts-odd");
    touch(&d.join("web.woff2"), "wOF2 not really");
    touch(&d.join("empty.otf"), "");
    for f in ["web.woff2", "empty.otf"] {
        assert!(crate::fontview::tile(&d.join(f), 64).is_none(), "{f}");
    }
}


// ------------------------------------------------------------------ the redesign

/// Dates said against today, on this machine's clock and time zone; and a saved config
/// from before version 2 gives up the old default pattern but keeps one chosen on purpose.
#[test]
fn dates_said_against_today_and_the_old_default_migrated() {
    use crate::timefmt::{parse_local, relative};
    let now = parse_local("2026-10-28T17:10:00").unwrap();
    let at = |s: &str| relative(parse_local(s).unwrap(), now);
    assert_eq!(at("2026-10-28T09:05:00"), "Today 09:05");
    assert_eq!(at("2026-10-27T23:59:00"), "Yesterday 23:59");
    // Wednesday the 22nd, within the week: its weekday (C locale names).
    assert_eq!(at("2026-10-22T22:57:00"), format!("{} 22:57", crate::timefmt::format(parse_local("2026-10-22T12:00:00").unwrap(), "%a")));
    assert_eq!(at("2026-10-21T08:00:00"), format!("21 {}", crate::timefmt::format(parse_local("2026-10-21T12:00:00").unwrap(), "%b")), "a week ago: the day and month");
    assert_eq!(at("2026-03-12T08:00:00"), format!("12 {}", crate::timefmt::format(parse_local("2026-03-12T12:00:00").unwrap(), "%b")));
    assert_eq!(at("2025-03-12T08:00:00"), format!("12 {} 2025", crate::timefmt::format(parse_local("2025-03-12T12:00:00").unwrap(), "%b")), "another year says it");
    // Empty pattern: relative; a pattern: as written.
    let t = parse_local("2025-03-12T08:00:00").unwrap();
    assert_eq!(crate::timefmt::date(t, "%Y-%m-%d"), "2025-03-12");
    assert!(crate::timefmt::date(t, "").starts_with("12 "));
    assert_eq!(crate::config::Config::default().date_format, "", "relative dates by default");
    let mut old: crate::config::Config = serde_json::from_str(r#"{"version":1,"date_format":"%x %H:%M"}"#).unwrap();
    crate::config::migrate(&mut old);
    assert_eq!((old.date_format.as_str(), old.version), ("", crate::config::CONFIG_VERSION));
    let mut chosen: crate::config::Config = serde_json::from_str(r#"{"version":1,"date_format":"%Y-%m-%d"}"#).unwrap();
    crate::config::migrate(&mut chosen);
    assert_eq!(chosen.date_format, "%Y-%m-%d", "a choice stays");
    let mut new: crate::config::Config = serde_json::from_str(r#"{"version":2,"date_format":"%x %H:%M"}"#).unwrap();
    crate::config::migrate(&mut new);
    assert_eq!(new.date_format, "%x %H:%M", "chosen after the change: kept");
}

/// Sizes in thousands, as drives and downloads are labelled, one decimal under 100.
#[test]
fn sizes_counted_in_thousands() {
    use crate::fsx::fmt_size;
    assert_eq!(fmt_size(999), "999 B");
    assert_eq!(fmt_size(1000), "1.0 KB");
    assert_eq!(fmt_size(1_500), "1.5 KB");
    assert_eq!(fmt_size(99_940), "99.9 KB");
    assert_eq!(fmt_size(123_456), "123 KB");
    assert_eq!(fmt_size(999_960), "1.0 MB", "not 1000 KB");
    assert_eq!(fmt_size(4_700_000_000), "4.7 GB", "a 4.7 GB film");
    assert_eq!(fmt_size(1_000_204_886_016), "1.0 TB", "a 1 TB drive");
    assert_eq!(crate::query::size("10M"), Some(10_000_000));
    assert_eq!(crate::query::size("1.5k"), Some(1_500));
}

/// Lays out `f` in one pass of a real context with Maia's fonts and this machine's.
fn in_a_frame<R>(f: impl FnOnce(&mut egui::Ui) -> R) -> (R, Vec<egui::epaint::ClippedShape>) {
    let ctx = egui::Context::default();
    crate::theme::install_fonts(&ctx, "", "");
    ctx.run_pass(egui::RawInput::default(), |_| {}).drop_without_applying_deltas();
    let mut out = None;
    let mut f = Some(f);
    let mut full = ctx.run_pass(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() }, |ctx| {
        ctx.root_ui(|root| {
            egui::CentralPanel::default().show(root, |ui| {
                if let Some(f) = f.take() {
                    out = Some(f(ui));
                }
            });
        });
    });
    full.textures_delta.clear();
    (out.unwrap(), full.shapes)
}

/// Names in the icon views break at spaces and punctuation, never inside the extension;
/// a word too long goes before the extension's dot; past the rows, the middle gives way.
#[test]
fn names_wrap_between_words() {
    let font = egui::FontId::proportional(13.0);
    let ((a, b, c, d, e), _) = in_a_frame(|ui| {
        let w = |name: &str, width: f32, rows: usize| crate::listing::wrap_name(ui, name, &font, width, rows);
        let width_of = |s: &str| ui.painter().layout_no_wrap(s.into(), font.clone(), egui::Color32::WHITE).size().x;
        // Room for "trip سفر" and no more: the break at the space, the extension whole.
        let room = width_of("trip سفر ").max(width_of("2026.png")) + 2.0;
        (w("trip سفر 2026.png", room, 2).text.clone(), w("report.pdf", width_of("report.pd") + 1.0, 2).text.clone(), w("a very long report about the quarterly numbers and more.pdf", width_of("a very long rep"), 2).text.clone(), w("letter.odt", 200.0, 2).text.clone(), w("x_y-z_long_name.tar", width_of("x_y-z_lo"), 2).text.clone())
    });
    assert_eq!(a, "trip سفر\n2026.png", "broken at the space");
    assert_eq!(b, "report\n.pdf", "a word too long for the line breaks before the extension's dot");
    let lines: Vec<&str> = c.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains('…') && lines[1].ends_with(".pdf") && !lines[1].contains(".p\n"), "the middle gives way, the extension stays: {c:?}");
    assert_eq!(d, "letter.odt", "room enough: one line");
    assert!(e.lines().next().is_some_and(|l| l.ends_with('_') || l.ends_with('-')), "breaks after punctuation: {e:?}");
    assert!(e.ends_with(".tar"));
}

/// The Type column's kinds, short: from shared-mime-info's descriptions on this machine
/// by one rule and a few overrides; and sorting by type goes by what is shown.
#[test]
fn short_kinds_and_sorting_by_them() {
    use crate::mime::short_kind;
    assert_eq!(short_kind("text/plain", "Plain text document"), "Text");
    assert_eq!(short_kind("text/markdown", "Markdown document"), "Markdown");
    assert_eq!(short_kind("text/csv", "CSV document"), "Spreadsheet");
    assert_eq!(short_kind("application/pdf", "PDF document"), "PDF");
    assert_eq!(short_kind("text/rust", "Rust source code"), "Rust source");
    assert_eq!(short_kind("image/png", "PNG image"), "PNG image");
    assert_eq!(short_kind("audio/mpeg", "MP3 audio"), "MP3 audio");
    assert_eq!(short_kind("x/y", "Disk image (raw)"), "Disk image");
    let db = crate::mime::mime_db();
    if db.comment("text/plain").is_empty() {
        eprintln!("no shared-mime-info here: the real lookups skipped");
        return;
    }
    assert_eq!(db.short("notes.txt", "txt", false, false), "Text");
    assert_eq!(db.short("Docs", "", true, false), "Folder");
    let d = tmpdir("kinds");
    for n in ["notes.txt", "readme.md", "photo.png", "table.csv", "zz.pdf"] {
        std::fs::write(d.join(n), "x").unwrap();
    }
    std::fs::create_dir(d.join("Folder A")).unwrap();
    let mut t = Tab::new(91, d.clone(), ViewSettings { sort: SortKey::Type, ..Default::default() }, egui::Context::default());
    pump(&mut t, &HashMap::new());
    let order: Vec<(String, String)> = t.view.iter().map(|&i| (t.entries[i as usize].name.clone(), crate::listing::type_text(&t.entries[i as usize], &t))).collect();
    let kinds: Vec<&str> = order.iter().map(|o| o.1.as_str()).collect();
    assert_eq!(kinds, ["Folder", "Markdown", "PDF", "PNG image", "Spreadsheet", "Text"], "{order:?}");
}

/// The high-contrast pair: text and borders at full strength, everything that is read at
/// 7:1 or more against where it sits, the selection's text readable on it.
#[test]
fn high_contrast_themes_reach_seven_to_one() {
    use crate::theme::{ThemeName, palette};
    assert!(ThemeName::ALL.contains(&ThemeName::HighContrastDark) && ThemeName::ALL.contains(&ThemeName::HighContrastLight));
    for (t, ink) in [(ThemeName::HighContrastDark, egui::Color32::WHITE), (ThemeName::HighContrastLight, egui::Color32::BLACK)] {
        let p = palette(t);
        assert!(p.strong, "{t:?}: a thick cursor ring");
        assert_eq!((p.text, p.border), (ink, ink), "{t:?}: text and borders at full strength");
        for (what, c) in [("dim text", p.text_dim), ("danger", p.danger), ("warning", p.warn), ("accent", p.accent), ("keyword", p.syntax.keyword), ("type", p.syntax.kind), ("string", p.syntax.string), ("comment", p.syntax.comment), ("number", p.syntax.number), ("function", p.syntax.function), ("property", p.syntax.property), ("meta", p.syntax.meta)] {
            for (on, bg) in [("panel", p.panel), ("sidebar", p.sidebar), ("popup", p.raised)] {
                let r = contrast(c, bg);
                assert!(r >= 7.0, "{t:?}: {what} on the {on} is {r:.1}:1");
            }
        }
        assert!(contrast(p.on_sel(), p.selected) >= 7.0, "{t:?}: text on the selection");
        assert!(contrast(p.on_accent(), p.accent) >= 4.5, "{t:?}: text on a main button");
    }
    assert_eq!(palette(ThemeName::HighContrastDark).on_sel(), egui::Color32::BLACK, "black on the yellow");
    assert!(!palette(ThemeName::Midnight).strong);
}

/// Which package holds a missing tool, by the family /etc/os-release names.
#[test]
fn install_commands_by_family() {
    use crate::needs::{Family, command, family_of, package, tool_in};
    assert_eq!(family_of("NAME=\"CachyOS Linux\"\nID=cachyos\nID_LIKE=arch\n"), Family::Arch);
    assert_eq!(family_of("ID=\"ubuntu\"\nID_LIKE=debian\n"), Family::Debian);
    assert_eq!(family_of("ID=pop\nID_LIKE=\"ubuntu debian\"\n"), Family::Debian, "known by its ID_LIKE");
    assert_eq!(family_of("ID=fedora\n"), Family::Fedora);
    assert_eq!(family_of("ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\n"), Family::Suse);
    assert_eq!(family_of("ID=nixos\n"), Family::Other);
    assert_eq!(family_of(""), Family::Other);
    assert_eq!(family_of("ID_LIKE=arch\nID=somethingnew\n"), Family::Arch, "ID first, then what it is like");
    assert_eq!([Family::Arch, Family::Debian, Family::Fedora, Family::Suse].map(|f| package("bsdtar", f)), [Some("libarchive"), Some("libarchive-tools"), Some("bsdtar"), Some("bsdtar")]);
    assert_eq!(command("bsdtar", Family::Debian).as_deref(), Some("sudo apt install libarchive-tools"));
    assert_eq!(command("ffmpeg", Family::Fedora).as_deref(), Some("sudo dnf install ffmpeg-free"));
    assert_eq!(command("exiftool", Family::Suse).as_deref(), Some("sudo zypper install exiftool"));
    assert_eq!(command("ImageMagick", Family::Arch).as_deref(), Some("sudo pacman -S imagemagick"));
    assert_eq!(command("bsdtar", Family::Other), None, "unknown system: the name alone");
    assert_eq!(tool_in("Sending folders needs bsdtar"), Some("bsdtar"));
    assert_eq!(tool_in("Encrypted 7z archives need 7z"), Some("7z"));
    assert_eq!(tool_in("Compare needs two panels"), None, "not a tool");
    assert_eq!(tool_in("Copied notes.txt"), None);
}

/// Toasts that offer something: how to install what a message says is missing, and an
/// Undo that goes once another change is made.
#[test]
fn toasts_offer_install_and_undo() {
    let d = tmpdir("toasts");
    let mut app = crate::app::App::new(&egui::Context::default(), vec![d.clone()]);
    app.toast("Sending folders needs bsdtar");
    assert_eq!(app.toast_tool, Some("bsdtar"));
    app.toast("Copied notes.txt");
    assert_eq!(app.toast_tool, None, "a plain toast has no action");
    app.undo.push(crate::app::UndoEntry { label: "Create a".into(), kind: crate::app::UndoKind::Created(vec![d.join("a")]) });
    app.toast_undoable("Created a");
    assert!(app.toast_undo_live(), "Undo offered for the newest change");
    app.undo.push(crate::app::UndoEntry { label: "Create b".into(), kind: crate::app::UndoKind::Created(vec![d.join("b")]) });
    assert!(!app.toast_undo_live(), "something else since: no Undo, it would take back b");
    app.toast_undoable("Created b");
    let _ = app.undo.pop();
    assert!(!app.toast_undo_live(), "undone already");
    app.toast("Something else");
    assert_eq!(app.toast_undo, None);
    use crate::ops::{OpKind, Progress};
    let mut p = Progress::default();
    let src = vec![d.join("a.txt"), d.join("b.txt"), d.join("c.txt")];
    assert_eq!(crate::ui::undo_note(&OpKind::Trash { paths: src.clone() }, &p).as_deref(), Some("3 items moved to Trash"));
    p.errors.push("busy".into());
    assert_eq!(crate::ui::undo_note(&OpKind::Trash { paths: src.clone() }, &p).as_deref(), Some("Moved 2 of 3 to Trash"));
    p.errors.clear();
    p.produced = vec![(src[0].clone(), d.join("Invoices/a.txt")), (src[1].clone(), d.join("Invoices/b.txt"))];
    assert_eq!(crate::ui::undo_note(&OpKind::Move { srcs: src[..2].to_vec(), dest: d.join("Invoices"), decide: Default::default() }, &p).as_deref(), Some("Moved 2 items to Invoices"));
    assert_eq!(crate::ui::undo_note(&OpKind::Copy { srcs: src.clone(), dest: d.join("Invoices"), decide: Default::default() }, &p).as_deref(), Some("Copied 2 of 3 to Invoices"));
    assert_eq!(crate::ui::undo_note(&OpKind::Trash { paths: vec![d.join("Beach.jpg")] }, &Progress::default()).as_deref(), Some("Beach.jpg moved to Trash"));
}

/// Tabs sharing a title say where they are; parents that match too are climbed past;
/// unique names, smart folders and places stay as they are.
#[test]
fn same_named_tabs_tell_themselves_apart() {
    let d = tmpdir("tabnames");
    for p in ["maia/src", "notes/src", "a/proj/Downloads", "b/proj/Downloads", "Invoices"] {
        std::fs::create_dir_all(d.join(p)).unwrap();
    }
    let ctx = egui::Context::default();
    let mut app = crate::app::App::new(&ctx, vec![d.join("maia/src")]);
    let id = app.active;
    let mut add = |app: &mut crate::app::App, p: PathBuf, n: u64| {
        let t = Tab::new(9000 + n, p, ViewSettings::default(), ctx.clone());
        app.panels.get_mut(&id).unwrap().tabs.push(t);
    };
    add(&mut app, d.join("notes/src"), 1);
    add(&mut app, d.join("a/proj/Downloads"), 2);
    add(&mut app, d.join("b/proj/Downloads"), 3);
    add(&mut app, d.join("Invoices"), 4);
    add(&mut app, d.join("Invoices"), 5);
    add(&mut app, PathBuf::from(crate::tab::COMPUTER), 6);
    add(&mut app, PathBuf::from(crate::tab::COMPUTER), 7);
    let first = app.panels[&id].tabs[0].id;
    let h = app.tab_hints();
    assert_eq!(h.get(&first).map(String::as_str), Some("maia"));
    assert_eq!(h.get(&9001).map(String::as_str), Some("notes"));
    assert_eq!(h.get(&9002).map(String::as_str), Some("a"), "proj matched: one further up");
    assert_eq!(h.get(&9003).map(String::as_str), Some("b"));
    assert!(!h.contains_key(&9004) && !h.contains_key(&9005), "the same folder twice: nothing to tell");
    assert!(!h.contains_key(&9006) && !h.contains_key(&9007), "Computer keeps its name");
    assert_eq!(h.len(), 4);
    // Worked out again only when the tabs change.
    assert!(std::sync::Arc::ptr_eq(&h, &app.tab_hints()));
    app.panels.get_mut(&id).unwrap().tabs.remove(1);
    assert!(!app.tab_hints().contains_key(&first), "the other src gone: maia's src is unique again");
}

/// The window's title names the folder shown.
#[test]
fn window_title_names_the_folder() {
    assert_eq!(crate::app::window_title("Invoices"), "Invoices · Maia");
    assert_eq!(crate::app::window_title("Computer"), "Computer · Maia");
    assert_eq!(crate::app::window_title(""), "Maia");
    assert_eq!(crate::app::window_title("a\0b"), "ab · Maia", "no NUL for X11's title property");
    let d = tmpdir("wintitle");
    let mut app = crate::app::App::new(&egui::Context::default(), vec![d.clone()]);
    assert_eq!(crate::app::window_title(&app.tab().title()), format!("{} · Maia", d.file_name().unwrap().to_string_lossy()));
}

/// A drive says it is filling only from 80%, in red from 95%.
#[test]
fn drive_meter_thresholds() {
    use crate::sidebar::fullness;
    assert_eq!(fullness(0.0), None);
    assert_eq!(fullness(0.79), None);
    assert_eq!(fullness(0.80), Some(false));
    assert_eq!(fullness(0.949), Some(false));
    assert_eq!(fullness(0.95), Some(true));
    assert_eq!(fullness(1.0), Some(true));
}

/// A search row's glyph sits on its letters' middle, with this machine's own font: on the
/// hint's while it is empty, on what is typed after.
#[test]
fn search_row_glyph_on_the_letters_middle() {
    for size in [13.0f32, 15.0, 16.5, 20.0] {
        for typed in ["", "Hunt"] {
            let pal = crate::theme::palette(crate::theme::ThemeName::Midnight);
            let mut text = typed.to_string();
            let (_, shapes) = in_a_frame(|ui| crate::ui::search_row(ui, &mut text, "Hunt", egui::FontId::proportional(size), crate::theme::Glyph::Search, 14.0, false, &pal));
            let mut glyph = None;
            let mut letters = None;
            fn walk(s: &egui::Shape, glyph: &mut Option<egui::Rect>, letters: &mut Option<f32>) {
                match s {
                    egui::Shape::Vec(v) => v.iter().for_each(|x| walk(x, glyph, letters)),
                    egui::Shape::Mesh(m) if glyph.is_none() => *glyph = Some(m.calc_bounds()),
                    egui::Shape::Text(t) if t.galley.text() == "Hunt" => {
                        // The ink of the capital H: its top and its baseline.
                        let row = &t.galley.rows[0];
                        let g = &row.row.glyphs[0];
                        let top = t.pos.y + row.pos.y + g.pos.y + g.uv_rect.offset.y;
                        *letters = Some(top + g.uv_rect.size.y / 2.0);
                    }
                    _ => {}
                }
            }
            for c in &shapes {
                walk(&c.shape, &mut glyph, &mut letters);
            }
            let (g, mid) = (glyph.expect("the glyph drawn"), letters.expect("the letters drawn"));
            assert!((g.center().y - mid).abs() <= 1.0, "at {size}, {typed:?} typed: glyph centre {} against the letters' middle {mid}", g.center().y);
        }
    }
}
