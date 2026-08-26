export const BRANCH_PALETTE = [
  "#00E5FF", // Electric Cyan
  "#B388FF", // Vivid Violet
  "#FF9100", // Neon Amber / Orange
  "#00E676", // Emerald Neon Green
  "#FF4081", // Hot Pink / Rose
  "#2979FF", // Royal Azure Blue
  "#FFD600", // Vivid Gold
  "#FF5252", // Coral Red
  "#1DE9B6", // Mint Teal
  "#7C4DFF", // Deep Indigo
  "#AEEA00", // Electric Lime
  "#E040FB", // Bright Lilac
];

export function getBranchColor(colorIndex: number): string {
  // ±Infinity slips past the NaN check and poisons the modulo (Infinity % n
  // is NaN), which used to hand strokeStyle an undefined and silently inherit
  // whatever colour the previous edge left behind.
  if (!Number.isFinite(colorIndex) || colorIndex < 0) return BRANCH_PALETTE[0];
  return BRANCH_PALETTE[colorIndex % BRANCH_PALETTE.length];
}

