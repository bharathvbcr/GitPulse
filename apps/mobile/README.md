# Waylode Mobile (`apps/mobile`)

Single cross-platform **iOS + Android** mobile companion for the Waylode Unified Control Plane, built on **Expo SDK 55 (React Native 0.83 New Architecture: Fabric + TurboModules)** + `@waylode/core` + shared Rust **`src-tauri/crates/waylode-protocol`** (UniFFI `XCFramework` / Android `.so`).

## Security Architecture (Opus M1–M5 & P0-4 / P0-7)

- **M1 — No Lock-Screen Approve:** Notification quick actions expose **Deny** and **Deny & Stop** only (`plugins/with-nse.ts`, `NotificationService.swift`, `WaylodeMessagingService.kt`). Tapping any approval notification opens the app to the native full-bytes confirmation sheet.
- **M2 — Two-Key Hardware Isolation:**
  - **Approval Key:** Non-exportable hardware **P-256** key in iOS Secure Enclave (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly` + `.biometryCurrentSet`) and Android StrongBox (`setUserAuthenticationRequired(true)` + `setInvalidatedByBiometricEnrollment(true)`), used exclusively for `approval_decide` and `steering_confirm_req` signatures.
  - **Identity Keys:** Ed25519 / X25519 keys stored in iOS App Group Keychain (`group.com.gitpulse.mobile`, `AfterFirstUnlockThisDeviceOnly`) / Android Keystore AES-256-GCM wrapped storage. Raw private key bytes never cross the JS bridge and are zeroized immediately after native/Rust calls.
- **M3 — Native Approval Sheet + Code-Signed OTA:** `app.config.ts` enforces mandatory offline-key code signing on `expo-updates` (`codeSigningCertificate` + `codeSigningMetadata`). The approval confirmation sheet (`WaylodeApprovalSheet`) runs in native UIKit / Android View, calling Rust `render_canonical_tool_input` and `hash_canonical`, failing closed on hash mismatch or non-canonical numbers.
- **M4 — Minimal FFI Surface:** Native bridges expose only the small, frozen `src-tauri/crates/waylode-protocol/src/uniffi_api.rs` surface.
- **M5 — Android FCM Hygiene:** `WaylodeMessagingService` always posts a visible notification within an 8s guard (inside Android's 10s FCM budget), falls back to a generic banner on decrypt failure/timeout, and removes `USE_FULL_SCREEN_INTENT` via `blockedPermissions` (`tools:node="remove"`).
- **P0-4 — Minimal Decision Surface:** Strictly `allow_once | deny | deny_and_interrupt`; `updatedInput` and `updatedPermissions` are rejected at both TypeScript and Rust boundaries.
- **P0-7 — Opt-In PTY Input Gate:** `app/pty/[sessionKey].tsx` is strictly read-only by default; `pty_input` requires an unexpired Desktop-granted time-boxed `PtyConsentGrant` plus local biometric step-up, auto-relocking on expiry or drawer close. **Accepted residual risk (Opus #23):** individual `pty.input` keystrokes within an active Desktop-granted consent window are authenticated by the Noise IK tunnel and audited under `audit_id`, rather than signed per keystroke.
- **Steering Authenticity (Opus #23 P1):** `steering_confirm_req` carries a hardware P-256 `SignedSteeringConfirm` (`gitpulse-steering/v1\n` + canonical JSON over `text_sha256`), produced exclusively by the native steering sheet (`WaylodeSteeringSheetPresenter` on iOS/Android) after rendering the exact steering text natively and verifying biometrics. The host must verify this signature (with mandatory revocation list) before depositing `HumanConfirmed`.

## Commands

```bash
# Install real Expo SDK 55 / React Native / @waylode/core dependencies
bun install

# Type-check against real React Native / Expo / @waylode/core types & run unit/security-invariant tests
bun run typecheck
bun run test

# Inspect local Rust FFI toolchain & build host staticlib/cdylib verification artifacts
./scripts/build-rust-ffi.sh --check
./scripts/build-rust-ffi.sh --platform host --release

# Build mobile Rust FFI libraries (requires mobile rustup targets + Xcode / cargo-ndk)
./scripts/build-rust-ffi.sh --platform ios --release
./scripts/build-rust-ffi.sh --platform android --release

# Native prebuild (requires public X.509 certificate at ./certificates/code-signing-certificate.pem
# or WAYLODE_CODE_SIGNING_CERT=./relative/path/to/public-cert.pem per Opus M3)
npx expo prebuild --platform ios --no-install
npx expo prebuild --platform android --no-install

# Dev-client runs (after building Rust FFI binaries & placing public OTA code-signing cert)
npx expo run:ios
npx expo run:android
```

## Rust FFI Toolchain Prerequisites

`src-tauri/crates/waylode-protocol` defines `crate-type = ["rlib", "staticlib", "cdylib"]`. To cross-compile the mobile native libraries with `./scripts/build-rust-ffi.sh`:

```bash
# iOS XCFramework (macOS + Xcode)
rustup target add aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios

# Android .so jniLibs (Android NDK + cargo-ndk)
rustup target add aarch64-linux-android armv7-linux-androideabi x86_64-linux-android
cargo install cargo-ndk

# Optional: UniFFI Swift/Kotlin binding generator
cargo install uniffi_bindgen_cli
```

Generated native build directories (`node_modules/`, `.expo/`, `ios/`, `android/`, `modules/waylode-crypto/ios/Frameworks/`, `modules/waylode-crypto/android/src/main/jniLibs/`) are gitignored so only source and config plugins are committed.

## Verified Local Toolchains & Checks

Verified on macOS (`arm64-apple-darwin`) with:

- **JS / TypeScript / Expo:** `bun 1.4.4`, `node v26.13.1`, `typescript 5.9.3`, `vitest 3.2.7`, `expo 55.0.32`, `react 19.2.0`, `react-native 0.83.0`, `expo-router 55.0.18`, `@shopify/flash-list 2.3.3`, `expo-updates 55.0.33`, `expo-notifications 55.0.27`, `@waylode/core` (`file:../../packages/waylode-core`).
  - `tsc --noEmit`: 0 errors (zero React/RN/Expo ambient shims).
  - `vitest run --config vitest.config.ts`: all unit, security-invariant, and `with-nse` plugin execution tests passing.
  - `expo prebuild --platform ios --no-install` & `--platform android --no-install`: verified end-to-end with an ephemeral public test cert (`plutil -lint` OK on generated NSE `Info.plist` and `.entitlements`; `AndroidManifest.xml` emits non-exported FCM service/receiver and `USE_FULL_SCREEN_INTENT` `tools:node="remove"`).
- **Rust (`src-tauri/crates/waylode-protocol`):** `cargo 1.100.0` (`aarch64-apple-darwin` host).
  - `./scripts/build-rust-ffi.sh --platform host --release`: builds `libwaylode_protocol.a` (`staticlib`) and `libwaylode_protocol.dylib` (`cdylib`).
  - `cargo clippy --manifest-path src-tauri/crates/waylode-protocol/Cargo.toml --all-targets --locked -- -D warnings`: 0 warnings.
  - `cargo test --manifest-path src-tauri/crates/waylode-protocol/Cargo.toml --locked`: 11/11 KAT tests passing.
  - Note: mobile cross-targets (`aarch64-apple-ios*`, `*-linux-android`), `cargo-ndk`, and `uniffi-bindgen` are not pre-installed on this host, so iOS `.xcframework` and Android `.so` cross-compilation was not executed here.
- **iOS Swift:** `Xcode 27.2`, `swiftc 6.4.2`, `iPhoneSimulator27.2.sdk`.
  - `swiftc -parse` passed on all 5 Swift files.
  - `xcrun --sdk iphonesimulator swiftc -typecheck -warnings-as-errors -target arm64-apple-ios16.0-simulator` passed on `NotificationService.swift` standalone, on all 4 `WaylodeCrypto` module Swift files against SDK 55 `ExpoModulesCore` API signatures, and on the `#if canImport(WaylodeProtocol)` branches.
- **Android Kotlin:** `kotlinc 2.4.21` (JRE 27.0.1.1), `ANDROID_HOME` (`platforms/android-36/android.jar`).
  - `WaylodeRustBridge.kt` compiled directly against `android-36/android.jar` with zero stubs.
  - All 6 Kotlin files (`WaylodeRustBridge.kt`, `WaylodeCryptoModule.kt`, `WaylodeApprovalSheetView.kt`, `WaylodeSteeringSheetView.kt`, `WaylodeMessagingService.kt`, `WaylodeNotificationActionReceiver.kt`) compiled with `kotlinc -Werror` against `android-36/android.jar` plus compile-time stubs for AAR dependencies (`expo-modules-core` SDK 55, `androidx.biometric`, `androidx.core`, `androidx.fragment`, `firebase-messaging`, `kotlinx-coroutines`); a full Gradle APK/AAR link was not run because Android NDK / `cargo-ndk` and Gradle Firebase config are not present in this checkout.
