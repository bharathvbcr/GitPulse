import { describe, expect, it, vi } from "vitest";
import {
  formatCommitAgentPrompt,
  getAgentCommitCliCommand,
  getAgentCommitSdkSnippet,
  launchAgentCommit,
  AGENT_COMMIT_NO_REPO,
  AGENT_COMMIT_UNSUPPORTED_LAUNCHER,
} from "./agentCommit";
import { terminalLaunchRequests, type PromptLauncher } from "../terminal/launchRequests";
import { AGENT_PROMPT_MAX_BYTES } from "../terminal/agentPromptText";

describe("agentCommit", () => {
  describe("formatCommitAgentPrompt", () => {
    it("throws an error when repository path is missing or empty", () => {
      expect(() => formatCommitAgentPrompt({ repoPath: "" })).toThrow(AGENT_COMMIT_NO_REPO);
      expect(() => formatCommitAgentPrompt({ repoPath: "   " })).toThrow(AGENT_COMMIT_NO_REPO);
    });

    it("generates instructions for quick commit (all uncommitted changes)", () => {
      const prompt = formatCommitAgentPrompt({
        repoPath: "/Users/dev/repo",
        onlyStaged: false,
      });

      expect(prompt).toContain("Review all uncommitted changes in this repository and commit them.");
      expect(prompt).toContain("Stage the files that belong together in this change set (`git add`).");
      expect(prompt).toContain("Repository: /Users/dev/repo");
      expect(prompt).toContain("Commit Scope: All uncommitted changes (quick commit)");
      expect(prompt).toContain("AGENTS.md / CLAUDE.md");
    });

    it("generates instructions for staged-only commit", () => {
      const prompt = formatCommitAgentPrompt({
        repoPath: "/Users/dev/repo",
        onlyStaged: true,
      });

      expect(prompt).toContain("Review and commit only the currently staged changes in this repository.");
      expect(prompt).toContain("Only commit files that are currently staged in the Git index.");
      expect(prompt).toContain("Commit Scope: Staged files only");
      expect(prompt).toContain("git diff --cached");
    });

    it("includes safe user note, files, and diff summary in context", () => {
      const prompt = formatCommitAgentPrompt({
        repoPath: "/Users/dev/repo",
        instruction: "Fix authentication header and token refresh",
        files: [
          { path: "src/auth.ts", status: "M", isStaged: true },
          { path: "src/token.ts", status: "A", isStaged: false },
        ],
        diffSummary: "+12 -4 lines across 2 files",
      });

      expect(prompt).toContain("User Note: Fix authentication header and token refresh");
      expect(prompt).toContain("- [staged] src/auth.ts (M)");
      expect(prompt).toContain("- [unstaged] src/token.ts (A)");
      expect(prompt).toContain("DIFF SUMMARY:\n+12 -4 lines across 2 files");
    });

    it("sanitizes dangerous control characters and BiDi overrides in paths and notes", () => {
      const prompt = formatCommitAgentPrompt({
        repoPath: "/Users/dev/\u202erepo\nrm -rf /",
        instruction: "Injected\x00note\r\nwith\u2028line",
        files: [{ path: "test\x1b[31mfile.ts", isStaged: false }],
      });

      expect(prompt).not.toContain("\x00");
      expect(prompt).not.toContain("\x1b");
      expect(prompt).toContain("\\u{202e}");
      expect(prompt).toContain("\\u{001b}");
    });

    it("strictly bounds oversized prompt under AGENT_PROMPT_MAX_BYTES", () => {
      const hugeFiles = Array.from({ length: 1500 }, (_, i) => ({
        path: `src/components/very/deeply/nested/path/to/component_${i}.svelte`,
        status: "M",
        isStaged: i % 2 === 0,
      }));

      const prompt = formatCommitAgentPrompt({
        repoPath: "/Users/dev/huge-repo",
        files: hugeFiles,
        diffSummary: "Massive change: " + "+10 ".repeat(500),
      });

      const byteLength = new TextEncoder().encode(prompt).byteLength;
      expect(byteLength).toBeLessThanOrEqual(AGENT_PROMPT_MAX_BYTES);
      expect(prompt).toContain("[GitPulse context clipped; inspect the repository for the remaining details.]");
      expect(prompt).toContain("Inspect the working directory changes and record an accurate Git commit.");
    });
  });

  describe("getAgentCommitCliCommand", () => {
    it("generates correct CLI invocation for Antigravity (agy)", () => {
      const cmd = getAgentCommitCliCommand("agy", "Commit these changes", "/path/to/repo");
      expect(cmd).toBe('cd "/path/to/repo" && agy --prompt-interactive "Commit these changes"');
    });

    it("generates correct CLI invocation for Claude Code (claude)", () => {
      const cmd = getAgentCommitCliCommand("claude", "Commit these changes", "/path/to/repo");
      expect(cmd).toBe('cd "/path/to/repo" && claude -- "Commit these changes"');
    });

    it("handles command generation without repoPath", () => {
      const cmdAgy = getAgentCommitCliCommand("agy", "Prompt text");
      expect(cmdAgy).toBe('agy --prompt-interactive "Prompt text"');

      const cmdClaude = getAgentCommitCliCommand("claude", "Prompt text");
      expect(cmdClaude).toBe('claude -- "Prompt text"');
    });
  });

  describe("getAgentCommitSdkSnippet", () => {
    it("generates Google Antigravity SDK Python snippet for agy", () => {
      const snippet = getAgentCommitSdkSnippet("agy", "Commit prompt", "/path/to/repo");
      expect(snippet).toContain("from google_antigravity import Agent, LocalAgentConfig, Conversation");
      expect(snippet).toContain('os.chdir("/path/to/repo")');
      expect(snippet).toContain('agent_behavior="autonomous"');
      expect(snippet).toContain('response = conversation.send_message(');
    });

    it("generates Claude CLI execution snippet for claude", () => {
      const snippet = getAgentCommitSdkSnippet("claude", "Commit prompt", "/path/to/repo");
      expect(snippet).toContain('subprocess.run(');
      expect(snippet).toContain('["claude", "--", prompt]');
      expect(snippet).toContain('cwd="/path/to/repo"');
    });
  });

  describe("launchAgentCommit", () => {
    it("refuses when repoPath is empty", async () => {
      const openTerminal = vi.fn();
      const res = await launchAgentCommit(
        { repoPath: "", launcher: "agy", prompt: "Commit" },
        openTerminal,
      );
      expect(res.ok).toBe(false);
      expect(res.error).toBe(AGENT_COMMIT_NO_REPO);
      expect(openTerminal).not.toHaveBeenCalled();
    });

    it("refuses when launcher is unsupported", async () => {
      const openTerminal = vi.fn();
      const res = await launchAgentCommit(
        { repoPath: "/repo", launcher: "invalid" as unknown as PromptLauncher, prompt: "Commit" },
        openTerminal,
      );
      expect(res.ok).toBe(false);
      expect(res.error).toBe(AGENT_COMMIT_UNSUPPORTED_LAUNCHER);
      expect(openTerminal).not.toHaveBeenCalled();
    });

    it("refuses when abort signal is already aborted", async () => {
      const openTerminal = vi.fn();
      const controller = new AbortController();
      controller.abort();
      const res = await launchAgentCommit(
        { repoPath: "/repo", launcher: "agy", prompt: "Commit", signal: controller.signal },
        openTerminal,
      );
      expect(res.ok).toBe(false);
      expect(res.error).toBe("Terminal launch cancelled");
      expect(openTerminal).not.toHaveBeenCalled();
    });

    it("successfully launches terminal session and opens dock", async () => {
      const openTerminal = vi.fn();
      const requestSpy = vi
        .spyOn(terminalLaunchRequests, "request")
        .mockResolvedValueOnce();

      const res = await launchAgentCommit(
        { repoPath: "/repo", launcher: "agy", prompt: "Commit prompt" },
        openTerminal,
      );

      expect(res.ok).toBe(true);
      expect(res.notice).toContain("Antigravity session opened in terminal");
      expect(openTerminal).toHaveBeenCalledWith(true);
      expect(requestSpy).toHaveBeenCalledWith("/repo", "agy", "Commit prompt", undefined);

      requestSpy.mockRestore();
    });

    it("returns error message when terminalLaunchRequests fails", async () => {
      const openTerminal = vi.fn();
      const requestSpy = vi
        .spyOn(terminalLaunchRequests, "request")
        .mockRejectedValueOnce(new Error("Capacity exceeded (max 32 sessions)"));

      const res = await launchAgentCommit(
        { repoPath: "/repo", launcher: "claude", prompt: "Commit prompt" },
        openTerminal,
      );

      expect(res.ok).toBe(false);
      expect(res.error).toContain("Capacity exceeded");
      expect(openTerminal).toHaveBeenCalledWith(true);

      requestSpy.mockRestore();
    });
  });
});
