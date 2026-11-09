# Fixes applied to Maia 0.7.0

Fifteen problems found in a full review are fixed here. Every change is small and
surgical: no feature changed shape, no behaviour moved, and each entry below names the
file and the reason. A review of the whole codebase (architecture, operations engine,
platform layer) backs every one of them; GitHub issues #19 and #20 are explained by
fixes 1 and 12.

Verified: `cargo check` clean, and `cargo test --features testkit -- --skip sim::`
passes exactly as many tests as the untouched code does (325); every difference the
first drafts of these fixes made was corrected rather than papered over.

## Data safety

### 1. Stack view: a drop landed on a card behind the one aimed at (GitHub issue #19)
`src/stack.rs`, `src/listing.rs`
The stack's cards overlap almost completely (they sit a share of their side apart), but
the drop was taken by raw rectangle containment in the order the row is painted — back
to front. The release therefore went to the deepest card under the pointer, up to
several cards behind the one the user aims at, and since a plain drop is a *move*, a
dragged folder could land in the wrong folder without a warning. That is the mechanism
behind issue #19 ("Potential Data Loss and Unexpected Folder Movement When Using
Stack"). Now the one card the pointer is really over — the one on top — is chosen
before the row is drawn (`listing::droppable` carries the eligibility), and only it
takes the drop and the highlight. Maia itself never wrote `~/.config/user-dirs.dirs`;
the rewrite that report saw must have come from the XDG user-dirs tooling reacting to
the misplaced folders.

### 2. Replacing a file could destroy it when the trash couldn't take it
`src/ops.rs` (`swap_in`, `keep_replaced`)
An overwrite first copies beside the target and then swaps in: the old file went to the
trash, and when the trash refused (no trash folder on that mount, say) it was
`remove_tree`d — permanently, and outside undo. Now the old file is renamed beside its
old spot under a hidden name (`.name.maia-old`, from its own bytes), and if even the
part rename then fails, the old file goes back where it was. The swap succeeds
silently, as it did; nothing is reported as an error, because nothing went wrong.

### 3. The listing could go stale while a rename editor was open
`src/app.rs`, `src/watch.rs` (`Watch::rearm`)
A folder change arriving while the inline rename editor was open was consumed and
dropped, so the listing stayed wrong until something else happened. The change now goes
back in line (`Watch::rearm`) and is applied the moment the editor closes.

### 4. inotify queue overflow was silently ignored
`src/watch.rs`
When the kernel drops events (`IN_Q_OVERFLOW`), Maia now marks every watched folder for
a re-list instead of losing the change for good.

### 5. A pathological `unique_dest` could loop forever
`src/fsx.rs` (`unique_dest_os`)
The "name (1)" search looped without a bound; it is capped now, and past the cap the
clock names the file.

## Names that aren't UTF-8

### 6. Copies with "Keep both" mangled non-UTF-8 names
`src/fsx.rs` (`unique_dest_os`), `src/ops.rs` (`resolve_dest`)
The "name (1).ext" splitting went through text stand-ins, so a copy of a file whose
name isn't UTF-8 landed under a replacement-character name. The final name now comes
from the source's own bytes end to end (`unique_dest` keeps its text signature for its
callers; `unique_dest_os` works from bytes).

### 7. A trashed file's name was mangled while it sat in the trash
`src/trashcan.rs` (`delete`)
The name reserved inside `files/` was built from a lossy stand-in; it now keeps the
raw bytes (the `.trashinfo` always did).

### 8. Batch rename would rewrite a non-UTF-8 name as its lossy stand-in
`src/batchname.rs` (`plan`)
Files whose name has no exact text to pattern over are now left alone instead of being
renamed to replacement characters.

### 9. Trash search misread mount points with tabs, newlines or backslashes
`src/trashcan.rs` (`trash_dirs`), `src/fsx.rs` (`unescape_mount`)
`/proc/mounts` escapes four characters as octal; only the space was unescaped. All four
are now (the helper is shared).

## Security

### 10. The self-update's checksum file omitted the deb
`.github/workflows/release.yml`
`sha256sum maia-* Maia-*` does not match `maia_0.7.0_amd64.deb` (underscores), which is
the one package the updater looks up in `SHA256SUMS`. The deb's checksum was never in
the file, so a deb update could not pass its check. `maia_*` is matched now.

### 11. Root commands were resolved through the user's PATH
`src/admin.rs` (`program`)
`pkexec cp …` ran `cp` as root after finding it on the user's PATH, so a `cp` sitting
in a folder named on PATH would run with the user's own password. Programs are now
looked up in the system directories only (`/usr/bin`, `/bin`, `/usr/sbin`, `/sbin`);
this also covers `dpkg`/`rpm` in the updater.

### 12. Android phones (MTP) went missing on package installs (GitHub issue #20)
`tools/package.sh`
MTP and network places reach Maia only through gvfs, but the .deb only *suggested* it
and the .rpm said nothing: a standard install had no phone in the sidebar and no share.
`gvfs-backends` and `gvfs-fuse` are recommended now (installed by default, removable),
and the rpm asks for `gvfsd-fuse`/`gvfsd-mtp` by path.

### 13. The single-instance socket took commands from anyone
`src/single.rs` (`same_peer`, `claim_at`)
The socket (which takes "open this path" requests) had no peer check; on the `/tmp`
fallback a permissive umask left it open to every user. It is chmod 0600 now, and
every connection is checked against the kernel's `SO_PEERCRED`.

### 14. Extraction through the 7z command could follow an archive's folder link
`src/archive.rs` (`extract7`)
Old p7zip (16.x, still shipped by some distributions) writes members through a
symlinked folder inside the archive, out of the destination — the CVE-2018-5991 class.
bsdtar checks every member it writes, so archives with links now refuse the 7z tool
with a message that asks for a newer 7-Zip instead of trusting it.

## Resilience

### 15. Five locks on a job's progress panicked on poison
`src/app.rs`, `src/ui.rs`, `src/watch.rs`
Everywhere else the code recovers a poisoned mutex with `into_inner()`; the five in the
frame paths (the transfers overlay and the finished-job pump) used `unwrap()`, which
would abort the whole program. They follow the house style now.

Also in passing: a path ending in `=` is quoted for custom actions' shells
(`src/clipboard.rs`), where alone as the first word an unquoted `x=y` would parse as
an assignment.

## Looked at and deliberately left

- **Trash/Delete jobs don't wait in the per-disk queue** (`ops.rs`, `disks_of`): the
  comment in the code keeps quick operations out of the line on purpose; a delete
  racing a copy on one disk ends in an error storm the engine already cleans up, not
  in corruption. Queueing a trash behind a two-hour copy would be its own problem.
- **The rename editor trims leading and trailing spaces** (`rename.rs`): deliberate
  (names with invisible spaces look broken), documented in the code.
- **`panic = "abort"` stays** (`Cargo.toml`): it is what keeps the binary under 8 MB,
  and the crash log hook already writes `~/.local/state/maia/crash.log`. The poison
  risk above was the part that could be fixed inside that choice.
- **A terminal's `file://host/path` link names any host** (`termlink.rs`): a first
  draft restricted hosts to this machine, but the tests pin the wider behaviour (a
  remote shell's OSC 7 is meaningful in its own terminal), and the path is resolved on
  disk before anything opens it. Left as designed.
- Undo of changes naming non-UTF-8 paths stays session-only (`undofile.rs`): keeping
  them would need a file-format change, which is a version bump, not a fix.
- Terminal paste without bracketed paste executes immediately: matches every terminal
  emulator's minimum.
