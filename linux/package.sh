#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
set -euo pipefail
action="${1:-all}"
case "$action" in deb|rpm|all) ;; *) echo "usage: $0 [deb|rpm|all]" >&2; exit 2 ;; esac
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$root/dist-linux"
if [[ -n "${APP_VERSION:-}" ]]; then
  raw_version="$APP_VERSION"
else
  command -v git >/dev/null || { echo "git is required to determine the package version (or set APP_VERSION)" >&2; exit 2; }
  raw_version="$(git -c safe.directory="$root" -C "$root" describe --match 'v[0-9]*' --tags HEAD --always)"
fi
if [[ -n "${PACKAGE_VERSION:-}" ]]; then
  package_version="$PACKAGE_VERSION"
elif [[ "$raw_version" == v[0-9]* ]]; then
  package_version="${raw_version#v}"
else
  package_version="0.0.0.git.$raw_version"
fi
package_version="${package_version//-/.}"
artifact_base="$out/aoostar-rs-${raw_version}-Linux-x64"
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
    cargo deb -p aster-launcher --no-build --deb-version "${package_version}-1" --output "${artifact_base}.deb" ;;
esac
case "$action" in
  rpm|all)
    command -v cargo-generate-rpm >/dev/null || { echo "cargo-generate-rpm is required: cargo install cargo-generate-rpm" >&2; exit 2; }
    cargo generate-rpm -p "$root/crates/aster-launcher" --set-metadata "version = \"${package_version}\"" -o "${artifact_base}.rpm" ;;
esac
echo "Linux packages version ${package_version} are in $out"
