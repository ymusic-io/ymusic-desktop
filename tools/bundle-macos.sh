#!/usr/bin/env bash
# Builds the release ymusic and assembles YMusic.app around it: Info.plist (ymusic:// links, media
# files under Open With at rank Alternate), the icon, the libav dylibs in Contents/Frameworks and the
# packed ffmpeg next to the executable (where downloads look for it), relinked to load only from
# inside the bundle, then signed.
#
# usage: tools/bundle-macos.sh [--no-build] [--out <dir>]
# env:   YMUSIC_V4_DIR      YMusicV4 checkout (default ../YMusicV4): icon sources
#        FFMPEG_ROOT        ffmpeg build (default <YMusicV4>/../ffmpeg-build/out/macos-<arch>)
#        CODESIGN_IDENTITY  default "-" (ad-hoc); a real identity also gets the hardened runtime
#        CARGO_TARGET_DIR   default <workspace>/target
set -euo pipefail

BUNDLE_ID=io.ymusic.desktop.rust
APP_NAME=YMusic

root=$(cd "$(dirname "$0")/.." && pwd)
build=1
out=""
while [[ $# -gt 0 ]]; do
    case $1 in
        --no-build) build=0 ;;
        --out) out=$2; shift ;;
        *) echo "usage: $0 [--no-build] [--out <dir>]" >&2; exit 2 ;;
    esac
    shift
done

v4=$(cd "${YMUSIC_V4_DIR:-$root/../YMusicV4}" && pwd)
arch=$(uname -m)
ffmpeg_root=${FFMPEG_ROOT:-$(cd "$v4/.." && pwd)/ffmpeg-build/out/macos-$arch}
target_dir=${CARGO_TARGET_DIR:-$root/target}
out=${out:-$target_dir/bundle}
identity=${CODESIGN_IDENTITY:--}

die() { echo "bundle-macos: $*" >&2; exit 1; }
[[ -f $ffmpeg_root/pack/ffmpeg ]] || die "no packed ffmpeg at $ffmpeg_root/pack/ffmpeg (set FFMPEG_ROOT)"
[[ -f $v4/desktopApp/ymusic.icns ]] || die "no icon at $v4/desktopApp/ymusic.icns (set YMUSIC_V4_DIR)"

if [[ $build == 1 ]]; then
    (cd "$root" && YMUSIC_V4_DIR=$v4 FFMPEG_ROOT=$ffmpeg_root cargo build --release -p ymusic)
fi
binary=$target_dir/release/ymusic
[[ -f $binary ]] || die "no release binary at $binary"
version=$(cd "$root" && cargo pkgid -p ymusic | sed 's/.*[#@]//')

app=$out/$APP_NAME.app
contents=$app/Contents
frameworks=$contents/Frameworks
rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources" "$frameworks"
cp "$binary" "$contents/MacOS/ymusic"
cp "$ffmpeg_root/pack/ffmpeg" "$contents/MacOS/ffmpeg"
cp "$v4/desktopApp/ymusic.icns" "$contents/Resources/ymusic.icns"
# macOS 26+ draws CFBundleIconName from Assets.car; older ones the .icns.
[[ -f $v4/desktopApp/macos/Assets.car ]] && cp "$v4/desktopApp/macos/Assets.car" "$contents/Resources/Assets.car"
printf 'APPL????' > "$contents/PkgInfo"

