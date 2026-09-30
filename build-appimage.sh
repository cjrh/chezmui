#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if [[ $(uname -s) != Linux || $(uname -m) != x86_64 ]]; then
    echo "AppImage builds require x86_64 Linux." >&2
    exit 1
fi

for tool in cargo python3 linuxdeploy patchelf pkg-config sha256sum tar; do
    command -v "$tool" >/dev/null || { echo "Missing build tool: $tool" >&2; exit 1; }
done

metadata=$(cargo metadata --no-deps --format-version 1 --locked)
version=$(python3 -c 'import json, sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "chezmui"))' <<< "$metadata")
target_dir=$(python3 -c 'import json, sys; print(json.load(sys.stdin)["target_directory"])' <<< "$metadata")
target=x86_64-unknown-linux-gnu
name="chezmui-v${version}-x86_64"
dist="$target_dir/dist"

# Keep each build separate. Never include files left over from an earlier build.
mkdir -p "$dist"
work=$(mktemp -d "$target_dir/appimage.XXXXXX")
trap 'rm -rf -- "$work"' EXIT
appdir="$work/chezmui.AppDir"
mkdir -p "$appdir/usr/share/licenses/chezmui"
install -m 644 LICENSE "$appdir/usr/share/licenses/chezmui/LICENSE"

cargo build --release --locked --target "$target"
binary="$target_dir/$target/release/chezmui"

# These libraries are opened with dlopen, so linuxdeploy cannot find them via ldd.
# Core display libraries, fonts, and XKB data still come from the host desktop.
libdir=$(pkg-config --variable=libdir xkbcommon)
wayland_libdir=$(pkg-config --variable=libdir wayland-client)
libraries=(
    "$libdir/libxkbcommon.so.0"
    "$libdir/libxkbcommon-x11.so.0"
    "$wayland_libdir/libwayland-client.so.0"
)
library_args=()
for library in "${libraries[@]}"; do
    if [[ ! -f $library ]]; then
        echo "Missing runtime library: $library" >&2
        exit 1
    fi
    library_args+=(--library "$library")
done

# The bundled patchelf can damage modern shared libraries. Use the system tool.
PATCHELF=$(command -v patchelf)
export PATCHELF
# Avoid FUSE in CI and linuxdeploy's old bundled strip implementation.
export APPIMAGE_EXTRACT_AND_RUN=1 NO_STRIP=1 ARCH=x86_64
export LINUXDEPLOY_OUTPUT_VERSION="$version" LDAI_OUTPUT="$work/$name.AppImage"
linuxdeploy \
    --appdir "$appdir" \
    --executable "$binary" \
    --desktop-file packaging/chezmui.desktop \
    --icon-file packaging/chezmui.svg \
    "${library_args[@]}" \
    --output appimage

# A packaging success does not prove that patched libraries can still load.
# Check each in a separate process so loader errors stop the release build.
for library in "${libraries[@]}"; do
    python3 -c 'import ctypes, sys; ctypes.CDLL(sys.argv[1])' \
        "$appdir/usr/lib/$(basename -- "$library")"
done

test -s "$work/$name.AppImage"
chmod +x "$work/$name.AppImage"

archive="chezmui-v${version}-${target}"
mkdir -p "$work/$archive"
install -m 755 "$binary" "$work/$archive/chezmui"
install -m 644 README.md LICENSE "$work/$archive/"
tar -C "$work" -czf "$work/$archive.tar.gz" "$archive"
(
    cd "$work"
    sha256sum "$name.AppImage" "$archive.tar.gz" > "$name.sha256"
)
mv -- "$work/$name.AppImage" "$work/$archive.tar.gz" "$work/$name.sha256" "$dist/"
printf 'Release artifacts: %s\n' "$dist/$name.AppImage" "$dist/$archive.tar.gz" "$dist/$name.sha256"
