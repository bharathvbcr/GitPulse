import {
  assertNoRawKeyMaterial,
  assertValidSignedApprovalDecision,
  type ApprovalDecisionKind,
  type BiometricStepUpResult,
  type DecryptedPushPreview,
  type DenyOnlySignRequest,
  type DevicePublicKeys,
  type NativeApprovalSheetOutcome,
  type NativeApprovalSheetRequest,
  type NativeGitPulseCryptoBinding,
  type RelayAuthHeaders,
  type SignedApprovalDecision,
  type TunnelInitiatorParams,
  type VerifyApprovalDecisionParams,
} from "./GitPulseCrypto.types";

let injectedBindingForTests: NativeGitPulseCryptoBinding | null = null;

function resolveNativeBinding(): NativeGitPulseCryptoBinding {
  if (injectedBindingForTests) {
    return injectedBindingForTests;
  }
  try {
    // eslint-disable-next-line @typescript-eslint/no-require-imports
    const expoModules = require("expo-modules-core") as {
      requireNativeModule: <T>(name: string) => T;
    };
    return expoModules.requireNativeModule<NativeGitPulseCryptoBinding>("GitPulseCrypto");
  } catch (err) {
    throw new Error(
      `GitPulseCrypto native module unavailable. Build with expo-dev-client and linked Rust UniFFI library. (${err instanceof Error ? err.message : String(err)})`,
    );
  }
}

/**
 * Test-only hook for injecting a mock native binding in Node/Vitest unit tests.
 */
export function __setGitPulseCryptoBindingForTests(
  binding: NativeGitPulseCryptoBinding | null,
): void {
  injectedBindingForTests = binding;
}

/**
 * Hardened TypeScript facade over the native `GitPulseCrypto` Expo Module (Opus M1–M4).
 *
 * Every boundary enforces:
 * - Zero raw private keys crossing the JS bridge (Opus M2)
 * - `allow_once` ONLY via the native full-bytes confirmation sheet (Opus M1 & M3)
 * - Strictly `allow_once | deny | deny_and_interrupt` with zero `updatedInput`/`updatedPermissions` (Opus P0-4)
 */
export const GitPulseCrypto = {
  async ensureDeviceKeys(): Promise<DevicePublicKeys> {
    const result = await resolveNativeBinding().ensureDeviceKeys();
    assertNoRawKeyMaterial(result, "ensureDeviceKeys");
    return result;
  },

  async hashCanonical(toolName: string, toolInputJson: string, cwd: string): Promise<string> {
    return resolveNativeBinding().hashCanonical(toolName, toolInputJson, cwd);
  },

  async renderCanonicalToolInput(
    toolName: string,
    toolInputJson: string,
    cwd: string,
  ): Promise<string> {
    return resolveNativeBinding().renderCanonicalToolInput(toolName, toolInputJson, cwd);
  },

  async signRelayRequest(
    method: string,
    requestUri: string,
    timestampMs: number,
    bodyBytesB64: string,
  ): Promise<RelayAuthHeaders> {
    const headers = await resolveNativeBinding().signRelayRequest(
      method,
      requestUri,
      timestampMs,
      bodyBytesB64,
    );
    assertNoRawKeyMaterial(headers, "signRelayRequest");
    return headers;
  },

  async openPreview(
    previewCtB64: string,
    senderEd25519PubB64: string,
    expectedSenderDeviceId: string,
  ): Promise<DecryptedPushPreview> {
    if (previewCtB64.length === 0) {
      throw new Error("preview_ct_b64 must be non-empty");
    }
    const preview = await resolveNativeBinding().openPreview(
      previewCtB64,
      senderEd25519PubB64,
      expectedSenderDeviceId,
    );
    assertNoRawKeyMaterial(preview, "openPreview");
    return preview;
  },

  async tunnelInitiatorNew(
    params: TunnelInitiatorParams,
  ): Promise<{ session_id: string; handshake_init_b64: string }> {
    const init = await resolveNativeBinding().tunnelInitiatorNew(params);
    assertNoRawKeyMaterial(init, "tunnelInitiatorNew");
    return init;
  },

  async tunnelEncrypt(sessionId: string, plaintextB64: string): Promise<string> {
    return resolveNativeBinding().tunnelEncrypt(sessionId, plaintextB64);
  },

  async tunnelDecrypt(sessionId: string, ciphertextB64: string): Promise<string> {
    return resolveNativeBinding().tunnelDecrypt(sessionId, ciphertextB64);
  },

  async tunnelClose(sessionId: string): Promise<void> {
    return resolveNativeBinding().tunnelClose(sessionId);
  },

  /**
   * Presents the native/Rust full-bytes confirmation sheet (Opus M1 & M3).
   * JS NEVER signs `allow_once` directly; native calls Rust `render_canonical_tool_input`
   * + `hash_canonical`, verifies `expected_input_sha256`, renders the exact canonical bytes
   * natively, prompts FaceID/TouchID/BiometricPrompt, and signs with the hardware P-256 key.
   */
  async presentApprovalSheet(
    request: NativeApprovalSheetRequest,
  ): Promise<NativeApprovalSheetOutcome> {
    const nowSec = Math.floor(Date.now() / 1000);
    if (request.expires_at <= nowSec) {
      return {
        status: "expired",
        error_message: "Approval ticket has already expired.",
      };
    }
    const outcome = await resolveNativeBinding().presentApprovalSheet(request);
    assertNoRawKeyMaterial(outcome, "presentApprovalSheet");
    if (outcome.status === "signed") {
      if (!outcome.signed_decision) {
        throw new Error("Native approval sheet returned status='signed' without signed_decision.");
      }
      assertValidSignedApprovalDecision(outcome.signed_decision);
      if (outcome.signed_decision.input_sha256 !== request.expected_input_sha256) {
        throw new Error(
          "SECURITY VIOLATION (Opus M1): Signed input_sha256 does not match ticket expected_input_sha256.",
        );
      }
    }
    return outcome;
  },

  /**
   * Allows background / notification quick-action signing ONLY for `deny` or `deny_and_interrupt`.
   * Explicitly rejects `allow_once` (Opus M1).
   */
  async signDenyWithoutApprove(request: DenyOnlySignRequest): Promise<SignedApprovalDecision> {
    if (
      (request.decision as string) !== "deny" &&
      (request.decision as string) !== "deny_and_interrupt"
    ) {
      throw new Error(
        `SECURITY VIOLATION (Opus M1): signDenyWithoutApprove forbids "${String(request.decision)}". Approvals MUST open the native full-bytes confirmation sheet.`,
      );
    }
    const signed = await resolveNativeBinding().signDenyDecision(request);
    assertValidSignedApprovalDecision(signed);
    if (signed.decision === "allow_once") {
      throw new Error(
        "SECURITY VIOLATION (Opus M1): Native deny path unexpectedly returned allow_once.",
      );
    }
    return signed;
  },

  async verifyApprovalDecision(
    params: VerifyApprovalDecisionParams,
  ): Promise<ApprovalDecisionKind> {
    const parsed = JSON.parse(params.decision_json) as unknown;
    assertValidSignedApprovalDecision(parsed);
    return resolveNativeBinding().verifyApprovalDecision(params);
  },

  /**
   * Hardware biometric step-up required before unlocking opt-in `pty.input` (Opus P0-7).
   */
  async authenticateBiometricStepUp(reason: string): Promise<BiometricStepUpResult> {
    const result = await resolveNativeBinding().authenticateBiometricStepUp(reason);
    assertNoRawKeyMaterial(result, "authenticateBiometricStepUp");
    return result;
  },
};
