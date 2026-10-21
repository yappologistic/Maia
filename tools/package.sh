#!/bin/sh
# Package a built Maia (dist/maia) for Linux distributions, into OUT:
#   maia-VERSION-linux-ARCH.tar.gz   the program, launcher entry, icon and an install.sh
#   maia_VERSION_DEBARCH.deb         Debian, Ubuntu, Mint, Pop!_OS and the like
#   maia-VERSION-1.ARCH.rpm          Fedora, openSUSE, RHEL and the like (needs rpmbuild)
#   maia-VERSION-1-ARCH.pkg.tar.zst  Arch Linux and the like (needs zstd)
#   Maia-VERSION-ARCH.AppImage       runs anywhere with FUSE (needs appimagetool)
# Usage: tools/package.sh VERSION ARCH [OUT]   ARCH is x86_64 or aarch64.
# A format whose tool is missing is skipped with a note; CI has them all.
set -eu
V=$1; A=$2; OUT=${3:-out}
# pacman versions can't hold a hyphen (1.0.0-rc1 is 1.0.0_rc1); file names keep the tag's.
PV=$(printf '%s' "$V" | tr - _)
C=$(dirname "$(dirname "$(readlink -f "$0")")")
BIN=$C/dist/maia
ID=io.github.yappologistic.Maia
[ -x "$BIN" ] || { echo "no $BIN: run ./build-dist.sh first" >&2; exit 1; }
case "$A" in
  x86_64) DEB=amd64 ;;
  aarch64) DEB=arm64 ;;
  *) echo "unknown arch $A" >&2; exit 1 ;;
esac
mkdir -p "$OUT"
OUT=$(readlink -f "$OUT")
W=$(mktemp -d)
trap 'rm -rf "$W"' EXIT

# The tree every package installs, under a prefix.
tree() {
  install -Dm755 "$BIN" "$1/bin/maia"
  install -Dm644 "$C/packaging/$ID.desktop" "$1/share/applications/$ID.desktop"
  install -Dm644 "$C/assets/maia.png" "$1/share/icons/hicolor/256x256/apps/$ID.png"
  install -Dm644 "$C/packaging/$ID.metainfo.xml" "$1/share/metainfo/$ID.metainfo.xml"
  install -Dm644 "$C/LICENSE" "$1/share/licenses/maia/LICENSE"
  install -Dm644 "$C/assets/THIRD-PARTY-NOTICES.txt" "$1/share/licenses/maia/THIRD-PARTY-NOTICES.txt"
}

# The Open and Save dialog's backend, for the system packages: xdg-desktop-portal before
# 1.20.1 reads backends only from /usr/share. It is used once portals.conf names it
# (maia --set-picker, or the Options row).
portal_files() {
  mkdir -p "$1/share/xdg-desktop-portal/portals" "$1/share/dbus-1/services"
  printf '[portal]\nDBusName=org.freedesktop.impl.portal.desktop.maia\nInterfaces=org.freedesktop.impl.portal.FileChooser;\n' > "$1/share/xdg-desktop-portal/portals/maia.portal"
  printf '[D-BUS Service]\nName=org.freedesktop.impl.portal.desktop.maia\nExec=/usr/bin/maia --portal\n' > "$1/share/dbus-1/services/org.freedesktop.impl.portal.desktop.maia.service"
  chmod 644 "$1/share/xdg-desktop-portal/portals/maia.portal" "$1/share/dbus-1/services/org.freedesktop.impl.portal.desktop.maia.service"
}

