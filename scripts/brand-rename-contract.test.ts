import { describe, expect, it } from "vitest";
import { applyExpectations, EXPECTATIONS, kindOf, plan, readerRanges, renameIn } from "./brand-rename.mjs";

function applyRename(source: string, kind: "ts" | "rust" | "svelte" | "html" | "md", path = ""): string {
  const { ranges } = readerRanges(source, kind, path);
  return renameIn(source, ranges).text;
}

describe("scripts/brand-rename.mjs contract", () => {
  it("renames GitPulse before escape sequences (\\n, \\') in TS and Rust literals", () => {
    const rust = `let tip = format!("GitPulse\\n{agents}{}\\n{}", a, b);`;
    expect(applyRename(rust, "rust", "src-tauri/src/desktop/tray.rs")).toBe(
      `let tip = format!("Waylode\\n{agents}{}\\n{}", a, b);`,
    );

    const ts = `const note = 'Written "GitPulse\\'s checks". The technical term stays in code.';`;
    expect(applyRename(ts, "ts", "src/lib/ui/copy/vocabulary.ts")).toBe(
      `const note = 'Written "Waylode\\'s checks". The technical term stays in code.';`,
    );
  });

  it("renames UI standalone \"GitPulse\" strings while keeping explicit allowlisted internal sites", () => {
    expect(applyRename('let title = "GitPulse";', "svelte", "src/lib/components/Logo.svelte")).toBe(
      'let title = "Waylode";',
    );
    expect(applyRename('return "GitPulse";', "ts", "src/lib/desktop/windowChrome.ts")).toBe(
      'return "Waylode";',
    );
    expect(applyRename('const APP: &str = "GitPulse";', "rust", "src-tauri/src/workbench/notifications/graft.rs")).toBe(
      'const APP: &str = "Waylode";',
    );

    // Explicit allowlist: internal storage/profile directories and stress fixtures stay untouched.
    for (const [path, snippet] of [
      ["src-tauri/src/tool_config.rs", 'PathBuf::from(base).join("GitPulse")'],
      ["src-tauri/src/logging.rs", 'PathBuf::from(home).join("Library").join("Logs").join("GitPulse")'],
      ["src-tauri/src/alerts/stress.rs", 'place: Some("GitPulse".into())'],
      ["src/lib/ui/copy/sentenceCase.ts", 'export const PROPER_NOUNS = ["GitPulse"];'],
    ] as const) {
      const scan = readerRanges(snippet, kindOf(path), path);
      expect(scan.ranges, path).toEqual([]);
      expect(scan.kept.length, path).toBeGreaterThan(0);
    }
  });

  it("skips inline #[cfg(test)] modules and functions inside non-*_tests.rs Rust files", () => {
    const src = `
pub fn ui_title() -> &'static str {
    "Welcome to GitPulse"
}

#[cfg(test)]
mod tests {
    #[test]
    fn keeps_fixture() {
        assert_eq!("Blocked by GitPulse", "Blocked by GitPulse");
    }
}

#[cfg(all(test, unix))]
fn unix_test_helper() -> &'static str {
    "GitPulse test helper"
}
`;
    const out = applyRename(src, "rust", "src-tauri/src/workbench/review.rs");
    expect(out).toContain('"Welcome to Waylode"');
    expect(out).toContain('"Blocked by GitPulse"');
    expect(out).toContain('"GitPulse test helper"');
  });

  it("excludes agent-facing prompt/hook functions in terminal_command.rs and agy_plugin.rs", () => {
    const termCmd = `
pub fn user_error() -> &'static str {
    "Provider does not support resuming from GitPulse."
}

pub(crate) const COMPLETION: &str = "Call gitpulse_complete_task on GitPulse.";

pub(super) fn arguments() -> Vec<String> {
    vec!["In this GitPulse terminal, read the brief.".into()]
}
`;
    const renamedTerm = applyRename(termCmd, "rust", "src-tauri/src/workbench/terminal_command.rs");
    expect(renamedTerm).toContain('"Provider does not support resuming from Waylode."');
    expect(renamedTerm).toContain('COMPLETION: &str = "Call gitpulse_complete_task on GitPulse."');
    expect(renamedTerm).toContain('"In this GitPulse terminal, read the brief."');

    const agyPlugin = `
pub fn user_notice() -> &'static str {
    "Open GitPulse settings."
}

#[cfg(unix)]
fn build_hook_wrapper(hook_bin: &Path) -> String {
    format!("{{\\"reason\\":\\"GitPulse safety hook binary is missing\\"}}")
}
`;
    const renamedAgy = applyRename(agyPlugin, "rust", "src-tauri/src/workbench/agy_plugin.rs");
    expect(renamedAgy).toContain('"Open Waylode settings."');
    expect(renamedAgy).toContain('GitPulse safety hook binary is missing');
  });

  it("never leaves unrenamed reader-facing GitPulse occurrences in the live repository", () => {
    const { changes, stale } = plan();
    expect(changes.map((c) => `${c.path} (${c.count})`)).toEqual([]);
    expect(stale).toEqual([]);
  });

  it("moves quoted expectations with the copy, and fails on one it cannot find", () => {
    const path = "src-tauri/src/desktop/tray.rs";
    const before = 'label: "Open GitPulse".into()\nlabel: "Quit GitPulse".into()';
    const once = applyExpectations(path, before);
    expect(once).toEqual({ text: 'label: "Open Waylode".into()\nlabel: "Quit Waylode".into()', count: 2, stale: [] });
    expect(applyExpectations(path, once.text)).toEqual({ text: once.text, count: 0, stale: [] });
    expect(applyExpectations(path, 'label: "Open".into()').stale).toEqual([
      'label: "Open GitPulse".into()',
      'label: "Quit GitPulse".into()',
    ]);
    expect(Object.keys(EXPECTATIONS)).toContain(path);
  });

  it("renames text in strings and markup but not comments, identifiers or paths", () => {
    const ts = [
      "// GitPulse in a line comment",
      "/* GitPulse in a block comment",
      "   GitPulse on a gutterless continuation line */",
      'const a = "GitPulse could not read it";',
      "const b = isGitPulseLane(kind);",
      'const c = "/Applications/GitPulse.app";',
      'const d = "Application Support/GitPulse";',
    ].join("\n");
    const out = applyRename(ts, "ts");
    expect(out).toContain("// GitPulse in a line comment");
    expect(out).toContain("GitPulse on a gutterless continuation line */");
    expect(out).toContain('"Waylode could not read it"');
    expect(out).toContain("isGitPulseLane(kind)");
    expect(out).toContain("/Applications/GitPulse.app");
    expect(out).toContain("Application Support/GitPulse");
  });

  it("keeps markers something reads back", () => {
    const ts = 'const id = "user.name=GitPulse";';
    expect(applyRename(ts, "ts")).toBe(ts);
    const rust = 'const M: &str = "# GitPulse: DevMap index state";\nlet r = r#"Managed by GitPulse terminal"#;';
    expect(applyRename(rust, "rust")).toBe(rust);
  });

  it("follows template literals through nested templates and keeps comments inside them", () => {
    const ts = "const m = `Quit GitPulse ${n > 1 ? `(${n} GitPulse tabs)` : /* GitPulse */ \"\"} now`;";
    const out = applyRename(ts, "ts");
    expect(out).toContain("Quit Waylode");
    expect(out).toContain("(${n} Waylode tabs)");
    expect(out).toContain("/* GitPulse */");
  });

  it("does not mistake a regex or a Rust char literal for a string", () => {
    expect(applyRename('const re = /"/;\n// GitPulse note\nconst s = "GitPulse ok";', "ts")).toBe(
      'const re = /"/;\n// GitPulse note\nconst s = "Waylode ok";',
    );
    expect(applyRename("let q = '\"';\n// GitPulse note\nlet s = \"GitPulse ok\";", "rust")).toBe(
      "let q = '\"';\n// GitPulse note\nlet s = \"Waylode ok\";",
    );
  });

  it("renames Markdown prose but not code blocks or code spans", () => {
    const md = "GitPulse is a desktop app.\n\n```sh\nopen -a GitPulse\n```\n\nRun `GitPulse --help`. GitPulse’s docs.";
    expect(applyRename(md, "md")).toBe(
      "Waylode is a desktop app.\n\n```sh\nopen -a GitPulse\n```\n\nRun `GitPulse --help`. Waylode’s docs.",
    );
  });

  it("leaves Svelte comments and styles alone", () => {
    const svelte = '<script lang="ts">\n  // GitPulse\n  const t = "Open GitPulse";\n</script>\n<!-- GitPulse -->\n<h1>Welcome to GitPulse</h1>\n<style>/* GitPulse */</style>';
    const out = applyRename(svelte, "svelte");
    expect(out).toContain("// GitPulse");
    expect(out).toContain('"Open Waylode"');
    expect(out).toContain("<!-- GitPulse -->");
    expect(out).toContain("<h1>Welcome to Waylode</h1>");
    expect(out).toContain("/* GitPulse */");
  });

  it("scopes the rename to app copy and user docs", () => {
    expect(kindOf("src/lib/ui/formatError.ts")).toBe("ts");
    expect(kindOf("src/lib/ui/formatError.test.ts")).toBeNull();
    expect(kindOf("src/lib/tools/agentConnect.ts")).toBeNull();
    expect(kindOf("src-tauri/src/mcp/mod.rs")).toBeNull();
    expect(kindOf("src-tauri/src/hooks/mod.rs")).toBeNull();
    expect(kindOf("src-tauri/src/desktop/tray.rs")).toBe("rust");
    expect(kindOf("src-tauri/vendored/dc-store/src/lib.rs")).toBeNull();
    expect(kindOf("CHANGELOG.md")).toBeNull();
    expect(kindOf("docs/RELEASE_NOTES_v0.2.0.md")).toBeNull();
    expect(kindOf("CLAUDE.md")).toBeNull();
    expect(kindOf("docs/INSTALLATION.md")).toBe("md");
  });
});
