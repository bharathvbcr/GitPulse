import { useState } from "react";
import { Pressable, StyleSheet, Text, View } from "react-native";
import { colors, radii, spacing } from "../theme/tokens";

export interface PtyAccessoryBarProps {
  readonly enabled: boolean;
  readonly onSendSequence: (bytes: string) => void;
}

const KEYS: ReadonlyArray<{ label: string; seq: string }> = [
  { label: "Esc", seq: "\x1b" },
  { label: "Tab", seq: "\t" },
  { label: "Ctrl-C", seq: "\x03" },
  { label: "↑", seq: "\x1b[A" },
  { label: "↓", seq: "\x1b[B" },
  { label: "←", seq: "\x1b[D" },
  { label: "→", seq: "\x1b[C" },
];

export function PtyAccessoryBar({ enabled, onSendSequence }: PtyAccessoryBarProps) {
  const [ctrlArmed, setCtrlArmed] = useState(false);

  return (
    <View style={[styles.bar, !enabled && styles.barDisabled]}>
      <Pressable
        style={[styles.key, ctrlArmed && styles.keyArmed]}
        disabled={!enabled}
        onPress={() => setCtrlArmed((prev) => !prev)}
      >
        <Text style={styles.keyText}>Ctrl</Text>
      </Pressable>

      {KEYS.map((item) => (
        <Pressable
          key={item.label}
          style={styles.key}
          disabled={!enabled}
          onPress={() => {
            setCtrlArmed(false);
            onSendSequence(item.seq);
          }}
        >
          <Text style={styles.keyText}>{item.label}</Text>
        </Pressable>
      ))}
    </View>
  );
}

const styles = StyleSheet.create({
  bar: {
    flexDirection: "row",
    flexWrap: "wrap",
    gap: spacing.xs,
    padding: spacing.sm,
    backgroundColor: colors.surface,
    borderTopWidth: 1,
    borderTopColor: colors.border,
  },
  barDisabled: {
    opacity: 0.35,
  },
  key: {
    backgroundColor: colors.surfaceElevated,
    borderWidth: 1,
    borderColor: colors.border,
    borderRadius: radii.sm,
    paddingHorizontal: spacing.md,
    paddingVertical: spacing.sm,
  },
  keyArmed: {
    borderColor: colors.accent,
    backgroundColor: colors.infoBg,
  },
  keyText: {
    color: colors.text,
    fontFamily: "Courier",
    fontSize: 12,
    fontWeight: "700",
  },
});
