#!/usr/bin/env bash
set -euo pipefail

case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) platform=linux-x86_64 ;;
    Linux/aarch64) platform=linux-aarch64 ;;
    *) printf '%s\n' 'Use Linux x86-64 or ARM64, or build from source.' >&2; exit 1 ;;
esac

release=https://github.com/xDilettante/vps-bench/releases/download/v0.5.0
archive="vps-bench-$platform.tar.gz"
tmp="$(mktemp -d)"
trap 'rm -f -- "$tmp/$archive" "$tmp/$archive.sha256" "$tmp/vps-bench"; rmdir -- "$tmp"' EXIT

curl -fsSL --retry 2 "$release/$archive" -o "$tmp/$archive"
curl -fsSL --retry 2 "$release/$archive.sha256" -o "$tmp/$archive.sha256"
(cd "$tmp" && sha256sum --check --status -- "$archive.sha256" && tar -xzf "$archive" -- vps-bench)
"$tmp/vps-bench" "$@"
