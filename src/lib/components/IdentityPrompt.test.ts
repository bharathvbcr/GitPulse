import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import IdentityPrompt, { firstEmptyField } from "./IdentityPrompt.svelte";

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
    expect(mount).toContain("formEl?.contains(document.activeElement)");
    expect(mount).toContain('firstEmptyField(name, email) === "name" ? nameEl : emailEl');
    expect(mount).toContain("focus({ preventScroll: true })");
  });

  it("binds the form and both inputs it may focus", () => {
    const { body } = render(IdentityPrompt, { props: { onSaved: () => {}, onDismiss: () => {} } });
    expect(body).toContain('placeholder="Your name"');
    expect(body).toContain('placeholder="you@example.org"');
    expect(source).toContain("bind:this={formEl}");
    expect(source).toContain("bind:this={nameEl}");
    expect(source).toContain("bind:this={emailEl}");
  });
});
