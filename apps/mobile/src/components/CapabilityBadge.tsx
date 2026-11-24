import { StyleSheet, Text, View } from "react-native";
import type { CapabilityBadge as CapabilityBadgeModel } from "../protocol/types";
import { colors, radii, spacing } from "../theme/tokens";

const TONE_STYLES = {
  success: { bg: colors.successBg, border: colors.success, fg: colors.success },
  warning: { bg: colors.warningBg, border: colors.warning, fg: colors.warning },
  info: { bg: colors.infoBg, border: colors.info, fg: colors.info },
  neutral: { bg: colors.surfaceElevated, border: colors.border, fg: colors.textMuted },
} as const;

export function CapabilityBadge({ badge }: { badge: CapabilityBadgeModel }) {
  const palette = TONE_STYLES[badge.tone];
  return (
    <View
      style={[
        styles.pill,
        { backgroundColor: palette.bg, borderColor: palette.border },
      ]}
      accessibilityLabel={`${badge.label}: ${badge.detail}`}
    >
      <Text style={[styles.label, { color: palette.fg }]}>{badge.label}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  pill: {
    borderWidth: 1,
    borderRadius: radii.pill,
    paddingHorizontal: spacing.sm,
    paddingVertical: 3,
    alignSelf: "flex-start",
  },
  label: {
    fontSize: 11,
    fontWeight: "700",
    letterSpacing: 0.2,
  },
});
