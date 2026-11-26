/**
 * Minimal stable TypeScript types for the native `WaylodeCrypto` Expo Module (Opus M1–M4).
 * Aligned 100% with `src-tauri/crates/waylode-protocol/src/uniffi_api.rs` & `wire.rs` (board #18).
 *
 * Security Invariants:
 * - Opus M2: Raw private key bytes (Ed25519, X25519, or P-256) NEVER cross the JS bridge.
 *   Only public keys, opaque tunnel handles, signatures, and decrypted application payloads
 *   are returned to JS.
 * - Opus M1 & M3: `allow_once` decisions may ONLY be produced by the native/Rust
 *   `WaylodeApprovalSheet`, which calls Rust `render_canonical_tool_input` + `hash_canonical`,
 *   renders the exact canonical bytes natively, and signs with hardware P-256 under biometrics.
 * - Opus P0-4: `ApprovalDecisionKind` is strictly `"allow_once" | "deny" | "deny_and_interrupt"`.
 *   `updatedInput` and `updatedPermissions` are forbidden.
 */

export const ALLOWED_APPROVAL_DECISIONS = [
  "allow_once",
  "deny",
  "deny_and_interrupt",
] as const;

export type ApprovalDecisionKind = (typeof ALLOWED_APPROVAL_DECISIONS)[number];
export type DenyOnlyDecisionKind = "deny" | "deny_and_interrupt";

/**
 * Public-only device key material returned by native Keychain/Keystore + Secure Enclave/StrongBox.
 * Notice: ZERO private key or seed properties exist on this interface.
 */
export interface DevicePublicKeys {
  readonly device_id: string;
  readonly ed25519_pub_b64: string;
  readonly x25519_pub_b64: string;
  /** Uncompressed SEC1 P-256 public key (base64) enrolled in DeviceCertificate v2. */
  readonly approval_pub_p256_b64: string;
  /** True if backed by iOS Secure Enclave or Android StrongBox; false if TEE fallback. */
  readonly hardware_backed: boolean;
  readonly key_storage_backend: "ios_secure_enclave" | "android_strongbox" | "android_tee";
}

export interface RelayAuthHeaders {
  readonly "X-GitPulse-Device": string;
  readonly "X-GitPulse-Timestamp": string;
  readonly "X-GitPulse-Signature": string;
}

export interface DecryptedPushPreview {
  readonly ticket_id: string;
  readonly title: string;
  readonly subtitle: string;
  readonly body: string;
}

export interface NativeApprovalSheetRequest {
  readonly host_id: string;
  readonly ticket_id: string;
  readonly tool_name: string;
  /** Raw JSON of tool_input; native calls Rust `render_canonical_tool_input` & `hash_canonical`. */
  readonly tool_input_json: string;
  readonly cwd: string;
  readonly expected_input_sha256: string;
  readonly repo_label: string;
  readonly worktree_label?: string;
  readonly harness_label: string;
  /** Unix timestamp in seconds (matches `ApprovalTicket.expires_at`). */
  readonly expires_at: number;
}

/**
 * Matches `src-tauri/crates/waylode-protocol/src/wire.rs` `SignedApprovalDecision` (`#[serde(deny_unknown_fields)]`).
 */
export interface SignedApprovalDecision {
  readonly host_id: string;
  readonly ticket_id: string;
  readonly input_sha256: string;
  readonly decision: ApprovalDecisionKind;
  readonly nonce: string;
  readonly issued_at: number;
  readonly expires_at: number;
  readonly device_id: string;
  readonly sig_p256_der_b64: string;
}

export interface NativeApprovalSheetOutcome {
  readonly status: "signed" | "cancelled" | "hash_mismatch" | "biometric_failed" | "expired";
  readonly signed_decision?: SignedApprovalDecision;
  readonly computed_input_sha256?: string;
  readonly error_message?: string;
}

export interface DenyOnlySignRequest {
  readonly host_id: string;
  readonly ticket_id: string;
  readonly input_sha256: string;
  readonly decision: DenyOnlyDecisionKind;
  readonly expires_at: number;
}

/**
 * Hardware P-256 signed steering confirmation (Opus #23 P1 steering authenticity).
 * Domain: `gitpulse-steering/v1\n` + canonical sorted JSON.
 * Signed exclusively from the native steering sheet after rendering the exact steering text
 * natively and authenticating with Secure Enclave / StrongBox biometrics.
 */
export interface SignedSteeringConfirm {
  readonly host_id: string;
  readonly session_key: string;
  readonly request_id: string;
  readonly expect_route: string;
  readonly text_sha256: string;
  readonly issued_at: number;
  readonly expires_at: number;
  readonly nonce: string;
  readonly device_id: string;
  readonly sig_p256_der_b64: string;
}

