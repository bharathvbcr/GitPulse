import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { EN, type MessageKey } from "./messages";
import { createTranslator, t } from "./translate";

const srcRoot = join(dirname(fileURLToPath(import.meta.url)), "..", "..");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.(svelte|ts)$/.test(entry.name) && !/\.test\.ts$/.test(entry.name) ? [path] : [];
  });
}

describe("message catalogue", () => {
  it("has no key that no source file looks up", () => {
    const own = ["messages.ts", "translate.ts"];
    const text = sourceFiles(srcRoot)
      .filter((file) => !own.some((name) => file.endsWith(join("i18n", name))))
      .map((file) => readFileSync(file, "utf8"))
      .join("\n");
    const used = new Set([...text.matchAll(/(?<![\w.$])t\(\s*["']([\w.]+)["']/g)].map((m) => m[1]));
    const unused = (Object.keys(EN) as MessageKey[]).filter((key) => !used.has(key));
    expect(unused).toEqual([]);
  });

  it("keeps every message non-empty", () => {
    for (const [key, value] of Object.entries(EN)) expect(value.trim(), key).not.toBe("");
  });
});

describe("translator", () => {
  it("returns the English text by default", () => {
    expect(t("setup.title")).toBe("Set up optional tools");
  });

  it("prefers a locale's translation and falls back per key", () => {
    const de = createTranslator({ "setup.close": "Schließen" });
    expect(de("setup.close")).toBe("Schließen");
    expect(de("setup.title")).toBe(EN["setup.title"]);
  });

  it("interpolates placeholders and leaves unknown ones visible", () => {
    const tr = createTranslator({ "setup.subtitle": "{tool} — {missing}" } as never);
    expect(tr("setup.subtitle", { tool: "devmap" })).toBe("devmap — {missing}");
  });

  it("does not read inherited properties as parameters", () => {
    const tr = createTranslator({ "setup.subtitle": "{toString}" });
    expect(tr("setup.subtitle", {})).toBe("{toString}");
  });
});
