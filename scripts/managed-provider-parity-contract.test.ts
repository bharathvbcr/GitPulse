import { execFileSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

import { MANAGED_PROVIDERS, PROVIDER_CHOICES, supportsManaged } from "../src/lib/workbench/taskHandoff";
import type { AgentProvider } from "../src/lib/workbench/vocabulary";

/**
 * Which providers GitPulse will run in the *managed* lane is decided in three
 * places, in three languages: the renderer that offers the choice, the Rust
 * workbench that prepares and launches the attempt, and — when the harness is
 * checked out beside this repository — the Go adapter set that has to speak the
 * provider's protocol.
 *
 * They are not redundant copies that could be collapsed. Each one is the gate
 * for a different process, and a disagreement between them is a specific,
 * silent failure:
 *
 *   - renderer wider than Rust → the option is offered and the launch is
 *     refused, with no way for the reader to know that was inevitable.
 *   - Rust wider than the harness → the attempt is stored and claimed, then
 *     dies at `prepare` with the repository capacity already taken.
 *   - harness wider than Rust → an adapter nobody can reach.
 *
 * So the lists are checked against each other rather than trusted, and the
 * harness arm degrades to a skip with a named reason when the sibling checkout
 * is absent, instead of quietly passing as if it had been compared.
 */
const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");

/** Providers the Rust workbench admits, read from the function that decides. */
function rustManagedProviders(): string[] {
  const source = readFileSync(path.join(ROOT, "src-tauri/src/workbench/terminal_command.rs"), "utf8");
  const fn = source.slice(source.indexOf("fn is_managed_provider"));
  const body = fn.slice(0, fn.indexOf("\n}"));
  const match = body.match(/matches!\(\s*provider\s*,([^)]*)\)/);
  if (!match) throw new Error("is_managed_provider no longer uses a matches! pattern this test can read");
  return [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]).sort();
}

/** Providers the harness has an adapter for, read from its own exported list. */
function harnessManagedProviders(): string[] | null {
  const source = path.join(ROOT, "..", "Manvi", "manvi", "codingagent", "codex.go");
  if (!existsSync(source)) return null;
  const text = readFileSync(source, "utf8");
  const match = text.match(/var ManagedProviders = \[\]string\{([^}]*)\}/);
  if (!match) throw new Error("codingagent.ManagedProviders is no longer a literal slice this test can read");
  return [...match[1].matchAll(/"([^"]+)"/g)].map((m) => m[1]).sort();
}

/**
 * The adapter set the *installed* harness publishes at handshake, or a named
 * reason it could not be asked.
 *
 * The four arms above all read source. Source parity is necessary and was not
 * sufficient: a machine whose `manvi` binary predated the Claude adapter passed
 * every one of them and still could not run a managed Claude attempt. It
 * offered the lane, stored the attempt, took the repository's one run slot, and
 * refused with a sentence written for the providers that build did have —
 * "this managed adapter requires a fresh Codex attempt" — at a reader who had
 * selected Claude Code.
 *
 * `ops` cannot answer this. `work.runs.managed.prepare` is registered whenever
 * a managed runner exists, so a codex-only build advertises exactly the same
 * operation as a codex+claude one. Only the adapter set separates them, which
 * is why the handshake carries it.
 */
function installedManagedProviders(): { providers: string[] } | { skipped: string } {
  const explicit = process.env.GITPULSE_MANVI_BIN;
  if (explicit !== undefined && !existsSync(explicit)) {
    // A path somebody typed is a statement; do not search past a typo, for the
    // same reason the host does not.
    return { skipped: `GITPULSE_MANVI_BIN is set to ${explicit}, which is not a file` };
  }
  let binary = explicit;
  if (!binary) {
    try {
      binary = execFileSync(process.platform === "win32" ? "where" : "which", ["manvi"], {
        encoding: "utf8",
      }).split("\n")[0]?.trim();
    } catch {
      binary = undefined;
    }
  }
  if (!binary) return { skipped: "no `manvi` binary is installed on this machine" };

  // A profile database is what enables the workbench module, and with it the
  // managed lane; without one the harness configures no managed runner and
  // would truthfully report no adapter set at all. The file is never created:
  // `hello` is answered before any store call, so this costs one short-lived
  // child and touches nothing.
  const profile = path.join(mkdtempSync(path.join(tmpdir(), "gp-parity-")), "profile.sqlite");
  let line: string;
  try {
    line = execFileSync(binary, ["serve", "--workbench-db", profile], {
      input: `${JSON.stringify({ id: "1", op: "hello", params: { protocol: 1, host: "gitpulse" } })}\n`,
      encoding: "utf8",
      timeout: 30_000,
      env: { ...process.env, MANVI_HARNESS_INIT_ENABLED: "false" },
    }).split("\n")[0] ?? "";
  } catch (cause) {
    return { skipped: `\`${binary} serve\` could not be handshaken: ${String(cause)}` };
  }
  const hello = JSON.parse(line)?.result;
  if (!hello?.ops?.includes("work.runs.managed.prepare")) {
    return { skipped: `${binary} serves no managed lane, so it has no adapter set to compare` };
  }
  if (hello.managed_providers === undefined) {
    return {
      skipped:
        `${binary} advertises a managed lane but publishes no adapter set, so which providers it ` +
        `can actually drive is unverifiable from here — this is the exact condition that made a ` +
        `stale harness refuse a Claude launch in Codex's words. Rebuild and install Manvi.`,
    };
  }
  return { providers: [...(hello.managed_providers as string[])].sort() };
}

