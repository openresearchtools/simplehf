# Third-party notices

SimpleHF's Rust download engine adapts the authenticated HTTP and adaptive
ranged-chunk architecture of
[rust-hf-downloader](https://github.com/JohannesBertens/rust-hf-downloader),
Copyright (c) Johannes Bertens, under the MIT License. The complete upstream
license is distributed in `licenses/rust-hf-downloader-MIT.txt`.

SimpleHF dynamically uses GTK 4 and libadwaita supplied by the operating
system. They are not copied into this repository or bundled into the Debian
package. Their copyright and license files remain available through the
corresponding Debian packages under `/usr/share/doc`. Python is used only at
package-build time to collect dependency license texts; the installed
application is entirely native Rust and does not require Python.

The Rust binary incorporates crates recorded in `Cargo.lock`. Their license
metadata is checked in CI with `cargo-deny`; accepted licenses are configured in
`deny.toml`. The package build also collects every resolved crate's license and
notice files under `/usr/share/doc/simplehf/cargo-licenses/`. No dependency
changes should be merged when either check fails.
