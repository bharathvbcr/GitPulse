import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * A `{@const}` is a derived over the block it sits in. Inside
 * `{#if menu}{@const b = liveBranch(menu.branch)}`, a handler that closes the
 * menu (`menu = null`) and then reads `b` re-derives it from a null `menu` and
 * throws inside the click — the component is not destroyed until the next
 * flush, so nothing guards that read. This is how the branch menu's Push,
 * Pull and Checkout did nothing but raise "Cannot read properties of null".
 *
 * The rule: read what the action needs into a local, then close. The scan is
 * a lower bound — it sees inline arrow handlers and calls named close/dismiss/
 * hide/reset or a direct `= null` — so a clean result is not proof, but the
 * shape it does see can no longer ship.
 */
const SRC = fileURLToPath(new URL("../src", import.meta.url));

function svelteFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) return svelteFiles(full);
    return full.endsWith(".svelte") ? [full] : [];
  });
}

/** The body of every `onX={() => { ... }}` handler, braces balanced. */
function inlineHandlers(source: string): { body: string; offset: number }[] {
  const found: { body: string; offset: number }[] = [];
  const opener = /\bon\w+=\{\s*\(\s*\w*\s*\)\s*=>\s*\{/g;
  for (const match of source.matchAll(opener)) {
    const start = (match.index ?? 0) + match[0].length;
    let depth = 1;
    let index = start;
    while (index < source.length && depth > 0) {
      const char = source[index];
      if (char === "{") depth++;
      else if (char === "}") depth--;
      index++;
    }
    if (depth === 0) found.push({ body: source.slice(start, index - 1), offset: match.index ?? 0 });
  }
  return found;
}

const CLOSE = /\b(?:close|dismiss|hide|reset)\w*\s*\([^)]*\)\s*;|\b\w+\s*=\s*null\s*;/;

export function closeThenRead(source: string): string[] {
  const consts = [...source.matchAll(/\{@const\s+(\w+)\s*=/g)].map((m) => m[1]);
  if (!consts.length) return [];
  const offenders: string[] = [];
  for (const { body, offset } of inlineHandlers(source)) {
    const close = CLOSE.exec(body);
    if (!close) continue;
    const after = body.slice(close.index + close[0].length);
    for (const name of consts) {
      if (new RegExp(`\\b${name}\\b`).test(after)) {
        const line = source.slice(0, offset).split("\n").length;
        offenders.push(`line ${line}: reads {@const ${name}} after \`${close[0].trim()}\``);
      }
    }
  }
  return offenders;
}

describe("svelte close-then-read contract", () => {
  it("recognises the shape that broke the branch menu, and the shape that fixed it", () => {
    const broken = `{#if menu}{@const b = liveBranch(menu.branch)}
      <button onclick={() => { closeMenu(); void repoStore.push(undefined, b.name); }}>Push</button>{/if}`;
    const nulled = `{#if menu}{@const t = menu.tag}
      <button onclick={() => {
        menu = null;
        checkoutName(t.name);
      }}>Checkout</button>{/if}`;
    const fixed = `{#if menu}{@const b = liveBranch(menu.branch)}
      <button onclick={() => { const name = b.name; closeMenu(); void repoStore.push(undefined, name); }}>Push</button>{/if}`;
    expect(closeThenRead(broken)).toHaveLength(1);
    expect(closeThenRead(nulled)).toHaveLength(1);
    expect(closeThenRead(fixed)).toEqual([]);
  });

  it("no component reads a {@const} after closing the block it derives from", () => {
    const files = svelteFiles(SRC);
    // A walk that found nothing would pass vacuously.
    expect(files.length).toBeGreaterThan(100);
    const offenders = files.flatMap((file) =>
      closeThenRead(readFileSync(file, "utf8")).map((hit) => `${path.relative(SRC, file)} ${hit}`),
    );
    expect(offenders).toEqual([]);
  });
});
