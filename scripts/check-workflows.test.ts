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
  hasWorkflowLevelPermissions,
  interpret,
  pipedDownloads,
  unpinnedActions,
  workflowFiles,
  workflowPolicyFindings,
  workflowsMissingPermissions,
} from "./check-workflows.mjs";

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
    expect([local!.program, ...local!.args].join(" ")).toContain("--test-threads=1");
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
