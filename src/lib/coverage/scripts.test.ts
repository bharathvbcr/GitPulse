import { describe, expect, it } from "vitest";
import {
  coverageCommandsAreCumulative,
  coverageFamilyRunLabel,
  coverageFamilyViews,
  missingCoveragePipelines,
  setupCoverageCommands,
  suggestedCoverageCommands,
} from "./scripts";
import { tokenizeCommand } from "../terminal/tokenize";
import type { CoverageFamilyStatus } from "./types";

function family(overrides: Partial<CoverageFamilyStatus> = {}): CoverageFamilyStatus {
  return {
    family: "rust",
    languages: ["Rust"],
    color_hex: "#dea584",
    expected_formats: ["lcov"],
    expected_paths: ["lcov.info"],
    found: false,
    suggested_commands: [
      "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info",
    ],
    setup_commands: [],
    tool_ready: true,
    tool_detail: "",
    duration_hint: "Generating Rust coverage can take several minutes.",
    ...overrides,
  };
}

describe("suggestedCoverageCommands", () => {
  it("returns a copy of the scanner-planned commands", () => {
    const status = family();
    const commands = suggestedCoverageCommands(status);
    expect(commands).toEqual(status.suggested_commands);
    commands.push("rm -rf /");
    expect(suggestedCoverageCommands(status)).toEqual(status.suggested_commands);
  });

  it("tokenizes every planned command as no-shell argv", () => {
    const commands = [
      "npm run coverage",
      "npx --no-install vitest run --coverage",
      "npx --no-install jest --coverage",
      "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info",
      "cargo llvm-cov --workspace --lcov --output-path lcov.info",
      "rustup component add llvm-tools-preview",
      "cargo install cargo-llvm-cov --locked",
      "pytest --cov --cov-report=xml",
      "go test ./... -coverprofile=coverage.out",
      "go -C backend/go_orchestrator test ./... -coverprofile=coverage.out",
      "npm run test:coverage",
      "./gradlew test jacocoTestReport",
      "mvn verify",
      "swift test --enable-code-coverage",
      "dart test --coverage=coverage",
    ];
    for (const command of commands) {
      const tokenized = tokenizeCommand(command);
      expect(tokenized.ok, `${command} must be shell-free argv`).toBe(true);
      if (tokenized.ok) {
        expect(tokenized.argv.length).toBeGreaterThan(0);
        expect(tokenized.argv.every((arg) => arg.trim().length > 0)).toBe(true);
      }
    }
  });

  it("drops blank, non-string, and missing payloads", () => {
    expect(suggestedCoverageCommands(undefined)).toEqual([]);
    expect(suggestedCoverageCommands(null)).toEqual([]);
    expect(suggestedCoverageCommands(family({ suggested_commands: [] }))).toEqual([]);
    expect(
      suggestedCoverageCommands(
        family({ suggested_commands: ["", "  ", "npm run coverage", 3 as unknown as string] }),
      ),
    ).toEqual(["npm run coverage"]);
    expect(
      suggestedCoverageCommands({ suggested_commands: undefined } as unknown as CoverageFamilyStatus),
    ).toEqual([]);
  });
});

