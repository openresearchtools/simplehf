# SimpleHF

SimpleHF is a small GTK 4/libadwaita application for selectively downloading
Hugging Face model repositories. It is designed around the familiar torrent
client workflow: inspect the complete indented tree, select files or folders,
add the selection to a queue, then inspect overall and per-file progress in a
lower details pane.

## Features

- Search Hugging Face models or open `organization/model` directly.
- In-memory Hugging Face token support for private and gated repositories.
- Expandable repository folder tree with file/folder checkboxes and All/None.
- Original filenames and formats—downloads use the Hub `resolve` endpoint.
- Exact repository structure under `<chosen folder>/<organization>/<model>/`.
- Concurrent ranged downloads across files, adaptive chunks, retry backoff,
  resumable `.part` files, and safe path validation.
- Repository-level and per-file status, byte progress, and live speed display.
- Native `.deb` package, artifact-only test builds, and tagged-release GitHub Actions workflows.

## Run locally

SimpleHF is native Rust. Building requires GTK 4 and libadwaita development
headers; running requires only their normal shared-library packages:

```sh
sudo apt install cargo rustc libgtk-4-dev libadwaita-1-dev
cargo run --bin simplehf
```

## Build the Debian package

```sh
sudo apt install debhelper cargo rustc libgtk-4-dev libadwaita-1-dev
dpkg-buildpackage --build=binary --no-sign
```

The package is written to the parent directory. GitHub Actions builds the same
artifact for every push and pull request and attaches it to `v*` releases.
Manual test builds can optionally upload the package as a 14-day workflow
artifact without creating a tag or GitHub release.

For a no-root local build on a machine that already has Cargo and `dpkg-deb`:

```sh
./scripts/build-deb.sh
```

This writes the installable package to `dist/`.

The release build targets the Ubuntu 22.04 GTK 4.6/libadwaita 1.1 baseline.
Newer Debian and Ubuntu releases dynamically use their own security-updated
GTK/libadwaita libraries; old copies are not bundled into the package.

## Authentication and privacy

Paste a Hugging Face read token into the password field before searching or
opening a gated/private model. The token is kept only in application memory,
passed to the worker through its private process environment, never written to
the manifest, configuration, logs, or command line, and disappears when the
application exits. You must separately accept a gated repository's terms on
Hugging Face.

## Licensing

SimpleHF is MIT licensed. The Rust engine derives its download architecture
from Johannes Bertens' MIT-licensed `rust-hf-downloader`; attribution and the
complete license are in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) and
[`licenses/`](licenses/). Rust dependency licenses are enforced in CI.
