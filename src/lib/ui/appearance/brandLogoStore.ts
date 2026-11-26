import { writable } from "svelte/store";
import {
  DEFAULT_BRAND_LOGO_PALETTE,
  DEFAULT_BRAND_LOGO_STYLE,
  isBrandLogoPalette,
  isBrandLogoStyle,
  type BrandLogoPalette,
  type BrandLogoStyle,
} from "./brandLogo";

export interface BrandLogoSelection {
  readonly logoStyle: BrandLogoStyle;
  readonly logoPalette: BrandLogoPalette;
}

/**
 * Lightweight reactive selection mirror for `Logo.svelte`.
 *
 * Kept separate from `interfaceStore.ts` so isolated windows like `StatusPopover`
 * can render `<Logo />` without pulling main-window `invoke` calls into their
 * ACL closure.
 */
export const brandLogoStore = writable<BrandLogoSelection>({
  logoStyle: DEFAULT_BRAND_LOGO_STYLE,
  logoPalette: DEFAULT_BRAND_LOGO_PALETTE,
});

/** Sync the shared mark selection whenever `interfaceStore` initializes or changes. */
export function syncBrandLogoStore(logoStyle: unknown, logoPalette: unknown): void {
  const next: BrandLogoSelection = {
    logoStyle: isBrandLogoStyle(logoStyle) ? logoStyle : DEFAULT_BRAND_LOGO_STYLE,
    logoPalette: isBrandLogoPalette(logoPalette) ? logoPalette : DEFAULT_BRAND_LOGO_PALETTE,
  };
  brandLogoStore.update((prev) =>
    prev.logoStyle === next.logoStyle && prev.logoPalette === next.logoPalette ? prev : next,
  );
}
