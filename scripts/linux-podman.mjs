#!/usr/bin/env node
/**
 * Local Podman Linux test harness for GitPulse (Ubuntu 22.04 release floor).
 *
 * Runs Linux native checks (GTK3/WebKitGTK 4.1 runtime, 47-harness WebKitGTK
 * regressions, real-binary `tauri-driver` E2E + screenshots, Rust clippy, and
 * `cargo nextest` suites) inside the host's Podman machine (e.g. macOS
 * `libkrun`), using named Podman volumes for `CARGO_TARGET_DIR`, `CARGO_HOME`,
 * `node_modules`, `dist`, and Bun cache so macOS host build artifacts and
 * native addons are never clobbered.
 *
 * Exit codes: 0 passed · 1 container test command failed · 2 missing Podman /
 * stopped Podman machine / invalid arguments.
 */

import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { formatUsage, wantsHelp } from "./usage.mjs";

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
export const DEFAULT_IMAGE = "localhost/gitpulse-linux-test:ubuntu22.04";
export const CONTAINERFILE = path.join(REPO_ROOT, "Containerfile.linux-test");

export const VALID_MODES = Object.freeze([
  "build-image",
  "gtk-runtime",
  "webview",
  "e2e",
  "clippy",
  "rust-test",
  "shell",
]);

export const VALID_DISPLAYS = Object.freeze(["x11", "wayland", "both"]);

/**
 * Named volumes that keep Linux Cargo artifacts, crate cache, and Linux
 * `node_modules`/`dist` completely separate from the macOS host checkout.
 * @param {string} [prefix]
 */
export function volumeMounts(prefix = "gitpulse-linux") {
  return [
    `${prefix}-target:/cargo-target`,
    `${prefix}-cargo:/cargo-home`,
    `${prefix}-node-modules:/workspace/node_modules`,
    `${prefix}-dist:/workspace/dist`,
    `${prefix}-bun-cache:/bun-cache`,
  ];
}

/**
 * @typedef {{
 *   mode: string,
 *   display: "x11" | "wayland" | "both",
 *   image: string,
 *   platform: string | null,
 *   harness: string | null,
 *   allHarnesses: boolean,
 *   filter: string | null,
 *   shard: string | null,
 *   libOnly: boolean,
 *   testTargets: string[],
 *   artifactsDir: string,
 *   rebuildImage: boolean,
 *   help: boolean,
 * }} PodmanOptions
 */

/**
 * @param {string[]} argv
 * @param {string} [repoRoot]
 * @returns {PodmanOptions}
 */
export function parseArgs(argv, repoRoot = REPO_ROOT) {
  /** @type {PodmanOptions} */
  const opts = {
    mode: "gtk-runtime",
    display: "x11",
    image: process.env.GITPULSE_LINUX_IMAGE || DEFAULT_IMAGE,
    platform: null,
    harness: null,
    allHarnesses: false,
    filter: null,
    shard: null,
    libOnly: false,
    testTargets: [],
    artifactsDir: path.join(repoRoot, ".gitpulse", "scratch", "e2e-artifacts"),
    rebuildImage: false,
    help: wantsHelp(argv),
  };
  if (opts.help) return opts;

  let modeSet = false;
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (!arg.startsWith("-") && !modeSet) {
      if (!VALID_MODES.includes(arg)) {
        throw new Error(`Unknown mode ${JSON.stringify(arg)}; expected one of ${VALID_MODES.join(", ")}`);
      }
      opts.mode = arg;
      modeSet = true;
      continue;
    }
    if (arg === "--all") {
      opts.allHarnesses = true;
      continue;
    }
    if (arg === "--rebuild-image") {
      opts.rebuildImage = true;
      continue;
    }
    if (arg === "--lib") {
      opts.libOnly = true;
      continue;
    }
    const next = () => {
      const value = argv[++i];
      if (value === undefined || value.startsWith("--")) throw new Error(`${arg} requires a value`);
      return value;
    };
    if (arg === "--display") {
      const value = next();
      if (!VALID_DISPLAYS.includes(value)) {
        throw new Error(`Invalid --display ${JSON.stringify(value)}; expected x11, wayland, or both`);
      }
      opts.display = /** @type {"x11" | "wayland" | "both"} */ (value);
    } else if (arg === "--image") {
      opts.image = next();
    } else if (arg === "--platform") {
      const value = next();
      if (!/^linux\/(?:arm64|amd64)$/.test(value)) {
        throw new Error(`Invalid --platform ${JSON.stringify(value)}; expected linux/arm64 or linux/amd64`);
      }
      opts.platform = value;
    } else if (arg === "--harness") {
      opts.harness = next();
    } else if (arg === "--filter") {
      opts.filter = next();
    } else if (arg === "--test") {
      opts.testTargets.push(next());
    } else if (arg === "--shard") {
      const value = next();
      const match = /^(\d+)\/(\d+)$/.exec(value);
      const index = match ? Number(match[1]) : 0;
      const total = match ? Number(match[2]) : 0;
      if (!match || index < 1 || total < 1 || index > total) {
        throw new Error("--shard must be formatted as M/N with 1 <= M <= N (e.g. 1/3)");
      }
      opts.shard = value;
    } else if (arg === "--artifacts") {
      opts.artifactsDir = path.resolve(next());
    } else {
      throw new Error(`Unknown argument: ${arg}`);
    }
  }
  return opts;
}

