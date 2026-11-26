#!/usr/bin/env bash
set -euo pipefail

# Build Rust FFI native libraries (`crates/gitpulse-protocol`) for GitPulse Mobile:
#   - host:    macOS/Linux host `staticlib` + `cdylib` verification build
#   - ios:     `GitPulseProtocol.xcframework` -> modules/gitpulse-crypto/ios/Frameworks/
#   - android: `libgitpulse_protocol.so` (arm64-v8a, armeabi-v7a, x86_64) via `cargo ndk`
#              -> modules/gitpulse-crypto/android/src/main/jniLibs/
#
# Usage:
#   ./scripts/build-rust-ffi.sh [--platform host|ios|android|all] [--debug|--release] [--check]

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
MOBILE_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
REPO_ROOT="$(cd "${MOBILE_ROOT}/../.." && pwd)"
CRATE_DIR="${REPO_ROOT}/crates/gitpulse-protocol"
CRATE_MANIFEST="${CRATE_DIR}/Cargo.toml"
TARGET_DIR="${CRATE_DIR}/target"

IOS_FRAMEWORKS_DIR="${MOBILE_ROOT}/modules/gitpulse-crypto/ios/Frameworks"
IOS_XCFRAMEWORK_OUT="${IOS_FRAMEWORKS_DIR}/GitPulseProtocol.xcframework"
IOS_HEADERS_STAGE="${TARGET_DIR}/xcframework-headers"
ANDROID_JNILIBS_OUT="${MOBILE_ROOT}/modules/gitpulse-crypto/android/src/main/jniLibs"

PLATFORM="all"
PROFILE="release"
CHECK_ONLY=0

usage() {
  cat <<'EOF'
Usage: build-rust-ffi.sh [options]

Options:
  --platform <host|ios|android|all>  Target platform(s) to build (default: all)
  --release                          Build with --release (default)
  --debug                            Build debug profile
  --check                            Check installed toolchains/targets without building
  -h, --help                         Show this help message
EOF
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --platform)
      PLATFORM="${2:-}"
      shift 2
      ;;
    --release)
      PROFILE="release"
      shift
      ;;
    --debug)
      PROFILE="debug"
      shift
      ;;
    --check)
      CHECK_ONLY=1
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "Unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

case "${PLATFORM}" in
  host|ios|android|all) ;;
  *)
    echo "Invalid --platform value: '${PLATFORM}' (expected host, ios, android, or all)" >&2
    exit 2
    ;;
esac

CARGO_PROFILE_ARGS=()
if [[ "${PROFILE}" == "release" ]]; then
  CARGO_PROFILE_ARGS=(--release)
fi

has_rust_target() {
  local target="$1"
  rustup target list --installed | grep -qx "${target}"
}

