import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readdirSync,
  readFileSync,
  realpathSync,
  rmSync,
  statSync,
} from "node:fs";
import net from "node:net";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const HERE = path.dirname(fileURLToPath(import.meta.url));
export const REPO_ROOT = path.resolve(HERE, "../../..");
export const DEFAULT_CARGO_TARGET_DIR = path.join(
  REPO_ROOT,
  ".gitpulse",
  "scratch",
  "target-e2e",
);

export function cargoTargetDir(): string {
  return process.env.CARGO_TARGET_DIR || DEFAULT_CARGO_TARGET_DIR;
}

export function defaultCargoBuildJobs(): string {
  const cpus = os.cpus()?.length || 4;
  return String(Math.max(1, Math.min(4, Math.floor(cpus / 2))));
}

export function buildCargoEnv(
  overrides: Record<string, string | undefined> = {},
): NodeJS.ProcessEnv {
  const env: NodeJS.ProcessEnv = {
    ...process.env,
    CARGO_TARGET_DIR: cargoTargetDir(),
    CARGO_BUILD_JOBS: process.env.CARGO_BUILD_JOBS || defaultCargoBuildJobs(),
  };
  for (const [key, value] of Object.entries(overrides)) {
    if (value === undefined) {
      delete env[key];
    } else {
      env[key] = value;
    }
  }
  return env;
}

export class TempDirManager {
  readonly dirs: string[] = [];

  create(prefix = "gp-e2e-", mode = 0o700): string {
    const base = os.tmpdir();
    const dir = mkdtempSync(path.join(base, prefix));
    chmodSync(dir, mode);
    this.dirs.push(dir);
    return dir;
  }

  /**
   * macOS limits Unix domain socket paths (`sun_path`) to 104 bytes, and
   * GitPulse's bridge rejects symlinked ancestors (such as `/tmp` -> `/private/tmp`).
   */
  createShortSocketDir(prefix = "gph-"): string {
    const base = existsSync("/tmp") ? realpathSync("/tmp") : realpathSync(os.tmpdir());
    const dir = mkdtempSync(path.join(base, prefix));
    chmodSync(dir, 0o700);
    this.dirs.push(dir);
    return dir;
  }

  cleanup(): void {
    while (this.dirs.length > 0) {
      const dir = this.dirs.pop();
      if (!dir) continue;
      rmSync(dir, { recursive: true, force: true, maxRetries: 3 });
    }
  }
}

let cachedHookBinaryPromise: Promise<string> | null = null;

function runProcess(
  command: string,
  args: string[],
  options: {
    cwd?: string;
    env?: NodeJS.ProcessEnv;
    stdin?: string;
    timeoutMs?: number;
  } = {},
): Promise<{ exitCode: number; signal: NodeJS.Signals | null; stdout: string; stderr: string }> {
  const timeoutMs = options.timeoutMs ?? 120_000;
  return new Promise((resolve, reject) => {
    const child = spawn(command, args, {
      cwd: options.cwd ?? REPO_ROOT,
      env: options.env ?? process.env,
      stdio: ["pipe", "pipe", "pipe"],
    });

    const stdoutChunks: Buffer[] = [];
    const stderrChunks: Buffer[] = [];
    let settled = false;
    let timer: ReturnType<typeof setTimeout> | undefined;

    if (timeoutMs > 0) {
      timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        child.kill("SIGKILL");
        reject(new Error(`Command timed out after ${timeoutMs}ms: ${command} ${args.join(" ")}`));
      }, timeoutMs);
    }

    child.stdout.on("data", (chunk: Buffer) => stdoutChunks.push(chunk));
    child.stderr.on("data", (chunk: Buffer) => stderrChunks.push(chunk));
    child.once("error", (err) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      reject(err);
    });
    child.once("close", (code, signal) => {
      if (settled) return;
      settled = true;
      if (timer) clearTimeout(timer);
      resolve({
        exitCode: code ?? (signal ? 1 : 0),
        signal,
        stdout: Buffer.concat(stdoutChunks).toString("utf8"),
        stderr: Buffer.concat(stderrChunks).toString("utf8"),
      });
    });

    if (options.stdin !== undefined) {
      child.stdin.write(options.stdin);
    }
    child.stdin.end();
  });
}

export async function ensureGitPulseHookBinary(): Promise<string> {
  if (!cachedHookBinaryPromise) {
    cachedHookBinaryPromise = (async () => {
      const targetDir = cargoTargetDir();
      const res = await runProcess(
        "cargo",
        [
          "build",
          "--manifest-path",
          "src-tauri/Cargo.toml",
          "--locked",
          "--bin",
          "gitpulse-hook",
        ],
        {
          env: buildCargoEnv({ CARGO_TARGET_DIR: targetDir }),
          timeoutMs: 300_000,
        },
      );
      if (res.exitCode !== 0) {
        cachedHookBinaryPromise = null;
        throw new Error(
          `Failed to build gitpulse-hook (exit ${res.exitCode}):\n${res.stderr || res.stdout}`,
        );
      }
      const ext = process.platform === "win32" ? ".exe" : "";
      const binPath = path.join(targetDir, "debug", `gitpulse-hook${ext}`);
      if (!existsSync(binPath)) {
        cachedHookBinaryPromise = null;
        throw new Error(`Built gitpulse-hook binary not found at ${binPath}`);
      }
      return binPath;
    })();
  }
  return cachedHookBinaryPromise;
}

export interface HookSocketRunOptions {
  args: string[];
  env?: Record<string, string | undefined>;
  stdinJson?: unknown;
  reply?: string;
  timeoutMs?: number;
}

export interface HookDropFile {
  path: string;
  content: string;
}