export interface NativeSteeringSheetRequest {
  readonly host_id: string;
  readonly session_key: string;
  readonly request_id: string;
  readonly expect_route: string;
  readonly delivery_mode: string;
  /** Exact steering text rendered natively and SHA-256 hashed in Rust before P-256 signing. */
  readonly text: string;
  readonly repo_label: string;
  readonly harness_label: string;
  readonly warning?: string;
  /** Validity window in seconds for the signed confirmation envelope (default 300s). */
  readonly ttl_sec?: number;
}

export interface NativeSteeringSheetOutcome {
  readonly status: "signed" | "cancelled" | "biometric_failed" | "error";
  readonly signed_confirm?: SignedSteeringConfirm;
  readonly computed_text_sha256?: string;
  readonly error_message?: string;
}

export interface VerifyApprovalDecisionParams {
  readonly decision_json: string;
  readonly cert_json: string;
  readonly expected_host_id: string;
  readonly expected_ticket_id: string;
  readonly expected_input_sha256: string;
  readonly revocations_json: string;
  readonly min_revocation_version: number;
  readonly now_sec: number;
}

export interface TunnelInitiatorParams {
  readonly remote_cert_json: string;
  readonly revocations_json: string;
  readonly min_revocation_version: number;
  readonly now_sec: number;
  readonly stream_id: string;
  readonly host_epoch: number;
}

export interface BiometricStepUpResult {
  readonly ok: boolean;
  readonly attested_at_ms?: number;
  readonly error_message?: string;
}

export interface NativeWaylodeCryptoBinding {
  ensureDeviceKeys(): Promise<DevicePublicKeys>;
  hashCanonical(toolName: string, toolInputJson: string, cwd: string): Promise<string>;
  renderCanonicalToolInput(toolName: string, toolInputJson: string, cwd: string): Promise<string>;
  signRelayRequest(
    method: string,
    requestUri: string,
    timestampMs: number,
    bodyBytesB64: string,
  ): Promise<RelayAuthHeaders>;
  openPreview(
    previewCtB64: string,
    senderEd25519PubB64: string,
    expectedSenderDeviceId: string,
  ): Promise<DecryptedPushPreview>;
  tunnelInitiatorNew(params: TunnelInitiatorParams): Promise<{
    session_id: string;
    handshake_init_b64: string;
  }>;
  tunnelEncrypt(sessionId: string, plaintextB64: string): Promise<string>;
  tunnelDecrypt(sessionId: string, ciphertextB64: string): Promise<string>;
  tunnelClose(sessionId: string): Promise<void>;
  presentApprovalSheet(request: NativeApprovalSheetRequest): Promise<NativeApprovalSheetOutcome>;
  presentSteeringConfirmSheet(
    request: NativeSteeringSheetRequest,
  ): Promise<NativeSteeringSheetOutcome>;
  signDenyDecision(request: DenyOnlySignRequest): Promise<SignedApprovalDecision>;
  verifyApprovalDecision(params: VerifyApprovalDecisionParams): Promise<ApprovalDecisionKind>;
  authenticateBiometricStepUp(reason: string): Promise<BiometricStepUpResult>;
}

const FORBIDDEN_KEY_FIELD_PATTERNS = [
  "private",
  "secret",
  "seed",
  "raw_key",
  "rawkey",
  "priv_key",
  "privkey",
  "signing_key",
  "signingkey",
  "pkcs8",
] as const;

const FORBIDDEN_APPROVAL_KEYS = new Set([
  "updatedInput",
  "updated_input",
  "updatedPermissions",
  "updated_permissions",
]);

const ALLOWED_SIGNED_DECISION_KEYS = new Set([
  "host_id",
  "ticket_id",
  "input_sha256",
  "decision",
  "nonce",
  "issued_at",
  "expires_at",
  "device_id",
  "sig_p256_der_b64",
]);

/**
 * Opus M2 guard: Ensures no native bridge return value ever leaks private/raw key bytes to JS.
 */
export function assertNoRawKeyMaterial(payload: unknown, path = "root"): void {
  if (payload === null || payload === undefined || typeof payload !== "object") {
    return;
  }
  if (payload instanceof Uint8Array || payload instanceof ArrayBuffer) {
    throw new Error(
      `SECURITY VIOLATION (Opus M2): Binary key buffer crossed JS bridge at ${path}.`,
    );
  }
  for (const [key, value] of Object.entries(payload as Record<string, unknown>)) {
    const keyLower = key.toLowerCase();
    for (const pattern of FORBIDDEN_KEY_FIELD_PATTERNS) {
      if (keyLower.includes(pattern)) {
        throw new Error(
          `SECURITY VIOLATION (Opus M2): Forbidden private/raw key field "${path}.${key}" crossed JS bridge.`,
        );
      }
    }
    if (value && typeof value === "object") {
      assertNoRawKeyMaterial(value, `${path}.${key}`);
    }
  }
}

