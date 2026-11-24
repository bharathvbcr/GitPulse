import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { afterEach, describe, expect, it } from "vitest";
import {
  buildMobileAppConfig,
  GITPULSE_IOS_APP_GROUP,
  validateMobileAppConfig,
} from "../app.config";
import {
  ACTION_DENY_AND_INTERRUPT_ID,
  ACTION_DENY_ID,
  APPROVAL_NOTIFICATION_ACTIONS,
  assertNoLockScreenApproveActions,
  describePushExtensionsPlan,
  NOTIFICATION_CATEGORIES,
  withGitPulsePushExtensions,
} from "../plugins/with-nse";
import {
  __setGitPulseCryptoBindingForTests,
  assertNoRawKeyMaterial,
  assertValidSignedApprovalDecision,
  assertValidSignedSteeringConfirm,
  GitPulseCrypto,
  type NativeGitPulseCryptoBinding,
  type SignedApprovalDecision,
  type SignedSteeringConfirm,
} from "../modules/gitpulse-crypto/src/index";
import {
  ControlPlaneClient,
  isPtyInputUnlocked,
  type WebSocketLike,
} from "../src/client/ControlPlaneClient";

const MOBILE_ROOT = path.resolve(__dirname, "..");

function makeValidDecision(
  overrides: Partial<SignedApprovalDecision> = {},
): SignedApprovalDecision {
  return {
    host_id: "host-mac-1",
    ticket_id: "tkt-123",
    input_sha256: "a".repeat(64),
    decision: "allow_once",
    nonce: "nonce-1",
    issued_at: 1_700_000_000,
    expires_at: 1_700_001_800,
    device_id: "ios-dev-1",
    sig_p256_der_b64: "MEUCIQDfakep256sig==",
    ...overrides,
  };
}

function makeMockBinding(
  overrides: Partial<NativeGitPulseCryptoBinding> = {},
): NativeGitPulseCryptoBinding {
  return {
    async ensureDeviceKeys() {
      return {
        device_id: "ios-dev-1",
        ed25519_pub_b64: "edPubB64==",
        x25519_pub_b64: "xPubB64==",
        approval_pub_p256_b64: "BAp256Sec1PubB64==",
        hardware_backed: true,
        key_storage_backend: "ios_secure_enclave",
      };
    },
    async hashCanonical() {
      return "a".repeat(64);
    },
    async renderCanonicalToolInput(_toolName, toolInputJson) {
      return toolInputJson;
    },
    async signRelayRequest() {
      return {
        "X-GitPulse-Device": "ios-dev-1",
        "X-GitPulse-Timestamp": "1700000000000",
        "X-GitPulse-Signature": "sigB64==",
      };
    },
    async openPreview() {
      return {
        ticket_id: "tkt-123",
        title: "Approval Required",
        subtitle: "Bash",
        body: "Open GitPulse to inspect",
      };
    },
    async tunnelInitiatorNew() {
      return { session_id: "sess-1", handshake_init_b64: "initB64==" };
    },
    async tunnelEncrypt() {
      return "ctB64==";
    },
    async tunnelDecrypt() {
      return "ptB64==";
    },
    async tunnelClose() {},
    async presentApprovalSheet(req) {
      return {
        status: "signed",
        computed_input_sha256: req.expected_input_sha256,
        signed_decision: makeValidDecision({
          host_id: req.host_id,
          ticket_id: req.ticket_id,
          input_sha256: req.expected_input_sha256,
          decision: "allow_once",
        }),
      };
    },
    async presentSteeringConfirmSheet(req) {
      const sha = "c".repeat(64);
      return {
        status: "signed",
        computed_text_sha256: sha,
        signed_confirm: {
          host_id: req.host_id,
          session_key: req.session_key,
          request_id: req.request_id,
          expect_route: req.expect_route,
          text_sha256: sha,
          issued_at: 1_700_000_000,
          expires_at: 1_700_000_300,
          nonce: "steer-nonce-1",
          device_id: "ios-dev-1",
          sig_p256_der_b64: "MEUCIQDsteeringp256sig==",
        },
      };
    },
    async signDenyDecision(req) {
      return makeValidDecision({
        host_id: req.host_id,
        ticket_id: req.ticket_id,
        input_sha256: req.input_sha256,
        decision: req.decision,
      });
    },
    async verifyApprovalDecision() {
      return "allow_once";
    },
    async authenticateBiometricStepUp() {
      return { ok: true, attested_at_ms: Date.now() };
    },
    ...overrides,
  };
}

