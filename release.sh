#!/usr/bin/env bash
# Builds EagleView in release mode and packages it as
# dist/EagleView-v<version>-linux-<arch>.zip
#
# Usage:  ./release.sh 0.0.3
#         ./release.sh v0.0.3 --force    (replace an existing release of that version)
set -euo pipefail

cd "$(dirname "$0")"

if [ $# -lt 1 ] || [ -z "$1" ]; then
    echo "Usage: ./release.sh <version> [--force]   e.g. ./release.sh 0.0.3" >&2
    exit 1
fi

version="${1#[vV]}"
force=""
case "${2:-}" in
    --force|-f) force=1 ;;
    "") ;;
    *) echo "Error: unknown option '$2'." >&2; exit 1 ;;
esac

if ! [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$ ]]; then
    echo "Error: version '$version' is not of the form MAJOR.MINOR.PATCH (e.g. 0.0.3)." >&2
    exit 1
fi

if ! command -v zip >/dev/null 2>&1; then
    echo "Error: 'zip' is not installed (e.g. sudo apt install zip)." >&2
    exit 1
fi

case "$(uname -m)" in
    x86_64|amd64) arch=x64 ;;
    aarch64|arm64) arch=arm64 ;;
    *) arch="$(uname -m)" ;;
esac

name="EagleView-v${version}-linux-${arch}"
stage="dist/$name"
zip_path="dist/$name.zip"

if [ -e "$stage" ] || [ -e "$zip_path" ]; then
    if [ -z "$force" ]; then
        echo "Error: $stage already exists. Pass --force to replace it." >&2
        exit 1
    fi
    rm -rf "$stage" "$zip_path"
fi

echo "Building release..."
cargo build --release

echo "Staging $stage..."
mkdir -p "$stage"
cp target/release/eagleview "$stage/"
cp sysml/tests/data/drone.sysml "$stage/example.sysml"
cp -r themes "$stage/themes"

# Ship the committed config, not the working copy: the app writes things such
# as the last opened view back into eagleview.toml while it runs.
if ! git show HEAD:eagleview.toml > "$stage/eagleview.toml" 2>/dev/null; then
    echo "Warning: could not read eagleview.toml from git; using the working copy." >&2
    cp eagleview.toml "$stage/eagleview.toml"
fi

echo "Zipping..."
(cd dist && zip -qr "$name.zip" "$name")

echo "Created $zip_path ($(du -h "$zip_path" | cut -f1))"
