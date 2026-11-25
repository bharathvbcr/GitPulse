import { spawnSync } from "node:child_process";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { GATES } from "./ci-local.mjs";
import {
  INSTALL_HINT,
  checkoutsPersistingCredentials,
  installActionsWithoutTool,
  hasWorkflowLevelPermissions,
  interpret,
  pipedDownloads,
  unpinnedActions,
  workflowFiles,
  workflowPolicyFindings,
  workflowsMissingPermissions,
  RELEASE_GATE_ALLOWLIST,
  gateFindings,
  releaseGateFindings,
  trustedWorkflowNames,
} from "./check-workflows.mjs";

type ReleaseGateAllowEntry = { workflow: string; job: string; step?: string; key: string; reason: string };

describe("check:workflows", () => {
  it("finds the repository's workflow files", () => {
    // `fileURLToPath`, never `URL.pathname`: on Windows the latter yields
    // "/D:/a/..." and every fs call against it misses or doubles the drive.
    const files = workflowFiles(fileURLToPath(new URL("../.github/workflows", import.meta.url)));
    expect(files).toContain("ci.yml");
    expect(files).toContain("coverage.yml");
    expect(files).toContain("release.yml");
  });

  it("reports a missing directory as empty rather than throwing", () => {
    expect(workflowFiles("/nonexistent/workflows")).toEqual([]);
  });

  it("separates 'actionlint is absent' from 'workflows are faulty'", () => {
    const enoent = Object.assign(new Error("spawn actionlint ENOENT"), { code: "ENOENT" });
    // A checker that could not run must never look like a checker that passed,
    // and must be distinguishable from one that ran and found problems.
    expect(interpret({ status: null, error: enoent })).toEqual({ code: 2, message: INSTALL_HINT });
    expect(interpret({ status: 1 }).code).toBe(1);
    expect(interpret({ status: 0 })).toEqual({ code: 0, message: null });
  });

  it("treats actionlint's own failure codes as a check that could not run", () => {
    // actionlint exits 2 for bad options and 3 for a fatal error; neither is a
    // verdict about the workflows.
    expect(interpret({ status: 2 }).code).toBe(2);
    expect(interpret({ status: 3 }).code).toBe(2);
  });

  it("requires a workflow-level permissions block so GITHUB_TOKEN is fail-closed", () => {
    expect(hasWorkflowLevelPermissions("on: push\njobs:\n  a:\n    runs-on: ubuntu-latest\n")).toBe(
      false,
    );
    expect(
      hasWorkflowLevelPermissions(
        "on: push\npermissions:\n  contents: read\njobs:\n  a:\n    runs-on: ubuntu-latest\n",
      ),
    ).toBe(true);
    // A job-only map is not enough: GitHub's default token still applies to
    // every other job, which is the finding this check exists to keep closed.
    expect(
      hasWorkflowLevelPermissions("on: push\njobs:\n  a:\n    permissions:\n      contents: read\n"),
    ).toBe(false);

    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    expect(workflowsMissingPermissions(dir)).toEqual([]);
  });

  it("refuses a third-party action that is not pinned to a commit", () => {
    const yaml = [
      "jobs:",
      "  a:",
      "    steps:",
      "      - uses: ./.github/actions/local",
      "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7",
      "      - name: Tagged",
      "        uses: oven-sh/setup-bun@v2",
      "      - name: Branch",
      "        uses: dtolnay/rust-toolchain@stable",
      "      - uses: docker://alpine:3.20",
      "      - uses: owner/short@3d3c42e",
    ].join("\n");
    // A local action has no ref to move, and a docker image is not an action
    // ref; a tag, a branch and an abbreviated SHA can all be repointed.
    expect(unpinnedActions(yaml)).toEqual([
      "oven-sh/setup-bun@v2",
      "dtolnay/rust-toolchain@stable",
      "owner/short@3d3c42e",
    ]);
  });

  it("refuses an install-action step that names no tool", () => {
    const yaml = [
      "jobs:",
      "  a:",
      "    steps:",
      "      - name: Bare release pin",
      "        uses: taiki-e/install-action@8cca0918f8471c33515202b73f56ab27f2bed283 # v2",
      "      - name: Named",
      "        uses: taiki-e/install-action@8cca0918f8471c33515202b73f56ab27f2bed283 # v2",
      "        with:",
      "          tool: cargo-nextest@0.9.150",
      "      - name: Empty tool",
      "        uses: taiki-e/install-action@8cca0918f8471c33515202b73f56ab27f2bed283 # v2",
      "        with:",
      "          tool:",
      "      - run: echo tool: cargo-llvm-cov",
    ].join("\n");
    // A `tool:` in another step, or one with no value, names nothing for this one.
    expect(installActionsWithoutTool(yaml)).toEqual([0, 2]);
  });

  it("refuses a checkout that leaves the token in .git/config", () => {
    const yaml = [
      "jobs:",
      "  a:",
      "    steps:",
      "      - name: Kept",
      "        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7",
      "      - name: Dropped",
      "        uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7",
      "        with:",
      "          fetch-depth: 0",
      "          persist-credentials: false",
      "      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7",
      "        with:",
      "          persist-credentials: true",
      "      - name: Next",
      "        run: echo persist-credentials: false",
    ].join("\n");
    // Only the checkout's own `with:` counts: a later step mentioning the key,
    // or the key set to true, does not drop the credential.
    expect(checkoutsPersistingCredentials(yaml)).toEqual([0, 2]);
  });

  it("holds every workflow in this repository to every rule", () => {
    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    expect(workflowPolicyFindings(dir)).toEqual([]);
  });

  it("finds a download piped or process-substituted into a shell, and ignores comments", () => {
    const yaml = [
      "        run: bash <(curl -fsSL https://example.com/install.sh) 1.0",
      "        run: curl -fsSL https://example.com/install.sh | sudo bash",
      "        run: wget -qO- https://example.com/install.sh | sh -s --",
      "        run: sh <(wget -qO- https://example.com/install.sh)",
      "        # run: bash <(curl -fsSL https://example.com/install.sh)",
      "        run: curl -fsSL -o tool.tgz https://example.com/tool.tgz",
      "        run: echo abc | sha256sum -c -",
    ].join("\n");
    expect(pipedDownloads(yaml)).toHaveLength(4);
  });

  it("installs actionlint from a tarball checked against a pinned SHA-256, not a script piped into bash", () => {
    const ci = readFileSync(fileURLToPath(new URL("../.github/workflows/ci.yml", import.meta.url)), "utf8");
    // v1.7.12's linux_amd64 tarball; the digest is in the release's
    // checksums.txt and in GitHub's asset metadata.
    expect(ci).toContain("8aca8db96f1b94770f1b0d72b6dddcb1ebb8123cb3712530b08cc387b349a3d8  actionlint_1.7.12_linux_amd64.tar.gz");
    expect(ci).toContain("sha256sum -c -");
    expect(ci).toContain("tar -xzf actionlint_1.7.12_linux_amd64.tar.gz actionlint");
    // ci:local matches the lint step by this exact command.
    expect(ci).toContain("run: ./actionlint -color");
    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    for (const name of workflowFiles(dir)) {
      expect(pipedDownloads(readFileSync(`${dir}/${name}`, "utf8")), name).toEqual([]);
    }
  });

  it("prints the rust coverage summary without rerunning the tests", () => {
    const source = readFileSync(
      fileURLToPath(new URL("../.github/workflows/coverage.yml", import.meta.url)),
      "utf8",
    );
    const summary = source
      .split("\n")
      .find((line) => line.includes("--summary-only"));
    expect(summary, source).toBeDefined();
    expect(summary).toContain("--no-run");
  });

  it("caps llvm-cov test threads so instrumented suites do not starve pipe and sidecar fixtures", () => {
    const coverage = readFileSync(
      fileURLToPath(new URL("../.github/workflows/coverage.yml", import.meta.url)),
      "utf8",
    );
    const lcov = coverage
      .split("\n")
      .find((line) => line.includes("--lcov") && line.includes("output-path lcov.info"));
    expect(lcov, coverage).toBeDefined();
    expect(lcov).toContain("--test-threads=1");
    expect(lcov).not.toContain("--no-run");
    // `ci:local` used to spell its whole chain in package.json, so the cap was
    // asserted as a substring of that field. It now delegates to a runner and
    // the flag travels with the gate that needs it. The invariant is that the
    // local llvm-cov run caps threads, not that package.json is where that is
    // written — so follow it to its owner and check the command itself, which
    // is a stronger assertion than the string this replaces.
    const pkg = JSON.parse(
      readFileSync(fileURLToPath(new URL("../package.json", import.meta.url)), "utf8"),
    );
    expect(pkg.scripts["ci:local"]).toContain("scripts/ci-local.mjs");
    const local = GATES.find((gate) => gate.args.includes("llvm-cov"));
    expect(local, "ci:local no longer runs llvm-cov").toBeDefined();
    const command = [local!.program, ...local!.args].join(" ");
    // 5394f4d3 moved the local coverage run onto nextest, where every test is
    // its own process and the cap is nextest's process count, kept in
    // src-tauri/.config/nextest.toml (four, measured there and in the gate's
    // `why`). A `--test-threads=1` in the command would now be the wrong cap,
    // so the invariant is checked where the cap lives: nextest runs the suite,
    // nothing on the command line overrides the config, and the config bounds
    // it to a handful rather than nextest's one-per-core default.
    expect(command).toContain("llvm-cov nextest");
    expect(command).not.toMatch(/--test-threads|-j\b|NEXTEST_TEST_THREADS/);
    const nextestConfig = readFileSync(
      fileURLToPath(new URL("../src-tauri/.config/nextest.toml", import.meta.url)),
      "utf8",
    );
    const cap = /^\[profile\.default\][^[]*?^test-threads\s*=\s*(\d+)\s*$/m.exec(nextestConfig);
    expect(cap, "nextest.toml no longer caps [profile.default] test-threads").not.toBeNull();
    const processes = Number(cap![1]);
    expect(processes).toBeGreaterThanOrEqual(1);
    expect(processes).toBeLessThanOrEqual(4);
  });

  it("caps uninstrumented cargo test threads so sidecar hello fixtures are not starved", () => {
    for (const name of ["ci.yml", "release.yml"]) {
      const source = readFileSync(
        fileURLToPath(new URL(`../.github/workflows/${name}`, import.meta.url)),
        "utf8",
      );
      const lines = source.split("\n").filter((line) => line.includes("cargo test --manifest-path"));
      expect(lines.length, name).toBeGreaterThan(0);
      for (const line of lines) {
        expect(line, name).toContain("--test-threads=1");
      }
    }
  });

  it("runs the #[ignore]d descriptor-inheritance burst in its own step and fails when it collects nothing", () => {
    const ci = readFileSync(
      fileURLToPath(new URL("../.github/workflows/ci.yml", import.meta.url)),
      "utf8",
    );
    const test = "procguard::tests::concurrent_spawns_never_inherit_a_siblings_descriptors";
    // The test is opt-in, so no other line of ci.yml runs it: deleting this
    // step would silently stop checking descriptor inheritance.
    const runs = ci.split("\n").filter((line) => line.includes(`--ignored --exact ${test}`));
    expect(runs.length, "no ci.yml line runs the ignored test").toBeGreaterThan(0);
    for (const line of runs) {
      expect(line).toContain("cargo test --manifest-path");
      expect(line).toContain("--test-threads=1");
    }
    // "0 tests" is a green cargo exit: only the count makes a rename fail.
    expect(ci).toContain('grep -q "test result: ok. 1 passed"');
    expect(ci).toMatch(/Descriptor inheritance under concurrent spawns[\s\S]*?runner\.os != 'Windows'/);
  });

  it("measures Rust branch coverage, on a pinned nightly because the flag is unstable", () => {
    const coverage = readFileSync(
      fileURLToPath(new URL("../.github/workflows/coverage.yml", import.meta.url)),
      "utf8",
    );
    for (const line of coverage.split("\n").filter((line) => line.includes("cargo llvm-cov") && line.includes("--manifest-path"))) {
      expect(line).toContain("--branch");
    }
    expect(coverage).toMatch(/toolchain:\s*nightly-\d{4}-\d{2}-\d{2}/);
  });

  // The MANVI gate tests drive a real `manvi`; no runner has one, so they failed
  // with "No MANVI harness is installed" on every job that runs the lib suite.
  it("builds the pinned manvi before every job that runs the lib suite", () => {
    const read = (name: string) =>
      readFileSync(fileURLToPath(new URL(`../.github/workflows/${name}`, import.meta.url)), "utf8");
    const jobsOf = (text: string) => {
      const body = text.slice(text.search(/^jobs:\s*$/m));
      const jobs = new Map<string, string>();
      const heads = [...body.matchAll(/^ {2}([a-z0-9-]+):\s*$/gm)];
      heads.forEach((head, i) => jobs.set(head[1], body.slice(head.index, heads[i + 1]?.index ?? body.length)));
      return jobs;
    };
    const suites: Record<string, { jobs: string[]; testStep: RegExp }> = {
      "ci.yml": { jobs: ["rust-test", "rust-test-macos"], testStep: /- name: Rust (Unit & Integration Tests|Tests)/ },
      "coverage.yml": { jobs: ["coverage"], testStep: /- name: Generate Rust Coverage/ },
      "test-parallel.yml": { jobs: ["rust-parallel"], testStep: /cargo test --manifest-path/ },
    };
    for (const [file, { jobs, testStep }] of Object.entries(suites)) {
      const text = read(file);
      expect(text, file).toMatch(/^ {2}MANVI_REV: [0-9a-f]{40}$/m);
      const all = jobsOf(text);
      for (const name of jobs) {
        const job = all.get(name);
        expect(job, `${file} ${name}`).toBeDefined();
        const checkout = job!.indexOf('git -C "$src" checkout --quiet "$MANVI_REV"');
        const install = job!.indexOf("- name: Install manvi");
        const test = job!.search(testStep);
        expect(checkout, `${file} ${name} checks Manvi out at MANVI_REV`).toBeGreaterThan(-1);
        expect(install, `${file} ${name} installs manvi`).toBeGreaterThan(checkout);
        expect(test, `${file} ${name} runs its tests after manvi`).toBeGreaterThan(install);
        // The pins Manvi's own commit names, never whatever sibling happens to exist.
        expect(job!.slice(install, test)).toContain('bash "$src/scripts/fetch-modules.sh"');
        // The built binary's directory reaches GITHUB_PATH on one line...
        const steps = job!.slice(install, test);
        expect(steps).toMatch(/"\$root\/bin"[^\n]*>> "\$GITHUB_PATH"/);
        // ...and on a Windows runner in its native spelling: the Rust test
        // step's own PATH walk drops a `/c/...` entry as not absolute.
        if (job!.includes("windows-latest")) {
          expect(steps, `${file} ${name} puts manvi on PATH in Windows spelling`).toContain('cygpath -w "$root/bin"');
        }
      }
    }
  });

  // Two ledger tests hold the write lock from the real `sqlite3` CLI, which the
  // Windows image lacks. The Chocolatey feed answered 504 once, so a single
  // `choco install` is not enough: the step must retry and then fail loudly.
  it("puts sqlite3 on PATH on the Windows Rust leg, retrying a failed install and failing after the last", () => {
    const text = readFileSync(fileURLToPath(new URL("../.github/workflows/ci.yml", import.meta.url)), "utf8");
    const body = text.slice(text.search(/^jobs:\s*$/m));
    const start = body.search(/^ {2}rust-test:\s*$/m);
    const rest = body.slice(start + 1);
    const job = body.slice(start, start + 1 + Math.max(rest.search(/^ {2}[a-z0-9-]+:\s*$/m), 0));
    expect(job).toContain("windows-latest");
    const step = job.indexOf("- name: Install sqlite3 (Windows)");
    const test = job.indexOf("- name: Rust Unit & Integration Tests");
    expect(step, "rust-test installs sqlite3").toBeGreaterThan(-1);
    expect(test, "rust-test runs its tests after sqlite3").toBeGreaterThan(step);
    // A step that fails skips every later one, so a feed outage here must not
    // take the manvi build down with it.
    expect(step, "sqlite3 comes after the manvi build").toBeGreaterThan(job.indexOf("- name: Keep the manvi build"));
    const sqlite = job.slice(step, test).split(/^ {6}- name: /m)[0];
    expect(sqlite).toContain("if: ${{ runner.os == 'Windows' }}");
    expect(sqlite).toContain("choco install sqlite");
    expect(sqlite, "retries").toMatch(/for \(\$i = 1; \$i -le \$attempts; \$i\+\+\)/);
    expect(sqlite, "bounded").toMatch(/\$attempts = \d+/);
    expect(sqlite, "verifies the CLI answers").toContain("sqlite3 -version");
    expect(sqlite, "fails loudly after the last attempt").toMatch(/throw "sqlite3 could not be installed/);
  });

  it("names the tool on every taiki-e/install-action step, so a re-pinned ref still installs it", () => {
    // A per-tool ref (`@cargo-llvm-cov`) carries the tool in its name; a pinned
    // v2 commit installs only what `tool:` lists. #28 re-pinned coverage's step
    // from the one to the other with no `tool:`, and `cargo llvm-cov` then
    // failed with "no such command" on every run.
    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    let steps = 0;
    for (const name of workflowFiles(dir)) {
      const lines = readFileSync(`${dir}/${name}`, "utf8").split("\n");
      lines.forEach((line, index) => {
        if (!/^\s*uses:\s*taiki-e\/install-action@/.test(line)) return;
        steps += 1;
        const indent = line.search(/\S/);
        const block: string[] = [];
        for (const next of lines.slice(index + 1)) {
          if (next.trim() !== "" && next.search(/\S/) <= indent - 2) break;
          block.push(next);
        }
        expect(block.join("\n"), `${name}:${index + 1}`).toMatch(/^\s*with:\s*\n\s*tool:\s*\S+@\S+/m);
      });
    }
    expect(steps).toBeGreaterThan(0);
  });

  it("keeps the serial gate and also runs the suite in parallel, in its own workflow", () => {
    const read = (name: string) =>
      readFileSync(fileURLToPath(new URL(`../.github/workflows/${name}`, import.meta.url)), "utf8");
    // The gate stays serial: the previous test asserts every ci.yml line does.
    expect(read("ci.yml")).toContain("--test-threads=1");
    const parallel = read("test-parallel.yml");
    const lines = parallel.split("\n").filter((line) => line.includes("cargo test --manifest-path"));
    expect(lines.length).toBeGreaterThan(0);
    for (const line of lines) {
      expect(line).toContain("--test-threads=8");
      expect(line).not.toContain("--test-threads=1");
    }
    // Often enough to catch a parallel-only flake the day it lands, not at release.
    expect(parallel).toMatch(/^\s*push:\s*\n\s*branches:\s*\[main\]/m);
    expect(parallel).toMatch(/^\s*schedule:/m);
  });

  it("scopes rust-cache to the runner image so native artifacts are not reused across MSVC upgrades", () => {
    for (const name of ["ci.yml", "coverage.yml", "release.yml"]) {
      const source = readFileSync(
        fileURLToPath(new URL(`../.github/workflows/${name}`, import.meta.url)),
        "utf8",
      );
      // Every workflow pins the action to the commit v2 resolved to.
      const caches = source.split("uses: Swatinem/rust-cache@d1527c8f67ef9af357290475d4af4b0fab8f12e0");
      expect(caches.length, name).toBeGreaterThan(1);
      for (const block of caches.slice(1)) {
        // ImageOS is a runner process env var, not a workflow `env` key, so
        // interpolating it here is always the empty string.
        expect(block, name).not.toMatch(/prefix-key:\s*\$\{\{\s*env\.ImageOS\s*\}\}/);
        expect(block, name).toContain("prefix-key: ${{ env.RUST_CACHE_PREFIX }}");
      }
      const exports = source.split("Key the Rust cache to this runner image");
      expect(exports.length, name).toBe(caches.length);
      for (const block of exports.slice(1)) {
        expect(block, name).toContain('RUST_CACHE_PREFIX=${ImageOS:-unknown}-${ImageVersion:-unknown}');
        expect(block, name).toContain("$GITHUB_ENV");
      }
    }
  });
});

/**
 * Adversarial cases for ft-ac3bba92 (adversary pass, build run 2026-10-26).
 * Each `it` says whether it broke the implementation (BREAKS) or documents a
 * case that held (HOLDS).
 */
describe("check:workflows adversarial: downloads run without a check", () => {
  const run = (script: string) => `        run: ${script}`;
  const block = (...lines: string[]) => ["        run: |", ...lines.map((line) => `          ${line}`)].join("\n");

  describe("BREAKS: shapes that execute a download unchecked and are not flagged", () => {
    it.each([
      ["source <(curl ...)", run("source <(curl -fsSL https://x.example/i.sh)")],
      ["bash < <(curl ...)", run("bash < <(curl -fsSL https://x.example/i.sh)")],
      ['bash -c "$(curl ...)"', run('bash -c "$(curl -fsSL https://x.example/i.sh)"')],
      ['eval "$(curl ...)"', run('eval "$(curl -fsSL https://x.example/i.sh)"')],
      ["curl ... | /bin/bash", run("curl -fsSL https://x.example/i.sh | /bin/bash")],
      ["curl ... | sudo -E bash", run("curl -fsSL https://x.example/i.sh | sudo -E bash")],
      ["curl ... | tee i.sh | bash", run("curl -fsSL https://x.example/i.sh | tee i.sh | bash")],
      ["a backslash continuation before the pipe", block("curl -fsSL https://x.example/i.sh \\", "  | bash")],
      ["a pipe that ends the line", block("curl -fsSL https://x.example/i.sh |", "  bash")],
      ["a URL on the continuation line", block("curl -fsSL \\", "  https://x.example/i.sh | bash")],
    ])("flags %s", (_label, yaml) => {
      expect(pipedDownloads(yaml)).not.toEqual([]);
    });
  });

  describe("BREAKS: lines that run no downloaded code and are flagged", () => {
    it.each([
      ["a download, then an unrelated pipe to sh after a semicolon", run("curl -fsSL -o x.tgz https://x.example/x.tgz; echo ok | sh -c 'cat'")],
      ["a download, then an unrelated pipe to bash after &&", run("curl -fsSL -o x.tgz https://x.example/x.tgz && echo ok | bash -c 'cat'")],
    ])("leaves alone %s", (_label, yaml) => {
      expect(pipedDownloads(yaml)).toEqual([]);
    });
  });

  it("HOLDS: a pipe into bash that carries no download, and a download piped into a non-shell, are left alone", () => {
    const yaml = [
      run('echo "ls -la" | bash -c "cat"'),
      run("curl -fsSL https://api.example/x | jq .name"),
      run("curl -fsSL https://x.example/i.sh | shellcheck -"),
      run('curl -fsSL -o t.tgz https://x.example/t.tgz && echo "abc  t.tgz" | sha256sum -c -'),
    ].join("\n");
    expect(pipedDownloads(yaml)).toEqual([]);
  });

  it("follows a download through redirections, wrappers and a step-level run: prefix, and ignores words that are not commands", () => {
    for (const yaml of [
      run("curl -fsSL https://x.example/i.sh 2>&1 | bash"),
      "      - run: curl -fsSL https://x.example/i.sh | sudo -u root bash",
      run("FOO=1 curl -fsSL https://x.example/i.sh | env X=1 /usr/bin/bash -s"),
      run("if curl -fsSL https://x.example/i.sh | bash; then echo ok; fi"),
      run("curl -fsSL https://x.example/i.sh | zsh"),
      block("curl -fsSL https://x.example/i.sh \\", "  --retry 3 \\", "  | sh"),
    ]) expect(pipedDownloads(yaml), yaml).toHaveLength(1);
    for (const yaml of [
      run('echo "curl -fsSL https://x.example/i.sh | bash"'),
      run("printf '%s\\n' 'curl x | bash'"),
      "        name: Install curl | bash helper",
      "        # run: curl x | bash",
      run("curl -fsSL https://x.example/i.sh # | bash"),
      run("curl -fsSL -o i.sh https://x.example/i.sh && sha256sum -c sums.txt && bash i.sh"),
      block("curl -fsSL -o i.sh https://x.example/i.sh", "echo \"$HASH  i.sh\" | sha256sum -c -", "bash i.sh"),
      block("run: |", "  set -e"),
      run("bash -c 'echo hi' | cat"),
    ]) expect(pipedDownloads(yaml), yaml).toEqual([]);
  });

  it("HOLDS: the common spellings are found, in a multi-line block too", () => {
    for (const script of [
      "curl -fsSL https://x.example/i.sh | sudo bash",
      "wget -qO- https://x.example/i.sh | sh",
      "bash <(curl -fsSL https://x.example/i.sh)",
      "curl -fsSL https://x.example/i.sh | bash -s -- --yes",
    ]) {
      expect(pipedDownloads(block("set -e", script, "echo done")), script).toHaveLength(1);
    }
  });

  const hasSha256sum = spawnSync("sha256sum", ["--version"], { stdio: "ignore" }).error === undefined;
  it.skipIf(process.platform === "win32" || !hasSha256sum)("HOLDS: a tampered actionlint tarball stops the install step before anything is extracted", () => {
    const ci = readFileSync(fileURLToPath(new URL("../.github/workflows/ci.yml", import.meta.url)), "utf8");
    const step = ci.slice(ci.indexOf("- name: Install actionlint"), ci.indexOf("- name: Workflow Lint"));
    const lines = step.split("\n");
    const start = lines.findIndex((line) => /^\s+run: \|\s*$/.test(line));
    expect(start).toBeGreaterThan(-1);
    const indent = /^\s*/.exec(lines[start + 1])?.[0].length ?? 0;
    const script = lines.slice(start + 1).filter((line) => /^\s+\S/.test(line) && !/^\s+shell:/.test(line)).map((line) => line.slice(indent)).join("\n");
    const root = mkdtempSync(path.join(tmpdir(), "gitpulse-actionlint-"));
    try {
      const evil = path.join(root, "evil");
      mkdirSync(evil);
      writeFileSync(path.join(evil, "actionlint"), "#!/bin/sh\ntouch PWNED\n");
      chmodSync(path.join(evil, "actionlint"), 0o755);
      const tarball = path.join(root, "tampered.tgz");
      expect(spawnSync("tar", ["-czf", tarball, "-C", evil, "actionlint"]).status).toBe(0);
      const bin = path.join(root, "bin");
      mkdirSync(bin);
      // A curl that honours `-o <file>` and serves the tampered tarball.
      writeFileSync(path.join(bin, "curl"), `#!/bin/bash\nwhile [ $# -gt 0 ]; do if [ "$1" = "-o" ]; then out="$2"; shift; fi; shift; done\ncp "${tarball}" "$out"\n`);
      chmodSync(path.join(bin, "curl"), 0o755);
      const work = path.join(root, "work");
      mkdirSync(work);
      const result = spawnSync("bash", ["--noprofile", "--norc", "-eo", "pipefail", "-c", script], {
        cwd: work, encoding: "utf8", env: { ...process.env, PATH: `${bin}${path.delimiter}${process.env.PATH ?? ""}` },
      });
      expect(result.status, `${result.stdout}${result.stderr}`).not.toBe(0);
      expect(`${result.stdout}${result.stderr}`).toMatch(/FAILED|did NOT match/);
      expect(existsSync(path.join(work, "actionlint"))).toBe(false);
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  });
});

/**
 * The release gate (`release-state.mjs`, `.githooks/pre-push`) reads a workflow
 * RUN's conclusion, never its jobs. GitHub concludes a run `success` when a job
 * was skipped, so a check job that gains an `if:`, a `continue-on-error`, or a
 * `needs:` on a skippable job would satisfy the gate without ever running.
 */
describe("check:workflows release-gate rule: a trusted check must not be able to skip", () => {
  const wf = (...body: string[]) => ["on: push", "permissions:", "  contents: read", "jobs:", ...body].join("\n");
  const gate = (text: string, allowlist: ReleaseGateAllowEntry[] = [], name = "ci.yml") =>
    gateFindings({ [name]: text }, allowlist);
  const allow = (over: Partial<ReleaseGateAllowEntry>): ReleaseGateAllowEntry => ({
    workflow: "ci.yml",
    job: "test",
    key: "if",
    reason: "a written reason",
    ...over,
  });
  const clean = wf("  test:", "    runs-on: ubuntu-latest", "    steps:", "      - name: Build", "        run: make");

  it("passes a workflow whose jobs and steps always run", () => {
    expect(gate(clean)).toEqual([]);
  });

  describe("job-level if", () => {
    const withIf = wf("  test:", "    if: github.ref == 'refs/heads/main'", "    runs-on: ubuntu-latest", "    steps:", "      - run: make");

    it("fails, naming the workflow, the job, the key and what to do", () => {
      const [finding, ...rest] = gate(withIf);
      expect(rest).toEqual([]);
      expect(finding).toContain("ci.yml");
      expect(finding).toContain('job "test"');
      expect(finding).toContain("`if`");
      expect(finding).toMatch(/remove the condition or add an allowlist entry with a reason/);
    });

    it("passes when an allowlist entry with a reason covers that job and key", () => {
      expect(gate(withIf, [allow({})])).toEqual([]);
    });

    it("does not let an entry for another key, job or workflow cover it", () => {
      for (const entry of [allow({ key: "continue-on-error" }), allow({ job: "other" }), allow({ workflow: "coverage.yml" })]) {
        expect(gate(withIf, [entry]).some((f) => f.includes('job "test"') && f.includes("`if`"))).toBe(true);
      }
    });
  });

  describe("continue-on-error", () => {
    it("fails on a job", () => {
      const [finding] = gate(wf("  test:", "    continue-on-error: true", "    runs-on: ubuntu-latest", "    steps:", "      - run: make"));
      expect(finding).toContain('job "test"');
      expect(finding).toContain("continue-on-error");
    });

    it("fails on a step, naming the step", () => {
      const [finding] = gate(wf("  test:", "    runs-on: ubuntu-latest", "    steps:", "      - name: Lint", "        continue-on-error: true", "        run: make lint"));
      expect(finding).toContain('job "test"');
      expect(finding).toContain('step "Lint"');
      expect(finding).toContain("continue-on-error");
    });

    it("accepts only a literal false; anything else, quoted or computed, fails", () => {
      const step = (value: string) => wf("  test:", "    runs-on: ubuntu-latest", "    steps:", "      - name: Lint", `        continue-on-error: ${value}`, "        run: x");
      expect(gate(step("false"))).toEqual([]);
      for (const value of ["true", "'false'", '"false"', "${{ false }}", "${{ matrix.experimental }}", "yes", ""]) {
        expect(gate(step(value)), value).toHaveLength(1);
      }
    });

    it("can be allowlisted on a step by name or by index", () => {
      const text = wf("  test:", "    runs-on: ubuntu-latest", "    steps:", "      - name: Lint", "        continue-on-error: true", "        run: x");
      expect(gate(text, [allow({ step: "Lint", key: "continue-on-error" })])).toEqual([]);
      expect(gate(text, [allow({ step: "#0", key: "continue-on-error" })])).toEqual([]);
    });
  });

  describe("needs on a job that can skip", () => {
    const skippable = ["  a:", "    if: github.event_name == 'workflow_dispatch'", "    runs-on: ubuntu-latest", "    steps:", "      - run: x"];
    const dependent = (needs: string) => ["  b:", `    needs: ${needs}`, "    runs-on: ubuntu-latest", "    steps:", "      - run: y"];

    it("fails the dependent job even when the job it needs is allowlisted", () => {
      const findings = gate(wf(...skippable, ...dependent("a")), [allow({ job: "a" })]);
      expect(findings).toHaveLength(1);
      expect(findings[0]).toContain('job "b"');
      expect(findings[0]).toContain("`needs`");
      expect(findings[0]).toContain('"a"');
    });

    it("sees through the list form and through a chain", () => {
      const text = wf(...skippable, ...dependent("[x, a]"), "  x:", "    runs-on: ubuntu-latest", "    steps:", "      - run: z", "  c:", "    needs: b", "    runs-on: ubuntu-latest", "    steps:", "      - run: w");
      const findings = gate(text, [allow({ job: "a" })]);
      expect(findings.filter((f) => f.includes('job "b"'))).toHaveLength(1);
      expect(findings.filter((f) => f.includes('job "c"'))).toHaveLength(1);
      expect(findings.some((f) => f.includes('job "x"'))).toBe(false);
    });

    it("passes when the dependent job is allowlisted for needs, and when the dependency cannot skip", () => {
      expect(gate(wf(...skippable, ...dependent("a")), [allow({ job: "a" }), allow({ job: "b", key: "needs" })])).toEqual([]);
      expect(gate(wf("  a:", "    runs-on: ubuntu-latest", "    steps:", "      - run: x", ...dependent("a")))).toEqual([]);
    });

    it("fails a needs that names a job the workflow does not define", () => {
      const findings = gate(wf(...dependent("ghost")));
      expect(findings).toHaveLength(1);
      expect(findings[0]).toContain('"ghost"');
    });
  });

  describe("step-level if", () => {
    const step = (condition: string, extra: string[] = []) =>
      wf("  test:", "    runs-on: ${{ matrix.os }}", "    strategy:", "      matrix:", "        os: [ubuntu-latest, macos-latest]", "    steps:", "      - name: Check", `        if: ${condition}`, ...extra.map((l) => `        ${l}`), "        run: x");

    it.each([
      "!cancelled()",
      "${{ !cancelled() }}",
      "${{!cancelled()}}",
      "matrix.os == 'ubuntu-latest'",
      "${{ matrix.os == 'macos-latest' }}",
      "${{ !cancelled() && matrix.os == 'ubuntu-latest' }}",
      "${{ !cancelled() && runner.os != 'Windows' }}",
      "${{ !cancelled() && runner.os == 'macOS' }}",
      "${{ matrix.os != 'macos-latest' && !cancelled() }}",
    ])("accepts the platform or cancellation form %s", (condition) => {
      expect(gate(step(condition)), condition).toEqual([]);
    });

    it.each([
      "github.ref == 'refs/heads/main'",
      "always()",
      "success()",
      "failure()",
      "${{ !cancelled() || matrix.os == 'ubuntu-latest' }}",
      "${{ !cancelled() && github.event_name == 'push' }}",
      "${{ (!cancelled()) }}",
      "matrix.os == 'ubuntu-latest' || true",
      "${{ matrix.os == github.ref }}",
      "contains(matrix.os, 'ubuntu')",
      "${{ !cancelled() }} && x",
      "${{ !cancelled() && }}",
      "false",
      "true",
    ])("fails the condition %s because it cannot be classified", (condition) => {
      const findings = gate(step(condition));
      expect(findings, condition).toHaveLength(1);
      expect(findings[0]).toContain('step "Check"');
      expect(findings[0]).toContain("`if`");
    });

    it("fails a platform selector that matches none of the job's legs", () => {
      expect(gate(step("matrix.os == 'solaris'"))).toHaveLength(1);
      expect(gate(step("runner.os == 'Solaris'"))).toHaveLength(1);
      expect(gate(step("matrix.arch == 'x64'"))).toHaveLength(1);
    });

    it("evaluates the whole condition against each leg, so one that no leg satisfies fails", () => {
      expect(gate(step("matrix.os == 'ubuntu-latest' && matrix.os == 'macos-latest'"))).toHaveLength(1);
      expect(gate(step("matrix.os == 'macos-latest' && runner.os == 'Linux'"))).toHaveLength(1);
      expect(gate(step("matrix.os == 'ubuntu-latest' && runner.os == 'Linux'"))).toEqual([]);
      expect(gate(step("${{ !cancelled() && matrix.os != 'ubuntu-latest' && runner.os != 'macOS' }}"))).toHaveLength(1);
    });

    it("checks runner.os against the operating systems the job's runs-on can give", () => {
      const single = (condition: string, runsOn = "ubuntu-latest") =>
        wf("  test:", `    runs-on: ${runsOn}`, "    steps:", "      - name: Check", `        if: ${condition}`, "        run: x");
      expect(gate(single("runner.os == 'Windows'"))).toHaveLength(1);
      expect(gate(single("runner.os != 'Linux'"))).toHaveLength(1);
      expect(gate(single("runner.os == 'Linux'"))).toEqual([]);
      expect(gate(single("runner.os == 'Linux'", "[self-hosted, big]"))).toHaveLength(1);
      expect(gate(single("runner.os == 'Linux'", "${{ inputs.runner }}"))).toHaveLength(1);
    });

    it("accepts only matrix.os, matrix.platform and runner.os as platform subjects", () => {
      const axis = (name: string) =>
        wf("  test:", `    runs-on: \${{ matrix.${name} }}`, "    strategy:", "      matrix:", `        ${name}: [ubuntu-latest, macos-latest]`, "    steps:", "      - name: Check", `        if: matrix.${name} == 'ubuntu-latest'`, "        run: x");
      expect(gate(axis("platform"))).toEqual([]);
      expect(gate(axis("os"))).toEqual([]);
      expect(gate(axis("flavour"))).toHaveLength(1);
    });

    it("fails an inequality that excludes every leg", () => {
      const single = wf("  test:", "    runs-on: ${{ matrix.os }}", "    strategy:", "      matrix:", "        os: [ubuntu-latest]", "    steps:", "      - name: Check", "        if: matrix.os != 'ubuntu-latest'", "        run: x");
      expect(gate(single)).toHaveLength(1);
    });

    it("can be allowlisted by step name", () => {
      expect(gate(step("always()"), [allow({ step: "Check" })])).toEqual([]);
    });

    it("refers to an unnamed step by its index", () => {
      const text = wf("  test:", "    runs-on: ubuntu-latest", "    steps:", "      - run: x", "      - if: always()", "        run: y");
      const [finding] = gate(text);
      expect(finding).toContain("step #1");
      expect(gate(text, [allow({ step: "#1" })])).toEqual([]);
    });

    it("judges a multi-line condition as one condition", () => {
      expect(gate(step(">-", ["  ${{ !cancelled()", "  && matrix.os == 'ubuntu-latest' }}"]))).toEqual([]);
      expect(gate(step("${{ !cancelled() &&", ["  matrix.os == 'ubuntu-latest' }}"]))).toEqual([]);
      expect(gate(step("${{ !cancelled() &&", ["  github.ref == 'x' }}"]))).toHaveLength(1);
    });
  });

  describe("a job that calls a reusable workflow, or has no steps", () => {
    const calls = (target: string, extra: string[] = []) => wf("  a:", `    uses: ${target}`, ...extra);

    it.each(["./.github/workflows/other.yml", "octo/repo/.github/workflows/x.yml@3d3c42e5aac5ba805825da76410c181273ba90b1"])(
      "fails a job-level uses of %s, whose jobs this rule cannot read",
      (target) => {
        const [finding, ...rest] = gate(calls(target));
        expect(rest).toEqual([]);
        expect(finding).toContain('job "a"');
        expect(finding).toContain("`uses`");
        expect(finding).toMatch(/reusable workflow/);
      },
    );

    it("honours an allowlist entry for uses, and fails a stale one", () => {
      expect(gate(calls("./.github/workflows/other.yml"), [allow({ job: "a", key: "uses" })])).toEqual([]);
      const stale = gate(clean, [allow({ job: "a", key: "uses" })]);
      expect(stale).toHaveLength(1);
      expect(stale[0]).toMatch(/stale/);
      // An entry for another key does not excuse it.
      expect(gate(calls("./x.yml"), [allow({ job: "a", key: "if" })]).some((f) => f.includes("`uses`"))).toBe(true);
    });

    it("fails a job with neither steps nor uses", () => {
      const [finding, ...rest] = gate(wf("  a:", "    runs-on: ubuntu-latest"));
      expect(rest).toEqual([]);
      expect(finding).toContain('job "a"');
      expect(finding).toContain("`steps`");
    });

    it("fails a job with both steps and uses, and still judges the rest of a uses job", () => {
      expect(gate(wf("  a:", "    uses: ./x.yml", "    steps:", "      - run: x"))).toHaveLength(1);
      expect(gate(calls("./x.yml", ["    if: always()"]))).toHaveLength(2);
    });
  });

  describe("matrix exclude", () => {
    const withExclude = (shape: string[]) =>
      wf("  test:", "    runs-on: ${{ matrix.os }}", "    strategy:", "      matrix:", "        os: [ubuntu-latest, windows-latest]", ...shape, "    steps:", "      - run: x");

    it("fails, because it removes legs without a trace", () => {
      for (const shape of [["        exclude:", "          - os: windows-latest"], ["        exclude: [{ os: windows-latest }]"]]) {
        const [finding, ...rest] = gate(withExclude(shape));
        expect(rest).toEqual([]);
        expect(finding).toContain('job "test"');
        expect(finding).toContain("exclude");
      }
    });

    it("passes when allowlisted for matrix.exclude", () => {
      expect(gate(withExclude(["        exclude:", "          - os: windows-latest"]), [allow({ key: "matrix.exclude" })])).toEqual([]);
    });

    it("fails a matrix it cannot read, such as one computed by fromJSON", () => {
      const text = wf("  test:", "    runs-on: ubuntu-latest", "    strategy:", "      matrix: ${{ fromJSON(needs.plan.outputs.matrix) }}", "    steps:", "      - run: x");
      expect(gate(text)).toHaveLength(1);
    });
  });

  describe("the allowlist itself", () => {
    const withIf = wf("  test:", "    if: always()", "    runs-on: ubuntu-latest", "    steps:", "      - run: x");

    it("fails an entry that matches nothing, naming it", () => {
      const findings = gate(clean, [allow({ job: "gone" })]);
      expect(findings).toHaveLength(1);
      expect(findings[0]).toMatch(/stale/);
      expect(findings[0]).toContain('"gone"');
    });

    it("fails an entry whose key or step no longer matches, even if the job does", () => {
      expect(gate(withIf, [allow({}), allow({ key: "continue-on-error" })]).filter((f) => /stale/.test(f))).toHaveLength(1);
      expect(gate(withIf, [allow({}), allow({ step: "Nope" })]).filter((f) => /stale/.test(f))).toHaveLength(1);
    });

    it("fails an entry with an empty or blank reason, and does not honour it", () => {
      for (const reason of ["", "   ", "\n"]) {
        const findings = gate(withIf, [allow({ reason })]);
        expect(findings.some((f) => /reason/.test(f)), JSON.stringify(reason)).toBe(true);
        expect(findings.some((f) => f.includes("remove the condition")), JSON.stringify(reason)).toBe(true);
      }
    });

    it("fails an entry that is malformed", () => {
      for (const entry of [{ workflow: "ci.yml", job: "test", key: "when", reason: "r" }, { workflow: "ci.yml", key: "if", reason: "r" }, { job: "test", key: "if", reason: "r" }]) {
        expect(gate(withIf, [entry as unknown as ReleaseGateAllowEntry]).some((f) => /allowlist entry/.test(f))).toBe(true);
      }
    });
  });

  describe("a guard that could not run does not pass", () => {
    it("fails a trusted workflow that is missing or unreadable", () => {
      const findings = gateFindings({ "ci.yml": null }, []);
      expect(findings).toHaveLength(1);
      expect(findings[0]).toContain("ci.yml");
      expect(findings[0]).toMatch(/cannot be read|missing/);
    });

    it.each([
      ["a tab used as indentation", "on: push\njobs:\n\ttest:\n\t\truns-on: x\n"],
      ["an unterminated quote", "on: push\njobs:\n  test:\n    name: 'oops\n    runs-on: x\n"],
      ["a duplicated key", "on: push\njobs:\n  test:\n    if: a\n    if: b\n    runs-on: x\n"],
      ["an anchor", "on: push\njobs:\n  test: &t\n    runs-on: x\n"],
      ["no jobs", "on: push\n"],
      ["jobs that is not a mapping", "on: push\njobs: nope\n"],
      ["a job that is not a mapping", "on: push\njobs:\n  test: nope\n"],
      ["steps that is not a list", "on: push\njobs:\n  test:\n    runs-on: x\n    steps: nope\n"],
      ["an empty file", ""],
    ])("fails %s", (_label, text) => {
      const findings = gate(text);
      expect(findings.length).toBeGreaterThan(0);
      expect(findings[0]).toContain("ci.yml");
    });
  });

  describe("adversarial YAML shapes", () => {
    it("reads quoted keys", () => {
      expect(gate(wf("  test:", '    "if": always()', "    runs-on: x", "    steps:", "      - run: x"))).toHaveLength(1);
      expect(gate(wf("  test:", "    runs-on: x", "    steps:", "      - run: x", "        'continue-on-error': true"))).toHaveLength(1);
      expect(gate(wf("  'test':", "    runs-on: x", "    steps:", "      - run: x"))).toEqual([]);
    });

    it("does not let an anchor or a tag hide a key", () => {
      expect(gate(wf("  test:", "    runs-on: x", "    steps:", "      - &a if: always()", "        run: x")).length).toBeGreaterThan(0);
      expect(gate(wf("  test:", "    runs-on: x", "    steps:", "      - run: x", "        !!str if: always()")).length).toBeGreaterThan(0);
      expect(gate(wf("  test:", "    runs-on: x", "    steps:", "      - run: x", "        *ref continue-on-error: true")).length).toBeGreaterThan(0);
      expect(gate(wf("  test:", "    runs-on: x", "    steps:", "      - run: x", '        "&not-anchor": fine'))).toEqual([]);
    });

    it("reads an if written with and without ${{ }}", () => {
      const job = (value: string) => wf("  test:", `    if: ${value}`, "    runs-on: x", "    steps:", "      - run: x");
      expect(gate(job("${{ github.ref == 'x' }}"))).toHaveLength(1);
      expect(gate(job("github.ref == 'x'"))).toHaveLength(1);
      expect(gate(job("true"))).toHaveLength(1);
    });

    it("reads a multi-line job condition", () => {
      const text = wf("  test:", "    if: >-", "      github.ref == 'x' &&", "      github.event_name == 'push'", "    runs-on: x", "    steps:", "      - run: x");
      expect(gate(text)).toHaveLength(1);
      const plain = wf("  test:", "    if: github.ref == 'x' &&", "      github.event_name == 'push'", "    runs-on: x", "    steps:", "      - run: x");
      expect(gate(plain)).toHaveLength(1);
    });

    it("does not mistake text inside a run block scalar, a comment or a string for a condition", () => {
      const text = wf(
        "  test:",
        "    # if: github.ref == 'x'",
        "    # continue-on-error: true",
        "    runs-on: x",
        "    steps:",
        "      - name: 'if: not a condition'",
        "        run: |",
        "          if [ -n \"$X\" ]; then echo go; fi",
        "          if: still text",
        "          continue-on-error: true",
        "            exclude: [a]",
        "        env:",
        "          NOTE: if: x",
        "      - name: Second  # if: nope",
        "        run: echo \"if: y\"",
        "        with:",
        "          if: a-with-input-named-if",
        "          continue-on-error: not-a-step-key",
      );
      expect(gate(text)).toEqual([]);
    });

    it("finds a real if that follows a block scalar", () => {
      const text = wf("  test:", "    runs-on: x", "    steps:", "      - run: |", "          echo hi", "        if: always()");
      expect(gate(text)).toHaveLength(1);
    });

    it("reads a file that starts with a byte-order mark", () => {
      const bom = String.fromCharCode(0xfeff);
      expect(gate(bom + clean)).toEqual([]);
      expect(gate(bom + wf("  test:", "    if: always()", "    runs-on: x", "    steps:", "      - run: x"))).toHaveLength(1);
    });

    it("reads CRLF and bare-CR line endings the same as LF", () => {
      const withIf = wf("  test:", "    if: always()", "    runs-on: x", "    steps:", "      - run: x", "        continue-on-error: true");
      const lf = gate(withIf);
      expect(lf).toHaveLength(2);
      expect(gate(withIf.replace(/\n/g, "\r\n"))).toEqual(lf);
      expect(gate(withIf.replace(/\n/g, "\r"))).toEqual(lf);
      expect(gate(clean.replace(/\n/g, "\r\n"))).toEqual([]);
    });

    it("reads a tab between the key and its value, and rejects a tab used as indentation", () => {
      expect(gate(wf("  test:", "    if:\talways()", "    runs-on: x", "    steps:", "      - run: x"))).toHaveLength(1);
      const tabbed = gate(wf("  test:", "\tif: always()", "    runs-on: x", "    steps:", "      - run: x"));
      expect(tabbed).toHaveLength(1);
      expect(tabbed[0]).toMatch(/cannot be parsed/);
    });

    it("reads a step written compactly after the dash, and a sequence at the key's own indent", () => {
      const text = wf("  test:", "    runs-on: x", "    steps:", "    - name: A", "      if: always()", "      run: x", "    - run: y");
      const [finding, ...rest] = gate(text);
      expect(rest).toEqual([]);
      expect(finding).toContain('step "A"');
    });

    it("reads flow sequences in needs and in matrix values, and a trailing comment after a value", () => {
      const text = wf("  a:", "    runs-on: x", "    steps:", "      - run: x", "  b:", "    needs: [ a ]  # after a", "    runs-on: x", "    steps:", "      - run: y");
      expect(gate(text)).toEqual([]);
      // Steps written as a flow collection cannot be inspected, so they fail.
      expect(gate(wf("  a:", "    runs-on: x", "    steps: [{ run: x, if: always() }]"))).toHaveLength(1);
    });

    it("treats duplicate step names as ambiguous and refers to them by index", () => {
      const text = wf("  test:", "    runs-on: x", "    steps:", "      - name: Same", "        if: always()", "        run: x", "      - name: Same", "        if: failure()", "        run: y");
      const findings = gate(text);
      expect(findings).toHaveLength(2);
      expect(findings[0]).toContain("step #0");
      expect(findings[1]).toContain("step #1");
    });
  });

  describe("which workflows are judged", () => {
    const withTempTree = (releaseState: string, files: Record<string, string>, body: (dir: string, state: string) => void) => {
      const root = mkdtempSync(path.join(tmpdir(), "gate-rule-"));
      try {
        const dir = path.join(root, "workflows");
        mkdirSync(dir);
        for (const [name, text] of Object.entries(files)) writeFileSync(path.join(dir, name), text);
        const state = path.join(root, "release-state.mjs");
        writeFileSync(state, releaseState);
        body(dir, state);
      } finally {
        rmSync(root, { recursive: true, force: true });
      }
    };
    const gateSource = (...lists: string[]) =>
      [
        "function requireSuccessfulPushRun(workflow) { return workflow; }",
        ...lists.map((list) => `for (const workflow of ${list}) requireSuccessfulPushRun(workflow);`),
      ].join("\n");
    const skipping = wf("  t:", "    if: always()", "    runs-on: x", "    steps:", "      - run: x");

    it("does not judge a workflow the release gate does not read", () => {
      withTempTree(gateSource('["ci.yml"]'), { "ci.yml": clean, "bench.yml": skipping }, (dir, state) => {
        expect(releaseGateFindings(dir, state, [])).toEqual([]);
      });
    });

    it("judges every workflow the gate reads", () => {
      withTempTree(gateSource('["ci.yml", "bench.yml"]'), { "ci.yml": clean, "bench.yml": skipping }, (dir, state) => {
        const findings = releaseGateFindings(dir, state, []);
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain("bench.yml");
      });
    });

    it("fails a trusted workflow whose file is not there", () => {
      withTempTree(gateSource('["ci.yml", "gone.yml"]'), { "ci.yml": clean }, (dir, state) => {
        const findings = releaseGateFindings(dir, state, []);
        expect(findings).toHaveLength(1);
        expect(findings[0]).toContain("gone.yml");
      });
    });

    it("fails when the list the gate reads cannot be determined", () => {
      withTempTree("export const nothing = 1;\n", { "ci.yml": clean }, (dir, state) => {
        const findings = releaseGateFindings(dir, state, []);
        expect(findings).toHaveLength(1);
        expect(findings[0]).toMatch(/trusted workflows/);
      });
      withTempTree(gateSource('["ci.yml"]') + "\n[x].map(requireSuccessfulPushRun);\n", { "ci.yml": clean }, (dir, state) => {
        expect(releaseGateFindings(dir, state, [])).toHaveLength(1);
      });
      expect(releaseGateFindings("/nonexistent/dir", "/nonexistent/release-state.mjs", [])).toHaveLength(1);
    });
  });

  describe("derivation from release-state.mjs", () => {
    const repo = (relative: string) => fileURLToPath(new URL(relative, import.meta.url));
    const releaseState = readFileSync(repo("./release-state.mjs"), "utf8");

    it("reads the workflows the gate checks, from every call site", () => {
      const { workflows, sites } = trustedWorkflowNames(releaseState);
      expect(workflows).toEqual(["ci.yml", "coverage.yml"]);
      expect(sites.length).toBeGreaterThanOrEqual(2);
      // The `ready` and `prepare` stages must trust the same workflows. If they
      // ever differ the guard judges the union, and this says so.
      for (const site of sites) expect(site).toEqual(workflows);
    });

    it("is the list the pre-push hook describes", () => {
      const hook = readFileSync(repo("../.githooks/pre-push"), "utf8");
      for (const name of trustedWorkflowNames(releaseState).workflows) expect(hook).toContain(name);
    });

    it("takes the union and fails closed on a call site it cannot read", () => {
      expect(trustedWorkflowNames("function requireSuccessfulPushRun(w) {}\nfor (const w of ['a.yml']) requireSuccessfulPushRun(w);\nrequireSuccessfulPushRun('b.yml');").workflows).toEqual(["a.yml", "b.yml"]);
      expect(() => trustedWorkflowNames("function requireSuccessfulPushRun(w) {}\nrequireSuccessfulPushRun(name);")).toThrow(/call site/);
      expect(() => trustedWorkflowNames("function requireSuccessfulPushRun(w) {}\nlist.forEach(requireSuccessfulPushRun);")).toThrow(/call site/);
      expect(() => trustedWorkflowNames("function requireSuccessfulPushRun(w) {}")).toThrow(/no call site/);
      expect(() => trustedWorkflowNames("function requireSuccessfulPushRun(w) {}\nfor (const w of ['../x.yml']) requireSuccessfulPushRun(w);")).toThrow(/workflow file name/);
    });

    it("ignores the identifier in comments", () => {
      const text = "// requireSuccessfulPushRun(x) is the gate\n/**\n * requireSuccessfulPushRun\n */\nfunction requireSuccessfulPushRun(w) {}\nfor (const w of ['a.yml']) requireSuccessfulPushRun(w);";
      expect(trustedWorkflowNames(text).workflows).toEqual(["a.yml"]);
    });
  });

  it("holds this repository's trusted workflows to the rule, with exactly the exemptions that are there", () => {
    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    const state = fileURLToPath(new URL("./release-state.mjs", import.meta.url));
    expect(releaseGateFindings(dir, state)).toEqual([]);
    for (const entry of RELEASE_GATE_ALLOWLIST) expect(entry.reason.trim().length, JSON.stringify(entry)).toBeGreaterThan(20);
  });

  it("does not let arm-bundle's exemption cover a new continue-on-error on the same job", () => {
    const dir = fileURLToPath(new URL("../.github/workflows", import.meta.url));
    const text = readFileSync(path.join(dir, "ci.yml"), "utf8");
    expect(text).toContain("if: github.event_name == 'workflow_dispatch'");
    const changed = text.replace("if: github.event_name == 'workflow_dispatch'", "if: github.event_name == 'workflow_dispatch'\n    continue-on-error: true");
    const findings = gateFindings({ "ci.yml": changed }, RELEASE_GATE_ALLOWLIST);
    expect(findings.some((f) => f.includes('job "arm-bundle"') && f.includes("continue-on-error"))).toBe(true);
  });
});