export function usage() {
  return formatUsage({
    name: "linux-podman",
    summary: "Run GitPulse Linux tests (Ubuntu 22.04 floor, WebKitGTK 4.1, X11 + Wayland, tauri-driver) inside Podman.",
    flags: [
      { flag: "<mode>", description: `One of: ${VALID_MODES.join(" | ")} (default: gtk-runtime)` },
      { flag: "--display <mode>", description: "Display server for GUI modes: x11 | wayland | both (default: x11)" },
      { flag: "--harness <name>", description: "Harness name for `webview` mode (default: diagnostics)" },
      { flag: "--all", description: "Run all registered browser harnesses in `webview` mode" },
      { flag: "--filter <expr>", description: "cargo-nextest filter expression for `rust-test` mode" },
      { flag: "--lib", description: "Compile and run only the library test target in `rust-test` mode" },
      { flag: "--test <name>", description: "Compile and run a specific integration test target in `rust-test` mode (repeatable)" },
      { flag: "--shard <M/N>", description: "Run nextest partition hash:M/N in `rust-test` mode" },
      { flag: "--platform <arch>", description: "Override container arch: linux/arm64 or linux/amd64" },
      { flag: "--artifacts <dir>", description: "Host output dir for E2E screenshots (default: .gitpulse/scratch/e2e-artifacts)" },
      { flag: "--image <tag>", description: `Container image tag (default: ${DEFAULT_IMAGE})` },
      { flag: "--rebuild-image", description: "Force rebuilding the container image before running" },
      { flag: "--help, -h", description: "Show this message" },
    ],
    exits: "0 passed · 1 test command failed · 2 podman unavailable or invalid options",
  });
}

/**
 * Shell-quotes one argument for safe embedding inside `bash -lc`.
 * @param {string} value
 */
export function shellQuote(value) {
  return `'${value.replace(/'/g, "'\\''")}'`;
}

/**
 * Builds the inner Linux shell command for a given mode and display backend.
 * @param {PodmanOptions} opts
 * @param {"x11" | "wayland"} display
 * @returns {string}
 */