describe("missingCoveragePipelines", () => {
  it("builds a per-language pipeline of setup then generate for missing families", () => {
    const families = [
      family({ family: "javascript", found: true, suggested_commands: ["npm run coverage"] }),
      family({
        family: "rust",
        found: false,
        tool_ready: false,
        tool_detail: "cargo-llvm-cov is not installed.",
        duration_hint: "Installing cargo-llvm-cov and generating Rust coverage can take several minutes.",
        setup_commands: [
          "rustup component add llvm-tools-preview",
          "cargo install cargo-llvm-cov --locked",
        ],
        suggested_commands: [
          "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info",
        ],
      }),
      family({ family: "native", found: false, suggested_commands: [] }),
    ];
    expect(missingCoveragePipelines(families)).toEqual([
      {
        family: "rust",
        label: "Rust",
        toolReady: false,
        toolDetail: "cargo-llvm-cov is not installed.",
        durationHint:
          "Installing cargo-llvm-cov and generating Rust coverage can take several minutes.",
        mode: "all",
        steps: [
          {
            family: "rust",
            kind: "setup",
            command: "rustup component add llvm-tools-preview",
          },
          {
            family: "rust",
            kind: "setup",
            command: "cargo install cargo-llvm-cov --locked",
          },
          {
            family: "rust",
            kind: "generate",
            command:
              "cargo llvm-cov --manifest-path src-tauri/Cargo.toml --workspace --lcov --output-path src-tauri/lcov.info",
          },
        ],
      },
    ]);
  });

  it("omits setup when the scanner reported the toolchain ready", () => {
    const pipelines = missingCoveragePipelines([
      family({
        family: "javascript",
        tool_ready: true,
        duration_hint: "Frontend coverage usually finishes in about a minute.",
        suggested_commands: ["npm run coverage"],
      }),
    ]);
    expect(pipelines).toHaveLength(1);
    expect(pipelines[0]?.toolReady).toBe(true);
    expect(pipelines[0]?.steps).toEqual([
      { family: "javascript", kind: "generate", command: "npm run coverage" },
    ]);
    expect(pipelines[0]?.durationHint).toContain("minute");
  });

  it("treats non-Rust generator commands as fallbacks instead of running every alternative", () => {
    const pipelines = missingCoveragePipelines([
      family({
        family: "javascript",
        suggested_commands: [
          "npx --no-install vitest run --coverage",
          "npx --no-install jest --coverage",
        ],
      }),
    ]);
    expect(pipelines[0]?.mode).toBe("first_success");
    expect(pipelines[0]?.steps).toHaveLength(2);

    const rust = missingCoveragePipelines([
      family({
        family: "rust",
        suggested_commands: [
          "cargo llvm-cov --manifest-path a/Cargo.toml --workspace --lcov --output-path a/lcov.info",
          "cargo llvm-cov --manifest-path b/Cargo.toml --workspace --lcov --output-path b/lcov.info",
        ],
      }),
    ]);
    expect(rust[0]?.mode).toBe("all");

    const go = missingCoveragePipelines([
      family({
        family: "go",
        suggested_commands: [
          "go -C api test ./... -coverprofile=coverage.out",
          "go -C cli test ./... -coverprofile=coverage.out",
        ],
      }),
    ]);
    expect(go[0]?.mode).toBe("all");
    expect(coverageCommandsAreCumulative("go")).toBe(true);
    expect(coverageCommandsAreCumulative("javascript")).toBe(false);
  });

  it("does not invent setup when tool_ready is omitted", () => {
    const pipelines = missingCoveragePipelines([
      {
        family: "rust",
        found: false,
        suggested_commands: ["cargo llvm-cov --workspace --lcov --output-path lcov.info"],
        setup_commands: ["cargo install cargo-llvm-cov --locked"],
      } as CoverageFamilyStatus,
    ]);
    expect(pipelines[0]?.toolReady).toBe(true);
    expect(pipelines[0]?.steps.every((step) => step.kind === "generate")).toBe(true);
  });

  it("survives hostile family arrays without throwing", () => {
    expect(missingCoveragePipelines(undefined)).toEqual([]);
    expect(missingCoveragePipelines(null)).toEqual([]);
    expect(missingCoveragePipelines([null as unknown as CoverageFamilyStatus])).toEqual([]);
  });
});

describe("coverageFamilyRunLabel", () => {
  it("names the Run coverage button after the language family", () => {
    expect(coverageFamilyRunLabel("rust")).toBe("Rust");
    expect(coverageFamilyRunLabel("javascript")).toBe("JavaScript");
    expect(coverageFamilyRunLabel("jvm")).toBe("JVM");
    expect(coverageFamilyRunLabel("native")).toBe("C / C++");
    expect(coverageFamilyRunLabel("swift")).toBe("Swift");
    expect(coverageFamilyRunLabel("dotnet")).toBe(".NET");
    expect(coverageFamilyRunLabel("php")).toBe("PHP");
    expect(coverageFamilyRunLabel("ruby")).toBe("Ruby");
    expect(coverageFamilyRunLabel("dart")).toBe("Dart");
    expect(coverageFamilyRunLabel("beam")).toBe("Elixir / Erlang");
  });
});

