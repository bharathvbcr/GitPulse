# GitPulse Mobile (`apps/mobile`)

Single cross-platform **iOS + Android** mobile companion for the GitPulse Unified Control Plane, built on **Expo (React Native New Architecture: Fabric + TurboModules)** + shared Rust **`crates/gitpulse-protocol`** (UniFFI XCFramework / Android `.so`).

## Security Architecture (Opus M1–M5 & P0-4 / P0-7)

- **M1 — No Lock-Screen Approve:** Notification quick actions expose **Deny** and **Deny & Stop** only (`plugins/with-nse.ts`, `NotificationService.swift`, `GitPulseMessagingService.kt`). Tapping any approval notification opens the app to the native full-bytes confirmation sheet.
- **M2 — Two-Key Hardware Isolation:**
  - **Approval Key:** Non-exportable hardware **P-256** key in iOS Secure Enclave (`kSecAttrAccessibleWhenUnlockedThisDeviceOnly` + `.biometryCurrentSet`) and Android StrongBox (`setUserAuthenticationRequired(true)` + `setInvalidatedByBiometricEnrollment(true)`), used exclusively for `approval_decide` signatures.
  - **Identity Keys:** Ed25519 / X25519 keys stored in iOS App Group Keychain (`group.com.gitpulse.mobile`, `AfterFirstUnlockThisDeviceOnly`) / Android Keystore AES-256-GCM wrapped storage. Raw private key bytes never cross the JS bridge and are zeroized immediately after native/Rust calls.
- **M3 — Native Approval Sheet + Code-Signed OTA:** `app.config.ts` enforces mandatory offline-key code signing on `expo-updates` (`codeSigningCertificate` + `codeSigningMetadata`). The approval confirmation sheet (`GitPulseApprovalSheet`) runs in native UIKit / Android View, calling Rust `render_canonical_tool_input` and `hash_canonical`, failing closed on hash mismatch or non-canonical numbers.
- **M4 — Minimal FFI Surface:** Native bridges expose only the small, frozen `crates/gitpulse-protocol/src/uniffi_api.rs` surface.
- **M5 — Android FCM Hygiene:** `GitPulseMessagingService` always posts a visible notification within an 8s guard (inside Android's 10s FCM budget), falls back to a generic banner on decrypt failure/timeout, and omits `USE_FULL_SCREEN_INTENT`.
- **P0-4 — Minimal Decision Surface:** Strictly `allow_once | deny | deny_and_interrupt`; `updatedInput` and `updatedPermissions` are rejected at both TypeScript and Rust boundaries.
- **P0-7 — Opt-In PTY Input Gate:** `app/pty/[sessionKey].tsx` is strictly read-only by default; `pty_input` requires an unexpired Desktop-granted time-boxed `PtyConsentGrant` plus local biometric step-up, auto-relocking on expiry or drawer close. **Accepted residual risk (Opus #23):** individual `pty.input` keystrokes within an active Desktop-granted consent window are authenticated by the Noise IK tunnel and audited under `audit_id`, rather than signed per keystroke.
- **Steering Authenticity (Opus #23 P1):** `steering_confirm_req` carries a hardware P-256 `SignedSteeringConfirm` (`gitpulse-steering/v1\n` + canonical JSON over `text_sha256`), produced exclusively by the native steering sheet (`GitPulseSteeringSheetPresenter` on iOS/Android) after rendering the exact steering text natively and verifying biometrics. The host must verify this signature (with mandatory revocation list) before depositing `HumanConfirmed`.

## Commands

```bash
# Type-check & unit/security-invariant tests
bun run typecheck
bun run test

# Native prebuild & dev-client runs (requires Xcode / Android SDK + Rust UniFFI binaries)
npx expo prebuild
npx expo run:ios
npx expo run:android
```
