#!/usr/bin/env node
/**
 * Validate LCOV integrity and enforce the repository's coverage floors.
 *
 * File-existence checks are not enough: a truncated report, a forged summary,
 * or a report with no executable data can otherwise produce a green CI job.
 * This checker validates every record's DA/BRDA data against its summaries,
 * aggregates the report, and then applies explicit floors.
 *
 * Exit codes: 0 reports are valid and above the floors · 1 a floor is missed
 * · 2 a report or checker input is invalid.
 */
import { readFileSync, statSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const MAX_LCOV_BYTES = 512 * 1024 * 1024;
const INTEGER = /^\d+$/;

/** @typedef {{ found: number, hit: number }} CoverageMetric */
/** @typedef {{ startLine: number, sourceFile: string | null, dataLines: Map<number, { line: number, count: number }>, branches: Array<{ taken: number }>, lineFound: number | null, lineHit: number | null, branchFound: number | null, branchHit: number | null, functionFound: number | null, functionHit: number | null }} RawRecord */
/** @typedef {{ sourceFile: string, lines: CoverageMetric, branches: CoverageMetric | null }} CoverageRecord */
/** @typedef {{ records: CoverageRecord[], totals: { lines: CoverageMetric, branches: CoverageMetric | null }, branchRecords: number }} ParsedLcov */
/** @typedef {{ root: string, frontendPath: string | null, rustPath: string | null, frontendLines: number, frontendBranches: number, rustLines: number, help: boolean }} CliOptions */
/** @typedef {{ ok: true, invalid: false, parsed: ParsedLcov } | { ok: false, invalid: true, error: string }} CheckedReport */

export const DEFAULT_THRESHOLDS = Object.freeze({
  frontend: Object.freeze({ lines: 90, branches: 85 }),
  rust: Object.freeze({ lines: 80 }),
});

/** @param {unknown} error */
function errorMessage(error) {
  return error instanceof Error ? error.message : String(error);
}

/** @param {string} value @param {string} field @param {string} location */
function parseNonNegativeInteger(value, field, location) {
  if (!INTEGER.test(value)) {
    throw new Error(`${location}: ${field} must be a non-negative integer, got ${JSON.stringify(value)}`);
  }
  const parsed = Number(value);
  if (!Number.isSafeInteger(parsed)) {
    throw new Error(`${location}: ${field} is outside the safe integer range`);
  }
  return parsed;
}

/** @param {string} value @param {string} flag */
function parseThreshold(value, flag) {
  if (!/^\d+(?:\.\d+)?$/.test(value)) {
    throw new Error(`${flag} must be a number from 0 to 100, got ${JSON.stringify(value)}`);
  }
  const threshold = Number(value);
  if (!Number.isFinite(threshold) || threshold < 0 || threshold > 100) {
    throw new Error(`${flag} must be a number from 0 to 100, got ${JSON.stringify(value)}`);
  }
  return threshold;
}

/** @param {string} value @param {string} location @returns {{ line: number, count: number }} */
function parseDa(value, location) {
  const fields = value.split(",");
  if (fields.length < 2 || fields[0] === "" || fields[1] === "") {
    throw new Error(`${location}: DA must contain line and execution count`);
  }
  return {
    line: parseNonNegativeInteger(fields[0], "DA line", location),
    count: parseNonNegativeInteger(fields[1], "DA execution count", location),
  };
}

/** @param {string} value @param {string} location @returns {{ taken: number }} */
function parseBrda(value, location) {
  const fields = value.split(",");
  if (fields.length !== 4 || fields[0] === "" || fields[1] === "" || fields[2] === "") {
    throw new Error(`${location}: BRDA must contain line, block, branch, and taken fields`);
  }
  if (fields[0] !== "-") parseNonNegativeInteger(fields[0], "BRDA line", location);
  const taken = fields[3] === "-" ? 0 : parseNonNegativeInteger(fields[3], "BRDA taken", location);
  return { taken };
}

/** @param {number} startLine @returns {RawRecord} */
function newRecord(startLine) {
  return {
    startLine,
    sourceFile: null,
    dataLines: new Map(),
    branches: [],
    lineFound: null,
    lineHit: null,
    branchFound: null,
    branchHit: null,
    functionFound: null,
    functionHit: null,
  };
}

/** @param {RawRecord} record @param {string} reportName @param {Set<string>} sourceFiles @returns {CoverageRecord} */
function finishRecord(record, reportName, sourceFiles) {
  const location = `${reportName}:${record.startLine}`;
  if (!record.sourceFile) throw new Error(`${location}: record has no SF source file`);
  const sourceFile = record.sourceFile;
  if (sourceFiles.has(record.sourceFile)) {
    throw new Error(`${location}: duplicate SF source file ${JSON.stringify(record.sourceFile)}`);
  }
  sourceFiles.add(record.sourceFile);
  if (record.lineFound === null || record.lineHit === null) {
    throw new Error(`${location}: record must contain both LF and LH summaries`);
  }
  const lineFound = record.lineFound;
  const lineHit = record.lineHit;
  if (record.dataLines.size === 0 && lineFound > 0) {
    throw new Error(`${location}: record has no DA data records`);
  }
  if (lineFound < record.dataLines.size) {
    throw new Error(
      `${location}: LF ${lineFound} is less than ${record.dataLines.size} DA records`,
    );
  }
  const hitLines = [...record.dataLines.values()].filter(({ count }) => count > 0).length;
  if (lineHit < hitLines) {
    throw new Error(`${location}: LH cannot be below DA hit count (${lineHit} vs ${hitLines})`);
  }
  if (lineHit > lineFound) {
    throw new Error(`${location}: LH cannot exceed LF`);
  }

  const hasBranchSummary = record.branchFound !== null || record.branchHit !== null;
  /** @type {CoverageMetric | null} */
  let branchSummary = null;
  if (hasBranchSummary && (record.branchFound === null || record.branchHit === null)) {
    throw new Error(`${location}: record must contain both BRF and BRH summaries`);
  }
  if (!hasBranchSummary && record.branches.length > 0) {
    throw new Error(`${location}: BRDA data requires both BRF and BRH summaries`);
  }
  if (hasBranchSummary) {
    if (record.branchFound === null || record.branchHit === null) {
      throw new Error(`${location}: record must contain both BRF and BRH summaries`);
    }
    const branchFound = record.branchFound;
    const branchHit = record.branchHit;
    if (branchFound !== record.branches.length) {
      throw new Error(
        `${location}: BRF ${branchFound} does not match ${record.branches.length} BRDA records`,
      );
    }
    const hitBranches = record.branches.filter(({ taken }) => taken > 0).length;
    if (branchHit !== hitBranches) {
      throw new Error(`${location}: BRH does not match BRDA hit count (${branchHit} vs ${hitBranches})`);
    }
    if (branchHit > branchFound) {
      throw new Error(`${location}: BRH cannot exceed BRF`);
    }
    branchSummary = { found: branchFound, hit: branchHit };
  }
  if (record.functionFound !== null || record.functionHit !== null) {
    if (record.functionFound === null || record.functionHit === null) {
      throw new Error(`${location}: record must contain both FNF and FNH summaries`);
    }
    if (record.functionHit > record.functionFound) {
      throw new Error(`${location}: FNH cannot exceed FNF`);
    }
  }

  return {
    sourceFile,
    lines: { found: lineFound, hit: lineHit },
    branches: branchSummary,
  };
}

/**
 * Parse and validate an LCOV document.
 *
 * @param {string} source
 * @param {string} [reportName]
 * @returns {{ records: Array<{ sourceFile: string, lines: { found: number, hit: number }, branches: { found: number, hit: number } | null }>, totals: { lines: { found: number, hit: number }, branches: { found: number, hit: number } | null }, branchRecords: number }}
 */
export function parseLcov(source, reportName = "<lcov>") {
  if (typeof source !== "string" || source.length === 0) {
    throw new Error(`${reportName}: report is empty`);
  }
  /** @type {ReturnType<typeof newRecord> | null} */
  let record = null;
  /** @type {CoverageRecord[]} */
  const records = [];
  const sourceFiles = new Set();
  const lines = source.split("\n");

  const complete = () => {
    if (!record) throw new Error(`${reportName}: end_of_record without an open record`);
    records.push(finishRecord(record, reportName, sourceFiles));
    record = null;
  };

  for (let index = 0; index < lines.length; index += 1) {
    const lineNumber = index + 1;
    const line = lines[index].replace(/\r$/, "");
    if (line.trim() === "") continue;
    if (line.trim() === "end_of_record") {
      complete();
      continue;
    }
    const separator = line.indexOf(":");
    if (separator <= 0) throw new Error(`${reportName}:${lineNumber}: malformed LCOV field`);
    const key = line.slice(0, separator);
    const value = line.slice(separator + 1);
    if (!record) {
      if (key !== "TN" && key !== "SF") {
        throw new Error(`${reportName}:${lineNumber}: record must start with TN or SF`);
      }
      record = newRecord(lineNumber);
    }
    const location = `${reportName}:${lineNumber}`;
    switch (key) {
      case "TN":
        break;
      case "SF":
        if (record.sourceFile !== null) throw new Error(`${location}: duplicate SF field`);
        if (value.trim() === "") throw new Error(`${location}: SF source file is empty`);
        record.sourceFile = value;
        break;
      case "DA": {
        const data = parseDa(value, location);
        if (data.line === 0) throw new Error(`${location}: DA line must be greater than zero`);
        if (record.dataLines.has(data.line)) {
          throw new Error(`${location}: duplicate DA line ${data.line}`);
        }
        record.dataLines.set(data.line, data);
        break;
      }
      case "LF":
        if (record.lineFound !== null) throw new Error(`${location}: duplicate LF field`);
        record.lineFound = parseNonNegativeInteger(value, "LF", location);
        break;
      case "LH":
        if (record.lineHit !== null) throw new Error(`${location}: duplicate LH field`);
        record.lineHit = parseNonNegativeInteger(value, "LH", location);
        break;
      case "BRDA":
        record.branches.push(parseBrda(value, location));
        break;
      case "BRF":
        if (record.branchFound !== null) throw new Error(`${location}: duplicate BRF field`);
        record.branchFound = parseNonNegativeInteger(value, "BRF", location);
        break;
      case "BRH":
        if (record.branchHit !== null) throw new Error(`${location}: duplicate BRH field`);
        record.branchHit = parseNonNegativeInteger(value, "BRH", location);
        break;
      case "FN":
      case "FNDA":
        if (value.trim() === "") throw new Error(`${location}: ${key} value is empty`);
        break;
      case "FNF":
        if (record.functionFound !== null) throw new Error(`${location}: duplicate FNF field`);
        record.functionFound = parseNonNegativeInteger(value, "FNF", location);
        break;
      case "FNH":
        if (record.functionHit !== null) throw new Error(`${location}: duplicate FNH field`);
        record.functionHit = parseNonNegativeInteger(value, "FNH", location);
        break;
      default:
        throw new Error(`${location}: unsupported LCOV field ${JSON.stringify(key)}`);
    }
  }
  if (record) throw new Error(`${reportName}:${record.startLine}: unterminated LCOV record`);
  if (records.length === 0) throw new Error(`${reportName}: report has no complete records`);

  /** @type {{ lines: CoverageMetric, branches: CoverageMetric | null }} */
  const totals = {
    lines: records.reduce(
      (total, current) => ({
        found: total.found + current.lines.found,
        hit: total.hit + current.lines.hit,
      }),
      { found: 0, hit: 0 },
    ),
    branches: null,
  };
  const branchRecords = records.filter((current) => current.branches !== null).length;
  if (branchRecords > 0) {
    totals.branches = records.reduce(
      (total, current) => {
        if (!current.branches) return total;
        return {
          found: total.found + current.branches.found,
          hit: total.hit + current.branches.hit,
        };
      },
      { found: 0, hit: 0 },
    );
  }
  return { records, totals, branchRecords };
}

/** @param {CoverageMetric} metric */
function percentage(metric) {
  return metric.found > 0 ? (metric.hit / metric.found) * 100 : null;
}

/**
 * @param {ReturnType<typeof parseLcov>} report
 * @param {{ lines?: number, branches?: number }} thresholds
 */
export function evaluateCoverage(report, thresholds) {
  const failures = [];
  /** @type {Array<"lines" | "branches">} */
  const metricNames = ["lines", "branches"];
  for (const metricName of metricNames) {
    const threshold = thresholds[metricName];
    if (threshold === undefined) continue;
    const metric = report.totals[metricName];
    if (!metric) {
      failures.push(`${metricName} coverage is unavailable`);
      continue;
    }
    if (metricName === "branches" && report.branchRecords !== report.records.length) {
      failures.push(
        `branches are incomplete (${report.branchRecords}/${report.records.length} records contain branch summaries)`,
      );
      continue;
    }
    const actual = percentage(metric);
    if (actual === null) {
      failures.push(`${metricName} coverage is unavailable`);
      continue;
    }
    if (actual + Number.EPSILON < threshold) {
      failures.push(`${metricName} ${actual.toFixed(2)}% is below required ${threshold.toFixed(2)}%`);
    }
  }
  return { ok: failures.length === 0, failures };
}

/** @param {string} filePath @param {string} label @returns {CheckedReport} */
function readReport(filePath, label) {
  let stats;
  try {
    stats = statSync(filePath);
  } catch (error) {
    return { ok: false, invalid: true, error: `${label} report cannot be read: ${errorMessage(error)}` };
  }
  if (!stats.isFile()) return { ok: false, invalid: true, error: `${label} report is not a regular file` };
  if (stats.size === 0) return { ok: false, invalid: true, error: `${label} report is empty` };
  if (stats.size > MAX_LCOV_BYTES) {
    return {
      ok: false,
      invalid: true,
      error: `${label} report exceeds the ${MAX_LCOV_BYTES} byte safety limit`,
    };
  }
  try {
    const parsed = parseLcov(readFileSync(filePath, "utf8"), filePath);
    return { ok: true, invalid: false, parsed };
  } catch (error) {
    return { ok: false, invalid: true, error: errorMessage(error) };
  }
}

/** @param {string[]} argv @returns {CliOptions} */
function parseArgs(argv) {
  /** @type {CliOptions} */
  const options = {
    root: REPO_ROOT,
    frontendPath: null,
    rustPath: null,
    frontendLines: DEFAULT_THRESHOLDS.frontend.lines,
    frontendBranches: DEFAULT_THRESHOLDS.frontend.branches,
    rustLines: DEFAULT_THRESHOLDS.rust.lines,
    help: false,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const flag = argv[index];
    if (flag === "--help" || flag === "-h") {
      options.help = true;
      return options;
    }
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) {
      throw new Error(`${flag} requires a value`);
    }
    switch (flag) {
      case "--root":
        options.root = path.resolve(value);
        break;
      case "--frontend":
        options.frontendPath = path.resolve(value);
        break;
      case "--rust":
        options.rustPath = path.resolve(value);
        break;
      case "--frontend-lines":
        options.frontendLines = parseThreshold(value, flag);
        break;
      case "--frontend-branches":
        options.frontendBranches = parseThreshold(value, flag);
        break;
      case "--rust-lines":
        options.rustLines = parseThreshold(value, flag);
        break;
      default:
        throw new Error(`unknown option ${flag}`);
    }
    index += 1;
  }
  return options;
}

