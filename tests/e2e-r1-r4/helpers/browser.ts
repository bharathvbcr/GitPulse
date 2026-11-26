import { spawn, type ChildProcess } from "node:child_process";
import { randomUUID } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { createServer, type Logger, type Plugin, type ViteDevServer } from "vite";
import { REPO_ROOT } from "./env";

export interface BrowserCheckRow {
  name: string;
  pass: boolean;
  detail?: string;
}

export interface BrowserVerdict<TData = Record<string, unknown>> {
  results: BrowserCheckRow[];
  passed: number;
  total: number;
  crashes: string[];
  unexpected: string[];
  errors: string[];
  diagnostics?: string;
  data?: TData;
}

export interface RunBrowserPageOptions {
  /** Served path relative to REPO_ROOT, e.g. `/tests/e2e-r1-r4/browser/notifications-logo.html`. */
  page: string;
  /** Optional query parameters appended to `?check=1&report=...`. */
  query?: Record<string, string | number | boolean> | string;
  deadlineSeconds?: number;
  windowSize?: { width: number; height: number };
  chromeArgs?: string[];
}

export function chromeBinary(): string {
  const bin =
    process.env.CHROME_BIN ||
    (process.platform === "darwin"
      ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
      : process.platform === "win32"
        ? "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe"
        : "google-chrome");

  if (path.isAbsolute(bin) && !existsSync(bin)) {
    throw new Error(
      `Headless Chrome binary not found at "${bin}". Install Google Chrome or set CHROME_BIN.`,
    );
  }
  return bin;
}

const WATCHDOG_SCRIPT = `
const runner = Number(process.argv[1]), chrome = Number(process.argv[2]), profile = process.argv[3];
if (!Number.isInteger(chrome) || chrome <= 1 || chrome === process.pid) process.exit(0);
const alive = (pid) => {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e && e.code === "EPERM";
  }
};
const readProcess = (pid) => {
  try {
    const out = require("node:child_process").execFileSync("ps", ["-o", "pgid=,command=", "-p", String(pid)], { encoding: "utf8" }).trim();
    const match = /^(\\d+)\\s+(.*)$/s.exec(out);
    return match ? { pgid: Number(match[1]), command: match[2] } : null;
  } catch {
    return null;
  }
};
const tick = setInterval(() => {
  if (alive(runner)) {
    if (!alive(chrome)) process.exit(0);
    return;
  }
  clearInterval(tick);
  const info = readProcess(chrome);
  if (info && info.command.includes(profile)) {
    const target = process.platform !== "win32" && info.pgid === chrome ? -chrome : chrome;
    try { process.kill(target, "SIGKILL"); } catch {}
  }
  try { require("node:fs").rmSync(profile, { recursive: true, force: true, maxRetries: 5 }); } catch {}
  process.exit(0);
}, 200);
`;

function startChrome(
  chrome: string,
  profile: string,
  url: string,
  windowSize: { width: number; height: number },
  extraArgs: string[],
): ChildProcess {
  const args = [
    "--headless=new",
    `--user-data-dir=${profile}`,
    "--no-first-run",
    "--no-default-browser-check",
    "--disable-background-networking",
    "--disable-gpu",
    "--enable-unsafe-swiftshader",
    "--no-sandbox",
    `--window-size=${windowSize.width},${windowSize.height}`,
    ...extraArgs,
    url,
  ];
  const child = spawn(chrome, args, {
    stdio: ["ignore", "ignore", "pipe"],
    detached: process.platform !== "win32",
  });
  if (child.pid) {
    spawn(
      process.execPath,
      ["-e", WATCHDOG_SCRIPT, String(process.pid), String(child.pid), profile],
      { detached: true, stdio: "ignore" },
    ).unref();
  }
  return child;
}

function parseVerdictPayload<TData>(rawBody: string): BrowserVerdict<TData> {
  const encoded = /data-gp-result="([^"]*)"/.exec(rawBody)?.[1];
  let parsed: unknown;
  if (encoded) {
    parsed = JSON.parse(decodeURIComponent(encoded));
  } else {
    // Allow direct JSON body fallback if a test page ever posts raw JSON.
    parsed = JSON.parse(rawBody);
  }
  if (!parsed || typeof parsed !== "object" || !Array.isArray((parsed as { results?: unknown }).results)) {
    throw new Error(`Invalid browser verdict payload: ${rawBody.slice(0, 500)}`);
  }
  const obj = parsed as Partial<BrowserVerdict<TData>> & { results: BrowserCheckRow[] };
  const results = obj.results;
  const passed = typeof obj.passed === "number" ? obj.passed : results.filter((r) => r.pass).length;
  const total = typeof obj.total === "number" ? obj.total : results.length;
  return {
    results,
    passed,
    total,
    crashes: Array.isArray(obj.crashes) ? obj.crashes : [],
    unexpected: Array.isArray(obj.unexpected) ? obj.unexpected : [],
    errors: Array.isArray(obj.errors) ? obj.errors : [],
    ...(typeof obj.diagnostics === "string" ? { diagnostics: obj.diagnostics } : {}),
    ...(obj.data !== undefined ? { data: obj.data } : {}),
  };
}