export interface HookSocketRunResult {
  exitCode: number;
  signal: NodeJS.Signals | null;
  stdout: string;
  stderr: string;
  receivedRaw: string[];
  receivedJson: Record<string, unknown>[];
  dropFiles: HookDropFile[];
  socketPath: string;
}

function collectFilesRecursive(dir: string): HookDropFile[] {
  if (!existsSync(dir)) return [];
  const results: HookDropFile[] = [];
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) {
      results.push(...collectFilesRecursive(full));
    } else if (entry.isFile()) {
      results.push({ path: full, content: readFileSync(full, "utf8") });
    }
  }
  return results;
}

export async function runGitPulseHookWithSocket(
  options: HookSocketRunOptions,
): Promise<HookSocketRunResult> {
  const binPath = await ensureGitPulseHookBinary();
  const temps = new TempDirManager();
  const dir = temps.createShortSocketDir("gph-");
  const sockPath = path.join(dir, "agent-notify.sock");
  const logDir = path.join(dir, "logs");
  mkdirSync(logDir, { mode: 0o700 });

  const receivedRaw: string[] = [];
  const receivedJson: Record<string, unknown>[] = [];
  const replyPayload = options.reply ?? '{"ok":true}\n';

  const server = net.createServer((socket) => {
    const chunks: Buffer[] = [];
    socket.on("data", (chunk: Buffer) => chunks.push(chunk));
    socket.on("end", () => {
      const text = Buffer.concat(chunks).toString("utf8");
      receivedRaw.push(text);
      for (const line of text.split(/\r?\n/)) {
        const trimmed = line.trim();
        if (!trimmed) continue;
        try {
          const parsed = JSON.parse(trimmed) as unknown;
          if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
            receivedJson.push(parsed as Record<string, unknown>);
          }
        } catch {
          // Leave raw line in receivedRaw if malformed JSON is sent.
        }
      }
      socket.end(replyPayload);
    });
  });

  try {
    await new Promise<void>((resolve, reject) => {
      server.once("error", reject);
      server.listen(sockPath, () => {
        try {
          chmodSync(sockPath, 0o600);
          resolve();
        } catch (err) {
          reject(err);
        }
      });
    });

    const stdinText =
      options.stdinJson === undefined
        ? undefined
        : typeof options.stdinJson === "string"
          ? options.stdinJson
          : JSON.stringify(options.stdinJson);

    const env = buildCargoEnv({
      GITPULSE_NOTIFY_SOCKET: sockPath,
      GITPULSE_LOG_DIR: logDir,
      ...(options.env ?? {}),
    });

    const proc = await runProcess(binPath, options.args, {
      cwd: REPO_ROOT,
      env,
      stdin: stdinText,
      timeoutMs: options.timeoutMs ?? 15_000,
    });

    const dropFiles = collectFilesRecursive(logDir);

    return {
      exitCode: proc.exitCode,
      signal: proc.signal,
      stdout: proc.stdout,
      stderr: proc.stderr,
      receivedRaw,
      receivedJson,
      dropFiles,
      socketPath: sockPath,
    };
  } finally {
    await new Promise<void>((resolve) => {
      server.close(() => resolve());
    });
    temps.cleanup();
  }
}

export interface CargoTestResult {
  ok: boolean;
  exitCode: number;
  stdout: string;
  stderr: string;
}

export async function runCargoTest(
  args: string[],
  options: { timeoutMs?: number; env?: Record<string, string | undefined> } = {},
): Promise<CargoTestResult> {
  const res = await runProcess(
    "cargo",
    ["test", "--manifest-path", "src-tauri/Cargo.toml", "--locked", ...args],
    {
      cwd: REPO_ROOT,
      env: buildCargoEnv(options.env),
      timeoutMs: options.timeoutMs ?? 180_000,
    },
  );
  return {
    ok: res.exitCode === 0,
    exitCode: res.exitCode,
    stdout: res.stdout,
    stderr: res.stderr,
  };
}

export interface PngInfo {
  width: number;
  height: number;
  bitDepth: number;
  colorType: number;
  compression: number;
  filter: number;
  interlace: number;
  byteLength: number;
  sha256: string;
}

const PNG_SIGNATURE = Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);

export function readPngInfo(filePath: string): PngInfo {
  const absPath = path.isAbsolute(filePath) ? filePath : path.resolve(REPO_ROOT, filePath);
  const data = readFileSync(absPath);
  if (data.length < 33 || !data.subarray(0, 8).equals(PNG_SIGNATURE)) {
    throw new Error(`Not a valid PNG file: ${absPath}`);
  }
  const ihdrType = data.subarray(12, 16).toString("ascii");
  if (ihdrType !== "IHDR") {
    throw new Error(`Missing PNG IHDR chunk in ${absPath} (found ${ihdrType})`);
  }
  return {
    width: data.readUInt32BE(16),
    height: data.readUInt32BE(20),
    bitDepth: data.readUInt8(24),
    colorType: data.readUInt8(25),
    compression: data.readUInt8(26),
    filter: data.readUInt8(27),
    interlace: data.readUInt8(28),
    byteLength: statSync(absPath).size,
    sha256: createHash("sha256").update(data).digest("hex"),
  };
}

export function repoPath(relPath: string): string {
  const resolved = path.resolve(REPO_ROOT, relPath);
  if (resolved !== REPO_ROOT && !resolved.startsWith(`${REPO_ROOT}${path.sep}`)) {
    throw new Error(`Path escapes repository root: ${relPath}`);
  }
  return resolved;
}

export function readRepoFile(relPath: string): string {
  return readFileSync(repoPath(relPath), "utf8");
}
