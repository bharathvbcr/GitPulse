import { readFileSync } from "node:fs";
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
      const caches = source.split("uses: Swatinem/rust-cache@6323deb102c322ba6fcbdcafc7e3dddab59af2b6");
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
