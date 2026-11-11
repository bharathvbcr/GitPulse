import { describe, expect, it } from "vitest";
import {
  formatAgentReference,
  formatDroppedPathsForSession,
  posixShellQuote,
  relativeToRepo,
} from "./dropFormatting";

describe("dropFormatting", () => {
  it("quotes shell metacharacters, spaces, quotes, and leading dashes safely", () => {
    expect(posixShellQuote("src/lib/main.ts")).toBe("src/lib/main.ts");
    expect(posixShellQuote("/tmp/capture-1.png")).toBe("/tmp/capture-1.png");
    expect(posixShellQuote("my folder/file (1).ts")).toBe("'my folder/file (1).ts'");
    expect(posixShellQuote("it's/a$test.sh")).toBe("'it'\\''s/a$test.sh'");
    expect(posixShellQuote("-rf")).toBe("'-rf'");
    expect(posixShellQuote("")).toBe("''");
    expect(posixShellQuote("a\0b.txt")).toBe("ab.txt");
  });

  it("computes repository-relative paths only when inside the repository root", () => {
    expect(relativeToRepo("/repo/root/src/app.ts", "/repo/root")).toBe("src/app.ts");
    expect(relativeToRepo("/repo/root/src/app.ts", "/repo/root/")).toBe("src/app.ts");
    expect(relativeToRepo("/repo/root", "/repo/root")).toBe(".");
    expect(relativeToRepo("/repo/root-other/src/app.ts", "/repo/root")).toBeNull();
    expect(relativeToRepo("/tmp/screenshot.png", "/repo/root")).toBeNull();
    expect(relativeToRepo("src/nested/file.rs", "/repo/root")).toBe("src/nested/file.rs");
    expect(relativeToRepo("../outside.rs", "/repo/root")).toBeNull();
    expect(relativeToRepo("/repo/root/src/../../etc/passwd", "/repo/root")).toBeNull();
    expect(relativeToRepo("/repo/root/src/../lib/ok.ts", "/repo/root")).toBe("lib/ok.ts");
    expect(relativeToRepo("src/../../etc/passwd", "/repo/root")).toBeNull();
  });

  it("formats @path references with optional line spans for Claude Code and Antigravity", () => {
    expect(formatAgentReference("/repo/src/index.ts", "/repo")).toBe("@src/index.ts");
    expect(formatAgentReference("/tmp/shot.png", "/repo")).toBe("@/tmp/shot.png");
    expect(
      formatAgentReference("/repo/src/index.ts", "/repo", { startLine: 12, endLine: 28 }),
    ).toBe("@src/index.ts#L12-L28");
    expect(formatAgentReference("/repo/src/index.ts", "/repo", { startLine: 7, endLine: 7 })).toBe(
      "@src/index.ts#L7",
    );
    expect(formatAgentReference("/repo/docs/My Plan.md", "/repo")).toBe('@"docs/My Plan.md"');
  });

  it("formats dropped file lists differently for coding agents vs interactive shells", () => {
    const files = ["/repo/src/a.ts", "/repo/docs/Read Me.md", "/tmp/ext's.png"];
    expect(formatDroppedPathsForSession(files, { launcher: "claude", repoPath: "/repo" })).toBe(
      '@src/a.ts @"docs/Read Me.md" @"/tmp/ext\'s.png" ',
    );
    expect(formatDroppedPathsForSession(files, { launcher: "agy", repoPath: "/repo" })).toBe(
      '@src/a.ts @"docs/Read Me.md" @"/tmp/ext\'s.png" ',
    );
    expect(formatDroppedPathsForSession(files, { launcher: "shell", repoPath: "/repo" })).toBe(
      "src/a.ts 'docs/Read Me.md' '/tmp/ext'\\''s.png' ",
    );
    expect(formatDroppedPathsForSession([], { launcher: "claude", repoPath: "/repo" })).toBe("");
  });
});
