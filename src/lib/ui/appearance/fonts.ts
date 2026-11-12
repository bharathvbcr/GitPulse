/**
 * Local fonts for the UI and the code surfaces (design language §6, §11).
 *
 * A family is a NAME the reader typed, never a URL: nothing here downloads a
 * font, loads a stylesheet or builds an `@font-face`. The name is validated with
 * the same pattern the theme file uses (`themeSchema.ts`), quoted, and put in
 * front of the system stack, so a family the machine does not have falls
 * through to the stack and the app still draws.
 */

/** The default stacks. `fonts.test.ts` pins them to app.css. */
export const DEFAULT_UI_STACK =
  'ui-sans-serif, system-ui, sans-serif, "Apple Color Emoji", "Segoe UI Emoji", "Segoe UI Symbol", "Noto Color Emoji"';
export const DEFAULT_MONO_STACK =
  'ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", "Courier New", monospace';

export const MAX_FAMILY_LENGTH = 64;

/** One local family name: letters, digits, spaces and hyphens (Unicode letters allowed). */
const FAMILY_PATTERN = /^[\p{L}\p{N} -]+$/u;

/**
 * The clean family name, `""` for "use the system font", or `null` when the
 * text is not a family name. Quotes are stripped; anything else that could
 * reach a stylesheet as something other than a name is refused.
 */
export function normalizeFamily(input: unknown): string | null {
  if (typeof input !== "string") return null;
  const name = input.replaceAll('"', "").replaceAll("'", "").replace(/\s+/g, " ").trim();
  if (name === "") return "";
  if (name.length > MAX_FAMILY_LENGTH) return null;
  return FAMILY_PATTERN.test(name) ? name : null;
}

/** `"Family", <fallback stack>`. The caller passes a name `normalizeFamily` accepted. */
export function fontStack(family: string, fallback: string): string {
  return `"${family}", ${fallback}`;
}

/**
 * Whether `family` is installed on this machine: `true`, `false`, or `null` when
 * the measurement could not run (no 2D canvas). `null` must not read as `true`.
 *
 * `document.fonts.check()` is not used: for a family that no `@font-face`
 * declares it answers `true` whether or not the machine has it, so it cannot
 * tell an installed font from a typo. Instead the family is drawn against three
 * generic baselines; an installed font differs from at least one of them in
 * width, while a missing family renders as the baseline it fell back to.
 */
export function isFamilyInstalled(family: string, canvas?: CanvasRenderingContext2D | null): boolean | null {
  const normalized = normalizeFamily(family);
  if (normalized === null || normalized === "") return null;
  let context = canvas ?? null;
  if (context === null) {
    if (typeof document === "undefined") return null;
    try {
      context = document.createElement("canvas").getContext("2d");
    } catch {
      return null;
    }
  }
  if (!context || typeof context.measureText !== "function") return null;
  const sample = "mmmmmmmmmmlliWWW0123456789@#";
  const baselines = ["monospace", "serif", "sans-serif"] as const;
  for (const baseline of baselines) {
    context.font = `72px ${baseline}`;
    const base = context.measureText(sample).width;
    context.font = `72px "${normalized}", ${baseline}`;
    const candidate = context.measureText(sample).width;
    if (!Number.isFinite(base) || !Number.isFinite(candidate)) return null;
    if (Math.abs(base - candidate) > 0.01) return true;
  }
  return false;
}
