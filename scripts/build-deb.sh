#!/bin/sh
set -eu

version=${SIMPLEHF_VERSION:-0.2.5}
architecture=${SIMPLEHF_ARCH:-$(dpkg --print-architecture)}
project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
stage_dir=$(mktemp -d "${TMPDIR:-/tmp}/simplehf-package.XXXXXX")
trap 'rm -rf "$stage_dir"' EXIT INT TERM

cd "$project_dir"
cargo build --release --locked
python3 scripts/collect-cargo-licenses.py

install -Dm755 target/release/simplehf-engine "$stage_dir/usr/libexec/simplehf-engine"
install -Dm755 target/release/simplehf "$stage_dir/usr/bin/simplehf"
install -Dm644 data/de.simplehf.SimpleHF.desktop "$stage_dir/usr/share/applications/de.simplehf.SimpleHF.desktop"
install -Dm644 data/de.simplehf.SimpleHF.metainfo.xml "$stage_dir/usr/share/metainfo/de.simplehf.SimpleHF.metainfo.xml"
install -Dm644 data/de.simplehf.SimpleHF.svg "$stage_dir/usr/share/icons/hicolor/scalable/apps/de.simplehf.SimpleHF.svg"
install -Dm644 debian/copyright "$stage_dir/usr/share/doc/simplehf/copyright"
install -Dm644 THIRD_PARTY_NOTICES.md "$stage_dir/usr/share/doc/simplehf/THIRD_PARTY_NOTICES.md"
cp -a licenses "$stage_dir/usr/share/doc/simplehf/"
cp -a target/third-party-licenses "$stage_dir/usr/share/doc/simplehf/cargo-licenses"
chmod 0755 "$stage_dir"
chmod -R go-w "$stage_dir"

installed_size=$(du -sk "$stage_dir" | cut -f1)
install -d "$stage_dir/DEBIAN"
cat >"$stage_dir/DEBIAN/control" <<EOF
Package: simplehf
Version: $version
Section: net
Priority: optional
Architecture: $architecture
Installed-Size: $installed_size
Maintainer: openresearchtools <openresearchtools@users.noreply.github.com>
Depends: libgtk-4-1 (>= 4.6), libadwaita-1-0 (>= 1.1)
Homepage: https://github.com/openresearchtools/simplehf
Description: selective Hugging Face model repository downloader
 Browse complete model repositories as expandable trees, select individual
 files or folders, and inspect overall and per-file download progress.
EOF

mkdir -p dist
dpkg-deb --root-owner-group --build "$stage_dir" "dist/simplehf_${version}_${architecture}.deb"
