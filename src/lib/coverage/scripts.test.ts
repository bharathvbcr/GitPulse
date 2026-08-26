import { describe, expect, it } from "vitest";
import {
  coverageCommandsAreCumulative,
  coverageFamilyRunLabel,
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
