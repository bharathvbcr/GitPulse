#!/usr/bin/env node
/**
 * Canonical CLI runner for GitPulse R1–R4 opaque-box E2E tests.
 *
 * Usage:
 *   node tests/e2e-r1-r4/run.mjs [--tier 1|2|3|4] [--req R1|R2|R3|R4] [--json] [--list]
 */
import { spawn } from "node:child_process";
import { readdirSync } from "node:fs";
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
  const passthrough = [];

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === "--help" || arg === "-h") {
      help = true;
    } else if (arg === "--json") {
      json = true;
    } else if (arg === "--list") {
      list = true;
    } else if (arg === "--tier" || arg.startsWith("--tier=")) {
      const value = arg.includes("=") ? arg.split("=", 2)[1] : argv[++i];
      if (!value || !["1", "2", "3", "4"].includes(value)) {
        throw new Error(`Invalid --tier value "${value ?? ""}"; expected 1, 2, 3, or 4`);
      }
      tier = value;
    } else if (arg === "--req" || arg.startsWith("--req=")) {
      const value = (arg.includes("=") ? arg.split("=", 2)[1] : argv[++i])?.toUpperCase();
      if (!value || !["R1", "R2", "R3", "R4"].includes(value)) {
        throw new Error(`Invalid --req value "${value ?? ""}"; expected R1, R2, R3, or R4`);
      }
      req = value;
    } else {
      passthrough.push(arg);
    }
  }

  return { tier, req, json, list, help, passthrough };
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
  };

  const allFiles = collectTestFiles(HERE);
  const matchingFiles = tier
    ? allFiles.filter((file) => path.basename(file).startsWith(`tier${tier}-`))
    : allFiles;

  if (list && matchingFiles.length === 0) {
    console.log(
      tier
        ? `No E2E test files found for tier ${tier} under tests/e2e-r1-r4/.`
        : "No E2E test files found under tests/e2e-r1-r4/.",
    );
    return;
  }

  const vitestArgs = [
    "x",
    "vitest",
    list ? "list" : "run",
    "--config",
    CONFIG_REL,
  ];

  if (json && !list) {
    vitestArgs.push("--reporter=json");
  }
  if (req) {
    vitestArgs.push("-t", `\\b${req}\\b`);
  }
  if (tier) {
    if (matchingFiles.length === 0) {
      vitestArgs.push(`tests/e2e-r1-r4/tier${tier}-`);
    } else {
      vitestArgs.push(...matchingFiles);
    }
  }
  vitestArgs.push(...passthrough);

  const child = spawn("bun", vitestArgs, {
    cwd: REPO_ROOT,
    env,
    stdio: "inherit",
  });

  const code = await new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (exitCode, signal) => {
      if (signal) resolve(1);
      else resolve(exitCode ?? 1);
    });
  });

  process.exitCode = code;
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : String(error));
  process.exitCode = 1;
});