export function buildContainerScript(opts, display) {
  const syncFrontend = [
    "if [ ! -f /workspace/node_modules/.gitpulse-bun-lock.sha ] || [ \"$(sha256sum /workspace/bun.lock | awk '{print $1}')\" != \"$(cat /workspace/node_modules/.gitpulse-bun-lock.sha)\" ]; then",
    "  bun install --frozen-lockfile",
    "  sha256sum /workspace/bun.lock | awk '{print $1}' > /workspace/node_modules/.gitpulse-bun-lock.sha",
    "fi",
  ].join("\n");
  const ensureDist = [
    syncFrontend,
    "if [ ! -f /workspace/dist/index.html ]; then bun run build; fi",
  ].join("\n");
  const buildDistFresh = [
    syncFrontend,
    "bun run build",
  ].join("\n");

  switch (opts.mode) {
    case "gtk-runtime":
      return [
        "set -euo pipefail",
        ensureDist,
        "cargo test --manifest-path src-tauri/Cargo.toml --locked --test gtk_runtime --no-run",
        `with-display ${display} cargo test --manifest-path src-tauri/Cargo.toml --locked --test gtk_runtime -- --nocapture`,
      ].join("\n");

    case "webview": {
      const harnessArgs = opts.allHarnesses
        ? "--native --all"
        : `--native --harness ${shellQuote(opts.harness ?? "diagnostics")}`;
      return [
        "set -euo pipefail",
        ensureDist,
        "cargo build --manifest-path src-tauri/Cargo.toml --locked --example webview_regressions",
        // Stage frontend sources onto native Linux tmpfs so Vite + Tailwind never
        // crawl thousands of files across macOS -> Linux VirtioFS (~80x faster).
        "rm -rf /tmp/gp-web && mkdir -p /tmp/gp-web/src-tauri",
        "cp -a /workspace/src /workspace/harness /workspace/public /workspace/scripts /workspace/package.json /workspace/vite.config.ts /workspace/svelte.config.js /workspace/tsconfig*.json /workspace/index.html /workspace/status.html /tmp/gp-web/",
        "cp -a /workspace/src-tauri/tauri.conf.json /workspace/src-tauri/Cargo.toml /workspace/src-tauri/vendored /tmp/gp-web/src-tauri/",
        "ln -s /workspace/src-tauri/src /tmp/gp-web/src-tauri/src",
        "ln -s /workspace/node_modules /tmp/gp-web/node_modules",
        `cd /tmp/gp-web && WEBKIT_DISABLE_DMABUF_RENDERER="\${WEBKIT_DISABLE_DMABUF_RENDERER:-0}" WEBKIT_DMABUF_RENDERER_FORCE_SHM="\${WEBKIT_DMABUF_RENDERER_FORCE_SHM:-1}" WEBKIT_SKIA_ENABLE_CPU_RENDERING="\${WEBKIT_SKIA_ENABLE_CPU_RENDERING:-1}" GITPULSE_VITE_CACHE_DIR=/tmp/gp-web/.vite-cache GITPULSE_WEBVIEW_RUNNER_BIN=/cargo-target/debug/examples/webview_regressions with-display ${display} node scripts/browser-regressions.mjs ${harnessArgs}`,
      ].join("\n");
    }

    case "e2e":
      return [
        "set -euo pipefail",
        buildDistFresh,
        "cargo build --manifest-path src-tauri/Cargo.toml --locked --features tauri/custom-protocol --bin gitpulse --bin gitpulse-ptyd --example e2e_seed_trust",
        `with-display ${display} node scripts/e2e-driver-smoke.mjs --artifacts /e2e-artifacts/linux-${display}`,
      ].join("\n");

    case "clippy":
      return [
        "set -euo pipefail",
        ensureDist,
        "cargo fmt --manifest-path src-tauri/Cargo.toml --all -- --check",
        "cargo clippy --manifest-path src-tauri/Cargo.toml --locked --all-targets -- -D warnings",
      ].join("\n");

    case "rust-test": {
      const extra = [];
      if (opts.libOnly) extra.push("--lib");
      for (const target of opts.testTargets) {
        extra.push("--test", shellQuote(target));
      }
      if (opts.shard) extra.push("--partition", shellQuote(`hash:${opts.shard}`));
      if (opts.filter) extra.push("-E", shellQuote(opts.filter));
      return [
        "set -euo pipefail",
        ensureDist,
        `with-display ${display} cargo nextest run --manifest-path src-tauri/Cargo.toml --locked --profile ci --no-fail-fast ${extra.join(" ")}`.trim(),
      ].join("\n");
    }

    default:
      throw new Error(`No script for mode ${opts.mode}`);
  }
}

/**
 * Builds the `podman run` argv for executing one script inside the container.
 * @param {PodmanOptions} opts
 * @param {string} script
 * @param {string} [repoRoot]
 */
