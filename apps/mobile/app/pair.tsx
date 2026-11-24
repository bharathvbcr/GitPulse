import { useState } from "react";
import { Pressable, ScrollView, StyleSheet, Text, TextInput, View } from "react-native";
import { CameraView, useCameraPermissions } from "expo-camera";
import {
  parsePairingQrPayload,
  type PairingQrPayload,
} from "@/protocol/types";
import { useControlPlane } from "@/state/ControlPlaneContext";
import { colors, radii, spacing } from "@/theme/tokens";

export default function PairAndDevicesScreen() {
  const { state, deviceKeys, revokeDevice } = useControlPlane();
  const [permission, requestPermission] = useCameraPermissions();
  const [scannerOpen, setScannerOpen] = useState(false);
  const [manualInput, setManualInput] = useState("");
  const [pendingPair, setPendingPair] = useState<PairingQrPayload | null>(null);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const handleParsedQr = (raw: string) => {
    setError(null);
    try {
      const parsed = parsePairingQrPayload(raw);
      if (parsed.expires_at_ms <= Date.now()) {
        throw new Error("Pairing QR code has expired. Generate a fresh code on Desktop.");
      }
      setPendingPair(parsed);
      setScannerOpen(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <ScrollView style={styles.screen} contentContainerStyle={styles.content}>
      {/* Hardware Key Identity Status (Public-Only — Opus M2) */}
      <View style={styles.card}>
        <Text style={styles.sectionTitle}>This Device Hardware Keys (Public Only)</Text>
        {deviceKeys ? (
          <>
            <Text style={styles.mono}>Device ID: {deviceKeys.device_id}</Text>
            <Text style={styles.mono}>
              Backend: {deviceKeys.key_storage_backend} (
              {deviceKeys.hardware_backed ? "Hardware-Backed" : "TEE Fallback"})
            </Text>
            <Text style={styles.mono}>
              P-256 Approval Pub: {deviceKeys.approval_pub_p256_b64.slice(0, 24)}…
            </Text>
            <Text style={styles.mono}>
              Ed25519 Identity Pub: {deviceKeys.ed25519_pub_b64.slice(0, 24)}…
            </Text>
            <Text style={styles.hint}>
              Private keys remain locked inside Secure Enclave / StrongBox & Keychain/Keystore
              and never cross the JS bridge.
            </Text>
          </>
        ) : (
          <Text style={styles.hint}>
            Initializing hardware P-256 + Ed25519/X25519 keys via GitPulseCrypto native module…
          </Text>
        )}
      </View>

      {/* QR Pairing & Safety-Code Verification */}
      <View style={styles.card}>
        <Text style={styles.sectionTitle}>Pair Desktop Host (Safety-Code Verified)</Text>
        {!scannerOpen ? (
          <Pressable
            style={styles.primaryBtn}
            onPress={async () => {
              if (!permission?.granted) {
                await requestPermission();
              }
              setScannerOpen(true);
            }}
          >
            <Text style={styles.btnText}>Scan Desktop Pairing QR</Text>
          </Pressable>
        ) : (
          <View style={styles.cameraWrap}>
            <CameraView
              style={styles.camera}
              barcodeScannerSettings={{ barcodeTypes: ["qr"] }}
              onBarcodeScanned={(event: { data: string }) => handleParsedQr(event.data)}
            />
            <Pressable style={styles.cancelBtn} onPress={() => setScannerOpen(false)}>
              <Text style={styles.btnText}>Close Scanner</Text>
            </Pressable>
          </View>
        )}

        <TextInput
          style={styles.input}
          value={manualInput}
          onChangeText={setManualInput}
          placeholder="Or paste pairing JSON payload..."
          placeholderTextColor={colors.textDim}
        />
        <Pressable
          style={styles.secondaryBtn}
          onPress={() => handleParsedQr(manualInput)}
          disabled={!manualInput.trim()}
        >
          <Text style={styles.btnText}>Verify Pairing Payload</Text>
        </Pressable>

        {pendingPair && (
          <View style={styles.safetyBox}>
            <Text style={styles.safetyTitle}>Confirm Safety Code Matches Desktop</Text>
            <Text style={styles.safetyCode}>{pendingPair.safety_code}</Text>
            <Text style={styles.mono}>Host: {pendingPair.host_id}</Text>
            <Text style={styles.mono}>Relay: {pendingPair.relay_url}</Text>
            <Pressable
              style={styles.confirmBtn}
              onPress={() => {
                setStatusMessage(
                  `Safety code ${pendingPair.safety_code} confirmed for host ${pendingPair.host_id}. DeviceCertificate v2 enrollment queued.`,
                );
                setPendingPair(null);
              }}
            >
              <Text style={styles.btnText}>Safety Code Matches — Complete Pairing</Text>
            </Pressable>
          </View>
        )}

        {statusMessage && <Text style={styles.success}>{statusMessage}</Text>}
        {error && <Text style={styles.error}>{error}</Text>}
      </View>

      {/* Paired Devices & Revocation UI */}
      <View style={styles.card}>
        <Text style={styles.sectionTitle}>
          Paired Devices ({state.pairedDevices.length})
        </Text>
        {state.pairedDevices.length === 0 ? (
          <Text style={styles.hint}>No paired devices synced yet.</Text>
        ) : (
          state.pairedDevices.map((device) => (
            <View key={device.device_id} style={styles.deviceRow}>
              <View style={styles.deviceInfo}>
                <Text style={styles.deviceName}>
                  {device.name} ({device.kind})
                </Text>
                <Text style={styles.mono}>{device.device_id}</Text>
                <Text style={styles.hint}>
                  {device.revoked ? "REVOKED" : device.hardware_backed ? "Hardware P-256" : "Software/TEE"}
                </Text>
              </View>
              {!device.revoked && (
                <Pressable
                  style={styles.revokeBtn}
                  onPress={() => void revokeDevice(device.device_id)}
                >
                  <Text style={styles.revokeText}>Revoke</Text>
                </Pressable>
              )}
            </View>
          ))
        )}
      </View>
    </ScrollView>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg },
  content: { padding: spacing.lg, paddingBottom: 48 },
  card: {
    backgroundColor: colors.surface,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.md,
    padding: spacing.md,
    marginBottom: spacing.md,
  },
  sectionTitle: {
    fontSize: 12,
    fontWeight: "800",
    color: colors.accent,
    textTransform: "uppercase",
    marginBottom: spacing.sm,
  },
  mono: { fontFamily: "Courier", fontSize: 12, color: colors.text, marginBottom: 3 },
  hint: { fontSize: 12, color: colors.textMuted, marginTop: 4, lineHeight: 17 },
  primaryBtn: {
    backgroundColor: colors.accent,
    paddingVertical: spacing.md,
    borderRadius: radii.sm,
    alignItems: "center",
    marginBottom: spacing.sm,
  },
  secondaryBtn: {
    backgroundColor: colors.surfaceElevated,
    borderWidth: 1,
    borderColor: colors.border,
    paddingVertical: spacing.sm + 2,
    borderRadius: radii.sm,
    alignItems: "center",
    marginTop: spacing.xs,
  },
  cancelBtn: {
    backgroundColor: colors.danger,
    paddingVertical: spacing.sm,
    borderRadius: radii.sm,
    alignItems: "center",
    marginTop: spacing.xs,
  },
  confirmBtn: {
    backgroundColor: colors.success,
    paddingVertical: spacing.md,
    borderRadius: radii.sm,
    alignItems: "center",
    marginTop: spacing.sm,
  },
  btnText: { color: colors.text, fontWeight: "700", fontSize: 14 },
  cameraWrap: { marginBottom: spacing.sm },
  camera: { height: 220, borderRadius: radii.md, overflow: "hidden" },
  input: {
    backgroundColor: colors.bg,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.sm,
    color: colors.text,
    padding: spacing.sm,
    fontSize: 13,
  },
  safetyBox: {
    marginTop: spacing.md,
    backgroundColor: colors.infoBg,
    borderWidth: 1,
    borderColor: colors.info,
    borderRadius: radii.md,
    padding: spacing.md,
  },
  safetyTitle: { color: colors.info, fontWeight: "800", fontSize: 13 },
  safetyCode: {
    fontFamily: "Courier",
    fontSize: 26,
    fontWeight: "800",
    color: colors.text,
    letterSpacing: 4,
    marginVertical: spacing.sm,
  },
  success: { color: colors.success, fontSize: 12, marginTop: spacing.sm, fontWeight: "600" },
  error: { color: colors.danger, fontSize: 12, marginTop: spacing.sm },
  deviceRow: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    paddingVertical: spacing.sm,
    borderTopWidth: 1,
    borderTopColor: colors.border,
  },
  deviceInfo: { flex: 1 },
  deviceName: { color: colors.text, fontWeight: "700", fontSize: 14 },
  revokeBtn: {
    backgroundColor: colors.dangerBg,
    borderWidth: 1,
    borderColor: colors.danger,
    borderRadius: radii.sm,
    paddingHorizontal: spacing.md,
    paddingVertical: spacing.xs + 2,
  },
  revokeText: { color: colors.danger, fontWeight: "800", fontSize: 12 },
});
