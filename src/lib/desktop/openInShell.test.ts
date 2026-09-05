import { readFileSync, readdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";
import { openInDefaultApp, revealInFileManager } from "./openInShell";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, "..", "..");

function shippedSources(dir: string, out: string[] = []): string[] {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "__tests__") continue;
      shippedSources(full, out);
    } else if (/\.(ts|js|svelte)$/.test(entry.name) && !/\.(test|spec)\./.test(entry.name)) {
      out.push(full);
    }
  }
  return out;
}

describe("openInShell", () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    vi.mocked(invoke).mockResolvedValue(undefined);
  });

  /**
   * The repo root and the repo-relative path stay two separate arguments all
   * the way to Rust. Joining them here would hand the webview the ability to
   * name an absolute path, which is exactly what the Rust gate refuses to
   * accept.
   */
  it("sends the root and the relative path as separate arguments", async () => {
    await openInDefaultApp("/repos/gitpulse", "src/main.rs");
    expect(invoke).toHaveBeenCalledWith("cmd_open_worktree_path", {
      repo: "/repos/gitpulse",
      relative: "src/main.rs",
    });
  });

  it("reveals through its own command, not the opener plugin", async () => {
    await revealInFileManager("/repos/gitpulse", "src/main.rs");
    expect(invoke).toHaveBeenCalledWith("cmd_reveal_worktree_path", {
      repo: "/repos/gitpulse",
      relative: "src/main.rs",
    });
  });

  /**
   * A refusal from the Rust gate has to reach the caller. MarkDevViewer used
   * to `catch {}` this, which is why its missing containment check produced no
   * diagnostics at all — the button simply did nothing.
   */
  it("propagates a refusal instead of resolving", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(
      new Error("Refusing to open a path outside the repository: ../etc/passwd"),
    );
    await expect(openInDefaultApp("/repos/gitpulse", "../etc/passwd")).rejects.toThrow(
      "outside the repository",
    );
  });

  it("propagates a reveal refusal too", async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error("Cannot read src/gone.rs"));
    await expect(revealInFileManager("/repos/gitpulse", "src/gone.rs")).rejects.toThrow(
      "Cannot read",
    );
  });

  /**
   * Passes the path through untouched, including multi-byte segments: the
   * crash this audit began from was a byte-index split on text exactly like
   * this, so nothing on the path may assume ASCII.
   */
  it("passes multi-byte paths through unchanged", async () => {
    await openInDefaultApp("/repos/gitpulse", "docs/日本語/ファイル—名.md");
    expect(invoke).toHaveBeenCalledWith("cmd_open_worktree_path", {
      repo: "/repos/gitpulse",
      relative: "docs/日本語/ファイル—名.md",
    });
  });
});

describe("shell-open adoption", () => {
  /**
   * Derived rather than hand-listed, for the same reason the opener guard is:
   * the value of one owner is that a call site cannot skip its check, and a
   * guard that names today's call sites stops proving that tomorrow.
   *
   * `open_path` and `reveal_item_in_dir` are also ungranted in
   * `capabilities/default.json`, so a component reaching for the plugin
   * directly would fail at runtime and in `src-tauri/tests/acl_contract.rs`.
   * This assertion is the fast, local half of that.
   */
  it("routes every shell-open through this module", () => {
    const direct = shippedSources(frontendRoot)
      .filter((file) => {
        const source = readFileSync(file, "utf8");
        return /\bopenPath\s*\(|\brevealItemInDir\s*\(/.test(source);
      })
      .map((file) => relative(frontendRoot, file).split(/[\\/]/).join("/"));
    expect(direct).toEqual([]);
  });

  it("scans a real, populated frontend tree", () => {
    expect(shippedSources(frontendRoot).length).toBeGreaterThan(50);
  });
});