ensure_rust_targets() {
  local missing=()
  for t in "$@"; do
    if ! has_rust_target "$t"; then
      missing+=("$t")
    fi
  done
  if [[ ${#missing[@]} -gt 0 ]]; then
    echo "Missing Rust target(s): ${missing[*]}" >&2
    echo "Install with: rustup target add ${missing[*]}" >&2
    return 1
  fi
}

print_toolchain_inventory() {
  echo "=== GitPulse Rust FFI Toolchain Inventory ==="
  echo "Repo root:        ${REPO_ROOT}"
  echo "Crate manifest:   ${CRATE_MANIFEST}"
  echo "Cargo:            $(cargo --version 2>/dev/null || echo 'missing')"
  echo "Xcodebuild:       $(xcodebuild -version 2>/dev/null | head -n 1 || echo 'missing')"
  echo "uniffi-bindgen:   $(command -v uniffi-bindgen 2>/dev/null || echo 'not installed')"
  echo "cargo-ndk:        $(command -v cargo-ndk 2>/dev/null || echo 'not installed')"
  echo "ANDROID_HOME:     ${ANDROID_HOME:-unset}"
  echo "ANDROID_NDK_HOME: ${ANDROID_NDK_HOME:-unset}"
  echo "Installed mobile targets:"
  for t in aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios \
           aarch64-linux-android armv7-linux-androideabi x86_64-linux-android; do
    if has_rust_target "$t"; then
      echo "  [installed] $t"
    else
      echo "  [missing]   $t"
    fi
  done
}

build_host() {
  echo "=== Building gitpulse-protocol host staticlib + cdylib (${PROFILE}) ==="
  cargo build --manifest-path "${CRATE_MANIFEST}" --lib --locked ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}

  local out_dir="${TARGET_DIR}/${PROFILE}"
  local static_lib="${out_dir}/libgitpulse_protocol.a"
  local dyn_lib=""
  if [[ -f "${out_dir}/libgitpulse_protocol.dylib" ]]; then
    dyn_lib="${out_dir}/libgitpulse_protocol.dylib"
  elif [[ -f "${out_dir}/libgitpulse_protocol.so" ]]; then
    dyn_lib="${out_dir}/libgitpulse_protocol.so"
  fi

  if [[ ! -f "${static_lib}" || -z "${dyn_lib}" ]]; then
    echo "Expected both staticlib (.a) and cdylib (.dylib/.so) in ${out_dir}" >&2
    exit 1
  fi

  echo "Built host artifacts:"
  ls -lh "${static_lib}" "${dyn_lib}"
}

prepare_ios_headers() {
  rm -rf "${IOS_HEADERS_STAGE}"
  mkdir -p "${IOS_HEADERS_STAGE}"

  if command -v uniffi-bindgen >/dev/null 2>&1; then
    local sample_lib="$1"
    echo "Generating Swift UniFFI bindings via uniffi-bindgen..."
    uniffi-bindgen generate \
      --library "${sample_lib}" \
      --language swift \
      --out-dir "${IOS_HEADERS_STAGE}"
  else
    # Minimal modulemap so xcodebuild -create-xcframework produces a linkable C/Swift module
    # even before uniffi-bindgen codegen is added to the toolchain.
    cat > "${IOS_HEADERS_STAGE}/module.modulemap" <<'EOF'
module GitPulseProtocolFFI {
    header "gitpulse_protocol.h"
    export *
}
EOF
    cat > "${IOS_HEADERS_STAGE}/gitpulse_protocol.h" <<'EOF'
#pragma once
/* Placeholder header for crates/gitpulse-protocol staticlib XCFramework packaging.
 * Install uniffi-bindgen to emit full UniFFI Swift/C headers automatically. */
EOF
  fi
}

build_ios() {
  echo "=== Building iOS GitPulseProtocol.xcframework (${PROFILE}) ==="
  if ! command -v xcodebuild >/dev/null 2>&1; then
    echo "xcodebuild is required to build GitPulseProtocol.xcframework (macOS + Xcode)." >&2
    exit 1
  fi

  ensure_rust_targets aarch64-apple-ios aarch64-apple-ios-sim

  local xcframework_args=()

  # iOS device slice (arm64)
  cargo build --manifest-path "${CRATE_MANIFEST}" --lib --locked \
    --target aarch64-apple-ios ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}
  local ios_device_lib="${TARGET_DIR}/aarch64-apple-ios/${PROFILE}/libgitpulse_protocol.a"

  # iOS simulator slice (arm64, plus x86_64 universal lipo if installed)
  cargo build --manifest-path "${CRATE_MANIFEST}" --lib --locked \
    --target aarch64-apple-ios-sim ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}
  local ios_sim_arm64="${TARGET_DIR}/aarch64-apple-ios-sim/${PROFILE}/libgitpulse_protocol.a"
  local ios_sim_lib="${ios_sim_arm64}"

  if has_rust_target x86_64-apple-ios; then
    cargo build --manifest-path "${CRATE_MANIFEST}" --lib --locked \
      --target x86_64-apple-ios ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}
    local ios_sim_x64="${TARGET_DIR}/x86_64-apple-ios/${PROFILE}/libgitpulse_protocol.a"
    local ios_sim_fat_dir="${TARGET_DIR}/ios-sim-universal/${PROFILE}"
    mkdir -p "${ios_sim_fat_dir}"
    ios_sim_lib="${ios_sim_fat_dir}/libgitpulse_protocol.a"
    lipo -create "${ios_sim_arm64}" "${ios_sim_x64}" -output "${ios_sim_lib}"
  fi

  prepare_ios_headers "${ios_device_lib}"

  xcframework_args+=(
    -library "${ios_device_lib}" -headers "${IOS_HEADERS_STAGE}"
    -library "${ios_sim_lib}" -headers "${IOS_HEADERS_STAGE}"
  )

  mkdir -p "${IOS_FRAMEWORKS_DIR}"
  rm -rf "${IOS_XCFRAMEWORK_OUT}"
  xcodebuild -create-xcframework "${xcframework_args[@]}" -output "${IOS_XCFRAMEWORK_OUT}"
  echo "Built iOS XCFramework: ${IOS_XCFRAMEWORK_OUT}"
}

build_android() {
  echo "=== Building Android libgitpulse_protocol.so via cargo-ndk (${PROFILE}) ==="
  if ! command -v cargo-ndk >/dev/null 2>&1; then
    echo "cargo-ndk is required for Android .so builds. Install with: cargo install cargo-ndk" >&2
    exit 1
  fi

  ensure_rust_targets aarch64-linux-android armv7-linux-androideabi x86_64-linux-android

  if [[ -z "${ANDROID_NDK_HOME:-}" && -n "${ANDROID_HOME:-}" && -d "${ANDROID_HOME}/ndk" ]]; then
    local latest_ndk
    latest_ndk="$(ls -1 "${ANDROID_HOME}/ndk" | sort -V | tail -n 1 || true)"
    if [[ -n "${latest_ndk}" ]]; then
      export ANDROID_NDK_HOME="${ANDROID_HOME}/ndk/${latest_ndk}"
    fi
  fi

  if [[ -z "${ANDROID_NDK_HOME:-}" || ! -d "${ANDROID_NDK_HOME}" ]]; then
    echo "ANDROID_NDK_HOME is not set (or no NDK found under ANDROID_HOME/ndk)." >&2
    exit 1
  fi

  mkdir -p "${ANDROID_JNILIBS_OUT}"
  cargo ndk \
    --manifest-path "${CRATE_MANIFEST}" \
    -t arm64-v8a \
    -t armeabi-v7a \
    -t x86_64 \
    -o "${ANDROID_JNILIBS_OUT}" \
    build --lib --locked ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"}

  echo "Built Android jniLibs in: ${ANDROID_JNILIBS_OUT}"
  find "${ANDROID_JNILIBS_OUT}" -name 'libgitpulse_protocol.so' -exec ls -lh {} +
}

if [[ "${CHECK_ONLY}" -eq 1 ]]; then
  print_toolchain_inventory
  exit 0
fi

case "${PLATFORM}" in
  host)
    build_host
    ;;
  ios)
    build_ios
    ;;
  android)
    build_android
    ;;
  all)
    build_host
    build_ios
    build_android
    ;;
esac
