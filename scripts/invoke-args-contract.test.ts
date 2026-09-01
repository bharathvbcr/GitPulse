import { readdirSync, readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * check:ipc proves every invoked command exists. It says nothing about the
 * arguments, and a wrong argument name fails at runtime with a deserialization
 * error rather than at build time — the command simply refuses, and only in
 * the code path that calls it.
 *
 * A required parameter must therefore be supplied by every call site, and no
 * call site may send a name the command does not declare. `Option<T>`
 * parameters are exempt from the first rule: Tauri reads an absent key as
 * None, which is how the graph is loaded without `skip`.
 */
const ROOT = fileURLToPath(new URL("..", import.meta.url));

function filesUnder(dir: string, exts: string[]): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = path.join(dir, entry);
    if (statSync(full).isDirectory()) return filesUnder(full, exts);
    return exts.some((e) => entry.endsWith(e)) ? [full] : [];
  });
}

/** Split on commas that are not inside <>, () or []. */
function splitTopLevel(text: string, on = ","): string[] {
  const out: string[] = [];
  let depth = 0;
  let buf = "";
  for (const ch of text) {
    if ("<([{".includes(ch)) depth += 1;
    else if (">)]}".includes(ch)) depth -= 1;
    if (depth === 0 && ch === on) {
      out.push(buf);
      buf = "";
      continue;
    }
    buf += ch;
  }
  out.push(buf);
  return out.map((s) => s.trim()).filter(Boolean);
}

const snakeToCamel = (s: string) =>
  s.split("_").map((w, i) => (i === 0 ? w : w.charAt(0).toUpperCase() + w.slice(1))).join("");

interface CommandParams {
  required: Set<string>;
  optional: Set<string>;
}

/** Every #[tauri::command] and the arguments it takes from the caller. */
function rustCommands(): Map<string, CommandParams> {
  const commands = new Map<string, CommandParams>();
  const source = filesUnder(path.join(ROOT, "src-tauri", "src"), [".rs"])
    .map((f) => readFileSync(f, "utf8"))
    .join("\n");
  const re =
    /#\[tauri::command[^\]]*\]\s*(?:#\[[^\]]*\]\s*)*(?:pub\s+)?(?:async\s+)?fn\s+(\w+)\s*\(([\s\S]*?)\)\s*(?:->|\{)/g;
  for (const match of source.matchAll(re)) {
    const [, name, rawParams] = match;
    const required = new Set<string>();
    const optional = new Set<string>();
    // Split at top level: `State<'_, crate::x::Y>` contains a comma that is
    // not a parameter boundary, and treating it as one invents a parameter.
    for (const param of splitTopLevel(rawParams)) {
      const colon = param.indexOf(":");
      if (colon === -1) continue;
      const ident = param.slice(0, colon).trim();
      const type = param.slice(colon + 1).trim();
      // Injected by Tauri rather than sent by the caller.
      if (/^(AppHandle|State<|Window<|Webview)/.test(type)) continue;
      if (["app", "state", "window"].includes(ident)) continue;
      (type.startsWith("Option<") ? optional : required).add(snakeToCamel(ident));
    }
    commands.set(name, { required, optional });
  }
  return commands;
}

/** Keys of an object literal, including shorthand properties. */
function objectKeys(body: string): Set<string> {
  return new Set(
    splitTopLevel(body)
      .map((entry) => entry.split(":")[0].trim())
      .filter((key) => /^\w+$/.test(key)),
  );
}

interface CallSite {
  file: string;
  command: string;
  keys: Set<string>;
}

function callSites(): CallSite[] {
  const sites: CallSite[] = [];
  for (const file of filesUnder(path.join(ROOT, "src"), [".ts", ".svelte"])) {
    if (file.includes(".test.")) continue;
    const text = readFileSync(file, "utf8");
    const re = /invoke\w*\s*(?:<[^>]*>)?\s*\(\s*"(\w+)"\s*,\s*\{/g;
    for (const match of text.matchAll(re)) {
      const open = text.indexOf("{", match.index! + match[0].length - 1);
      let depth = 0;
      let close = open;
      for (let i = open; i < text.length; i += 1) {
        if (text[i] === "{") depth += 1;
        else if (text[i] === "}") {
          depth -= 1;
          if (depth === 0) {
            close = i;
            break;
          }
        }
      }
      sites.push({
        file: path.relative(ROOT, file),
        command: match[1],
        keys: objectKeys(text.slice(open + 1, close)),
      });
    }
  }
  return sites;
}

describe("invoke argument contract", () => {
  const commands = rustCommands();
  const sites = callSites();

  it("parsed both sides", () => {
    // Either regex silently matching nothing would make this vacuous.
    expect(commands.size).toBeGreaterThan(80);
    expect(sites.length).toBeGreaterThan(50);
    // A generic containing a comma must not become a parameter.
    for (const [name, params] of commands) {
      expect([...params.required, ...params.optional], name).not.toContain("crate");
    }
  });

  it("sends every required argument at every call site", () => {
    const missing = sites
      .filter((site) => commands.has(site.command))
      .map((site) => {
        const { required } = commands.get(site.command)!;
        const absent = [...required].filter((name) => !site.keys.has(name));
        return absent.length ? `${site.command} missing ${absent.join(", ")} (${site.file})` : null;
      })
      .filter(Boolean);
    expect(missing).toEqual([]);
  });

  it("never sends an argument the command does not declare", () => {
    const unknown = sites
      .filter((site) => commands.has(site.command))
      .map((site) => {
        const { required, optional } = commands.get(site.command)!;
        const extra = [...site.keys].filter((k) => !required.has(k) && !optional.has(k));
        return extra.length ? `${site.command} sends unknown ${extra.join(", ")} (${site.file})` : null;
      })
      .filter(Boolean);
    expect(unknown).toEqual([]);
  });
});
