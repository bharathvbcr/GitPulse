import { describe, expect, it } from "vitest";
import {
  applyTriggerReplacement,
  buildAgentModelOptions,
  detectTrigger,
  filterAgentModels,
  filterRepoFiles,
  filterSlashCommands,
} from "./composerTriggers";

describe("detectTrigger", () => {
  it("triggers at index 0 for /, @, and &", () => {
    expect(detectTrigger("/", 1)).toEqual({ kind: "/", query: "", start: 0, end: 1 });
    expect(detectTrigger("/rev", 4)).toEqual({ kind: "/", query: "rev", start: 0, end: 4 });
    expect(detectTrigger("@src/main", 9)).toEqual({ kind: "@", query: "src/main", start: 0, end: 9 });
    expect(detectTrigger("&opus", 5)).toEqual({ kind: "&", query: "opus", start: 0, end: 5 });
  });

  it("triggers immediately after spaces, tabs, or newlines", () => {
    expect(detectTrigger("check @foo", 10)).toEqual({ kind: "@", query: "foo", start: 6, end: 10 });
    expect(detectTrigger("line1\n/plan", 11)).toEqual({ kind: "/", query: "plan", start: 6, end: 11 });
    expect(detectTrigger("use\t&agy", 8)).toEqual({ kind: "&", query: "agy", start: 4, end: 8 });
  });

  it("tracks the caret when text follows the active token", () => {
    const text = "inspect @src/lib/mavi/prefs and tests";
    const caret = "inspect @src/lib/mavi/prefs".length;
    expect(detectTrigger(text, caret)).toEqual({
      kind: "@",
      query: "src/lib/mavi/prefs",
      start: 8,
      end: caret,
    });
    expect(detectTrigger(text, caret + 1)).toBeNull();
  });

  it("never triggers mid-word, inside URLs, or inside email addresses", () => {
    expect(detectTrigger("foo/bar", 7)).toBeNull();
    expect(detectTrigger("https://example.com/path", 24)).toBeNull();
    expect(detectTrigger("mail a@b.com", 12)).toBeNull();
    expect(detectTrigger("R&D", 3)).toBeNull();
    expect(detectTrigger("", 0)).toBeNull();
    expect(detectTrigger("/new", 0)).toBeNull();
  });

  it("stops triggering once whitespace follows the token", () => {
    expect(detectTrigger("/new ", 5)).toBeNull();
    expect(detectTrigger("check @src/foo.ts next", 22)).toBeNull();
  });
});

describe("applyTriggerReplacement", () => {
  it("replaces only the active trigger span and preserves surrounding text", () => {
    const text = "please /review before commit";
    const trigger = detectTrigger(text, "please /review".length)!;
    const result = applyTriggerReplacement(
      text,
      trigger,
      "Review the uncommitted changes for bugs, edge cases, and missing tests.",
    );
    expect(result).toEqual({
      text: "please Review the uncommitted changes for bugs, edge cases, and missing tests. before commit",
      caret: 7 + "Review the uncommitted changes for bugs, edge cases, and missing tests.".length,
    });
  });

  it("strips a slash action cleanly when replaced with empty string", () => {
    const text = "/clear";
    const trigger = detectTrigger(text, text.length)!;
    expect(applyTriggerReplacement(text, trigger, "")).toEqual({ text: "", caret: 0 });
  });

  it("inserts a file mention with trailing space", () => {
    const text = "check @pref";
    const trigger = detectTrigger(text, text.length)!;
    expect(applyTriggerReplacement(text, trigger, "@src/lib/mavi/prefs.svelte.ts ")).toEqual({
      text: "check @src/lib/mavi/prefs.svelte.ts ",
      caret: "check @src/lib/mavi/prefs.svelte.ts ".length,
    });
  });
});

