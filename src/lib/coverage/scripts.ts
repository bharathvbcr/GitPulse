/**
 * Curated commands that produce coverage artifacts, keyed by the exact family
 * strings the coverage scanner emits ("rust", "javascript", …). GitPulse runs
 * one command at a time with no shell (see tokenize.ts), so every entry is
 * plain argv — no pipes, chaining or redirection.
 */
export const SUGGESTED_COVERAGE_COMMANDS: Readonly<Record<string, readonly string[]>> = {
  rust: ["cargo llvm-cov --lcov --output-path lcov.info"],
  javascript: ["npx vitest run --coverage", "npx jest --coverage"],
  python: ["pytest --cov --cov-report=xml"],
  go: ["go test ./... -coverprofile=coverage.out"],
  jvm: ["./gradlew test jacocoTestReport", "mvn verify"],
  native: [],
  swift: [],
};

/** Suggestions for a family; unknown families get none. Returns a fresh array. */
export function suggestedCoverageCommands(family: string): string[] {
  const commands = SUGGESTED_COVERAGE_COMMANDS[family];
  return commands ? [...commands] : [];
}
