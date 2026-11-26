#!/usr/bin/env node
/**
 * Canonical CLI runner for GitPulse R1–R4 opaque-box E2E tests.
 *
 * Usage:
 *   node tests/e2e-r1-r4/run.mjs [--tier 1|2|3|4] [--req R1|R2|R3|R4] [--json] [--list]
 */
import { spawn } from "node:child_process";
import { existsSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO_ROOT = path.resolve(HERE, "../..");
const CONFIG_REL = "tests/e2e-r1-r4/vitest.config.ts";
const DEFAULT_CARGO_TARGET_DIR = path.join(
  REPO_ROOT,
  ".gitpulse",
  "scratch",
  "target-e2e",
);

function parseArgs(argv) {
  let tier = null;
  let req = null;
  let json = false;
  let list = false;
  let help = false;
  let afterSeparator = false;
  const passthrough = [];

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--") {
      afterSeparator = true;
      continue;
    }
    if (afterSeparator) {
      passthrough.push(arg);
      continue;
    }
    if (arg === "--help" || arg === "-h") {
      help = true;
    } else if (arg === "--json" || arg === "--json=true") {
      json = true;
    } else if (arg.startsWith("--json=")) {
      throw new Error(
        `Unsupported ${arg} flag; pass --json so run.mjs emits JSON safely to stdout.`,
      );
    } else if (arg === "--list") {
      list = true;
    } else if (arg === "--tier" || arg.startsWith("--tier=")) {
      const value = arg.startsWith("--tier=") ? arg.slice("--tier=".length) : argv[++i];
      if (!value || !["1", "2", "3", "4"].includes(value)) {
        throw new Error(`Invalid --tier value "${value ?? ""}"; expected 1, 2, 3, or 4`);
      }
      tier = value;
    } else if (arg === "--req" || arg.startsWith("--req=")) {
      const value = (arg.startsWith("--req=") ? arg.slice("--req=".length) : argv[++i])?.toUpperCase();
      if (!value || !["R1", "R2", "R3", "R4"].includes(value)) {
        throw new Error(`Invalid --req value "${value ?? ""}"; expected R1, R2, R3, or R4`);
      }
      req = value;
    } else if (arg === "-t" || arg === "--testNamePattern" || arg === "--test-name-pattern") {
      passthrough.push(arg);
      if (i + 1 < argv.length) {
        passthrough.push(argv[++i]);
      }
    } else {
      passthrough.push(arg);
    }
  }

  return { tier, req, json, list, help, passthrough };
}

function extractTestNameFilters(args) {
  const userPatterns = [];
  const rest = [];

  for (let i = 0; i < args.length; i += 1) {
    const arg = args[i];
    if (arg === "-t" || arg === "--testNamePattern" || arg === "--test-name-pattern") {
      const next = args[++i];
      if (next === undefined) {
        throw new Error(`Option ${arg} requires a pattern argument`);
      }
      userPatterns.push(next);
    } else if (arg.startsWith("-t=")) {
      userPatterns.push(arg.slice("-t=".length));
    } else if (arg.startsWith("--testNamePattern=")) {
      userPatterns.push(arg.slice("--testNamePattern=".length));
    } else if (arg.startsWith("--test-name-pattern=")) {
      userPatterns.push(arg.slice("--test-name-pattern=".length));
    } else {
      rest.push(arg);
    }
  }

  return { userPatterns, rest };
}

function buildCombinedNamePattern(req, userPatterns) {
  if (!req && userPatterns.length === 0) return null;
  if (req && userPatterns.length === 0) return `\\b${req}\\b`;
  if (!req && userPatterns.length === 1) return userPatterns[0];

  const lookaheads = [];
  if (req) lookaheads.push(`(?=[\\s\\S]*\\b${req}\\b)`);
  for (const pattern of userPatterns) {
    lookaheads.push(`(?=[\\s\\S]*(?:${pattern}))`);
  }
  return `^${lookaheads.join("")}`;
}

