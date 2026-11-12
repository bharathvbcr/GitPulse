import { derived, get } from "svelte/store";
import type { DensityMode } from "../canvas/GraphRenderer";
import { rowDensity, type Density } from "../ui/appearance/density";
import { interfaceStore } from "./interfaceStore";

export type { DensityMode } from "../canvas/GraphRenderer";
export type { Density } from "../ui/appearance/density";

/**
 * The row and graph density every fixed-row surface indexes its tables by.
 *
 * The preference itself is the global three-step `interfaceStore.density`
 * (design language §7), which is also what `html[data-density]` is stamped
 * from. This store is the two-valued view of it that the existing consumers
 * read (`rowDensity`: comfortable and spacious draw the "spacious" tables,
 * compact the "compact" ones), so none of them changed type. The old
 * `gitpulse_density_mode` value is migrated by `interfaceStore` on first read.
 */
export const densityStore = {
  subscribe: derived(interfaceStore, (prefs): DensityMode => rowDensity(prefs.density)).subscribe,
  /** Sets the preference. The legacy two values stay valid. */
  setDensity: (density: Density) => interfaceStore.setDensity(density),
  /** Flips between compact and the default, as the old two-step toggle did. */
  toggle: () => {
    interfaceStore.setDensity(get(densityStore) === "compact" ? "comfortable" : "compact");
  },
};
