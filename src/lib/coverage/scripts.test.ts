import { describe, expect, it } from "vitest";
import { SUGGESTED_COVERAGE_COMMANDS, suggestedCoverageCommands } from "./scripts";
import { tokenizeCommand } from "../terminal/tokenize";

const DOCUMENTED_FAMILIES = ["javascript", "rust", "python", "go", "jvm", "native", "swift"];

describe("suggestedCoverageCommands", () => {
  it("returns the curated commands for each family that has them", () => {
    expect(suggestedCoverageCommands("rust")).toEqual([
      "cargo llvm-cov --lcov --output-path lcov.info",
    ]);
    expect(suggestedCoverageCommands("javascript")).toEqual([
      "npx vitest run --coverage",
      "npx jest --coverage",
    ]);
    expect(suggestedCoverageCommands("python")).toEqual(["pytest --cov --cov-report=xml"]);
    expect(suggestedCoverageCommands("go")).toEqual(["go test ./... -coverprofile=coverage.out"]);
    expect(suggestedCoverageCommands("jvm")).toEqual([
      "./gradlew test jacocoTestReport",
      "mvn verify",
    ]);
  });

  it("tokenizes every suggestion cleanly for every documented family", () => {
    for (const fam of DOCUMENTED_FAMILIES) {
      for (const command of suggestedCoverageCommands(fam)) {
        const tokenized = tokenizeCommand(command);
        expect(tokenized.ok, `${fam}: ${command} must be shell-free argv`).toBe(true);
        if (tokenized.ok) {
          expect(tokenized.argv.length).toBeGreaterThan(0);
          expect(tokenized.argv.every((arg) => arg.trim().length > 0)).toBe(true);
        }
      }
    }
  });

  it("returns no commands for native and swift families", () => {
    expect(suggestedCoverageCommands("native")).toEqual([]);
    expect(suggestedCoverageCommands("swift")).toEqual([]);
  });

  it("returns no commands for unknown or blank families", () => {
    expect(suggestedCoverageCommands("cobol")).toEqual([]);
    expect(suggestedCoverageCommands("")).toEqual([]);
    expect(suggestedCoverageCommands("Rust")).toEqual([]);
  });

  it("maps exactly the seven documented families with no extras", () => {
    expect(Object.keys(SUGGESTED_COVERAGE_COMMANDS).sort()).toEqual([...DOCUMENTED_FAMILIES].sort());
  });

  it("returns a fresh array so callers cannot mutate the table", () => {
    const first = suggestedCoverageCommands("python");
    first.push("rm -rf /");
    expect(suggestedCoverageCommands("python")).toEqual(["pytest --cov --cov-report=xml"]);
  });
});
