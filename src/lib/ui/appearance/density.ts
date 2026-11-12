/**
 * The global density preference (design language §7): three steps, stamped on
 * `html[data-density]` and read by the `--gp-row-h` / `--gp-header-h` /
 * `--gp-pad` tokens.
 *
 * It lives here, not in the canvas module, so the stores can import it without
 * pulling the graph renderer in; `canvas/GraphRenderer.ts` re-exports the type
 * and keeps `DensityMode` ("spacious" | "compact") as the key of its own row
 * tables.
 */
export type Density = "compact" | "comfortable" | "spacious";

export const DENSITIES: readonly Density[] = ["compact", "comfortable", "spacious"];

export function isDensity(value: unknown): value is Density {
  return value === "compact" || value === "comfortable" || value === "spacious";
}

/** The two-valued key the row and graph tables are indexed by. */
export type RowDensity = "spacious" | "compact";

/**
 * Maps the preference onto that key. Comfortable is the geometry the tables
 * have always called "spacious" (the default before the preference existed), so
 * choosing it changes nothing; compact is compact. Spacious also maps to
 * "spacious" for now: its extra air comes from the CSS tokens, and a canvas that
 * reads `--gp-density` for a third geometry is a follow-up in the graph's lane.
 */
export function rowDensity(density: Density): RowDensity {
  return density === "compact" ? "compact" : "spacious";
}

/**
 * Reads the pre-merge `gitpulse_density_mode` value: "spacious" stays
 * spacious, "compact" stays compact, anything else is the default.
 */
export function densityFromLegacy(value: unknown): Density {
  return value === "compact" || value === "spacious" ? value : "comfortable";
}