function buildQueryString(
  reportPath: string,
  query?: Record<string, string | number | boolean> | string,
): string {
  const params = new URLSearchParams();
  params.set("check", "1");
  params.set("report", reportPath);
  if (typeof query === "string") {
    const extra = new URLSearchParams(query.replace(/^\?/, ""));
    for (const [k, v] of extra.entries()) params.set(k, v);
  } else if (query) {
    for (const [k, v] of Object.entries(query)) {
      params.set(k, String(v));
    }
  }
  return params.toString();
}

function createBufferedSilentLogger(viteLogs: string[]): Logger {
  const loggedErrors = new WeakSet<Error | object>();
  const warnedMessages = new Set<string>();
  const pushLog = (level: "warn" | "error", msg: string) => {
    if (viteLogs.length >= 20) return;
    const clean = msg.replace(/\u001b\[[0-9;]*m/g, "").trim();
    if (clean) viteLogs.push(`[${level}] ${clean}`);
  };
  const logger: Logger = {
    hasWarned: false,
    info() {},
    warn(msg) {
      logger.hasWarned = true;
      pushLog("warn", msg);
    },
    warnOnce(msg) {
      if (warnedMessages.has(msg)) return;
      warnedMessages.add(msg);
      logger.hasWarned = true;
      pushLog("warn", msg);
    },
    error(msg, options) {
      if (options?.error) loggedErrors.add(options.error);
      const detail = options?.error?.message ? `${msg} (${options.error.message})` : msg;
      pushLog("error", detail);
    },
    clearScreen() {},
    hasErrorLogged(error) {
      return loggedErrors.has(error);
    },
  };
  return logger;
}

async function removeProfileWithRetry(dir: string, waitForViteSettle = false): Promise<void> {
  const maxMs = waitForViteSettle ? 4000 : 2500;
  const pollMs = waitForViteSettle ? 200 : 150;
  const requiredQuietChecks = waitForViteSettle ? 3 : 2;
  const deadline = Date.now() + maxMs;
  let quietChecks = 0;

  while (Date.now() <= deadline) {
    if (existsSync(dir)) {
      quietChecks = 0;
      try {
        await rm(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 50 });
      } catch {
        // Retry if Chrome or Vite background optimizeDeps raced with rm.
      }
    } else {
      quietChecks += 1;
      if (quietChecks >= requiredQuietChecks) {
        return;
      }
    }
    await new Promise((resolve) => setTimeout(resolve, pollMs));
  }

  if (existsSync(dir)) {
    await rm(dir, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 }).catch(() => {});
  }
}