function collectTestFiles(dir, prefix = "") {
  const found = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const rel = prefix ? `${prefix}/${entry.name}` : entry.name;
    if (entry.isDirectory()) {
      if (entry.name === "node_modules" || entry.name === "browser" || entry.name === "helpers") {
        continue;
      }
      found.push(...collectTestFiles(path.join(dir, entry.name), rel));
    } else if (entry.isFile() && entry.name.endsWith(".test.ts")) {
      found.push(`tests/e2e-r1-r4/${rel}`);
    }
  }
  return found.sort();
}

function defaultBuildJobs() {
  const cpus = os.cpus()?.length || 4;
  return String(Math.max(1, Math.min(4, Math.floor(cpus / 2))));
}

function isDisallowedOutputOrReporterArg(arg) {
  const prefixes = [
    "--outputFile",
    "--output-file",
    "--reporter",
    "--reporters",
    "--no-outputFile",
    "--no-output-file",
    "--no-reporter",
    "--no-reporters",
  ];
  return prefixes.some(
    (prefix) => arg === prefix || arg.startsWith(`${prefix}=`) || arg.startsWith(`${prefix}.`),
  );
}

function printHelp() {
  console.log(`GitPulse R1–R4 E2E Runner

Usage:
  node tests/e2e-r1-r4/run.mjs [options] [-- <vitest args>]

Options:
  --tier 1|2|3|4     Filter to tier<N>-*.test.ts files
  --req R1|R2|R3|R4  Filter tests matching requirement tag via Vitest -t
  --json             Use Vitest JSON reporter (--reporter=json)
  --list             List discovered E2E tests without executing them
  -h, --help         Show this help text
`);
}

