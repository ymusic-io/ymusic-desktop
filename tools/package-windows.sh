#!/usr/bin/env bash
# Builds the release ymusic on Windows (Git Bash, MinGW gcc on PATH) and stages the app folder:
# YMusic.exe, the packed ffmpeg.exe and the libav / MinGW runtime DLLs beside it. --setup also
# compiles the Inno Setup installer that the JVM app's updater runs over its own install
# (tools/windows/ymusic.iss), named as the release asset YMusic-<version>-windows-<x64|arm64>-setup.exe.
#
# usage: tools/package-windows.sh [--no-build] [--debug] [--setup] [--out <dir>]
#   --debug    stage target/debug/ymusic.exe (local update tests; dev hooks stay on)
# env:   YMUSIC_V4_DIR      YMusicV4 checkout (default ../YMusicV4): icon, version line
#        FFMPEG_ROOT        ffmpeg build (default <YMusicV4>/../ffmpeg-build/out/windows-<arch>)
#        ISCC               Inno Setup's compiler (default: the newest installed Inno Setup)
#        YMUSIC_VERSION_NAME, YMUSIC_VERSION_CODE  as for the build (crates/ymusic/build.rs)
#        CARGO_TARGET_DIR   default <workspace>/target
set -euo pipefail

APP_NAME=YMusic

root=$(cd "$(dirname "$0")/.." && pwd)
build=1
profile=release
setup=0
out=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --no-build) build=0 ;;
        --debug) profile=debug ;;
        --setup) setup=1 ;;
        --out) out=$2; shift ;;
        *) echo "usage: $0 [--no-build] [--debug] [--setup] [--out <dir>]" >&2; exit 2 ;;
    esac
    shift
done

die() { echo "package-windows: $*" >&2; exit 1; }
v4=$(cd "${YMUSIC_V4_DIR:-$root/../YMusicV4}" && pwd)
case $(uname -m) in
    aarch64 | arm64) arch=arm64; inno_arch=arm64 ;;
    *) arch=x86_64; inno_arch=x64compatible ;;
esac
asset_arch=$([[ $arch == arm64 ]] && echo arm64 || echo x64)
ffmpeg_root=${FFMPEG_ROOT:-$(cd "$v4/.." && pwd)/ffmpeg-build/out/windows-$arch}
target_dir=${CARGO_TARGET_DIR:-$root/target}
out=${out:-$target_dir/bundle}
icon=$v4/desktopApp/ymusic.ico
[[ -f $ffmpeg_root/pack/ffmpeg.exe ]] || die "no packed ffmpeg at $ffmpeg_root/pack/ffmpeg.exe (set FFMPEG_ROOT)"
[[ -f $icon ]] || die "no icon at $icon (set YMUSIC_V4_DIR)"
command -v gcc > /dev/null || die "no MinGW gcc on PATH (the C sources need GCC/Clang)"

if [[ $build == 1 ]]; then
    cargo_profile=()
    [[ $profile == release ]] && cargo_profile=(--release)
    (cd "$root" && YMUSIC_V4_DIR=$v4 FFMPEG_ROOT=$ffmpeg_root cargo build ${cargo_profile[@]+"${cargo_profile[@]}"} -p ymusic)
fi
binary=$target_dir/$profile/ymusic.exe
[[ -f $binary ]] || die "no $profile binary at $binary"
# The version line the binary reports (crates/ymusic/build.rs reads the same inputs).
version=${YMUSIC_VERSION_NAME:-$(sed -n 's/^ymusic\.versionName=//p' "$v4/gradle.properties" | tr -d '[:space:]')}
[[ -n $version ]] || die "no version in $v4/gradle.properties"

app=$out/$APP_NAME
rm -rf "$app"
mkdir -p "$app"
cp "$binary" "$app/$APP_NAME.exe"
cp "$ffmpeg_root"/pack/*.dll "$ffmpeg_root/pack/ffmpeg.exe" "$app/"
# Every DLL the executable imports is shipped here or part of Windows (api-ms-win-* are API sets).
missing=$(objdump -p "$app/$APP_NAME.exe" | sed -n 's/^\s*DLL Name: //p' | while read -r dll; do
    [[ $dll == api-ms-win-* || -f $app/$dll || -f /c/Windows/System32/$dll ]] || echo "$dll"
done)
[[ -z $missing ]] || die "imports neither shipped nor in System32: $missing"
echo "package-windows: $app"

[[ $setup == 1 ]] || exit 0
iscc=${ISCC:-}
if [[ -z $iscc ]]; then
    for candidate in "${LOCALAPPDATA:-}"/Programs/Inno\ Setup\ */ISCC.exe "/c/Program Files (x86)"/Inno\ Setup\ */ISCC.exe \
        "/c/Program Files"/Inno\ Setup\ */ISCC.exe; do
        [[ -f $candidate ]] && iscc=$candidate
    done
fi
[[ -n $iscc && -f $iscc ]] || die "no Inno Setup compiler (install Inno Setup or set ISCC)"
win() { cygpath -w "$1"; }
# Git Bash would rewrite ISCC's /D… switches as POSIX paths.
MSYS2_ARG_CONV_EXCL='*' "$iscc" /Qp "/DAppVersion=$version" "/DAppSourceDir=$(win "$app")" "/DIconFile=$(win "$icon")" \
    "/DOutputDir=$(win "$out")" "/DArch=$inno_arch" "/DOutputBaseName=$APP_NAME-$version-windows-$asset_arch-setup" \
    "$(win "$root/tools/windows/ymusic.iss")"
echo "package-windows: $out/$APP_NAME-$version-windows-$asset_arch-setup.exe"