# ---------------------------------------------------------------- tarball
T=$W/tar/maia-$V-linux-$A
tree "$T"
install -Dm644 "$C/README.md" "$T/README.md"
cat > "$T/install.sh" <<'SH'
#!/bin/sh
# Install Maia for this user (~/.local) or, as root, for everyone (/usr/local).
set -e
D=$(dirname "$(readlink -f "$0")")
if [ "$(id -u)" = 0 ]; then P=${PREFIX:-/usr/local}; else P=${PREFIX:-$HOME/.local}; fi
for f in bin/maia share/applications/io.github.yappologistic.Maia.desktop share/icons/hicolor/256x256/apps/io.github.yappologistic.Maia.png share/metainfo/io.github.yappologistic.Maia.metainfo.xml; do
  mkdir -p "$P/$(dirname "$f")"
  cp "$D/$f" "$P/$f.new" && mv -f "$P/$f.new" "$P/$f"
done
chmod 755 "$P/bin/maia"
# Earlier versions' SVG icon would win over the PNG at every size.
rm -f "$P/share/icons/hicolor/scalable/apps/io.github.yappologistic.Maia.svg"
touch "$P/share/icons/hicolor"
command -v update-desktop-database >/dev/null && update-desktop-database "$P/share/applications" 2>/dev/null || true
echo "Installed to $P/bin/maia"
SH
chmod 755 "$T/install.sh"
tar -C "$W/tar" --owner=0 --group=0 --numeric-owner -czf "$OUT/maia-$V-linux-$A.tar.gz" "maia-$V-linux-$A"
echo "tarball: maia-$V-linux-$A.tar.gz"

# ---------------------------------------------------------------- .deb
if command -v dpkg-deb >/dev/null; then
  R=$W/deb
  tree "$R/usr"
  portal_files "$R/usr"
  # Debian keeps licences under /usr/share/doc.
  mkdir -p "$R/usr/share/doc/maia"
  mv "$R/usr/share/licenses/maia/LICENSE" "$R/usr/share/doc/maia/copyright"
  mv "$R/usr/share/licenses/maia/THIRD-PARTY-NOTICES.txt" "$R/usr/share/doc/maia/"
  rm -r "$R/usr/share/licenses"
  mkdir -p "$R/DEBIAN"
  KB=$(du -sk "$R/usr" | cut -f1)
  cat > "$R/DEBIAN/control" <<EOF
Package: maia
Version: $V
Architecture: $DEB
Maintainer: yappologistic <yappologistic@users.noreply.github.com>
Installed-Size: $KB
Depends: libc6 (>= 2.35), libegl1, libxkbcommon0
Recommends: libwayland-client0, libx11-6, xdg-utils, libarchive-tools, git
Suggests: ffmpeg, poppler-utils, imagemagick, 7zip | p7zip-full, udisks2, gvfs, libreoffice-core
Section: utils
Priority: optional
Homepage: https://github.com/yappologistic/Maia
Description: Fast, keyboard-first file manager
 A file manager drawn on the GPU, with split panels, tabs, live filtering,
 previews, git history and staging, and background file operations.
EOF
  dpkg-deb --root-owner-group -Zxz --build "$R" "$OUT/maia_${V}_$DEB.deb" >/dev/null
  echo "deb: maia_${V}_$DEB.deb"
else
  echo "deb: skipped (no dpkg-deb)"
fi

# ---------------------------------------------------------------- .rpm
if command -v rpmbuild >/dev/null; then
  R=$W/rpm
  mkdir -p "$R/SOURCES" "$R/SPECS" "$R/BUILD" "$R/RPMS" "$R/SRPMS"
  tree "$R/SOURCES/usr"
  portal_files "$R/SOURCES/usr"
  # Libraries by the file they provide, so the names Fedora and openSUSE give their
  # packages don't matter; the rest Maia uses when it finds it.
  if [ "$A" = x86_64 ] || [ "$A" = aarch64 ]; then L="()(64bit)"; else L=""; fi
  cat > "$R/SPECS/maia.spec" <<EOF