async function main() {
  const { tier, req, json, list, help, passthrough } = parseArgs(process.argv.slice(2));
  if (help) {
    printHelp();
    return;
  }

  const env = {
    ...process.env,
    CARGO_TARGET_DIR: process.env.CARGO_TARGET_DIR || DEFAULT_CARGO_TARGET_DIR,
    CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS || defaultBuildJobs(),
    ...(json && !process.env.VITE_CONFIG_NATIVE_IGNORE_WARNING
      ? { VITE_CONFIG_NATIVE_IGNORE_WARNING: "true" }
      : {}),
  };

  const allFiles = collectTestFiles(HERE);
  const matchingFiles = tier
    ? allFiles.filter((file) => path.basename(file).startsWith(`tier${tier}-`))
    : allFiles;

  if (list && matchingFiles.length === 0) {
    if (json) {
      console.log("[]");
    } else {
      console.log(
        tier
          ? `No E2E test files found for tier ${tier} under tests/e2e-r1-r4/.`
          : "No E2E test files found under tests/e2e-r1-r4/.",
      );
    }
    return;
  }

  const { userPatterns, rest } = extractTestNameFilters(passthrough);
  for (const arg of rest) {
    if (arg === "--json" || arg.startsWith("--json=")) {
      throw new Error(
        `Do not pass ${arg} after --; pass --json before -- so run.mjs manages JSON output safely.`,
      );
    }
    if (!list && isDisallowedOutputOrReporterArg(arg)) {
      throw new Error(
        `Do not pass ${arg} after --; run.mjs manages --reporter and --outputFile.json internally.`,
      );
    }
  }
  const combinedPattern = buildCombinedNamePattern(req, userPatterns);

  const vitestArgs = [
    "x",
    "vitest",
    list ? "list" : "run",
    "--config",
    CONFIG_REL,
    "--no-fsModuleCache",
  ];

  let jsonTempDir = null;
  let jsonOutputPath = null;
  if (json || !list) {
    jsonTempDir = mkdtempSync(path.join(os.tmpdir(), "gp-e2e-json-"));
    jsonOutputPath = path.join(jsonTempDir, "output.json");
  }
  if (list && json) {
    vitestArgs.push(`--json=${jsonOutputPath}`);
  } else if (!list) {
    if (json) {
      vitestArgs.push("--reporter=json", `--outputFile.json=${jsonOutputPath}`);
    } else {
      vitestArgs.push(
        "--reporter=default",
        "--reporter=json",
        `--outputFile.json=${jsonOutputPath}`,
      );
    }
  }

  if (combinedPattern) {
    vitestArgs.push(`--testNamePattern=${combinedPattern}`);
  }
  if (tier) {
    if (matchingFiles.length === 0) {
      vitestArgs.push(`tests/e2e-r1-r4/tier${tier}-`);
    } else {
      vitestArgs.push(...matchingFiles);
    }
  }
  vitestArgs.push(...rest);

  let child = null;
  let cleaningUpSignal = false;
  const cleanupJsonTempDir = () => {
    if (jsonTempDir) {
      try {
        rmSync(jsonTempDir, { recursive: true, force: true, maxRetries: 3 });
      } catch {
        // Best-effort cleanup of temp directory.
      }
      jsonTempDir = null;
    }
  };
  const handleSignal = (sig) => {
    if (cleaningUpSignal) return;
    cleaningUpSignal = true;
    if (child && child.exitCode === null && child.signalCode === null) {
      try {
        child.kill(sig);
      } catch {
        // Child may have already exited.
      }
    }
    cleanupJsonTempDir();
    const sigNum = os.constants.signals[sig] ?? (sig === "SIGINT" ? 2 : 15);
    process.exit(128 + sigNum);
  };
  process.on("SIGINT", handleSignal);
  process.on("SIGTERM", handleSignal);

  try {
    const captureForJson = Boolean(json);
    child = spawn("bun", vitestArgs, {
      cwd: REPO_ROOT,
      env,
      stdio: captureForJson ? ["ignore", "pipe", "pipe"] : "inherit",
    });

    const stdoutChunks = [];
    const stderrChunks = [];
    if (captureForJson) {
      child.stdout?.on("data", (chunk) => stdoutChunks.push(chunk));
      child.stderr?.on("data", (chunk) => stderrChunks.push(chunk));
    }

    let code = await new Promise((resolve, reject) => {
      child.once("error", reject);
      child.once("close", (exitCode, signal) => {
        if (signal) resolve(1);
        else resolve(exitCode ?? 1);
      });
    });

    if (jsonOutputPath) {
      if (existsSync(jsonOutputPath)) {
        const rawJson = readFileSync(jsonOutputPath, "utf8");
        if (json) {
          process.stdout.write(rawJson.endsWith("\n") ? rawJson : `${rawJson}\n`);
        }
        try {
          const parsed = JSON.parse(rawJson);
          if (!list) {
            const passed = Number(parsed?.numPassedTests ?? 0);
            const failed = Number(parsed?.numFailedTests ?? 0);
            const executed = passed + failed;
            if (executed === 0) {
              console.error(
                "E2E runner failed: 0 tests executed (all tests skipped or unmatched by filter).",
              );
              code = 1;
            }
          }
        } catch (err) {
          console.error(
            `E2E runner failed to parse Vitest JSON report: ${err instanceof Error ? err.message : String(err)}`,
          );
          code = 1;
        }
        if (code !== 0 && captureForJson) {
          const errText = Buffer.concat(stderrChunks).toString("utf8");
          if (errText) process.stderr.write(errText);
        }
      } else {
        if (captureForJson) {
          const outText = Buffer.concat(stdoutChunks).toString("utf8");
          const errText = Buffer.concat(stderrChunks).toString("utf8");
          if (outText) process.stderr.write(outText);
          if (errText) process.stderr.write(errText);
        }
        if (code === 0) {
          console.error(
            list
              ? "E2E runner failed: Vitest did not emit a JSON test list."
              : "E2E runner failed: Vitest did not emit a test execution report.",
          );
          code = 1;
        }
      }
    }

    process.exitCode = code;
  } finally {
    process.off("SIGINT", handleSignal);
    process.off("SIGTERM", handleSignal);
    cleanupJsonTempDir();
  }
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