describe("filterSlashCommands", () => {
  it("filters thread-only, working-only, and start-page-only commands by context", () => {
    const startPageIds = filterSlashCommands("", { inThread: false, working: false }).map((c) => c.id);
    expect(startPageIds).not.toContain("new");
    expect(startPageIds).not.toContain("changes");
    expect(startPageIds).not.toContain("terminal");
    expect(startPageIds).not.toContain("handoff");
    expect(startPageIds).not.toContain("stop");
    expect(startPageIds).not.toContain("end");
    expect(startPageIds).not.toContain("compact");
    expect(startPageIds).toContain("worktree");
    expect(startPageIds).toContain("customize");
    expect(startPageIds).toContain("clear");
    expect(startPageIds).toContain("explain");
    expect(startPageIds).toContain("diff");
    expect(startPageIds).toContain("fix");
    expect(startPageIds).toContain("refactor");

    const idleThreadIds = filterSlashCommands("", { inThread: true, working: false }).map((c) => c.id);
    expect(idleThreadIds).toContain("new");
    expect(idleThreadIds).toContain("changes");
    expect(idleThreadIds).toContain("terminal");
    expect(idleThreadIds).toContain("handoff");
    expect(idleThreadIds).toContain("end");
    expect(idleThreadIds).toContain("compact");
    expect(idleThreadIds).toContain("diff");
    expect(idleThreadIds).toContain("fix");
    expect(idleThreadIds).toContain("refactor");
    expect(idleThreadIds).not.toContain("worktree");
    expect(idleThreadIds).not.toContain("stop");

    const workingThreadIds = filterSlashCommands("", { inThread: true, working: true }).map((c) => c.id);
    expect(workingThreadIds).toContain("stop");
    expect(workingThreadIds).toContain("end");
    expect(workingThreadIds).toContain("compact");
    expect(workingThreadIds).not.toContain("worktree");
  });

  it("provides expected insertText and metadata for prompt and context-scoped commands", () => {
    const startItems = filterSlashCommands("", { inThread: false, working: false });
    const byIdStart = new Map(startItems.map((c) => [c.id, c]));
    expect(byIdStart.get("diff")).toMatchObject({
      command: "/diff",
      kind: "prompt",
      label: "Summarize diff",
      insertText: "Show a concise summary of the current git diff and highlight any risky edits.",
    });
    expect(byIdStart.get("fix")).toMatchObject({
      command: "/fix",
      kind: "prompt",
      label: "Fix issue",
      insertText: "Diagnose the issue, identify the root cause, fix it cleanly, and verify with tests.",
    });
    expect(byIdStart.get("refactor")).toMatchObject({
      command: "/refactor",
      kind: "prompt",
      label: "Refactor target",
      insertText:
        "Refactor the target code for clarity and maintainability without changing behavior, then verify tests pass.",
    });
    expect(byIdStart.get("worktree")).toMatchObject({
      command: "/worktree",
      kind: "action",
      label: "Toggle worktree",
      requiresStartPage: true,
    });

    const threadItems = filterSlashCommands("", { inThread: true, working: false });
    const byIdThread = new Map(threadItems.map((c) => [c.id, c]));
    expect(byIdThread.get("compact")).toMatchObject({
      command: "/compact",
      kind: "prompt",
      label: "Compact summary",
      requiresThread: true,
      insertText: "Summarize our progress so far, key decisions made, and what remains.",
    });
    expect(byIdThread.get("end")).toMatchObject({
      command: "/end",
      kind: "action",
      label: "End session",
      requiresThread: true,
    });
  });

  it("matches query case-insensitively across command, label, and description", () => {
    const byCmd = filterSlashCommands("HAND", { inThread: true, working: false });
    expect(byCmd.map((c) => c.id)).toEqual(["handoff"]);

    const ranked = filterSlashCommands("han", { inThread: true, working: false });
    expect(ranked[0]?.id).toBe("handoff");

    const byDesc = filterSlashCommands("worktree", { inThread: true, working: false });
    expect(byDesc.map((c) => c.id)).toEqual(["terminal"]);

    const byPrompt = filterSlashCommands("TEST", { inThread: false, working: false });
    expect(byPrompt.map((c) => c.id)).toContain("tests");
  });

  it("returns empty for non-matching queries like a double slash", () => {
    expect(filterSlashCommands("/", { inThread: true, working: true })).toEqual([]);
  });
});

