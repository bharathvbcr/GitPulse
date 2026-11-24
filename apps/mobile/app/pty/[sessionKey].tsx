import { useEffect, useRef, useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { useLocalSearchParams } from "expo-router";
import { WebView } from "react-native-webview";
import { GitPulseCrypto } from "@gitpulse/crypto";
import { isPtyInputUnlocked } from "@/client/ControlPlaneClient";
import { PtyAccessoryBar } from "@/components/PtyAccessoryBar";
import { useControlPlane } from "@/state/ControlPlaneContext";
import { colors, radii, spacing } from "@/theme/tokens";

/**
 * Quarantined xterm-compatible terminal viewport HTML.
 * Strict CSP blocks all network/remote scripts; read-only unless parent unlocks input.
 */
const QUARANTINED_PTY_HTML = `<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline';" />
    <style>
      html, body { margin: 0; padding: 8px; background: #020617; color: #e2e8f0; font-family: ui-monospace, Menlo, monospace; font-size: 12px; line-height: 1.4; }
      #term { white-space: pre-wrap; word-break: break-all; }
    </style>
  </head>
  <body>
    <pre id="term"></pre>
    <script>
      const term = document.getElementById('term');
      window.__gitpulseAppendChunk = function(text) {
        term.textContent += text;
        window.scrollTo(0, document.body.scrollHeight);
      };
    </script>
  </body>
</html>`;

function encodeUtf8ToBase64(input: string): string {
  const g = globalThis as unknown as {
    btoa?: (s: string) => string;
    Buffer?: { from(s: string, enc: string): { toString(enc: string): string } };
  };
  if (g.Buffer) {
    return g.Buffer.from(input, "utf8").toString("base64");
  }
  if (g.btoa) {
    return g.btoa(input);
  }
  return input;
}

/**
 * Quarantined Opt-In PTY Drawer (Opus P0-7).
 *
 * Security invariants:
 * - Strictly READ-ONLY by default.
 * - Unlocking `pty_input` requires BOTH:
 *   1. An active, unexpired Desktop-granted time-boxed `PtyConsentGrant` for this session + device.
 *   2. Local hardware biometric step-up (`GitPulseCrypto.authenticateBiometricStepUp`).
 * - Auto-relocks immediately when grant expires or drawer unmounts.
 * - Accepted residual risk (Opus #23): Individual `pty.input` keystrokes sent within an
 *   active Desktop-granted time-boxed consent window + biometric step-up are authenticated
 *   by the Noise IK session and audited under `grant.audit_id`, not signed per keystroke.
 */
export default function PtyDrawerScreen() {
  const { sessionKey } = useLocalSearchParams<{ sessionKey: string }>();
  const decodedKey = sessionKey ? decodeURIComponent(sessionKey) : "";
  const { state, deviceKeys, client, sendPtyInput } = useControlPlane();

  const webViewRef = useRef<{ injectJavaScript?: (js: string) => void } | null>(null);
  const [biometricAttestedAtMs, setBiometricAttestedAtMs] = useState<number | null>(null);
  const [nowMs, setNowMs] = useState(() => Date.now());
  const [stepUpError, setStepUpError] = useState<string | null>(null);

  const grant = state.ptyGrants[decodedKey];
  const deviceId = deviceKeys?.device_id ?? "mobile-pending";

  const inputUnlocked = isPtyInputUnlocked({
    grant,
    sessionKey: decodedKey,
    deviceId,
    biometricAttestedAtMs,
    nowMs,
  });

  // Periodic tick to auto-relock immediately when time-boxed Desktop consent expires
  useEffect(() => {
    const id = setInterval(() => setNowMs(Date.now()), 1000);
    return () => {
      clearInterval(id);
      setBiometricAttestedAtMs(null);
    };
  }, []);

  // Stream read-only PTY output into quarantined WebView
  useEffect(() => {
    return client.subscribePtyOutput((key, chunkB64) => {
      if (key !== decodedKey) return;
      const safeJson = JSON.stringify(chunkB64);
      webViewRef.current?.injectJavaScript?.(
        `window.__gitpulseAppendChunk && window.__gitpulseAppendChunk(atob(${safeJson})); true;`,
      );
    });
  }, [client, decodedKey]);

  const handleBiometricUnlock = async () => {
    setStepUpError(null);
    const result = await GitPulseCrypto.authenticateBiometricStepUp(
      `Unlock time-boxed PTY input for ${decodedKey}`,
    );
    if (result.ok && result.attested_at_ms) {
      setBiometricAttestedAtMs(result.attested_at_ms);
    } else {
      setStepUpError(result.error_message ?? "Biometric step-up failed.");
    }
  };

  const remainingSec =
    grant && grant.expires_at_ms > nowMs
      ? Math.ceil((grant.expires_at_ms - nowMs) / 1000)
      : 0;

  return (
    <View style={styles.screen}>
      {/* Opus P0-7 Read-Only / Consent Status Banner */}
      <View style={[styles.banner, inputUnlocked ? styles.bannerUnlocked : styles.bannerLocked]}>
        <Text style={styles.bannerTitle}>
          {inputUnlocked
            ? `PTY INPUT UNLOCKED (${remainingSec}s left) • AUDITED`
            : "READ-ONLY PTY STREAM (Opus P0-7)"}
        </Text>
        <Text style={styles.bannerBody}>
          {!grant || remainingSec <= 0
            ? "Desktop has not granted active time-boxed `pty.input` consent for this session. Grant consent from GitPulse Desktop first."
            : inputUnlocked
              ? `Audit ID: ${grant.audit_id}. Input auto-relocks on expiry or drawer close.`
              : `Desktop consent granted (${remainingSec}s remaining). Complete biometric step-up to unlock input.`}
        </Text>

        {grant && remainingSec > 0 && !inputUnlocked && (
          <Pressable style={styles.unlockBtn} onPress={handleBiometricUnlock}>
            <Text style={styles.unlockBtnText}>Biometric Step-Up to Unlock Input</Text>
          </Pressable>
        )}
        {inputUnlocked && (
          <Pressable
            style={styles.relockBtn}
            onPress={() => setBiometricAttestedAtMs(null)}
          >
            <Text style={styles.unlockBtnText}>Lock Back to Read-Only</Text>
          </Pressable>
        )}
        {stepUpError && <Text style={styles.error}>{stepUpError}</Text>}
      </View>

      {/* Quarantined WebView (CSP-locked, zero external scripts) */}
      <WebView
        ref={webViewRef}
        style={styles.webview}
        originWhitelist={["about:blank"]}
        source={{ html: QUARANTINED_PTY_HTML }}
        javaScriptEnabled
        allowFileAccess={false}
        allowUniversalAccessFromFileURLs={false}
      />

      {/* Native Accessory Bar — strictly disabled when read-only */}
      <PtyAccessoryBar
        enabled={inputUnlocked}
        onSendSequence={(seq) => {
          sendPtyInput(decodedKey, encodeUtf8ToBase64(seq), biometricAttestedAtMs);
        }}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  banner: {
    padding: spacing.md,
    borderBottomWidth: 1,
  },
  bannerLocked: { backgroundColor: colors.surface, borderBottomColor: colors.border },
  bannerUnlocked: { backgroundColor: colors.warningBg, borderBottomColor: colors.warning },
  bannerTitle: { color: colors.text, fontWeight: "800", fontSize: 12 },
  bannerBody: { color: colors.textMuted, fontSize: 12, marginTop: 4, lineHeight: 17 },
  unlockBtn: {
    marginTop: spacing.sm,
    backgroundColor: colors.warning,
    paddingVertical: spacing.sm,
    borderRadius: radii.sm,
    alignItems: "center",
  },
  relockBtn: {
    marginTop: spacing.sm,
    backgroundColor: colors.surfaceElevated,
    paddingVertical: spacing.sm,
    borderRadius: radii.sm,
    alignItems: "center",
  },
  unlockBtnText: { color: colors.bg, fontWeight: "800", fontSize: 13 },
  error: { color: colors.danger, fontSize: 12, marginTop: spacing.xs },
  webview: { flex: 1, backgroundColor: "#020617" },
});