export function buildPodmanRunArgs(opts, script, repoRoot = REPO_ROOT) {
  const prefix = opts.platform === "linux/amd64" ? "gitpulse-linux-amd64" : "gitpulse-linux";
  const buildJobs = process.env.CARGO_BUILD_JOBS || "4";
  const args = [
    "run",
    "--rm",
    "--init",
    "--shm-size=2g",
    // Drop root's DAC bypass so Linux permission-denial unit tests (chmod 0555/0000)
    // behave like GitHub Actions' non-root `runner` without changing volume ownership.
    "--cap-drop=DAC_OVERRIDE",
    "--cap-drop=DAC_READ_SEARCH",
    "-e",
    `CARGO_BUILD_JOBS=${buildJobs}`,
  ];
  if (opts.mode === "shell") args.push("-it");
  if (opts.platform) args.push("--platform", opts.platform);
  args.push("-v", `${repoRoot}:/workspace`);
  for (const vol of volumeMounts(prefix)) {
    args.push("-v", vol);
  }
  if (opts.mode === "e2e") {
    args.push("-v", `${opts.artifactsDir}:/e2e-artifacts`);
  }
  args.push("-w", "/workspace", opts.image);
  if (opts.mode === "shell") {
    args.push("bash");
  } else {
    args.push("bash", "-lc", script);
  }
  return args;
}

/** @param {string[]} args */
function runPodman(args, capture = false) {
  return spawnSync("podman", args, {
    cwd: REPO_ROOT,
    stdio: capture ? ["ignore", "pipe", "pipe"] : "inherit",
    encoding: "utf8",
  });
}

/** @param {PodmanOptions} opts */
function ensurePodmanReady(opts) {
  const probe = runPodman(["info", "--format", "{{.Host.Arch}}"], true);
  if (probe.error) {
    console.error(`FAIL: podman could not run (${probe.error.message}). Install Podman and start its machine with \`podman machine start\`.`);
    return false;
  }
  if (probe.status !== 0) {
    console.error(`FAIL: podman machine is not reachable (${(probe.stderr || probe.stdout).trim()}). Start it with \`podman machine start\`.`);
    return false;
  }
  const hasImage = runPodman(["image", "exists", opts.image], true).status === 0;
  if (opts.mode === "build-image" || opts.rebuildImage || !hasImage) {
    if (!existsSync(CONTAINERFILE)) {
      console.error(`FAIL: Containerfile missing at ${CONTAINERFILE}`);
      return false;
    }
    const buildArgs = ["build", "-f", CONTAINERFILE, "-t", opts.image];
    if (opts.platform) buildArgs.push("--platform", opts.platform);
    buildArgs.push(REPO_ROOT);
    console.log(`Building ${opts.image} from Containerfile.linux-test...`);
    const built = runPodman(buildArgs);
    if (built.error || built.status !== 0) {
      console.error(`FAIL: podman build exited ${built.status ?? "error"}`);
      return false;
    }
  }
  return true;
}

/** @param {string[]} [argv] */
export function main(argv = process.argv.slice(2)) {
  /** @type {PodmanOptions} */
  let opts;
  try {
    opts = parseArgs(argv);
  } catch (error) {
    console.error(`linux-podman: ${error instanceof Error ? error.message : String(error)}`);
    return 2;
  }
  if (opts.help) {
    console.log(usage());
    return 0;
  }
  if (!ensurePodmanReady(opts)) return 2;
  if (opts.mode === "build-image") {
    console.log(`OK: built ${opts.image}`);
    return 0;
  }
  if (opts.mode === "e2e") mkdirSync(opts.artifactsDir, { recursive: true });
  if (opts.mode === "shell") {
    const res = runPodman(buildPodmanRunArgs(opts, ""));
    return res.status ?? 1;
  }

  /** @type {Array<"x11" | "wayland">} */
  const displays = opts.mode === "clippy"
    ? ["x11"]
    : opts.display === "both"
      ? ["x11", "wayland"]
      : [opts.display];

  for (const display of displays) {
    console.log(`=== Linux Podman [${opts.mode}] (display: ${display}) ===`);
    const script = buildContainerScript(opts, display);
    const res = runPodman(buildPodmanRunArgs(opts, script));
    if (res.error || res.status !== 0) {
      console.error(`FAIL: linux-podman [${opts.mode}] (${display}) exited ${res.status ?? "error"}`);
      return 1;
    }
  }
  console.log(`OK: linux-podman [${opts.mode}] passed (${displays.join(", ")})`);
  return 0;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  process.exitCode = main();
}