afterEach(() => {
  __setGitPulseCryptoBindingForTests(null);
});

describe("Opus M1: No lock-screen Approve action exists anywhere", () => {
  it("notification categories expose ONLY Deny and Deny & Stop", () => {
    expect(NOTIFICATION_CATEGORIES).toHaveLength(1);
    const approvalCat = NOTIFICATION_CATEGORIES[0]!;
    expect(approvalCat.allowApproveOnLockScreen).toBe(false);
    expect(APPROVAL_NOTIFICATION_ACTIONS.map((a) => a.identifier)).toEqual([
      ACTION_DENY_ID,
      ACTION_DENY_AND_INTERRUPT_ID,
    ]);
    expect(APPROVAL_NOTIFICATION_ACTIONS.map((a) => a.buttonTitle)).toEqual([
      "Deny",
      "Deny & Stop",
    ]);
    expect(() => assertNoLockScreenApproveActions(NOTIFICATION_CATEGORIES)).not.toThrow();

    const plan = describePushExtensionsPlan();
    expect(plan.iosUsesHermes).toBe(false);
    expect(plan.iosMemoryBudgetMb).toBe(24);
    expect(plan.androidAlwaysPostsVisibleNotification).toBe(true);
    expect(plan.androidBudgetSeconds).toBe(10);
  });

  it("assertNoLockScreenApproveActions fails closed if any Approve/Allow action is injected", () => {
    expect(() =>
      assertNoLockScreenApproveActions([
        {
          identifier: "GITPULSE_APPROVAL",
          actions: [{ identifier: "GITPULSE_ACTION_APPROVE", buttonTitle: "Approve" }],
        },
      ]),
    ).toThrow(/Opus M1/i);

    expect(() =>
      assertNoLockScreenApproveActions([
        {
          identifier: "GITPULSE_APPROVAL",
          actions: [{ identifier: "ALLOW_ONCE", buttonTitle: "Allow Once" }],
        },
      ]),
    ).toThrow(/Opus M1/i);
  });

  it("iOS NotificationService.swift never loads Hermes and never registers Approve actions", () => {
    const swiftSrc = fs.readFileSync(
      path.join(MOBILE_ROOT, "plugins/templates/ios/NotificationService.swift"),
      "utf8",
    );
    expect(swiftSrc).toContain("UNNotificationServiceExtension");
    expect(swiftSrc).toContain("maxPreviewCiphertextBytes = 2048");
    expect(swiftSrc).toContain("GITPULSE_APPROVAL");
    expect(swiftSrc).not.toMatch(/import\s+React|import\s+Hermes|RCTBridge/i);
    expect(swiftSrc).not.toMatch(/GITPULSE_ACTION_APPROVE|UNNotificationAction\(identifier:.*allow/i);
  });

  it("Android GitPulseMessagingService.kt always posts visible notification, omits full-screen intent, and exposes Deny-only actions", () => {
    const ktService = fs.readFileSync(
      path.join(MOBILE_ROOT, "plugins/templates/android/GitPulseMessagingService.kt"),
      "utf8",
    );
    const ktReceiver = fs.readFileSync(
      path.join(MOBILE_ROOT, "plugins/templates/android/GitPulseNotificationActionReceiver.kt"),
      "utf8",
    );

    expect(ktService).toContain("postVisibleApprovalNotification");
    expect(ktService).toContain('addAction(0, "Deny", denyPendingIntent)');
    expect(ktService).toContain('addAction(0, "Deny & Stop", denyStopPendingIntent)');
    expect(ktService).not.toMatch(/addAction\([^)]*Approve|addAction\([^)]*Allow/i);
    expect(ktService).not.toMatch(/\.setFullScreenIntent\s*\(/);

    expect(ktReceiver).toContain("ACTION_DENY");
    expect(ktReceiver).toContain("ACTION_DENY_AND_INTERRUPT");
    expect(ktReceiver).not.toMatch(/allow_once/i);
  });

  it("withGitPulsePushExtensions executes iOS NSE + Android FCM mods and enforces AndroidManifest hygiene", async () => {
    const tmpRoot = fs.mkdtempSync(path.join(os.tmpdir(), "gitpulse-nse-test-"));
    const iosRoot = path.join(tmpRoot, "ios");
    const androidRoot = path.join(tmpRoot, "android");
    fs.mkdirSync(iosRoot, { recursive: true });
    fs.mkdirSync(androidRoot, { recursive: true });

    try {
      const base = buildMobileAppConfig({});
      const modded = withGitPulsePushExtensions(base, {
        appGroup: GITPULSE_IOS_APP_GROUP,
        nseBundleIdentifier: "com.gitpulse.mobile.NotificationService",
        androidPackage: "com.gitpulse.mobile",
      }) as unknown as {
        mods?: {
          ios?: {
            dangerous?: (cfg: {
              modRequest: { projectRoot: string; platformProjectRoot: string };
            }) => Promise<unknown>;
          };
          android?: {
            dangerous?: (cfg: {
              modRequest: { projectRoot: string; platformProjectRoot: string };
            }) => Promise<unknown>;
            manifest?: (cfg: {
              modResults: { manifest: { application: Array<Record<string, unknown>> } };
            }) => Promise<{
              modResults: { manifest: { application: Array<Record<string, unknown>> } };
            }>;
          };
        };
      };

      expect(typeof modded.mods?.ios?.dangerous).toBe("function");
      expect(typeof modded.mods?.android?.dangerous).toBe("function");
      expect(typeof modded.mods?.android?.manifest).toBe("function");

      await modded.mods!.ios!.dangerous!({
        modRequest: { projectRoot: tmpRoot, platformProjectRoot: iosRoot },
      });
      const nseDir = path.join(iosRoot, "GitPulseNotificationService");
      const renderedSwift = fs.readFileSync(path.join(nseDir, "NotificationService.swift"), "utf8");
      const renderedPlist = fs.readFileSync(path.join(nseDir, "Info.plist"), "utf8");
      const renderedEntitlements = fs.readFileSync(
        path.join(nseDir, "GitPulseNotificationService.entitlements"),
        "utf8",
      );
      expect(renderedSwift).toContain(`appGroup = "${GITPULSE_IOS_APP_GROUP}"`);
      expect(renderedSwift).not.toContain("__APP_GROUP__");
      expect(renderedPlist).toContain("com.gitpulse.mobile.NotificationService");
      expect(renderedEntitlements).toContain(GITPULSE_IOS_APP_GROUP);

      await modded.mods!.android!.dangerous!({
        modRequest: { projectRoot: tmpRoot, platformProjectRoot: androidRoot },
      });
      const pushDir = path.join(androidRoot, "app/src/main/java/com/gitpulse/mobile/push");
      expect(fs.existsSync(path.join(pushDir, "GitPulseMessagingService.kt"))).toBe(true);
      expect(fs.existsSync(path.join(pushDir, "GitPulseNotificationActionReceiver.kt"))).toBe(true);

      const manifestRes = await modded.mods!.android!.manifest!({
        modResults: { manifest: { application: [{}] } },
      });
      const appNode = manifestRes.modResults.manifest.application[0]!;
      const services = appNode.service as Array<{ $: Record<string, string> }>;
      const receivers = appNode.receiver as Array<{ $: Record<string, string> }>;
      expect(services).toHaveLength(1);
      expect(services[0]!.$["android:name"]).toBe(".push.GitPulseMessagingService");
      expect(services[0]!.$["android:exported"]).toBe("false");
      expect(receivers).toHaveLength(1);
      expect(receivers[0]!.$["android:name"]).toBe(".push.GitPulseNotificationActionReceiver");
      expect(receivers[0]!.$["android:exported"]).toBe("false");
    } finally {
      fs.rmSync(tmpRoot, { recursive: true, force: true });
    }
  });
});

describe("Opus P0-4: ApprovalDecision rejects updatedInput and updatedPermissions", () => {
  it("accepts strictly allow_once, deny, and deny_and_interrupt", () => {
    for (const decision of ["allow_once", "deny", "deny_and_interrupt"] as const) {
      expect(() =>
        assertValidSignedApprovalDecision(makeValidDecision({ decision })),
      ).not.toThrow();
    }
  });

  it("rejects updatedInput, updatedPermissions, unknown fields, and invalid decisions", () => {
    expect(() =>
      assertValidSignedApprovalDecision({
        ...makeValidDecision(),
        updatedInput: { command: "rm -rf /" },
      }),
    ).toThrow(/updatedInput/i);

    expect(() =>
      assertValidSignedApprovalDecision({
        ...makeValidDecision(),
        updated_input: { command: "ls" },
      }),
    ).toThrow(/updated_input/i);

    expect(() =>
      assertValidSignedApprovalDecision({
        ...makeValidDecision(),
        updatedPermissions: [{ tool: "Bash", allow: true }],
      }),
    ).toThrow(/updatedPermissions/i);

    expect(() =>
      assertValidSignedApprovalDecision({
        ...makeValidDecision(),
        updated_permissions: ["*"],
      }),
    ).toThrow(/updated_permissions/i);

    expect(() =>
      assertValidSignedApprovalDecision({
        ...makeValidDecision(),
        decision: "allow_always",
      }),
    ).toThrow(/Invalid approval decision/i);

    expect(() =>
      assertValidSignedApprovalDecision({
        ...makeValidDecision(),
        extra_field: true,
      }),
    ).toThrow(/Unknown field/i);
  });

  it("ControlPlaneClient.submitApprovalDecision blocks payloads containing updatedInput", () => {
    const sent: string[] = [];
    const mockWs: WebSocketLike = {
      send: (d) => sent.push(d),
      close: () => {},
      onopen: null,
      onmessage: null,
      onclose: null,
      onerror: null,
    };
    const client = new ControlPlaneClient({
      relayWsBaseUrl: "wss://relay.example.test/v1/tunnel/ws",
      deviceId: "ios-dev-1",
      transport: { connect: () => mockWs },
    });
    client.connect();

    expect(() =>
      client.submitApprovalDecision({
        ...makeValidDecision(),
        updatedInput: { cmd: "echo hacked" },
      } as unknown as SignedApprovalDecision),
    ).toThrow(/Opus P0-4/i);
    expect(sent.filter((s) => s.includes("approval_decide"))).toHaveLength(0);
  });
});

describe("Opus M2: JS bridge never exposes raw key bytes & enforces two-key hardware isolation", () => {
  it("ensureDeviceKeys returns ONLY public keys and throws if native returns private/seed bytes", async () => {
    __setGitPulseCryptoBindingForTests(makeMockBinding());
    const keys = await GitPulseCrypto.ensureDeviceKeys();
    expect(keys.approval_pub_p256_b64).toBeTruthy();
    expect(keys.ed25519_pub_b64).toBeTruthy();
    expect(keys.x25519_pub_b64).toBeTruthy();

    __setGitPulseCryptoBindingForTests(
      makeMockBinding({
        async ensureDeviceKeys() {
          return {
            device_id: "ios-dev-1",
            ed25519_pub_b64: "pub",
            x25519_pub_b64: "pub",
            approval_pub_p256_b64: "pub",
            hardware_backed: true,
            key_storage_backend: "ios_secure_enclave",
            ed25519_seed: "LEAKED_SEED",
          } as unknown as Awaited<ReturnType<NativeGitPulseCryptoBinding["ensureDeviceKeys"]>>;
        },
      }),
    );

    await expect(GitPulseCrypto.ensureDeviceKeys()).rejects.toThrow(/Opus M2/i);
    expect(() => assertNoRawKeyMaterial({ raw: new Uint8Array([1, 2, 3]) })).toThrow(/Opus M2/i);
  });

  it("signDenyWithoutApprove refuses allow_once even if called dynamically", async () => {
    __setGitPulseCryptoBindingForTests(makeMockBinding());

    await expect(
      GitPulseCrypto.signDenyWithoutApprove({
        host_id: "host-1",
        ticket_id: "tkt-1",
        input_sha256: "a".repeat(64),
        decision: "allow_once" as unknown as "deny",
        expires_at: Math.floor(Date.now() / 1000) + 60,
      }),
    ).rejects.toThrow(/Opus M1/i);

    const denyResult = await GitPulseCrypto.signDenyWithoutApprove({
      host_id: "host-1",
      ticket_id: "tkt-1",
      input_sha256: "a".repeat(64),
      decision: "deny_and_interrupt",
      expires_at: Math.floor(Date.now() / 1000) + 60,
    });
    expect(denyResult.decision).toBe("deny_and_interrupt");
  });

  it("presentApprovalSheet rejects mismatched input_sha256 from native result", async () => {
    __setGitPulseCryptoBindingForTests(
      makeMockBinding({
        async presentApprovalSheet(req) {
          return {
            status: "signed",
            computed_input_sha256: "b".repeat(64),
            signed_decision: makeValidDecision({
              host_id: req.host_id,
              ticket_id: req.ticket_id,
              input_sha256: "b".repeat(64),
            }),
          };
        },
      }),
    );

    await expect(
      GitPulseCrypto.presentApprovalSheet({
        host_id: "host-1",
        ticket_id: "tkt-1",
        tool_name: "Bash",
        tool_input_json: '{"command":"ls"}',
        cwd: "/repo",
        expected_input_sha256: "a".repeat(64),
        repo_label: "GitPulse",
        harness_label: "claude",
        expires_at: Math.floor(Date.now() / 1000) + 600,
      }),
    ).rejects.toThrow(/does not match/i);
  });

  it("native Swift and Kotlin modules enforce Secure Enclave / StrongBox P-256 and zeroize secrets", () => {
    const swiftModule = fs.readFileSync(
      path.join(MOBILE_ROOT, "modules/gitpulse-crypto/ios/GitPulseCryptoModule.swift"),
      "utf8",
    );
    expect(swiftModule).toContain("kSecAttrTokenIDSecureEnclave");
    expect(swiftModule).toContain("kSecAttrAccessibleWhenUnlockedThisDeviceOnly");
    expect(swiftModule).toContain(".biometryCurrentSet");
    expect(swiftModule).toContain("kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly");
    expect(swiftModule).toContain("resetBytes");

    const kotlinModule = fs.readFileSync(
      path.join(
        MOBILE_ROOT,
        "modules/gitpulse-crypto/android/src/main/java/com/gitpulse/crypto/GitPulseCryptoModule.kt",
      ),
      "utf8",
    );
    expect(kotlinModule).toContain("setIsStrongBoxBacked(true)");
    expect(kotlinModule).toContain("setUserAuthenticationRequired(true)");
    expect(kotlinModule).toContain("setInvalidatedByBiometricEnrollment(true)");
    expect(kotlinModule).toContain("Arrays.fill");
  });
});

describe("Opus M3 & M5: app.config.ts mandates New Architecture & code-signed expo-updates", () => {
  it("default mobile config passes validation with New Arch, code signing, App Group, and no full-screen intent", () => {
    const cfg = buildMobileAppConfig({});
    expect(cfg.newArchEnabled).toBe(true);
    expect(cfg.updates.codeSigningCertificate).toBe("./certificates/code-signing-certificate.pem");
    expect(cfg.updates.codeSigningMetadata.alg).toBe("rsa-v1_5-sha256");
    expect(
      (cfg.ios?.entitlements as Record<string, unknown>)["com.apple.security.application-groups"],
    ).toContain(GITPULSE_IOS_APP_GROUP);
    expect(cfg.android?.permissions).not.toContain("android.permission.USE_FULL_SCREEN_INTENT");
  });

  it("validateMobileAppConfig fails closed on missing code signing or leaked private key", () => {
    const base = buildMobileAppConfig({});
    expect(() =>
      validateMobileAppConfig({
        ...base,
        updates: { ...base.updates, codeSigningCertificate: "" },
      }),
    ).toThrow(/Opus M3/i);

    expect(() =>
      validateMobileAppConfig({
        ...base,
        updates: { ...base.updates, codeSigningCertificate: "./private-key.pem" },
      }),
    ).toThrow(/Private signing keys/i);

    expect(() =>
      validateMobileAppConfig({ ...base, newArchEnabled: false }),
    ).toThrow(/New Architecture/i);
  });
});

describe("Opus P0-7: PTY input is read-only by default and requires Desktop consent + biometric step-up", () => {
  it("blocks pty_input without valid grant or biometric attestation and unlocks only when both match", () => {
    const now = 1_700_000_500_000;
    const grant = {
      session_key: "sess-1",
      host_id: "host-1",
      device_id: "ios-dev-1",
      scope: "pty.input" as const,
      granted_at_ms: now - 5_000,
      expires_at_ms: now + 60_000,
      audit_id: "aud-99",
    };

    // Missing grant -> locked
    expect(
      isPtyInputUnlocked({
        grant: undefined,
        sessionKey: "sess-1",
        deviceId: "ios-dev-1",
        biometricAttestedAtMs: now,
        nowMs: now,
      }),
    ).toBe(false);

    // Missing biometric -> locked
    expect(
      isPtyInputUnlocked({
        grant,
        sessionKey: "sess-1",
        deviceId: "ios-dev-1",
        biometricAttestedAtMs: null,
        nowMs: now,
      }),
    ).toBe(false);

    // Biometric before grant -> locked
    expect(
      isPtyInputUnlocked({
        grant,
        sessionKey: "sess-1",
        deviceId: "ios-dev-1",
        biometricAttestedAtMs: grant.granted_at_ms - 1,
        nowMs: now,
      }),
    ).toBe(false);

    // Expired grant -> locked
    expect(
      isPtyInputUnlocked({
        grant,
        sessionKey: "sess-1",
        deviceId: "ios-dev-1",
        biometricAttestedAtMs: now,
        nowMs: grant.expires_at_ms,
      }),
    ).toBe(false);

    // Valid grant + fresh biometric -> unlocked
    expect(
      isPtyInputUnlocked({
        grant,
        sessionKey: "sess-1",
        deviceId: "ios-dev-1",
        biometricAttestedAtMs: now - 1_000,
        nowMs: now,
      }),
    ).toBe(true);
  });
});

describe("Opus #23 P1: Steering authenticity via native biometric P-256 SignedSteeringConfirm", () => {
  function makeValidSignedSteering(
    overrides: Partial<SignedSteeringConfirm> = {},
  ): SignedSteeringConfirm {
    return {
      host_id: "host-1",
      session_key: "claude:repo:1",
      request_id: "req-1",
      expect_route: "peer_inbox",
      text_sha256: "d".repeat(64),
      issued_at: 1_700_000_000,
      expires_at: 1_700_000_300,
      nonce: "nonce-steer-1",
      device_id: "ios-dev-1",
      sig_p256_der_b64: "MEUCIQDsteering==",
      ...overrides,
    };
  }

  it("validates SignedSteeringConfirm strictly and rejects malformed/unknown/expired fields", () => {
    expect(() => assertValidSignedSteeringConfirm(makeValidSignedSteering())).not.toThrow();

    expect(() =>
      assertValidSignedSteeringConfirm({
        ...makeValidSignedSteering(),
        text_sha256: "not-a-sha256",
      }),
    ).toThrow(/text_sha256/i);

    expect(() =>
      assertValidSignedSteeringConfirm({
        ...makeValidSignedSteering(),
        expires_at: 1_700_000_000,
      }),
    ).toThrow(/expires_at/i);

    expect(() =>
      assertValidSignedSteeringConfirm({
        ...makeValidSignedSteering(),
        extra_injected: true,
      }),
    ).toThrow(/Unknown field/i);
  });

  it("presentSteeringConfirmSheet enforces metadata & hash parity and rejects spoofing", async () => {
    __setGitPulseCryptoBindingForTests(makeMockBinding());
    const ok = await GitPulseCrypto.presentSteeringConfirmSheet({
      host_id: "host-1",
      session_key: "claude:repo:1",
      request_id: "req-1",
      expect_route: "peer_inbox",
      delivery_mode: "immediate",
      text: "Run cargo test",
      repo_label: "GitPulse",
      harness_label: "claude",
    });
    expect(ok.status).toBe("signed");
    expect(ok.signed_confirm?.text_sha256).toBe("c".repeat(64));

    // Spoofed session_key in returned signed_confirm must fail closed
    __setGitPulseCryptoBindingForTests(
      makeMockBinding({
        async presentSteeringConfirmSheet(req) {
          return {
            status: "signed",
            computed_text_sha256: "c".repeat(64),
            signed_confirm: makeValidSignedSteering({
              host_id: req.host_id,
              session_key: "other-session-hijacked",
              request_id: req.request_id,
              expect_route: req.expect_route,
              text_sha256: "c".repeat(64),
            }),
          };
        },
      }),
    );

    await expect(
      GitPulseCrypto.presentSteeringConfirmSheet({
        host_id: "host-1",
        session_key: "claude:repo:1",
        request_id: "req-1",
        expect_route: "peer_inbox",
        delivery_mode: "immediate",
        text: "Run cargo test",
        repo_label: "GitPulse",
        harness_label: "claude",
      }),
    ).rejects.toThrow(/Opus #23 P1/i);
  });

  it("native Swift and Kotlin steering sheets render exact text and sign via hardware P-256", () => {
    const swiftSheet = fs.readFileSync(
      path.join(MOBILE_ROOT, "modules/gitpulse-crypto/ios/GitPulseSteeringSheetView.swift"),
      "utf8",
    );
    expect(swiftSheet).toContain("hashSteeringText");
    expect(swiftSheet).toContain("buildSteeringSigningBytes");
    expect(swiftSheet).toContain("textView.text = exactText");
    expect(swiftSheet).toContain('"text_sha256": textSha256');

    const kotlinSheet = fs.readFileSync(
      path.join(
        MOBILE_ROOT,
        "modules/gitpulse-crypto/android/src/main/java/com/gitpulse/crypto/GitPulseSteeringSheetView.kt",
      ),
      "utf8",
    );
    expect(kotlinSheet).toContain("nativeHashSteeringText");
    expect(kotlinSheet).toContain("nativeBuildSteeringSigningBytes");
    expect(kotlinSheet).toContain("text = exactText");
    expect(kotlinSheet).toContain("BiometricPrompt.CryptoObject(signature)");
    expect(kotlinSheet).toContain('"text_sha256" to textSha256');
  });
});
