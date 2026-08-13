#!/usr/bin/env bash
# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Build & test the aoostar-rs workspace from WSL.
#
# Usage:
#   ./build-from-wsl.sh [test|build|windows-check|windows-build]
#     test             (default) rustfmt check + unit tests for aster-launcher & aster-sysinfo
#     build            rustfmt check + debug build of aster-launcher & aster-sysinfo
#     windows-check    rustfmt check + `cargo check` of aster-launcher for x86_64-pc-windows-msvc
#                      (compiles the Windows-only tray/process code, which Linux tests skip)
#     windows-build    build the REAL deployable Windows binaries by calling the Windows
#                      toolchain directly from WSL (cargo.exe + csc.exe + powershell.exe)
#
# WHY this file exists / WSL gotchas it encodes (learned implementing refresh_time):
#   1. cargo/rustc ARE installed on this WSL host but NOT on PATH.
#        -> export PATH="$HOME/.cargo/bin:$PATH"
#   2. The WSL host has NO C compiler (cc/gcc missing) and no root/sudo, so a plain
#      `cargo build` fails in every build script with:  linker `cc` not found
#        -> build inside a Debian container that has gcc + a rustup toolchain.
#   3. Docker IS available and works from WSL.
#        -> docker run with the repo mounted read-write at /work.
#   4. CARGO_TARGET_DIR=/tmp/target keeps root-owned build artifacts out of the repo
#      (the repo's own target/ stays untouched; nothing to chown afterwards).
#   5. `cargo fmt` works on the HOST (no compilation needed) -- run it there.
#   6. Cross-checking the WINDOWS target from WSL needs a Windows resource compiler:
#      aster-launcher's build.rs (winresource) panics without `llvm-rc`.
#        -> apt-get install llvm  (provides /usr/lib/llvm-14/bin/llvm-rc)
#        -> rustup target add x86_64-pc-windows-msvc
#        -> PATH must include /usr/lib/llvm-14/bin
#      `cargo check` (no linking) is enough to type-check the Windows-only code.
#   7. The REAL Windows build can be done straight from WSL via interop -- no Docker
#      needed. The Windows toolchain lives at /mnt/c/Users/<user>/.cargo/bin and is
#      callable as cargo.exe/rustc.exe; csc.exe ships with .NET Framework; package
#      script runs via powershell.exe. Requirements:
#        - the repo must live on a /mnt/c path (Windows cwd translation), and
#        - aster-launcher.exe must NOT be running, or package-dist.ps1 refuses to
#          overwrite dist\aster-launcher.exe (quit it via the tray icon first).
#   8. Not buildable at all from WSL:
#        - hwbridge/HwBridge.exe is C#, not Rust. It IS buildable from WSL via
#          csc.exe (see windows-build); no .NET SDK needed, just .NET Framework.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
TOOLCHAIN="${TOOLCHAIN:-1.97}"   # must satisfy workspace rust-version = "1.88"
ACTION="${1:-test}"

export PATH="$HOME/.cargo/bin:$PATH"

# Run a command inside a Debian container with gcc (linker) + rustup toolchain.
# Container-local CARGO_TARGET_DIR keeps the repo's target/ root-free.
# `extra_pkgs` / `extra_path` allow adding what a specific action needs
# (llvm for the Windows resource compiler `llvm-rc`).
run_in_container() {
    local cmd="$1"
    local extra_pkgs="${2:-}"
    local extra_path="${3:-}"
    docker run --rm \
        -v "$REPO_ROOT:/work" \
        -w /work \
        -e CARGO_TARGET_DIR=/tmp/target \
        debian:bookworm-slim \
        bash -lc "
            apt-get update -qq >/dev/null 2>&1 &&
            apt-get install -y -qq gcc curl $extra_pkgs >/dev/null 2>&1 &&
            curl -sSf https://sh.rustup.rs -o /tmp/rustup.sh &&
            sh /tmp/rustup.sh -y --profile minimal --default-toolchain $TOOLCHAIN >/dev/null 2>&1 &&
            export PATH=\"\$HOME/.cargo/bin${extra_path:+:$extra_path}:\$PATH\" &&
            cd /work &&
            $cmd
        "
}

# Windows-target type-check: catches compile errors in the Windows-only code
# (tray.rs, spawn_and_watch/kill_named in process.rs) that Linux tests skip.
windows_check() {
    local cmd="rustup target add x86_64-pc-windows-msvc >/dev/null 2>&1 && cargo check -p aster-launcher --target x86_64-pc-windows-msvc"
    run_in_container "$cmd" "llvm" "/usr/lib/llvm-14/bin"
}

# REAL Windows build straight from WSL via interop: release binaries with the
# Windows Rust toolchain, HwBridge.exe with the .NET Framework csc.exe, then
# assemble dist\ via powershell.exe. Requires the repo on a /mnt/c path and
# aster-launcher.exe NOT running (package-dist.ps1 refuses to overwrite a
# running exe; quit it via the tray icon first).
windows_build() {
    local csc="/mnt/c/Windows/Microsoft.NET/Framework64/v4.0.30319/csc.exe"

    echo "== windows release build (cargo.exe) =="
    cargo.exe build --release || return 1

    echo "== hwbridge build (csc.exe) =="
    (cd "$REPO_ROOT/hwbridge" && "$csc" /nologo /r:LibreHardwareMonitorLib.dll /out:HwBridge.exe HwBridge.cs) || return 1

    echo "== package dist (powershell.exe) =="
    (cd "$REPO_ROOT" && powershell.exe -NoProfile -ExecutionPolicy Bypass -File windows/package-dist.ps1) || return 1

    echo "done. dist\ is ready: double-click dist\\aster-launcher.exe."
}

fmt_check() {
    echo "== rustfmt check (host) =="
    if cargo fmt --all -- --check; then
        echo "fmt: clean"
    else
        # Non-fatal: the repo has ONE known pre-existing fmt diff at
        # crates/aster-sysinfo/src/main.rs:437 (see header) that would
        # otherwise abort every build. The diff is still printed above.
        echo "fmt: diffs found (see above; known pre-existing: crates/aster-sysinfo/src/main.rs:437) -- continuing" >&2
    fi
}

case "$ACTION" in
    test)
        fmt_check
        echo "== tests (container) =="
        run_in_container "cargo test -p aster-launcher && cargo test -p aster-sysinfo"
        ;;
    build)
        fmt_check
        echo "== build (container) =="
        run_in_container "cargo build -p aster-launcher -p aster-sysinfo"
        ;;
    windows-check)
        fmt_check
        echo "== windows-target check (container) =="
        windows_check
        ;;
    windows-build)
        echo "== real windows build (direct from WSL) =="
        windows_build
        ;;
    *)
        echo "usage: $0 [test|build|windows-check|windows-build]" >&2
        exit 2
        ;;
esac

echo "done. Windows-only artifacts (hwbridge, dist\\) must be built on Windows -- see header."
