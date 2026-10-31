import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import IdentityPrompt, { firstEmptyField, focusUnmoved } from "./IdentityPrompt.svelte";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "IdentityPrompt.svelte"),
  "utf8",
);

describe("IdentityPrompt initial focus", () => {
  it("lands on the first thing that is missing", () => {
    expect(firstEmptyField("", "")).toBe("name");
    expect(firstEmptyField("", "me@example.org")).toBe("name");
    expect(firstEmptyField("Ada", "")).toBe("email");
    // Whitespace is not a name.
    expect(firstEmptyField("  ", "me@example.org")).toBe("name");
    expect(firstEmptyField("Ada", "  ")).toBe("email");
    // Nothing missing: the name is still the first field.
    expect(firstEmptyField("Ada", "ada@example.org")).toBe("name");
  });

  it("focuses it after the prefill settles, without pulling focus out of the form", () => {
    const mount = source.slice(source.indexOf("onMount(() => {"), source.indexOf("async function save()"));
    // The focus happens after gitIdentity() has been applied or has failed,
    // otherwise it would pick a field the prefill is about to fill.
    expect(mount.indexOf("repoStore.gitIdentity()")).toBeLessThan(mount.indexOf(".catch(() => {})"));
    expect(mount.indexOf(".catch(() => {})")).toBeLessThan(mount.indexOf(".focus("));
    expect(mount).toContain("await tick();");
    expect(mount).toContain("const shownWith = document.activeElement;");
    expect(mount).toContain("!focusUnmoved(shownWith, document.activeElement)");
    expect(mount).toContain('firstEmptyField(name, email) === "name" ? nameEl : emailEl');
    expect(mount).toContain("focus({ preventScroll: true })");
  });

  it("takes focus only if the user has not moved it since the prompt appeared", () => {
    const body = { ownerDocument: null as unknown };
    const doc = { body };
    body.ownerDocument = doc;
    const el = (name: string) => ({ name, ownerDocument: doc }) as unknown as Element;
    const commitButton = el("commit");
    const textarea = el("message");
    const nameInput = el("name");
    // Still on the Commit button that raised the prompt.
    expect(focusUnmoved(commitButton, commitButton)).toBe(true);
    // The element went away, so focus fell to the body (or nowhere).
    expect(focusUnmoved(commitButton, body as unknown as Element)).toBe(true);
    expect(focusUnmoved(commitButton, null)).toBe(true);
    // Moved back to the message box, or into this form, while git was slow.
    expect(focusUnmoved(commitButton, textarea)).toBe(false);
    expect(focusUnmoved(commitButton, nameInput)).toBe(false);
  });

  it("binds both inputs it may focus", () => {
    const { body } = render(IdentityPrompt, { props: { onSaved: () => {}, onDismiss: () => {} } });
    expect(body).toContain('placeholder="Your name"');
    expect(body).toContain('placeholder="you@example.org"');
    expect(source).toContain("bind:this={nameEl}");
    expect(source).toContain("bind:this={emailEl}");
  });
});
