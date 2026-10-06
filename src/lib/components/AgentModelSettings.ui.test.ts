/**
 * Contracts for the agent model settings row.
 *
 * Source-read, like the other agent-setting contracts: these are structural
 * properties — the row is its own search target, its controls come from the
 * backend's field map rather than a hand-kept list, it never spells a flag,
 * and it says what it does not reach — that a render test would assert less
 * directly. Its behaviour against a real backend answer is driven in
 * `harness/settings.html`.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { SETTINGS_CATALOG } from "../ui/settingsCatalog";

const here = dirname(fileURLToPath(import.meta.url));
const row = readFileSync(join(here, "AgentModelSettings.svelte"), "utf8");
const modal = readFileSync(join(here, "SettingsModal.svelte"), "utf8");
const markup = row.slice(row.indexOf("</script>"));

describe("agent models", () => {
  it("is findable by the words a reader would type, in its own wrapper", () => {
    const entry = SETTINGS_CATALOG.find((item) => item.id === "agent-models");
    expect(entry?.section).toBe("agents");
    for (const word of ["model", "effort", "fallback", "advisor", "antigravity", "opus", "fable", "gemini", "routing"]) {
      expect(entry?.keywords, `"${word}" does not find this setting`).toContain(word);
    }
    // Its own wrapper: nested in another entry's, a search matching only
    // this row hid it along with its parent.
    expect(modal).toMatch(/data-setting="agent-models"[^>]*>\s*<AgentModelSettings/);
  });

  it("derives its launchers and fields from the backend's map, never a literal list", () => {
    expect(row).toContain("view.modelFields[entry.kind]");
    expect(row).toContain("{#each row.fields as field (field)}");
    expect(row).toContain("{#each view.effortLevels as level (level)}");
    for (const literal of ['"claude"', '"agy"', '"codex"', '"grok"']) {
      expect(markup, `the markup names ${literal} by hand`).not.toContain(literal);
    }
  });

  it("carries names and levels only; the flags are the backend's", () => {
    for (const flag of ["--model", "--effort", "--fallback-model", "--settings", "advisorModel"]) {
      expect(row, flag).not.toContain(flag);
    }
  });

  it("stores the CLI's own choice as absence and carries every other default through", () => {
    expect(row).toContain("saveAgentDefaults(withModelChoice(view.defaults, launcher, next))");
    expect(row).toContain('text === "" ? undefined');
  });

  it("checks a value before saving it and restores the field when a save is refused", () => {
    const commit = row.slice(row.indexOf("async function commit("));
    const body = commit.slice(0, commit.indexOf("\n  }\n"));
    expect(body.indexOf("refusal(field, text)")).toBeLessThan(body.indexOf("saveAgentDefaults("));
    expect(body).toContain("input.value = valueOf(current, field);");
  });

  it("asks a CLI for its list only when the reader presses the button", () => {
    // `agy models` is a network call under the reader's sign-in; rendering
    // a settings pane must not make one. Only a `known` catalog — a local
    // file read — loads on its own.
    const auto = row.slice(row.indexOf("// A `known` catalog"));
    const effect = auto.slice(0, auto.indexOf("\n  });"));
    expect(effect).toContain('how === "known"');
    expect(effect).not.toContain('"listed"');
    expect(markup).toContain("onclick={() => void loadAgentModels(row.kind");
    expect(markup).toContain("{#if listed}");
  });

  it("warns about a stored model the listing does not know, and only from a successful listing", () => {
    expect(markup).toContain("{#if listed && known === false}");
    expect(markup).toContain("listedModel(catalogs[row.kind], choice.model)");
  });

  it("says what it cannot check and what it does not reach", () => {
    const note = row.slice(row.indexOf('data-testid="agent-models-note"'));
    expect(note).toContain("not that the model exists");
    expect(note).toContain("falls back to its default model");
    expect(note).toContain("Managed sessions are not affected");
    expect(note).toContain("at least as capable");
  });
});
