#!/usr/bin/python3
"""Collect license/notice files for every resolved non-workspace Rust crate."""

from __future__ import annotations

import json
import os
import shutil
import subprocess
from pathlib import Path


root = Path(__file__).resolve().parent.parent
host = os.environ.get("CARGO_BUILD_TARGET")
if not host:
    host = next(line.split(": ", 1)[1] for line in subprocess.check_output(["rustc", "-vV"], text=True).splitlines() if line.startswith("host: "))
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", host], cwd=root
))
resolved = {node["id"] for node in metadata["resolve"]["nodes"]}
destination = root / "target" / "third-party-licenses"
shutil.rmtree(destination, ignore_errors=True)
destination.mkdir(parents=True)
index: list[str] = []

for package in sorted(metadata["packages"], key=lambda item: (item["name"], item["version"])):
    if package["id"] not in resolved or package.get("source") is None:
        continue
    source = Path(package["manifest_path"]).parent
    target = destination / f'{package["name"]}-{package["version"]}'
    notices = [
        path for path in source.iterdir()
        if path.is_file() and path.name.upper().startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE"))
    ]
    if not notices:
        raise SystemExit(f"No license file found for {package['name']} {package['version']}")
    target.mkdir()
    for notice in notices:
        shutil.copyfile(notice, target / notice.name)
    index.append(
        f"{package['name']} {package['version']} | {package.get('license') or 'see included files'} | "
        f"{package.get('repository') or package.get('homepage') or 'crates.io'}"
    )

(destination / "INDEX.txt").write_text("\n".join(index) + "\n", encoding="utf-8")
