#!/usr/bin/env node
/**
 * Fail when the linked / installed schema versions disagree.
 *
 * GitPulse vendors `devmap-store` and reads maps the CLI built. A silent drift
 * between those two is exactly how the panel went dead at schema 11 vs 19.
 * This check makes that a CI failure instead of a blank HealthPanel.
 *
 * When `devmap` is not on PATH the CLI half is reported as unavailable — never
 * as a pass. A check that could not run must not look like one that ran clean.
 *
 * Exit: 0 match · 1 mismatch or unavailable CLI when required · 2 usage error.
 */
import { execFileSync, spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const SCHEMA_RS = path.join(
  REPO,
  "src-tauri",
  "vendored",
  "devmap-store",
  "src",
  "schema.rs",
);

function usage() {
  return formatUsage({
    name: "check-vendor-schema",
    summary: "Pin vendored CURRENT_SCHEMA_VERSION against the installed devmap CLI.",
    flags: [
      { flag: "--allow-missing-cli", description: "Pass when `devmap` is not installed (CI without the CLI)" },
      { flag: "--json", description: "Machine-readable output" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 match · 1 mismatch · 2 usage / could not read",
  });
}

export function readVendoredSchema() {
  if (!existsSync(SCHEMA_RS)) {
    throw new Error(`missing ${SCHEMA_RS}; run npm run vendor`);
  }
  const text = readFileSync(SCHEMA_RS, "utf8");
  const match = /pub const CURRENT_SCHEMA_VERSION:\s*i32\s*=\s*(\d+)/.exec(text);
  if (!match) {
    throw new Error(`CURRENT_SCHEMA_VERSION not found in ${SCHEMA_RS}`);
  }
  return Number(match[1]);
}

function readCliSchema() {
  // Prefer `devmap doctor --json` (binary-declared expected_schema_version).
  // Fall back to `--version` prose only when doctor is absent (older installs).
  const doctor = spawnSync("devmap", ["doctor", "--json"], {
    encoding: "utf8",
    cwd: REPO,
    timeout: 30_000,
  });
  if (!doctor.error && doctor.status === 0) {
    try {
      const parsed = JSON.parse((doctor.stdout || "").trim().split("\n").pop() || "");
      if (typeof parsed.expected_schema_version === "number") {
        return {
          available: true,
          expected: parsed.expected_schema_version,
          versionLine: `doctor expected_schema_version ${parsed.expected_schema_version} (code_graph ${parsed.code_graph_schema_version ?? "?"}, version ${parsed.version ?? "?"})`,
          source: "doctor",
        };
      }
    } catch (e) {
      // Fall through to --version scrape.
    }
  }

  const which = spawnSync("devmap", ["--version"], { encoding: "utf8" });
  if (which.error || which.status !== 0) {
    return {
      available: false,
      reason: which.stderr?.trim() || which.error?.message || "devmap not found",
    };
  }
  const versionLine = (which.stdout || "").trim();
  const fromVersion = /store schema (\d+)/i.exec(versionLine);
  let expected = fromVersion ? Number(fromVersion[1]) : null;
  try {
    const status = execFileSync("devmap", ["status", "--json", "--progress", "never"], {
      encoding: "utf8",
      cwd: REPO,
      timeout: 30_000,
    });
    const parsed = JSON.parse(status);
    if (typeof parsed.expected_schema_version === "number") {
      expected = parsed.expected_schema_version;
    } else if (typeof parsed.schema_version === "number" && expected == null) {
      expected = parsed.schema_version;
    }
  } catch {
    // status may fail with no store; the --version line is still authoritative for the binary.
  }
  if (expected == null) {
    return { available: false, reason: `could not parse schema from: ${versionLine}` };
  }
  return { available: true, expected, versionLine, source: "version" };
}

/** @param {string[]} argv */
export function main(argv = process.argv.slice(2)) {
  if (wantsHelp(argv)) {
    console.log(usage());
    return 0;
  }
  const allowMissing = argv.includes("--allow-missing-cli");
  const asJson = argv.includes("--json");
  const unknown = argv.find(
    (a) => a !== "--allow-missing-cli" && a !== "--json" && a !== "--help" && a !== "-h",
  );
  if (unknown) {
    console.error(`FAIL: unknown option ${JSON.stringify(unknown)}\n`);
    console.error(usage());
    return 2;
  }

  let vendored;
  try {
    vendored = readVendoredSchema();
  } catch (e) {
    const msg = e instanceof Error ? e.message : String(e);
    if (asJson) console.log(JSON.stringify({ ok: false, error: msg }));
    else console.error(`FAIL: ${msg}`);
    return 2;
  }

  const cli = readCliSchema();
  const result = {
    ok: false,
    vendored,
    cli,
  };

  if (!cli.available) {
    result.ok = allowMissing;
    if (asJson) console.log(JSON.stringify(result));
    else if (allowMissing) {
      console.log(`OK: vendored schema ${vendored}; CLI unavailable (${cli.reason}) — allowed`);
    } else {
      console.error(`FAIL: vendored schema ${vendored}; CLI unavailable (${cli.reason})`);
      console.error("Install `devmap` or pass --allow-missing-cli");
    }
    return result.ok ? 0 : 1;
  }

  result.ok = cli.expected === vendored;
  if (asJson) {
    console.log(JSON.stringify(result));
  } else if (result.ok) {
    console.log(`OK: vendored schema ${vendored} matches CLI (${cli.versionLine})`);
  } else {
    console.error(
      `FAIL: vendored schema ${vendored} != CLI expected ${cli.expected} (${cli.versionLine})`,
    );
    console.error("Re-vendor from DevCouncil: GITPULSE_DEVCOUNCIL_ROOT=… npm run vendor");
  }
  return result.ok ? 0 : 1;
}

if (import.meta.url === `file://${process.argv[1]}` || process.argv[1]?.endsWith("check-vendor-schema.mjs")) {
  process.exitCode = main();
}
