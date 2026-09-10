// Test-only bridge to the real Manvi host, with a deterministic loopback model.
import { spawn } from "node:child_process";
import { createServer } from "node:http";

/** @param {string} binary @param {string} storeBinary @param {string} root @param {string} database */
export async function previewWorker(binary, storeBinary, root, database) {
  const model = createServer(async (req, res) => {
    if (req.url === "/v1/models" && req.method === "GET") {
      res.setHeader("Content-Type", "application/json");
      res.end(JSON.stringify({ data: [{ id: "fixture-model", object: "model", max_model_len: 32768 }] })); return;
    }
    if (req.url !== "/v1/chat/completions" || req.method !== "POST") { res.writeHead(404).end(); return; }
    try {
      let size = 0; const chunks = [];
      for await (const chunk of req) { size += chunk.length; if (size > 128 * 1024) throw new Error("Fixture model request too large"); chunks.push(chunk); }
      const input = JSON.parse(Buffer.concat(chunks).toString("utf8"));
      const prompt = JSON.parse(input.messages.at(-1).content);
      /** @type {{ rationale: string; title?: string; description?: string }} */
      const proposal = { rationale: "Deterministic test output; this is not a model-quality evaluation." };
      for (const field of prompt.fields) {
        if (field === "title") proposal.title = `${prompt.task.title} (clarified)`;
        else if (field === "description") proposal.description = `${prompt.task.description}\nConfirm the result against the saved acceptance criteria.`;
        else throw new Error("Unexpected fixture field");
      }
      const timer = setTimeout(() => {
        if (res.destroyed) return;
        res.setHeader("Content-Type", "text/event-stream");
        res.end(`data: ${JSON.stringify({ choices: [{ delta: { content: JSON.stringify(proposal) }, finish_reason: "stop" }] })}\n\ndata: [DONE]\n\n`);
      }, 1500);
      res.once("close", () => clearTimeout(timer));
    } catch { res.writeHead(400).end("Invalid fixture model request"); }
  });
  await new Promise((resolve, reject) => { model.once("error", reject); model.listen(0, "127.0.0.1", () => resolve(undefined)); });
  const address = model.address();
  if (!address || typeof address === "string") throw new Error("Fixture model did not bind");
  const child = spawn(binary, ["serve", "--workbench-db", database], {
    cwd: root, stdio: ["pipe", "pipe", "pipe"],
    env: { ...process.env, MANVI_HARNESS_INIT_ENABLED: "false", DEVCOUNCIL_ROOT: root,
      MANVI_STORE_BINARY: storeBinary, MANVI_LLM_PROVIDER_DEFAULT: "local",
      MANVI_LLM_LOCAL_BASE_URL: `http://127.0.0.1:${address.port}/v1`,
      MANVI_LLM_LOCAL_MODEL: "fixture-model", LOCAL_API_KEY: "synthetic-preview-credential", MANVI_MODEL: "" },
  });
  let sequence = 0, buffer = Buffer.alloc(0), dead = false;
  /** @type {{ id:string; resolve:(result:unknown)=>void; reject:(error:Error)=>void; timer:ReturnType<typeof setTimeout> } | null} */
  let pending = null;
  /** @param {string} reason */
  const fail = (reason) => {
    dead = true; buffer = Buffer.alloc(0);
    if (pending) { const call = pending; pending = null; clearTimeout(call.timer); call.reject(new Error(reason)); }
  };
  child.once("error", (error) => fail(error.message));
  child.stdin.on("error", (error) => fail(error.message));
  child.once("exit", () => fail("Preview Manvi host exited"));
  child.stderr.on("data", () => {}); // Drain; credentials or arbitrary diagnostics never enter the page.
  child.stdout.on("data", (chunk) => {
    if (dead) return;
    buffer = Buffer.concat([buffer, chunk]);
    if (buffer.length > 2 * 1024 * 1024 + 4096) { fail("Preview host response exceeded its bound"); child.kill("SIGTERM"); return; }
    for (let end; (end = buffer.indexOf(10)) >= 0;) {
      const line = buffer.subarray(0, end).toString("utf8"); buffer = buffer.subarray(end + 1);
      if (!line.trim()) continue;
      try {
        const reply = JSON.parse(line);
        if (!pending || reply.id !== pending.id || typeof reply.ok !== "boolean") throw new Error("Unexpected preview protocol reply");
        const call = pending; pending = null; clearTimeout(call.timer);
        call.resolve(reply.ok ? reply.result : { ok: false, code: reply.error?.code ?? "protocol_error", error: reply.error?.message ?? "Incomplete host refusal" });
      } catch (error) { fail(String(error)); child.kill("SIGTERM"); return; }
    }
  });
  /** @param {string} op @param {Record<string, unknown>} params */
  const call = (op, params) => new Promise((resolve, reject) => {
    if (pending || dead) { reject(new Error("Preview host is busy or unavailable")); return; }
    const id = String(++sequence);
    const timer = setTimeout(() => { fail("Preview host response timed out; outcome is uncertain"); child.kill("SIGTERM"); }, 20_000);
    pending = { id, resolve, reject, timer };
    child.stdin.write(JSON.stringify({ id, op, params }) + "\n", (error) => { if (error) fail(error.message); });
  });
  async function close() {
    const closed = new Promise(resolve => { if (!child.pid || child.exitCode !== null || child.signalCode !== null) resolve(undefined); else child.once("exit", () => resolve(undefined)); });
    child.stdin.end();
    const terminate = setTimeout(() => child.kill("SIGTERM"), 5500);
    const kill = setTimeout(() => child.kill("SIGKILL"), 6500);
    await closed; clearTimeout(terminate); clearTimeout(kill);
    model.closeAllConnections(); await new Promise(resolve => model.close(resolve));
  }
  try {
    const hello = await call("hello", { protocol: 1 });
    if (!hello || typeof hello !== "object" || !("ops" in hello) || !Array.isArray(hello.ops) || !hello.ops.includes("work.enhancements.generate")) {
      throw new Error("Preview Manvi binary does not support generation");
    }
  } catch (error) { await close(); throw error; }
  return {
    /** @param {string} method @param {string} input */
    request: (method, input) => {
      // Match GitPulse Rust: `model` is spawn/env only and must not reach Manvi's
      // empty-object host methods or generate params (unsupported-field refusal).
      const parsed = JSON.parse(input);
      if (parsed && typeof parsed === "object" && !Array.isArray(parsed) && "model" in parsed) {
        const { model: _model, ...rest } = parsed;
        return call(`work.${method}`,
          method === "enhancements.configuration" || method === "enhancements.wake" || method === "enhancements.worker"
            ? {}
            : rest);
      }
      return call(`work.${method}`, parsed);
    },
    close,
  };
}
