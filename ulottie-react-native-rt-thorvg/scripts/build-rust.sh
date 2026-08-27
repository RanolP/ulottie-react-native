#!/usr/bin/env bash
# Builds the ulottie-rt static library with the ThorVG backend (feature
# `thorvg`, tiny-skia off) and drops it (plus its C header) where the
# podspec's `vendored_libraries` expects it: ios/rust/. ThorVG v1.1.1 itself
# is fetched and compiled by ulottie-rt/build.rs and lands inside this same
# archive (cargo bundles native static libs into staticlib output).
#
# The archive is post-processed so only the `ulottie_rt_tvg_*` C ABI stays
# global: the compare app links this pod next to UlottieRtTinySkia, whose
# archive is the very same Rust crate built with the other feature — without
# localization the two archives collide on every internal Rust symbol at app
# link time.
#
# Simulator-only for now; see the tiny-skia package's build-rust.sh for the
# device-slice/XCFramework path.
# Usage: build-rust.sh [ios|android|all]   (default: ios)
#
# Android: cargo cross-compiles the same staticlib per ABI with the NDK
# toolchain (the NDK clang also builds the bundled ThorVG via the cc crate)
# and drops it under android/rust/<abi>/. No symbol localization there — each
# backend becomes its own .so (CMake links with --exclude-libs,ALL), and the
# dynamic boundary is what keeps the two backends' internal symbols apart.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
OUT="$ROOT/ulottie-react-native-rt-thorvg/ios/rust"
TARGETS=(aarch64-apple-ios-sim)
MODE="${1:-ios}"

build_android() {
  local sdk="${ANDROID_HOME:-$HOME/Library/Android/sdk}"
  local ndk="${ANDROID_NDK_ROOT:-}"
  if [ -z "$ndk" ]; then
    ndk="$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1)"
  fi
  if [ ! -d "$ndk" ]; then
    echo "error: Android NDK not found (set ANDROID_NDK_ROOT)" >&2
    exit 1
  fi
  local bin="$ndk/toolchains/llvm/prebuilt/darwin-x86_64/bin"
  local api=24
  for spec in "aarch64-linux-android:arm64-v8a" "x86_64-linux-android:x86_64"; do
    local target="${spec%%:*}" abi="${spec##*:}"
    local cc="$bin/${target}${api}-clang"
    local envtarget="${target//-/_}"
    env \
      "CC_${envtarget}=$cc" \
      "CXX_${envtarget}=${cc}++" \
      "AR_${envtarget}=$bin/llvm-ar" \
      "CARGO_TARGET_$(echo "$envtarget" | tr '[:lower:]' '[:upper:]')_LINKER=$cc" \
      cargo build \
      --manifest-path "$ROOT/Cargo.toml" \
      -p ulottie-rt \
      --no-default-features \
      --features thorvg \
      --profile rt \
      --target "$target"
    local dst="$ROOT/ulottie-react-native-rt-thorvg/android/rust/$abi"
    mkdir -p "$dst"
    cp "$ROOT/target/$target/rt/libulottie_rt.a" "$dst/libulottie_rt.a"
    echo "wrote $dst/libulottie_rt.a ($(du -h "$dst/libulottie_rt.a" | cut -f1))"
  done
}

if [ "$MODE" = "android" ] || [ "$MODE" = "all" ]; then
  build_android
fi
if [ "$MODE" = "android" ]; then
  exit 0
fi

for target in "${TARGETS[@]}"; do
  cargo build \
    --manifest-path "$ROOT/Cargo.toml" \
    -p ulottie-rt \
    --no-default-features \
    --features thorvg \
    --profile rt \
    --target "$target"
done

mkdir -p "$OUT"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
cat > "$TMP/exports.txt" <<'EOF'
_ulottie_rt_tvg_instance_create
_ulottie_rt_tvg_instance_destroy
_ulottie_rt_tvg_instance_load
_ulottie_rt_tvg_instance_set_buffer
_ulottie_rt_tvg_render_frame
EOF
SDK_VERSION="$(xcrun -sdk iphonesimulator --show-sdk-version)"
xcrun ld -r -arch arm64 \
  -platform_version ios-simulator 16.4 "$SDK_VERSION" \
  -all_load "$ROOT/target/aarch64-apple-ios-sim/rt/libulottie_rt.a" \
  -exported_symbols_list "$TMP/exports.txt" \
  -o "$TMP/ulottie_rt.o"
# Strip unwind info from the merged object. The crate is panic=abort (and the
# thorvg build compiles ThorVG -fno-exceptions), so nothing ever unwinds
# through these frames — but the localized `rust_eh_personality` copies the
# unwind tables reference would push the app image past compact unwind's
# 3-personality limit (each rt pod carries its own localized copy), and the
# old workaround, `-Wl,-no_compact_unwind` on the APP link, broke every C++
# catch in the app dylib (an uncaught std::invalid_argument from reanimated's
# stoi aborted the process whenever a LogBox badge mounted).
OBJCOPY="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/host: //p')/bin/rust-objcopy"
"$OBJCOPY" \
  --remove-section=__TEXT,__eh_frame \
  --remove-section=__LD,__compact_unwind \
  "$TMP/ulottie_rt.o"
# Named apart from the tiny-skia pod's libulottie_rt.a: CocoaPods links
# vendored libraries by `-l<name>`, so two pods vendoring the same basename
# collide in the app target.
xcrun libtool -static "$TMP/ulottie_rt.o" -o "$OUT/libulottie_rt_tvg.a"
rm -f "$OUT/libulottie_rt.a"
cp "$ROOT/ulottie-rt/include/ulottie_rt_tvg.h" "$OUT/ulottie_rt_tvg.h"
echo "wrote $OUT/libulottie_rt_tvg.a ($(du -h "$OUT/libulottie_rt_tvg.a" | cut -f1))"