describe("setupCoverageCommands", () => {
  it("returns scanner-planned setup only", () => {
    expect(setupCoverageCommands(family())).toEqual([]);
    expect(
      setupCoverageCommands(
        family({ setup_commands: ["cargo install cargo-llvm-cov --locked"] }),
      ),
    ).toEqual(["cargo install cargo-llvm-cov --locked"]);
    expect(setupCoverageCommands(undefined)).toEqual([]);
  });
});

describe("install-then-generate pipelines (regression)", () => {
  it("orders the Python virtualenv build before the install before the run", () => {
    // Ordering is the whole safety property: pip must not run before the venv
    // exists, and pytest must not run before pip. The pipeline runner stops at
    // the first failed setup step, so a broken venv never reaches a test run.
    const [pipeline] = missingCoveragePipelines([
      family({
        family: "python",
        found: false,
        tool_ready: false,
        tool_detail: "pytest is not installed.",
        setup_commands: [
          "python3 -m venv .venv",
          ".venv/bin/python -m pip install pytest pytest-cov",
        ],
        suggested_commands: [".venv/bin/python -m pytest --cov --cov-report=xml"],
      }),
    ]);
    expect(pipeline.steps.map((step) => [step.kind, step.command])).toEqual([
      ["setup", "python3 -m venv .venv"],
      ["setup", ".venv/bin/python -m pip install pytest pytest-cov"],
      ["generate", ".venv/bin/python -m pytest --cov --cov-report=xml"],
    ]);
    expect(pipeline.label).toBe("Python");
    expect(pipeline.toolReady).toBe(false);
  });

  it("installs the JS coverage provider before running vitest --coverage", () => {
    // Running vitest --coverage without a provider fails with "Cannot find
    // dependency '@vitest/coverage-v8'"; the install step is what stops that.
    const [pipeline] = missingCoveragePipelines([
      family({
        family: "javascript",
        found: false,
        tool_ready: false,
        tool_detail: "Vitest is present but no coverage provider is declared.",
        setup_commands: ["npm install --save-dev @vitest/coverage-v8"],
        suggested_commands: ["npx --no-install vitest run --coverage"],
      }),
    ]);
    expect(pipeline.steps).toEqual([
      {
        family: "javascript",
        command: "npm install --save-dev @vitest/coverage-v8",
        kind: "setup",
      },
      {
        family: "javascript",
        command: "npx --no-install vitest run --coverage",
        kind: "generate",
      },
    ]);
  });

  it("never replays setup once the scanner reports the toolchain ready", () => {
    // A rescan after a successful install reports ready; re-running the
    // installer on every subsequent generate would be pure churn.
    const [pipeline] = missingCoveragePipelines([
      family({
        family: "python",
        found: false,
        tool_ready: true,
        setup_commands: [".venv/bin/python -m pip install pytest pytest-cov"],
        suggested_commands: [".venv/bin/python -m pytest --cov --cov-report=xml"],
      }),
    ]);
    expect(pipeline.steps.every((step) => step.kind === "generate")).toBe(true);
  });

  it("plans nothing for a family the scanner could not give a generate command", () => {
    // No amount of setup helps when there is no command to run afterwards.
    expect(
      missingCoveragePipelines([
        family({
          family: "native",
          found: false,
          tool_ready: false,
          tool_detail: "No CMakeLists.txt or Makefile in this repository.",
          setup_commands: ["make install-deps"],
          suggested_commands: [],
        }),
      ]),
    ).toEqual([]);
  });
});