function printHelp() {
  console.log("Usage: node scripts/check-coverage-floor.mjs [options]");
  console.log("  --root <dir>                 scratch/report root (default: repository root)");
  console.log("  --frontend <path>            frontend LCOV path");
  console.log("  --rust <path>                Rust LCOV path");
  console.log("  --frontend-lines <percent>   frontend line floor (default: 90)");
  console.log("  --frontend-branches <percent> frontend branch floor (default: 85)");
  console.log("  --rust-lines <percent>       Rust line floor (default: 80)");
}

/** @param {CoverageMetric} metric */
function formatMetric(metric) {
  const actual = percentage(metric);
  return `${actual === null ? "unavailable" : `${actual.toFixed(2)}%`} (${metric.hit}/${metric.found})`;
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  let options;
  try {
    options = parseArgs(argv);
  } catch (error) {
    console.error(`FAIL: coverage checker input: ${errorMessage(error)}`);
    return 2;
  }
  if (options.help) {
    printHelp();
    return 0;
  }
  const config = options;

  /** @type {Array<{ label: string, path: string, thresholds: { lines?: number, branches?: number } }>} */
  const reports = [
    {
      label: "Frontend",
      path: config.frontendPath ?? path.join(config.root, "coverage", "lcov.info"),
      thresholds: { lines: config.frontendLines, branches: config.frontendBranches },
    },
    {
      label: "Rust",
      path: config.rustPath ?? path.join(config.root, "lcov.info"),
      thresholds: { lines: config.rustLines },
    },
  ];
  let invalid = false;
  let belowFloor = false;
  for (const report of reports) {
    const checked = readReport(report.path, report.label);
    if (!checked.ok) {
      invalid = true;
      console.error(`FAIL: ${report.label} invalid: ${checked.error}`);
      continue;
    }
    const evaluation = evaluateCoverage(checked.parsed, report.thresholds);
    /** @type {Array<"lines" | "branches">} */
    const metricNames = ["lines", "branches"];
    for (const metricName of metricNames) {
      const metric = checked.parsed.totals[metricName];
      const threshold = report.thresholds[metricName];
      if (threshold === undefined) continue;
      console.log(
        `${report.label} ${metricName} ${metric ? formatMetric(metric) : "unavailable"} (required >= ${threshold.toFixed(2)}%)`,
      );
    }
    for (const failure of evaluation.failures) {
      belowFloor = true;
      console.error(`FAIL: ${report.label} ${failure}`);
    }
  }
  if (invalid) return 2;
  if (belowFloor) return 1;
  console.log("OK: coverage floors hold");
  return 0;
}

const invokedPath = process.argv[1] ? path.resolve(process.argv[1]) : "";
if (invokedPath === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