describe("filterRepoFiles", () => {
  const files = [
    "README.md",
    "docs/mavi/guide.md",
    "src/lib/components/mavi/MaviComposer.svelte",
    "src/lib/components/mavi/MaviView.svelte",
    "src/lib/mavi/prefs.svelte.ts",
    "src/lib/mavi/scrollPin.ts",
    "src/lib/mavi/composerTriggers.ts",
    "src/app.css",
  ];

  it("prioritizes touched files when query is empty and respects limit", () => {
    const result = filterRepoFiles(files, "", ["src/lib/mavi/scrollPin.ts", "src/app.css"], 4);
    expect(result).toEqual([
      "src/lib/mavi/scrollPin.ts",
      "src/app.css",
      "README.md",
      "docs/mavi/guide.md",
    ]);
  });

  it("prioritizes priorityPaths, then filename prefix/substring, then path matches", () => {
    const result = filterRepoFiles(files, "mavi", ["src/lib/mavi/prefs.svelte.ts"]);
    expect(result[0]).toBe("src/lib/mavi/prefs.svelte.ts");
    // Filename prefix matches ("MaviComposer.svelte", "MaviView.svelte") rank before path-only matches
    expect(result.slice(1, 3)).toEqual([
      "src/lib/components/mavi/MaviView.svelte",
      "src/lib/components/mavi/MaviComposer.svelte",
    ]);
  });

  it("defaults limit to 12 and handles limit <= 0", () => {
    const many = Array.from({ length: 30 }, (_, i) => `src/file-${i}.ts`);
    expect(filterRepoFiles(many, "")).toHaveLength(12);
    expect(filterRepoFiles(many, "", undefined, 0)).toEqual([]);
  });
});

describe("buildAgentModelOptions & filterAgentModels", () => {
  it("builds default entries for both agents, deduplicates Claude aliases, and appends Antigravity models", () => {
    const options = buildAgentModelOptions({
      claude: [
        { id: "opus", label: "Claude Opus" },
        { id: "claude-sonnet-4-5", label: "Sonnet 4.5" },
      ],
      agy: [
        { id: "gemini-2.5-pro", label: "Gemini 2.5 Pro" },
        { id: "gemini-2.5-pro", label: "Duplicate" },
        { id: "claude-sonnet-4-thinking", label: null },
      ],
    });

    const defaults = options.filter((o) => o.isAgentDefault);
    expect(defaults.map((d) => ({ agent: d.agent, label: d.label, model: d.model }))).toEqual([
      { agent: "claude", label: "Claude Code", model: null },
      { agent: "agy", label: "Antigravity", model: null },
    ]);

    const claudeIds = options.filter((o) => o.agent === "claude" && !o.isAgentDefault).map((o) => o.model);
    expect(claudeIds).toEqual(["fable", "opus", "sonnet", "haiku", "claude-sonnet-4-5"]);
    expect(options.find((o) => o.id === "claude:opus")?.label).toBe("Claude Opus");

    const agyModels = options.filter((o) => o.agent === "agy" && !o.isAgentDefault);
    expect(agyModels.map((o) => o.model)).toEqual(["gemini-2.5-pro", "claude-sonnet-4-thinking"]);
    expect(agyModels[1]?.label).toBe("claude-sonnet-4-thinking");
  });

  it("still provides Claude fallback aliases and Antigravity default when catalogs are empty", () => {
    const options = buildAgentModelOptions({});
    expect(options.some((o) => o.agent === "agy" && o.isAgentDefault)).toBe(true);
    expect(options.filter((o) => o.agent === "claude" && !o.isAgentDefault).map((o) => o.model)).toEqual([
      "fable",
      "opus",
      "sonnet",
      "haiku",
    ]);
  });

  it("filters across agent, agentLabel, model, label, and detail with default limit 14", () => {
    const options = buildAgentModelOptions({
      agy: Array.from({ length: 16 }, (_, i) => ({ id: `agy-model-${i}`, label: `Model ${i}` })),
    });

    expect(filterAgentModels(options, "")).toHaveLength(14);
    expect(filterAgentModels(options, "antigravity").every((o) => o.agent === "agy")).toBe(true);
    expect(filterAgentModels(options, "opus").map((o) => o.id)).toEqual(["claude:opus"]);
    expect(filterAgentModels(options, "agy-model-12").map((o) => o.model)).toEqual(["agy-model-12"]);
  });
});