describe("managed provider parity", () => {
  const renderer: AgentProvider[] = [...MANAGED_PROVIDERS].sort();

  it("offers a managed lane for Codex and Claude Code, and for nothing else", () => {
    expect(renderer).toEqual(["claude", "codex"]);
    for (const provider of PROVIDER_CHOICES) {
      expect(supportsManaged(provider)).toBe(renderer.includes(provider));
    }
  });

  it("agrees with the Rust workbench about who may be launched", () => {
    expect(rustManagedProviders()).toEqual(renderer);
  });

  it("never offers a managed lane to a provider with no terminal lane either", () => {
    // A managed provider is always also a terminal provider: the terminal lane
    // is the fallback every launch path can reach, so a provider that only had
    // a managed one would have no way to recover from a harness failure.
    const terminal = readFileSync(path.join(ROOT, "src-tauri/src/workbench/terminal_command.rs"), "utf8");
    const fn = terminal.slice(terminal.indexOf("fn is_terminal_provider"));
    const body = fn.slice(0, fn.indexOf("\n}"));
    const terminalProviders = [...body.matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    for (const provider of renderer) {
      expect(terminalProviders).toContain(provider);
    }
  });

  it("leaves no wire guard with its own transcribed provider list", () => {
    // The fourth decision site, and the one that drifted. `launchManagedRun`
    // validated the launch reply with `run.provider === "codex"` written out
    // by hand, so when Claude Code gained an adapter every managed Claude
    // launch came back "Task storage returned an invalid response" — a
    // protocol error naming a cause that was not true, from a lane that had
    // been opened everywhere else. Three whole-process gates agreeing is worth
    // nothing if a response parser quietly holds a fourth opinion.
    //
    // Whole-word, so `MANAGED_PROVIDERS` and `supportsManaged` do not match
    // and the fixtures' own provider values are not mistaken for a gate.
    const client = readFileSync(path.join(ROOT, "src/lib/workbench/client.ts"), "utf8");
    for (const line of client.split("\n")) {
      if (!/\bprovider\b/.test(line)) continue;
      expect(line, "client.ts compares a provider to a literal instead of asking supportsManaged")
        .not.toMatch(/\bprovider\b[^\n]*(===|!==)\s*"(codex|claude|grok|agy)"/);
    }
    expect(client).toContain("supportsManaged(run.provider)");
  });

  it("agrees with the adapter set the installed harness actually publishes", () => {
    const installed = installedManagedProviders();
    if ("skipped" in installed) {
      // Not a pass, and deliberately loud. Every source arm can agree while
      // the binary on this machine disagrees with all of them, so a run that
      // could not reach the binary has established nothing about it and says
      // which of the reachable states it was in instead of staying quiet.
      expect(installed.skipped).toMatch(
        /no `manvi` binary|not a file|could not be handshaken|serves no managed lane|publishes no adapter set/,
      );
      console.warn(`managed provider parity: installed-harness arm did not run — ${installed.skipped}`);
      return;
    }
    expect(installed.providers).toEqual(renderer);
  });

  it("agrees with the harness's adapter set, or says it could not look", () => {
    const harness = harnessManagedProviders();
    if (harness === null) {
      // Not a pass. The sibling checkout is absent, so this comparison did not
      // run, and saying so is the whole point — a check that could not run must
      // never read like one that ran and agreed.
      expect(existsSync(path.join(ROOT, "..", "Manvi"))).toBe(false);
      return;
    }
    expect(harness).toEqual(renderer);
  });
});
