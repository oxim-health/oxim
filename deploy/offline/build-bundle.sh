#!/usr/bin/env bash
# Builds the offline installation bundle for a Linux or macOS target:
#
#   deploy/offline/build-bundle.sh --version 1.0.0 \
#       --target x86_64-unknown-linux-gnu \
#       --binary target/x86_64-unknown-linux-gnu/release/oxim \
#       [--out dist]
#
# Writes dist/oxim-<version>-<target>.tar.gz and a .sha256 file next to it.
# The bundle holds the binary, the configuration template, example channels
# and tables, the systemd files, the installation guides, the licenses,
# install.sh / uninstall.sh and SHA256SUMS of its own content. Installing it
# needs no network access. Windows bundles are built by build-bundle.ps1.
set -euo pipefail

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
}

version=""
target=""
binary=""
out="dist"
while [ $# -gt 0 ]; do
    case "$1" in
        --version) version="${2:?}"; shift 2 ;;
        --target) target="${2:?}"; shift 2 ;;
        --binary) binary="${2:?}"; shift 2 ;;
        --out) out="${2:?}"; shift 2 ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
done
[ -n "$version" ] && [ -n "$target" ] && [ -n "$binary" ] || usage
[ -f "$binary" ] || { echo "no such binary: $binary" >&2; exit 1; }
version="${version#v}"

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
name="oxim-${version}-${target}"
stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT
root="$stage/$name"

sha256() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$@"
    else
        shasum -a 256 "$@"
    fi
}

mkdir -p "$root/bin" "$root/config" "$root/examples/channels" "$root/examples/tables" \
    "$root/systemd/oxim.service.d" "$root/systemd/sysusers.d" "$root/systemd/tmpfiles.d" \
    "$root/docs"
install -m 0755 "$binary" "$root/bin/oxim"
install -m 0644 "$repo/deploy/linux/oxim.yaml" "$root/config/oxim.yaml"
install -m 0644 "$repo"/deploy/examples/channels/* "$root/examples/channels/"
install -m 0644 "$repo"/deploy/examples/tables/* "$root/examples/tables/"
install -m 0644 "$repo/deploy/systemd/oxim.service" "$root/systemd/oxim.service"
install -m 0644 "$repo/deploy/systemd/oxim.service.d/10-package.conf" "$root/systemd/oxim.service.d/"
install -m 0644 "$repo/deploy/systemd/sysusers.d/oxim.conf" "$root/systemd/sysusers.d/"
install -m 0644 "$repo/deploy/systemd/tmpfiles.d/oxim.conf" "$root/systemd/tmpfiles.d/"
install -m 0644 "$repo"/docs/install/*.md "$root/docs/"
install -m 0644 "$repo/README.md" "$repo/LICENSE-MIT" "$repo/LICENSE-APACHE" "$root/"
install -m 0755 "$repo/deploy/offline/install.sh" "$repo/deploy/offline/uninstall.sh" "$root/"
printf '%s\n' "$version" > "$root/VERSION"

(
    cd "$root"
    find . -type f ! -name SHA256SUMS | LC_ALL=C sort | sed 's|^\./||' | while IFS= read -r file; do
        sha256 "$file"
    done > SHA256SUMS
)

mkdir -p "$out"
archive="$(cd "$out" && pwd)/$name.tar.gz"
if tar --version 2>/dev/null | grep -q 'GNU tar'; then
    tar -C "$stage" --owner=0 --group=0 --numeric-owner --sort=name -czf "$archive" "$name"
else
    tar -C "$stage" --uid 0 --gid 0 -czf "$archive" "$name"
fi
(cd "$out" && sha256 "$name.tar.gz" > "$name.tar.gz.sha256")
echo "created $archive"