/**
 * The panel offered coverage generation from two places — the header strip and
 * the empty-state sidebar — each deciding for itself what was runnable. These
 * tests pin the single decision that replaced them.
 */
describe("coverageFamilyViews (one decision point)", () => {
  it("never yields a runnable view the pipeline lookup would then reject", () => {
    // The dead button, exactly. The strip drew "Run coverage" whenever the
    // scanner published a command, while `runCoverageFamily` resolved the row
    // through `missingCoveragePipelines`, which additionally required a
    // non-empty family name. A row with a blank name rendered a button that
    // did nothing at all when pressed.
    const views = coverageFamilyViews([
      family({
        family: "",
        found: false,
        tool_ready: true,
        suggested_commands: ["npm run coverage"],
      }),
    ]);
    expect(views).toEqual([]);

    // And for every view that does render a Run button, the pipeline the click
    // handler resolves is the one the view already carries.
    const good = coverageFamilyViews([
      family({
        family: "javascript",
        found: false,
        tool_ready: true,
        suggested_commands: ["npm run coverage"],
      }),
    ]);
    expect(good).toHaveLength(1);
    expect(good[0].pipeline).not.toBeNull();
    expect(missingCoveragePipelines([good[0].status])).toEqual([good[0].pipeline]);
  });

  it("keeps the reason for a family that has no runnable plan at all", () => {
    // `native` and `beam` publish no command and a stated reason. The sidebar
    // reached `tool_detail` only through a pipeline, so those rows lost the one
    // sentence that explained them.
    const [view] = coverageFamilyViews([
      family({
        family: "native",
        found: false,
        tool_ready: false,
        tool_detail: "C/C++ coverage needs an instrumented build.",
        suggested_commands: [],
      }),
    ]);
    expect(view.pipeline).toBeNull();
    expect(view.commands).toEqual([]);
    expect(view.toolDetail).toBe("C/C++ coverage needs an instrumented build.");
  });

  it("withholds the bare command chips until the toolchain is ready", () => {
    // Running the venv pytest on its own, before the install step, is the
    // failure the pipeline exists to prevent — so an unready family gets the
    // pipeline button and no chips.
    const [unready] = coverageFamilyViews([
      family({
        family: "python",
        found: false,
        tool_ready: false,
        tool_detail: "pytest is not installed.",
        setup_commands: [".venv/bin/python -m pip install pytest pytest-cov"],
        suggested_commands: [".venv/bin/python -m pytest --cov --cov-report=xml"],
      }),
    ]);
    expect(unready.commands).toEqual([]);
    expect(unready.pipeline?.steps.map((s) => s.kind)).toEqual(["setup", "generate"]);

    const [ready] = coverageFamilyViews([
      family({
        family: "python",
        found: false,
        tool_ready: true,
        suggested_commands: [".venv/bin/python -m pytest --cov --cov-report=xml"],
      }),
    ]);
    expect(ready.commands).toEqual([".venv/bin/python -m pytest --cov --cov-report=xml"]);
  });

  it("offers nothing for a family that already has a report", () => {
    const [view] = coverageFamilyViews([
      family({ family: "go", found: true, tool_ready: true, suggested_commands: ["go test ./..."] }),
    ]);
    expect(view.found).toBe(true);
    expect(view.pipeline).toBeNull();
    expect(view.commands).toEqual([]);
  });

  it("survives hostile family payloads without inventing an offer", () => {
    const views = coverageFamilyViews([
      null as unknown as CoverageFamilyStatus,
      undefined as unknown as CoverageFamilyStatus,
      "nope" as unknown as CoverageFamilyStatus,
      family({ family: 7 as unknown as string, suggested_commands: ["rm -rf /"] }),
      family({
        family: "rust",
        found: false,
        tool_ready: true,
        suggested_commands: [42 as unknown as string, "cargo llvm-cov --workspace"],
      }),
    ]);
    expect(views).toHaveLength(1);
    expect(views[0].family).toBe("rust");
    expect(views[0].commands).toEqual(["cargo llvm-cov --workspace"]);
  });
});