# One file per library, named by its major version: ymusic links libav by its full version and
# ffmpeg by its major one, and two names for one library would load it twice.
for real in "$ffmpeg_root"/lib/lib*.dylib; do
    [[ -L $real ]] && continue
    file=$(basename "$real")
    base=${file%%.*}
    rest=${file#*.}
    cp "$real" "$frameworks/$base.${rest%%.*}.dylib"
done

rpaths() { otool -l "$1" | awk '/cmd LC_RPATH/ { getline; getline; print $2 }'; }

relink() {
    local macho=$1 dep name base candidates
    for dep in $(otool -L "$macho" | tail -n +2 | awk '{ print $1 }' | grep '^@rpath/' || true); do
        name=${dep#@rpath/}
        base=${name%%.*}
        candidates=("$frameworks/$base".*.dylib)
        [[ -f ${candidates[0]} ]] || die "$(basename "$macho") needs $name, not in $ffmpeg_root/lib"
        [[ $name == "$(basename "${candidates[0]}")" ]] ||
            install_name_tool -change "$dep" "@rpath/$(basename "${candidates[0]}")" "$macho" 2>/dev/null
    done
    local foreign
    foreign=$(otool -L "$macho" | tail -n +2 | awk '{ print $1 }' | grep -v -e '^@rpath/' -e '^/usr/lib/' -e '^/System/' || true)
    [[ -z $foreign ]] || die "$(basename "$macho") links outside the bundle and the OS: $foreign"
}

for dylib in "$frameworks"/*.dylib; do
    install_name_tool -id "@rpath/$(basename "$dylib")" "$dylib" 2>/dev/null
    relink "$dylib"
done
for executable in "$contents/MacOS/ymusic" "$contents/MacOS/ffmpeg"; do
    for rpath in $(rpaths "$executable"); do
        install_name_tool -delete_rpath "$rpath" "$executable" 2>/dev/null
    done
    install_name_tool -add_rpath @executable_path/../Frameworks "$executable" 2>/dev/null
    relink "$executable"
done

min_os=$(for macho in "$contents/MacOS/"* "$frameworks"/*.dylib; do
    otool -l "$macho" | awk '/cmd LC_BUILD_VERSION/ { found = 1 } found && $1 == "minos" { print $2; exit }'
done | sort -V | tail -n 1)

cat > "$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>ymusic</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>CFBundleIconFile</key>
    <string>ymusic</string>
    <key>CFBundleIconName</key>
    <string>YMusic</string>
    <key>LSMinimumSystemVersion</key>
    <string>$min_os</string>
    <key>LSApplicationCategoryType</key>
    <string>public.app-category.music</string>
    <key>NSPrincipalClass</key>
    <string>NSApplication</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key>
            <string>$BUNDLE_ID</string>
            <key>CFBundleURLSchemes</key>
            <array><string>ymusic</string></array>
        </dict>
    </array>
    <key>CFBundleDocumentTypes</key>
    <array>
        <dict>
            <key>CFBundleTypeName</key>
            <string>Media file</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>LSHandlerRank</key>
            <string>Alternate</string>
            <key>LSItemContentTypes</key>
            <array>
                <string>public.mp3</string>
                <string>com.apple.m4a-audio</string>
                <string>public.mpeg-4-audio</string>
                <string>public.aac-audio</string>
                <string>org.xiph.flac</string>
                <string>com.microsoft.waveform-audio</string>
                <string>public.mpeg-4</string>
                <string>com.apple.m4v-video</string>
                <string>com.apple.quicktime-movie</string>
            </array>
        </dict>
        <dict>
            <key>CFBundleTypeName</key>
            <string>Media file</string>
            <key>CFBundleTypeRole</key>
            <string>Viewer</string>
            <key>LSHandlerRank</key>
            <string>Alternate</string>
            <key>CFBundleTypeExtensions</key>
            <array>
                <string>m4b</string><string>ogg</string><string>oga</string><string>opus</string>
                <string>mka</string><string>webm</string><string>mkv</string>
            </array>
        </dict>
    </array>
</dict>
</plist>
PLIST
plutil -lint -s "$contents/Info.plist"

sign_flags=(--force --sign "$identity")
if [[ $identity == - ]]; then
    sign_flags+=(--timestamp=none)
else
    sign_flags+=(--timestamp --options runtime)
fi
# Inside out: nested code first, the bundle seal last.
for dylib in "$frameworks"/*.dylib; do
    codesign "${sign_flags[@]}" "$dylib"
done
codesign "${sign_flags[@]}" --identifier "$BUNDLE_ID.ffmpeg" "$contents/MacOS/ffmpeg"
codesign "${sign_flags[@]}" "$app"
codesign --verify --deep --strict "$app"

echo "$app ($version, macOS $min_os+): $(du -sh "$app" | cut -f1)"
