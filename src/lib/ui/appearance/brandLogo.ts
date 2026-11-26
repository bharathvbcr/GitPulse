import type { Appearance } from "../color/derive";

export type BrandLogoStyle = "way-arc" | "folded-ribbon" | "lode-compass";
export const DEFAULT_BRAND_LOGO_STYLE: BrandLogoStyle = "way-arc";

export type BrandLogoPalette = "cobalt" | "lodestar" | "oxblood" | "gilded";
export const DEFAULT_BRAND_LOGO_PALETTE: BrandLogoPalette = "cobalt";

export interface BrandLogoStyleOption {
  readonly id: BrandLogoStyle;
  readonly label: string;
  readonly description: string;
}

export const BRAND_LOGO_STYLES: readonly BrandLogoStyleOption[] = [
  {
    id: "way-arc",
    label: "Way-Arc",
    description: "Open magnetic ring leading into an ascending crest and lodestar.",
  },
  {
    id: "folded-ribbon",
    label: "Folded Ribbon",
    description: "Faceted W ribbon forged across parallel lit and shaded rails.",
  },
  {
    id: "lode-compass",
    label: "Lode Compass",
    description: "Beveled compass crystal pointing along opposing magnetic wings.",
  },
] as const;

export interface BrandLogoPaletteSpec {
  readonly id: BrandLogoPalette;
  readonly label: string;
  readonly plate: readonly [string, string];
  readonly glow: string;
  readonly lit: readonly [string, string, string];
  readonly shade: readonly [string, string];
  readonly deep: string;
  readonly darkGrad: readonly [string, string, string];
  readonly lightGrad: readonly [string, string, string];
}

export const BRAND_LOGO_PALETTE_MAP: Readonly<Record<BrandLogoPalette, BrandLogoPaletteSpec>> = {
  cobalt: {
    id: "cobalt",
    label: "Cobalt",
    plate: ["#0f172a", "#060a14"],
    glow: "#38bdf8",
    lit: ["#7dd3fc", "#38bdf8", "#3b82f6"],
    shade: ["#1d4ed8", "#1e3a8a"],
    deep: "#0c1b4d",
    darkGrad: ["#7dd3fc", "#38bdf8", "#3b82f6"],
    lightGrad: ["#0284c7", "#2563eb", "#1d4ed8"],
  },
  lodestar: {
    id: "lodestar",
    label: "Lodestar",
    plate: ["#151d2b", "#0a0d14"],
    glow: "#2dd4bf",
    lit: ["#fbbf24", "#2dd4bf", "#38bdf8"],
    shade: ["#0f766e", "#0369a1"],
    deep: "#082f49",
    darkGrad: ["#fbbf24", "#2dd4bf", "#38bdf8"],
    lightGrad: ["#d97706", "#0d9488", "#0284c7"],
  },
  oxblood: {
    id: "oxblood",
    label: "Oxblood",
    plate: ["#170b0e", "#07080b"],
    glow: "#b91c1c",
    lit: ["#f87171", "#ef4444", "#b91c1c"],
    shade: ["#991b1b", "#630e0e"],
    deep: "#330606",
    darkGrad: ["#f87171", "#ef4444", "#b91c1c"],
    lightGrad: ["#dc2626", "#b91c1c", "#991b1b"],
  },
  gilded: {
    id: "gilded",
    label: "Gilded",
    plate: ["#1c160b", "#0a0805"],
    glow: "#f59e0b",
    lit: ["#fef08a", "#fbbf24", "#d97706"],
    shade: ["#b45309", "#78350f"],
    deep: "#3b1806",
    darkGrad: ["#fef08a", "#fbbf24", "#d97706"],
    lightGrad: ["#d97706", "#b45309", "#92400e"],
  },
} as const;

export const BRAND_LOGO_PALETTES: readonly BrandLogoPaletteSpec[] = [
  BRAND_LOGO_PALETTE_MAP.cobalt,
  BRAND_LOGO_PALETTE_MAP.lodestar,
  BRAND_LOGO_PALETTE_MAP.oxblood,
  BRAND_LOGO_PALETTE_MAP.gilded,
] as const;

export function isBrandLogoStyle(value: unknown): value is BrandLogoStyle {
  return value === "way-arc" || value === "folded-ribbon" || value === "lode-compass";
}

export function isBrandLogoPalette(value: unknown): value is BrandLogoPalette {
  return value === "cobalt" || value === "lodestar" || value === "oxblood" || value === "gilded";
}

export function brandLogoPaletteSpec(palette: BrandLogoPalette): BrandLogoPaletteSpec {
  return BRAND_LOGO_PALETTE_MAP[palette];
}

export function brandGradientCss(palette: BrandLogoPalette, appearance: Appearance): string {
  const stops =
    appearance === "light"
      ? BRAND_LOGO_PALETTE_MAP[palette].lightGrad
      : BRAND_LOGO_PALETTE_MAP[palette].darkGrad;
  return `linear-gradient(135deg, ${stops[0]} 0%, ${stops[1]} 52%, ${stops[2]} 100%)`;
}
