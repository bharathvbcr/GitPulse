import type React from "react";
import type {
  NativeApprovalSheetOutcome,
  NativeApprovalSheetRequest,
} from "./WaylodeCrypto.types";

export interface WaylodeApprovalSheetViewProps {
  request: NativeApprovalSheetRequest;
  onOutcome: (event: { nativeEvent: NativeApprovalSheetOutcome }) => void;
  style?: Record<string, unknown>;
}

/**
 * Native ExpoView component backed by:
 * - iOS: `WaylodeApprovalSheetView.swift` (UIKit + Secure Enclave P-256 + Rust XCFramework)
 * - Android: `WaylodeApprovalSheetView.kt` (Android View + StrongBox BiometricPrompt + Rust `.so`)
 *
 * Security invariant (Opus M1 & M3):
 * Native code renders the raw canonical payload bytes directly and recomputes `input_sha256`
 * in Rust before biometric P-256 signing, preventing any compromised JS/OTA bundle from
 * spoofing what was displayed versus what was signed.
 */
let cachedNativeView: React.ComponentType<WaylodeApprovalSheetViewProps> | null = null;

export function getNativeApprovalSheetView(): React.ComponentType<WaylodeApprovalSheetViewProps> | null {
  if (cachedNativeView) {
    return cachedNativeView;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const expoModules = require("expo-modules-core") as {
      requireNativeView: <P>(moduleName: string, viewName?: string) => React.ComponentType<P>;
    };
    cachedNativeView = expoModules.requireNativeView<WaylodeApprovalSheetViewProps>(
      "WaylodeCrypto",
      "WaylodeApprovalSheetView",
    );
    return cachedNativeView;
  } catch {
    return null;
  }
}
