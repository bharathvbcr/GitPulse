import { spawnSync } from "node:child_process";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  realpathSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
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

  /**
   * The copy-paste command is judged by a real shell, not by its string shape.
   *
   * This used to assert `cd "/repo/$(evil)"` under the name "prevents shell
   * injection" — but double quotes stop neither `$(...)` nor backticks, so
   * that exact string runs `evil` in every POSIX shell. The only honest
   * oracle is to run the command: a fake `agy`/`claude` on PATH records the
   * argv it was handed, the repository directory itself carries a hostile
   * name, and any payload that executes leaves a marker file behind.
   */
  describe.skipIf(process.platform === "win32")("copy-paste CLI command under a real shell", () => {
    const shells = ["/bin/sh", "/bin/bash", "/bin/zsh"].filter((shell) => existsSync(shell));
    /**
     * Wall-clock allowance for one case: a fresh `shell -c` plus the fake
     * agent it execs, run serially with `spawnSync`. These tests' cost is
     * process spawns, not CPU, so it scales with whatever else the host is
     * running and vitest's flat 5s default is the wrong budget: the 300-case
     * fuzz took 2.3s alone and 3.4-4.5s beside three parallel full suites, and
     * timed out in one full run. Every case still runs in its own shell —
     * batching them would let one case's leaked quoting judge the next.
     * Measured 2026-10-05 on an 18-core host: about 7ms per case at p50 alone;
     * 12ms at p50 and 108ms at worst beside two parallel `npx vitest run`.
     * This is an average allowance summed over the corpus, about 8x the loaded
     * p50, not a per-case ceiling: that ceiling is each spawn's own 10s
     * timeout, which still fails a hung case by name. Each test's budget is
     * this times its case count, so it moves with the corpus.
     */
    const SHELL_CASE_BUDGET_MS = 100;
    const FUZZ_CASES_PER_SHELL = 100;
    let sandbox = "";
    let bin = "";

    beforeAll(() => {
      sandbox = mkdtempSync(join(tmpdir(), "gp-agent-cli-"));
      bin = join(sandbox, "bin");
      mkdirSync(bin);
      for (const name of ["agy", "claude"]) {
        const script = join(bin, name);
        // NUL-separated argv, plus the directory the shell actually reached.
        writeFileSync(
          script,
          '#!/bin/sh\nout="$GP_ARGV_OUT"\npwd > "$out.cwd"\nprintf \'%s\\0\' "$@" > "$out"\n',
        );
        chmodSync(script, 0o755);
      }
    });

    afterAll(() => {
      if (sandbox) rmSync(sandbox, { recursive: true, force: true });
    });

    const marker = () => join(sandbox, "PWNED");
    const payloads = (): string[] => [
      `$(touch ${marker()})`,
      `\`touch ${marker()}\``,
      `"; touch ${marker()}; echo "`,
      `'; touch ${marker()}; echo '`,
      `' && touch ${marker()} && echo '`,
      `test\ntouch ${marker()}\n`,
      `test\r\ntouch ${marker()}`,
      `hello" && touch ${marker()} && echo "`,
      `""$IFS$9touch$IFS${marker()}`,
      `| touch ${marker()}`,
      `& touch ${marker()}`,
      `$'\\x27'; touch ${marker()}`,
      `!!; touch ${marker()}`,
      "it's a 'quoted' \\ backslash",
      "-leading-dash --flag",
      "",
      "unicode 🚀 ünïcödé \u202e rtl",
    ];

    function run(shell: string, command: string) {
      const out = join(sandbox, "argv");
      rmSync(out, { force: true });
      rmSync(`${out}.cwd`, { force: true });
      const result = spawnSync(shell, ["-c", command], {
        cwd: sandbox,
        env: { PATH: `${bin}:/usr/bin:/bin`, GP_ARGV_OUT: out, HOME: sandbox },
        encoding: "utf8",
        timeout: 10_000,
      });
      const argv = existsSync(out) ? readFileSync(out, "utf8").split("\0").slice(0, -1) : null;
      const cwd = existsSync(`${out}.cwd`) ? readFileSync(`${out}.cwd`, "utf8").trim() : null;
      // A spawn killed by its timeout has a null status and often no stderr;
      // name why it ended so that failure does not read as a quoting bug.
      const ended = result.error ? `${result.error.message}; ` : result.signal ? `signal ${result.signal}; ` : "";
      return { status: result.status, stderr: `${ended}${result.stderr}`, argv, cwd };
    }

    it("hands every hostile prompt to the agent byte-for-byte and executes none of it", {
      timeout: shells.length * payloads().length * 2 * SHELL_CASE_BUDGET_MS,
    }, () => {
      expect(shells.length).toBeGreaterThan(0);
      const repo = join(sandbox, `repo $(touch ${marker()}) \`touch ${marker()}\` it's`);
      mkdirSync(repo, { recursive: true });
      const realRepo = realpathSync(repo);
      let runs = 0;
      for (const shell of shells) {
        for (const prompt of payloads()) {
          for (const launcher of ["agy", "claude"] as const) {
            const label = `${shell} ${launcher} ${JSON.stringify(prompt)}`;
            const outcome = run(shell, getAgentCommitCliCommand(launcher, prompt, repo));
            runs += 1;
            expect(outcome.status, `${label}: ${outcome.stderr}`).toBe(0);
            const expected = launcher === "agy" ? ["--prompt-interactive", prompt] : ["--", prompt];
            expect(outcome.argv, label).toEqual(expected);
            expect(outcome.cwd && realpathSync(outcome.cwd), label).toBe(realRepo);
            expect(existsSync(marker()), `${label} executed its payload`).toBe(false);
          }
        }
      }
      expect(runs).toBe(shells.length * payloads().length * 2);
    });

    it("survives a seeded fuzz of shell metacharacters without executing or altering a byte", {
      timeout: shells.length * FUZZ_CASES_PER_SHELL * SHELL_CASE_BUDGET_MS,
    }, () => {
      const alphabet = [..."'\"$`\\!;&|<>(){}[]*?~#%^=,. \t\n\rab-_/:@", "🚀", "é", "\u202e"];
      let seed = 0x9e3779b9;
      const next = () => {
        seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0;
        return seed;
      };
      const repo = join(sandbox, "fuzz repo 'x' $(y)");
      mkdirSync(repo, { recursive: true });
      let runs = 0;
      for (const shell of shells) {
        for (let i = 0; i < FUZZ_CASES_PER_SHELL; i += 1) {
          const length = next() % 40;
          let prompt = "";
          for (let c = 0; c < length; c += 1) prompt += alphabet[next() % alphabet.length];
          const launcher = i % 2 === 0 ? "agy" : "claude";
          const label = `${shell} ${JSON.stringify(prompt)}`;
          const outcome = run(shell, getAgentCommitCliCommand(launcher, prompt, repo));
          runs += 1;
          expect(outcome.status, `${label}: ${outcome.stderr}`).toBe(0);
          expect(outcome.argv?.at(-1), label).toBe(prompt);
          expect(existsSync(marker()), label).toBe(false);
        }
      }
      expect(runs).toBe(shells.length * FUZZ_CASES_PER_SHELL);
    });
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
