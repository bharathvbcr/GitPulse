// Disposable UI integration fixture backed by the real Manvi store binary.
// Usage: node scripts/workbench-preview.mjs /absolute/path/to/dcstore [/absolute/path/to/manvi]
import { execFile } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { isAbsolute, join } from "node:path";
import { promisify } from "node:util";
import { createServer as createHTTPServer } from "node:http";
import { createServer } from "vite";
import { previewWorker } from "./workbench-preview-worker.mjs";

const binary = process.argv[2];
const manviBinary = process.argv[3];
if (!binary || !isAbsolute(binary)) throw new Error("Supply an absolute dcstore binary path.");
if (manviBinary && !isAbsolute(manviBinary)) throw new Error("Supply an absolute Manvi binary path.");
const root = await mkdtemp(join(tmpdir(), "gitpulse-workbench-preview-"));
console.log(`Disposable fixture directory: ${root}`);
// Vite's optimizer can finish writing a cancelled bundle after server.close()
// resolves. Remove the disposable profile/cache only once those writes drain.
// The normal close path also removes it eagerly for top-level startup errors,
// where Node may exit without a beforeExit event.
async function removeFixture() {
  // The optimizer may finish its last write while removal walks the cache.
  // Retry only the transient filesystem errors handled by Node, with a
  // finite 1.5 second backoff budget. Persistent failures remain visible.
  await rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
}
process.once("beforeExit", async () => {
  try { await removeFixture(); }
  catch (error) { console.error(error); process.exitCode = 1; }
});
const db = join(root, "profile.sqlite");
const run = promisify(execFile);
const methods = new Set(["decisions.list", "decisions.get", "decisions.decide", "notifications.activations.list", "notifications.settings.get", "notifications.settings.put", "notifications.delivery.get", "notifications.ack", "runs.list", "runs.get", "attention.list", "attention.get", "attention.update", "workspaces.list", "workspaces.get", "workspaces.put", "workspaces.delete", "repositories.list", "repositories.get", "repositories.put", "items.list", "items.get", "items.brief.get", "items.put", "items.delete", "items.history", "events.list", "enhancements.list", "enhancements.get", "enhancements.create", "enhancements.accept", "enhancements.dismiss", "enhancements.undo", "enhancements.recover", "enhancements.revise", "enhancements.generate", "enhancements.configuration", "enhancements.wake", "enhancements.worker", "automation.get", "automation.put", "automation.list"]);
/** @type {Awaited<ReturnType<typeof previewWorker>> | null} */
let worker = null;
/** @type {Awaited<ReturnType<typeof createServer>> | null} */
let server = null;
// Middleware mode leaves termination with this fixture. Vite's standalone
// signal handler otherwise calls process.exit before the model worker closes.
const http = createHTTPServer((req, res) => {
  if (!server) { res.writeHead(503).end(); return; }
  server.middlewares(req, res);
});
let closing = false, starting = true, stopRequested = false;
async function close() {
  if (closing) return;
  closing = true;
  try { await server?.close(); }
  finally {
    http.closeAllConnections();
    try {
      if (http.listening) await new Promise((resolve, reject) => http.close((error) => error ? reject(error) : resolve(undefined)));
    } finally { try { await worker?.close(); } finally { await removeFixture(); } }
  }
}
function stop() {
  stopRequested = true;
  if (!starting) void close().catch((error) => { console.error(error); process.exitCode = 1; });
}
function requireRunning() { if (stopRequested) throw new Error("Preview stopped during startup"); }
process.on("SIGINT", stop);
process.on("SIGTERM", stop);
let inFlight = 0;
/** @param {string} method @param {string} input */
async function request(method, input) {
  requireRunning();
  if (!methods.has(method) || typeof input !== "string" || Buffer.byteLength(input) > 128 * 1024) throw new Error("Invalid preview request");
  if (inFlight >= 8) throw new Error("Preview request queue is full");
  inFlight++;
  try {
    if (["enhancements.generate", "enhancements.configuration", "enhancements.wake", "enhancements.worker"].includes(method)) {
      if (!worker) return { ok: false, code: "not_installed", error: "Pass the Manvi binary as the preview's second argument to test generation." };
      return await worker.request(method, input);
    }
    const { stdout } = await run(binary, ["work", "--db", db, "--method", method, "--input", input], { timeout: 10_000, maxBuffer: 2 * 1024 * 1024 + 4096 });
    return JSON.parse(stdout);
  } finally { inFlight--; }
}
/** @param {string} method @param {Record<string, unknown>} input */
async function seed(method, input) {
  const response = await request(method, JSON.stringify(input));
  if (response.ok !== true) throw new Error(`Fixture refused: ${JSON.stringify(response)}`);
}
try {
worker = manviBinary ? await previewWorker(manviBinary, binary, root, db) : null;
requireRunning();
// Seed data is not a user save. Tests explicitly enable automatic suggestions
// after exercising manual review, so fixture setup cannot consume the slot.
await seed("automation.put", { id: "profile", request_id: "fixture-disable-auto", expected_revision: 1, enabled: false });
for (const [id, name] of [["gitpulse", "GitPulse"], ["manvi", "Manvi"], ["docs", "Documentation"]]) {
  await seed("repositories.put", { request_id: id, id, expected_revision: 0, name, identity_key: `fixture:${id}` });
}
await seed("workspaces.put", { request_id: "workspace", id: "devtools", expected_revision: 0, name: "Developer tools", icon: "◈", pinned: true, repository_ids: ["gitpulse", "manvi"] });
for (const [index, status] of ["inbox", "backlog", "ready", "in_progress", "review", "done"].entries()) {
  await seed("items.put", { request_id: `task-${index}`, id: `task-${index}`, expected_revision: 0, title: ["Capture repository evidence", "Reduce search latency", "Group related repositories", "Keep task details through edits", "Review agent changes", "Persist workspace membership"][index], status, kind: index === 1 ? "bug" : "feature", priority: index % 4, description: "Reproduce the behavior and preserve this detailed evidence through every board move.", acceptance_criteria: ["Verify persistence after restart", "Preserve unrelated task fields"], repository_ids: index === 4 ? ["gitpulse", "manvi"] : [index === 5 ? "docs" : "gitpulse"], primary_repository_id: index === 5 ? "docs" : "gitpulse", labels: ["agentic"], home_workspace_id: index === 5 ? "devtools" : null });
}
let allowedHost = "";
server = await createServer({
  configFile: "vite.config.ts",
  cacheDir: join(root, "vite-cache"),
  server: { middlewareMode: { server: http }, ws: { server: http }, watch: { ignored: ["**/coverage/**", "**/lcov.info"] } },
  plugins: [{ name: "disposable-workbench-fixture", configureServer(server) {
    server.middlewares.use("/__workbench", async (req, res) => {
      if (!allowedHost || req.method !== "POST" || req.headers.host !== allowedHost || (req.headers.origin && req.headers.origin !== `http://${allowedHost}`) || !req.headers["content-type"]?.startsWith("application/json")) { res.writeHead(403).end(); return; }
      try {
        const chunks = []; let size = 0;
        for await (const chunk of req) { size += chunk.length; if (size > 256 * 1024) throw new Error("Preview body too large"); chunks.push(chunk); }
        const body = JSON.parse(Buffer.concat(chunks).toString("utf8"));
        const result = await request(body.method, body.input);
        res.setHeader("Content-Type", "application/json"); res.end(JSON.stringify(result));
      } catch (error) { res.writeHead(500, { "Content-Type": "application/json" }).end(JSON.stringify({ ok: false, code: "preview_error", error: String(error) })); }
    });
  } }],
});
requireRunning();
await new Promise((resolve, reject) => { http.once("error", reject); http.listen(0, "127.0.0.1", () => resolve(undefined)); });
requireRunning();
const address = http.address();
if (!address || typeof address === "string") throw new Error("Preview has no TCP address");
allowedHost = `127.0.0.1:${address.port}`;
console.log(`Disposable profile: ${db}\nPreview: http://${allowedHost}/harness/workbench.html`);
starting = false;
} catch (error) { starting = false; await close(); if (!stopRequested) throw error; }
