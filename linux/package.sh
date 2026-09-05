#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
set -euo pipefail
action="${1:-all}"
case "$action" in deb|rpm|all) ;; *) echo "usage: $0 [deb|rpm|all]" >&2; exit 2 ;; esac
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/dist-linux"
mkdir -p "$out"
command -v cargo >/dev/null || { echo "cargo is required (Rust 1.88+)" >&2; exit 2; }
if command -v desktop-file-validate >/dev/null; then
  desktop-file-validate "$root/linux/io.github.shine911.aoostar.desktop"
fi
cargo build --release -p aster-launcher -p asterctl -p aster-sysinfo
case "$action" in
  deb|all)
    command -v cargo-deb >/dev/null || { echo "cargo-deb is required: cargo install cargo-deb" >&2; exit 2; }
    command -v dpkg-shlibdeps >/dev/null || { echo "dpkg-shlibdeps is required (install dpkg-dev)" >&2; exit 2; }
    cargo deb -p aster-launcher --no-build --output "$out/aoostar-rs.deb" ;;
esac
case "$action" in
  rpm|all)
    command -v cargo-generate-rpm >/dev/null || { echo "cargo-generate-rpm is required: cargo install cargo-generate-rpm" >&2; exit 2; }
    cargo generate-rpm -p "$root/crates/aster-launcher" -o "$out/aoostar-rs.rpm" ;;
esac
echo "Linux packages are in $out"