%global debug_package %{nil}
%global __os_install_post %{nil}
%define _build_id_links none
Name: maia
Version: $V
Release: 1
Summary: Fast, keyboard-first file manager
License: GPL-3.0-or-later
URL: https://github.com/yappologistic/Maia
AutoReqProv: no
Requires: libc.so.6$L, libEGL.so.1$L, libxkbcommon.so.0$L
Recommends: /usr/bin/xdg-open, /usr/bin/bsdtar, /usr/bin/git
Suggests: /usr/bin/ffmpeg, /usr/bin/pdftoppm, /usr/bin/udisksctl

%description
A file manager drawn on the GPU, with split panels, tabs, live filtering,
previews, git history and staging, and background file operations.

%install
mkdir -p %{buildroot}
cp -a %{_sourcedir}/usr %{buildroot}/

%files
/usr/bin/maia
/usr/share/applications/$ID.desktop
/usr/share/icons/hicolor/256x256/apps/$ID.png
/usr/share/metainfo/$ID.metainfo.xml
/usr/share/xdg-desktop-portal/portals/maia.portal
/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.maia.service
%license /usr/share/licenses/maia/LICENSE
%license /usr/share/licenses/maia/THIRD-PARTY-NOTICES.txt
EOF
  rpmbuild --quiet -bb --define "_topdir $R" --target "$A-linux" "$R/SPECS/maia.spec"
  cp "$R"/RPMS/*/maia-*.rpm "$OUT/maia-$V-1.$A.rpm"
  echo "rpm: maia-$V-1.$A.rpm"
else
  echo "rpm: skipped (no rpmbuild)"
fi

# ---------------------------------------------------------------- Arch
# A pacman package is a tar of the tree and a .PKGINFO; pacman -U takes it as is.
if command -v zstd >/dev/null; then
  R=$W/arch
  tree "$R/usr"
  SZ=$(du -sb "$R/usr" | cut -f1)
  cat > "$R/.PKGINFO" <<EOF
pkgname = maia
pkgbase = maia
pkgver = $PV-1
pkgdesc = Fast, keyboard-first file manager
url = https://github.com/yappologistic/Maia
builddate = ${SOURCE_DATE_EPOCH:-$(date +%s)}
packager = Unknown Packager
size = $SZ
arch = $A
license = GPL-3.0-or-later
depend = glibc
depend = libglvnd
depend = libxkbcommon
optdepend = wayland: Wayland
optdepend = libx11: X11
optdepend = xdg-utils: open files with other programs
optdepend = libarchive: more archive formats
optdepend = git: git history and staging
optdepend = ffmpeg: convert sound and video
optdepend = imagemagick: convert pictures
optdepend = poppler: PDF previews
optdepend = udisks2: mount and format drives
optdepend = gvfs: network places
optdepend = libreoffice-fresh: Office document previews
EOF
  (cd "$R" && tar --owner=0 --group=0 --numeric-owner --sort=name -cf - .PKGINFO usr) \
    | zstd -q -19 -o "$OUT/maia-$V-1-$A.pkg.tar.zst"
  echo "pacman: maia-$V-1-$A.pkg.tar.zst"
else
  echo "pacman: skipped (no zstd)"
fi

# ---------------------------------------------------------------- AppImage
AIT=${APPIMAGETOOL:-$(command -v appimagetool || true)}
if [ -n "$AIT" ]; then
  R=$W/Maia.AppDir
  tree "$R/usr"
  cp "$C/packaging/$ID.desktop" "$R/$ID.desktop"
  cp "$C/assets/maia.png" "$R/$ID.png"
  ln -s "$ID.png" "$R/.DirIcon"
  ln -s usr/bin/maia "$R/AppRun"
  # Without FUSE (a CI runner) appimagetool, itself an AppImage, unpacks and runs.
  ARCH=$A APPIMAGE_EXTRACT_AND_RUN=1 "$AIT" --no-appstream "$R" "$OUT/Maia-$V-$A.AppImage" >/dev/null
  chmod 755 "$OUT/Maia-$V-$A.AppImage"
  echo "AppImage: Maia-$V-$A.AppImage"
else
  echo "AppImage: skipped (no appimagetool)"
fi
