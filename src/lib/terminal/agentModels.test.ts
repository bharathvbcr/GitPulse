/**
 * Model settings for the agent CLIs GitPulse starts in a terminal.
 *
 * The flags live in exactly one place — `model_control` in
 * `src-tauri/src/workbench/terminal_command.rs` — and the frontend carries
 * names, levels and per-launcher field lists only. These tests bind every one
 * of those lists to the Rust source, so a field offered here is always one a
 * launch can apply, and pin the sanitizer's one rule: a value that cannot be
 * applied in full is dropped, never passed in part.
 */
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  EFFORT_LEVELS,
  MAX_FALLBACK_MODELS,
  MAX_MODEL_ID_LEN,
  MODEL_FIELDS,
  MODEL_FIELDS_BY_LAUNCHER,
  MODEL_SUGGESTIONS,
  isModelId,
  modelChoiceOf,
  parseFallbackList,
  sanitizeAgentDefaults,
  sanitizeModelChoice,
  withModelChoice,
  type AgentDefaults,
  type ModelField,
} from "./agentDefaults";
import { LAUNCHERS, type LauncherKind } from "./tabs";

const repoRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "..");
const rust = readFileSync(join(repoRoot, "src-tauri", "src", "workbench", "terminal_command.rs"), "utf8");