/**
 * Opus P0-4 guard: Mirrors `#[serde(deny_unknown_fields)]` on Rust `SignedApprovalDecision`.
 * Strictly accepts ONLY `allow_once | deny | deny_and_interrupt` and rejects
 * `updatedInput`, `updatedPermissions`, or any unknown field.
 */
export function assertValidSignedApprovalDecision(
  candidate: unknown,
): asserts candidate is SignedApprovalDecision {
  if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
    throw new Error("SECURITY VIOLATION (Opus P0-4): ApprovalDecision must be a JSON object.");
  }

  const record = candidate as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (FORBIDDEN_APPROVAL_KEYS.has(key)) {
      throw new Error(
        `SECURITY VIOLATION (Opus P0-4): Forbidden field "${key}" in ApprovalDecision. Mobile may only emit allow_once, deny, or deny_and_interrupt.`,
      );
    }
    if (!ALLOWED_SIGNED_DECISION_KEYS.has(key)) {
      throw new Error(
        `SECURITY VIOLATION (Opus P0-4): Unknown field "${key}" rejected by SignedApprovalDecision deny_unknown_fields contract.`,
      );
    }
  }

  const decision = record.decision;
  if (
    typeof decision !== "string" ||
    !(ALLOWED_APPROVAL_DECISIONS as readonly string[]).includes(decision)
  ) {
    throw new Error(
      `SECURITY VIOLATION (Opus P0-4): Invalid approval decision "${String(decision)}". Allowed: allow_once | deny | deny_and_interrupt.`,
    );
  }

  const requiredStringFields = [
    "host_id",
    "ticket_id",
    "input_sha256",
    "nonce",
    "device_id",
    "sig_p256_der_b64",
  ] as const;
  for (const field of requiredStringFields) {
    if (typeof record[field] !== "string" || (record[field] as string).length === 0) {
      throw new Error(`Invalid SignedApprovalDecision: missing non-empty "${field}".`);
    }
  }

  if (typeof record.issued_at !== "number" || typeof record.expires_at !== "number") {
    throw new Error("Invalid SignedApprovalDecision: issued_at and expires_at must be numbers.");
  }

  assertNoRawKeyMaterial(record, "SignedApprovalDecision");
}

const ALLOWED_SIGNED_STEERING_KEYS = new Set([
  "host_id",
  "session_key",
  "request_id",
  "expect_route",
  "text_sha256",
  "issued_at",
  "expires_at",
  "nonce",
  "device_id",
  "sig_p256_der_b64",
]);

/**
 * Opus #23 P1 guard: Strictly validates a hardware-signed `SignedSteeringConfirm`
 * (`#[serde(deny_unknown_fields)]` parity) before sending over the tunnel.
 */
export function assertValidSignedSteeringConfirm(
  candidate: unknown,
): asserts candidate is SignedSteeringConfirm {
  if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
    throw new Error(
      "SECURITY VIOLATION (Opus #23 P1): SignedSteeringConfirm must be a JSON object.",
    );
  }
  const record = candidate as Record<string, unknown>;
  for (const key of Object.keys(record)) {
    if (!ALLOWED_SIGNED_STEERING_KEYS.has(key)) {
      throw new Error(
        `SECURITY VIOLATION (Opus #23 P1): Unknown field "${key}" rejected in SignedSteeringConfirm.`,
      );
    }
  }

  const requiredStringFields = [
    "host_id",
    "session_key",
    "request_id",
    "expect_route",
    "text_sha256",
    "nonce",
    "device_id",
    "sig_p256_der_b64",
  ] as const;
  for (const field of requiredStringFields) {
    if (typeof record[field] !== "string" || (record[field] as string).length === 0) {
      throw new Error(`Invalid SignedSteeringConfirm: missing non-empty "${field}".`);
    }
  }

  if (!/^[0-9a-f]{64}$/.test(record.text_sha256 as string)) {
    throw new Error(
      "Invalid SignedSteeringConfirm: text_sha256 must be a 64-char lowercase hex SHA-256.",
    );
  }
  if (typeof record.issued_at !== "number" || typeof record.expires_at !== "number") {
    throw new Error("Invalid SignedSteeringConfirm: issued_at and expires_at must be numbers.");
  }
  if (record.expires_at <= record.issued_at) {
    throw new Error("Invalid SignedSteeringConfirm: expires_at must be after issued_at.");
  }

  assertNoRawKeyMaterial(record, "SignedSteeringConfirm");
}