export async function runBrowserPage<TData = Record<string, unknown>>(
  options: RunBrowserPageOptions,
): Promise<BrowserVerdict<TData>> {
  const chrome = chromeBinary();
  const pagePath = options.page.startsWith("/") ? options.page : `/${options.page}`;
  const deadlineSeconds = options.deadlineSeconds ?? 60;
  const windowSize = options.windowSize ?? { width: 1280, height: 820 };
  const profile = await mkdtemp(path.join(tmpdir(), "gitpulse-e2e-browser-"));
  const reportPath = `/__gp_result/${randomUUID()}`;

  let server: ViteDevServer | undefined;
  let browser: ChildProcess | undefined;
  let browserSpawnError = false;
  let browserClosed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let requests = 0;
  let completedRequests = 0;
  let lastRequest = "none";
  const httpErrors: string[] = [];
  const viteLogs: string[] = [];
  const chromeStderr: Buffer[] = [];

  const formatDiagnostics = () => {
    const stderrText = Buffer.concat(chromeStderr).toString("utf8").trim();
    const parts: string[] = [];
    if (httpErrors.length > 0) {
      parts.push(`HTTP errors: ${httpErrors.join(", ")}`);
    }
    if (viteLogs.length > 0) {
      parts.push(`Vite logs:\n${viteLogs.join("\n")}`);
    }
    if (stderrText) {
      parts.push(`Chrome stderr:\n${stderrText}`);
    }
    return parts.length > 0 ? `\n${parts.join("\n")}` : "";
  };

  let deliver: (body: string) => void = () => {};
  let fail: (err: Error) => void = () => {};
  const verdict = new Promise<string>((resolve, reject) => {
    deliver = resolve;
    fail = reject;
    timer = setTimeout(() => {
      reject(
        new Error(
          `Browser page ${pagePath} did not report within ${deadlineSeconds}s (${requests} HTTP requests, ${completedRequests} completed; last: ${lastRequest})${formatDiagnostics()}`,
        ),
      );
    }, deadlineSeconds * 1000);
  });
  void verdict.catch(() => {});

  const receiver: Plugin = {
    name: "gitpulse-e2e-browser-verdict",
    configureServer(srv) {
      srv.middlewares.use((req, res, next) => {
        if (req.url === "/favicon.ico") {
          res.statusCode = 204;
          res.end();
          return;
        }
        const url = req.url ?? "unknown";
        requests += 1;
        lastRequest = url;
        res.once("finish", () => {
          completedRequests += 1;
          if (res.statusCode >= 400 && httpErrors.length < 20) {
            httpErrors.push(`${res.statusCode} ${url}`);
          }
        });
        next();
      });
      srv.middlewares.use(reportPath, (req, res) => {
        if (req.method !== "POST") {
          res.statusCode = 405;
          res.end();
          return;
        }
        let body = "";
        req.setEncoding("utf8");
        req.on("data", (chunk: string) => {
          body += chunk;
          if (body.length > 8 * 1024 * 1024) {
            req.destroy();
            fail(new Error("Browser verdict exceeded 8 MiB"));
          }
        });
        req.on("error", (err) => fail(err));
        req.on("end", () => {
          res.end("ok");
          deliver(body);
        });
      });
    },
  };

  try {
    server = await createServer({
      root: REPO_ROOT,
      appType: "mpa",
      plugins: [receiver],
      cacheDir: path.join(profile, "vite-cache"),
      optimizeDeps: {
        entries: [pagePath.replace(/^\//, "")],
      },
      server: { host: "127.0.0.1", port: 0, strictPort: false, hmr: false },
      customLogger: createBufferedSilentLogger(viteLogs),
    });

    await server.listen();
    const address = server.httpServer?.address();
    if (!address || typeof address === "string") {
      throw new Error("Vite did not bind an IP port");
    }

    const qs = buildQueryString(reportPath, options.query);
    const url = `http://127.0.0.1:${address.port}${pagePath}?${qs}`;

    browser = startChrome(chrome, profile, url, windowSize, options.chromeArgs ?? []);
    browser.once("error", () => {
      browserSpawnError = true;
    });
    browser.once("close", () => {
      browserClosed = true;
    });
    browser.stderr?.on("data", (chunk: Buffer) => chromeStderr.push(chunk));

    const exited = new Promise<never>((_, reject) => {
      browser?.once("error", reject);
      browser?.once("exit", (code, signal) => {
        reject(
          new Error(
            `Chrome exited before reporting for ${pagePath} (code=${code}, signal=${signal})${formatDiagnostics()}`,
          ),
        );
      });
    });
    void exited.catch(() => {});

    const rawBody = await Promise.race([verdict, exited]);
    return parseVerdictPayload<TData>(rawBody);
  } finally {
    if (timer) clearTimeout(timer);
    if (
      browser &&
      !browserSpawnError &&
      !browserClosed &&
      browser.pid !== undefined &&
      browser.exitCode === null &&
      browser.signalCode === null
    ) {
      const closed = new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(closeTimeout);
          resolve();
        };
        const closeTimeout = setTimeout(done, 3000);
        browser?.once("close", done);
        browser?.once("error", done);
      });
      if (process.platform !== "win32") {
        try {
          process.kill(-browser.pid, "SIGKILL");
        } catch {
          try {
            browser.kill("SIGKILL");
          } catch {
            // Process already exited.
          }
        }
      } else {
        try {
          browser.kill("SIGKILL");
        } catch {
          // Process already exited.
        }
      }
      await closed;
    }
    const depsOptimizer = (
      server as ViteDevServer & {
        environments?: {
          client?: {
            depsOptimizer?: {
              metadata?: {
                discovered?: Record<string, unknown>;
                optimized?: Record<string, unknown>;
              };
            };
          };
        };
      }
    )?.environments?.client?.depsOptimizer;
    const discoveredKeys = Object.keys(depsOptimizer?.metadata?.discovered ?? {});
    const optimizedMeta = depsOptimizer?.metadata?.optimized ?? {};
    const hadInflightOptimize =
      Boolean(depsOptimizer) && discoveredKeys.some((key) => !(key in optimizedMeta));
    try {
      await server?.close();
    } finally {
      await removeProfileWithRetry(profile, hadInflightOptimize);
    }
  }
}