function rustList(name: string): string[] {
  const match = rust.match(new RegExp(`pub\\(crate\\) const ${name}:\\s*\\[&str;\\s*\\d+\\]\\s*=\\s*\\[([^\\]]*)\\]`));
  expect(match, `${name} not found in terminal_command.rs`).not.toBeNull();
  return [...(match?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
}

function rustNumber(name: string): number {
  const match = rust.match(new RegExp(`pub\\(crate\\) const ${name}: usize = (\\d+);`));
  expect(match, `${name} not found in terminal_command.rs`).not.toBeNull();
  return Number(match?.[1]);
}

/** Every `(providers, "field")` arm of `model_control`, expanded per provider. */
const arms = (() => {
  const start = rust.indexOf("fn model_control(provider: &str, field: &str)");
  expect(start, "model_control not found").toBeGreaterThan(-1);
  const body = rust.slice(start, rust.indexOf("\n}\n", start));
  const out: { provider: string; field: string }[] = [];
  for (const arm of body.matchAll(/\(((?:"[a-z]+"\s*\|?\s*)+),\s*"([a-z]+)"\)\s*=>/g)) {
    for (const provider of arm[1].matchAll(/"([a-z]+)"/g)) out.push({ provider: provider[1], field: arm[2] });
  }
  // A reshaped table must fail loudly rather than silently stop checking.
  expect(out.length, "no arms parsed from model_control").toBeGreaterThan(0);
  return out;
})();

describe("the model vocabulary is the Rust table's", () => {
  it("names the same fields, levels and bounds, in the same order", () => {
    expect([...MODEL_FIELDS]).toEqual(rustList("MODEL_FIELDS"));
    expect([...EFFORT_LEVELS]).toEqual(rustList("EFFORT_LEVELS"));
    expect(MAX_FALLBACK_MODELS).toBe(rustNumber("MAX_FALLBACK_MODELS"));
    expect(MAX_MODEL_ID_LEN).toBe(rustNumber("MAX_MODEL_ID_LEN"));
  });

  it("offers each launcher exactly the fields the flag table can apply", () => {
    const fromRust: Record<string, string[]> = {};
    for (const { provider, field } of arms) (fromRust[provider] ??= []).push(field);
    for (const fields of Object.values(fromRust)) fields.sort((a, b) => MODEL_FIELDS.indexOf(a as ModelField) - MODEL_FIELDS.indexOf(b as ModelField));
    expect(MODEL_FIELDS_BY_LAUNCHER).toEqual(fromRust);
  });

  it("only offers model fields to launchers the tab strip has", () => {
    const strip = new Set<string>(LAUNCHERS.map((entry) => entry.kind));
    for (const launcher of Object.keys(MODEL_FIELDS_BY_LAUNCHER)) expect(strip.has(launcher), launcher).toBe(true);
    expect(MODEL_FIELDS_BY_LAUNCHER.shell).toBeUndefined();
    expect(MODEL_FIELDS_BY_LAUNCHER.manvi).toBeUndefined();
  });

  it("suggests only names a save would accept, for fields the launcher takes", () => {
    for (const [launcher, byField] of Object.entries(MODEL_SUGGESTIONS)) {
      for (const [field, names] of Object.entries(byField ?? {})) {
        expect(MODEL_FIELDS_BY_LAUNCHER[launcher as LauncherKind]).toContain(field);
        for (const name of names ?? []) expect(isModelId(name), `${launcher}.${field}: ${name}`).toBe(true);
      }
    }
  });

  it("checks shape by the same character rule the backend applies", () => {
    // The backend's rule, read from its source rather than restated.
    expect(rust).toContain(`"._:/@[]-".contains(*c)`);
    expect(rust).toContain("if !id.as_bytes()[0].is_ascii_alphanumeric() {");
  });
});

describe("isModelId", () => {
  it.each([
    "opus",
    "opus[1m]",
    "opusplan",
    "claude-opus-5-5",
    "claude-opus-4-1@20250805",
    "us.anthropic.claude-sonnet-4-5-20250929-v1:0",
    "arn:aws:bedrock:us-east-1:123456789012:inference-profile/us.anthropic.claude-opus",
    "gemini-3.8-flash-high",
    "9",
  ])("accepts %s", (id) => {
    expect(isModelId(id)).toBe(true);
  });

  it.each(["", "-m", "--model", "--", "a,b", "a b", " opus", "opus\n", "op\u0000us", "ópus", "opus;rm", "$(x)", "[1m]", ".hidden", "/abs"])(
    "refuses %j",
    (id) => {
      expect(isModelId(id)).toBe(false);
    },
  );

  it("bounds the length and refuses what is not a string", () => {
    expect(isModelId("a".repeat(MAX_MODEL_ID_LEN))).toBe(true);
    expect(isModelId("a".repeat(MAX_MODEL_ID_LEN + 1))).toBe(false);
    for (const value of [undefined, null, 7, ["opus"], { model: "opus" }]) expect(isModelId(value)).toBe(false);
  });
});

describe("sanitizeModelChoice", () => {
  const claude = MODEL_FIELDS_BY_LAUNCHER.claude ?? [];
  const codex = MODEL_FIELDS_BY_LAUNCHER.codex ?? [];

  it("keeps a full, valid choice exactly", () => {
    const choice = { model: "opus", effort: "high", fallback: ["sonnet", "haiku"], advisor: "fable" };
    expect(sanitizeModelChoice(choice, claude)).toEqual(choice);
  });

  it("drops a field the launcher does not take, and nothing else", () => {
    expect(sanitizeModelChoice({ model: "gpt-6", effort: "high" }, codex)).toEqual({ model: "gpt-6" });
  });

  it("drops a fallback list that cannot be applied in full rather than passing part of it", () => {
    for (const fallback of [[], ["a", "a"], ["a", "-b"], ["a,b"], Array.from({ length: MAX_FALLBACK_MODELS + 1 }, (_, i) => `m${i}`), "sonnet", [1]]) {
      expect(sanitizeModelChoice({ model: "opus", fallback }, claude), JSON.stringify(fallback)).toEqual({ model: "opus" });
    }
  });

  it("is null when nothing usable is left, which is stored as no entry", () => {
    for (const value of [undefined, null, "opus", [], {}, { model: "" }, { effort: "ultra" }, { advisor: "--x" }]) {
      expect(sanitizeModelChoice(value, claude), JSON.stringify(value)).toBeNull();
    }
  });

  it("never returns a value the backend would refuse, whatever it is fed", () => {
    // A seeded generator, so a failure is reproducible.
    let seed = 0x5eed;
    const next = () => (seed = (seed * 1103515245 + 12345) & 0x7fffffff);
    const pool = ["opus", "opus[1m]", "-x", "", "a,b", "x".repeat(MAX_MODEL_ID_LEN + 1), "high", "xhigh", "ultra", 7, null, ["sonnet"], ["a", "a"], { a: 1 }];
    const pick = () => pool[next() % pool.length];
    for (let i = 0; i < 5_000; i += 1) {
      const raw: Record<string, unknown> = {};
      for (const field of [...MODEL_FIELDS, "thinking"]) if (next() % 2) raw[field] = field === "fallback" && next() % 2 ? [pick(), pick()] : pick();
      for (const [launcher, fields] of Object.entries(MODEL_FIELDS_BY_LAUNCHER)) {
        const out = sanitizeModelChoice(raw, fields ?? []);
        if (out === null) continue;
        for (const key of Object.keys(out)) expect(fields, `${launcher} got ${key}`).toContain(key);
        if (out.model !== undefined) expect(isModelId(out.model)).toBe(true);
        if (out.advisor !== undefined) expect(isModelId(out.advisor)).toBe(true);
        if (out.effort !== undefined) expect(EFFORT_LEVELS).toContain(out.effort);
        if (out.fallback !== undefined) {
          expect(out.fallback.length).toBeGreaterThan(0);
          expect(out.fallback.length).toBeLessThanOrEqual(MAX_FALLBACK_MODELS);
          expect(new Set(out.fallback).size).toBe(out.fallback.length);
          for (const id of out.fallback) expect(isModelId(id)).toBe(true);
        }
        // Idempotent: what it returns survives a second pass unchanged.
        expect(sanitizeModelChoice(out, fields ?? [])).toEqual(out);
      }
    }
  });
});

describe("model settings among the other agent defaults", () => {
  it("a broken entry costs that launcher's models and nothing beside them", () => {
    const sanitized = sanitizeAgentDefaults({
      permission: { claude: "edit" },
      max_live_runs: 9,
      models: { claude: "opus", codex: { model: "gpt-6", effort: "high" }, grok: { model: "--x" }, gemini: { model: "x" }, agy: { model: "gemini-3.8-flash-high" } },
    });
    expect(sanitized).toEqual({
      permission: { claude: "edit" },
      max_live_runs: 9,
      models: { codex: { model: "gpt-6" }, agy: { model: "gemini-3.8-flash-high" } },
    });
    for (const models of [null, "opus", [], [{ model: "x" }], {}]) {
      expect(sanitizeAgentDefaults({ permission: {}, models })).toEqual({ permission: {} });
    }
  });

  it("follows the field map it is given, not the fallback table", () => {
    // An older backend that offers Claude only a model must not have an
    // effort it would refuse passed back to it.
    const sanitized = sanitizeAgentDefaults({ permission: {}, models: { claude: { model: "opus", effort: "high" }, agy: { model: "x" } } }, undefined, { claude: ["model"] });
    expect(sanitized.models).toEqual({ claude: { model: "opus" } });
  });

  it("withModelChoice stores an empty choice as absence and keeps the rest", () => {
    const base: AgentDefaults = { permission: { claude: "edit" }, max_live_runs: 3, models: { codex: { model: "gpt-6" } } };
    const set = withModelChoice(base, "claude", { model: "opus", effort: undefined, fallback: [], advisor: "" });
    expect(set).toEqual({ permission: { claude: "edit" }, max_live_runs: 3, models: { codex: { model: "gpt-6" }, claude: { model: "opus" } } });
    expect(base.models).toEqual({ codex: { model: "gpt-6" } });
    const cleared = withModelChoice(withModelChoice(set, "claude", {}), "codex", {});
    expect(cleared).toEqual({ permission: { claude: "edit" }, max_live_runs: 3 });
    expect(modelChoiceOf(cleared, "claude")).toEqual({});
  });

  it("parseFallbackList splits on commas and whitespace and drops blanks", () => {
    expect(parseFallbackList(" sonnet, haiku ,,opus\tfable ")).toEqual(["sonnet", "haiku", "opus", "fable"]);
    expect(parseFallbackList("")).toEqual([]);
    expect(parseFallbackList(" , ")).toEqual([]);
  });
});
