/**
 * A Mavi thread as the host would replay it: Claude Code stream-json lines
 * (shapes taken from the captures in src/lib/mavi/__fixtures__) plus the
 * host's `mavi_*` markers. Turn 1 reads, searches and edits; a test run
 * changes a snapshot file the agent never edited; turn 2 is waiting for an
 * answer on a proposed edit.
 */
const ROOT = "/Users/me/code/payments";

let uuid = 0;
const id = () => `00000000-0000-4000-8000-${String(++uuid).padStart(12, "0")}`;

function assistantTool(msg: string, tool: string, name: string, input: Record<string, unknown>) {
  return { type: "assistant", message: { id: msg, role: "assistant", content: [{ type: "tool_use", id: tool, name, input }] }, uuid: id() };
}
function result(tool: string, content: string, extra: Record<string, unknown> = {}) {
  return { type: "user", message: { role: "user", content: [{ type: "tool_result", tool_use_id: tool, content }] }, ...extra, uuid: id() };
}
function text(msg: string, body: string) {
  return { type: "assistant", message: { id: msg, role: "assistant", content: [{ type: "text", text: body }] }, uuid: id() };
}

export const MAIN_SESSION = "mavi-1795400000000-0";
export const DONE_SESSION = "mavi-1795400000000-1";

export function mainLines(): string[] {
  const lines: unknown[] = [
    { type: "mavi_user", text: "The retry in charge.ts gives up after one attempt when the gateway times out. Make it back off properly and add a test.", turn: 1, at: Date.now() - 260_000 },
    { type: "system", subtype: "init", session_id: "7c0e6c1a-4d1f-4c55-9b0e-2f8f5a1d3b21", model: "claude-opus-5-5", permissionMode: "manual", cwd: ROOT, claude_code_version: "2.1.330", tool_count: 24 },
    { type: "assistant", message: { id: "m1", role: "assistant", content: [{ type: "thinking", thinking: "", signature: "" }] } },
    assistantTool("m1", "t1", "Read", { file_path: `${ROOT}/src/charge.ts` }),
    result("t1", "1\timport { gateway } from './gateway';\n…"),
    assistantTool("m2", "t2", "Grep", { pattern: "retryWithBackoff", path: `${ROOT}/src` }),
    result("t2", "src/util/retry.ts:12:export async function retryWithBackoff("),
    assistantTool("m3", "t3", "Read", { file_path: `${ROOT}/src/util/retry.ts` }),
    result("t3", "12\texport async function retryWithBackoff(fn, { attempts = 3, baseMs = 200 } = {}) {"),
    text("m4", "The charge path calls the gateway directly instead of going through `retryWithBackoff`, so a timeout is final. I'll route it through the helper and treat timeouts as retryable."),
    assistantTool("m5", "t4", "Edit", { file_path: `${ROOT}/src/charge.ts`, old_string: "  const res = await gateway.charge(req);\n", new_string: "  const res = await retryWithBackoff(() => gateway.charge(req), {\n    attempts: 4,\n    retryOn: isTimeout,\n  });\n" }),
    { type: "control_request", request_id: "r1", request: { subtype: "can_use_tool", tool_name: "Edit", tool_use_id: "t4", input: {}, description: "src/charge.ts" } },
    { type: "mavi_answered", request_id: "r1", decision: "allow" },
    result("t4", "The file has been updated.", {
      tool_use_result: {
        filePath: `${ROOT}/src/charge.ts`,
        structuredPatch: [
          { oldStart: 41, oldLines: 4, newStart: 41, newLines: 7, lines: [" export async function charge(req: ChargeRequest) {", "   validate(req);", "-  const res = await gateway.charge(req);", "+  const res = await retryWithBackoff(() => gateway.charge(req), {", "+    attempts: 4,", "+    retryOn: isTimeout,", "+  });", "   return toReceipt(res);"] },
          { oldStart: 1, oldLines: 2, newStart: 1, newLines: 3, lines: [" import { gateway } from './gateway';", "+import { retryWithBackoff, isTimeout } from './util/retry';", " import { validate } from './validate';"] },
        ],
      },
    }),
    assistantTool("m6", "t5", "Bash", { command: "npm test -- charge", description: "Run the charge tests" }),
    { type: "control_request", request_id: "r2", request: { subtype: "can_use_tool", tool_name: "Bash", tool_use_id: "t5", input: {}, description: "Run the charge tests" } },
    { type: "mavi_answered", request_id: "r2", decision: "allow" },
    result("t5", "PASS src/charge.test.ts\n  ✓ charges once (12 ms)\n  ✓ snapshot (4 ms)\n\nTests: 2 passed"),
    text("m7", "Charging now retries timeouts up to **4 times** with exponential backoff; other gateway errors still fail fast.\n\n- `src/charge.ts` goes through `retryWithBackoff`\n- The test run updated a snapshot as a side effect"),
    { type: "system", subtype: "post_turn_summary", status_category: "review_ready", status_detail: "retry wired; test still to add", needs_action: "" },
    { type: "result", subtype: "success", duration_ms: 94_000, total_cost_usd: 0.41, usage: {} },
    { type: "mavi_turn_files", turn: 1, available: true, partial: false, moved: [
      { path: "src/charge.ts", code: " M" },
      { path: "src/__snapshots__/charge.test.ts.snap", code: " M" },
    ] },
    { type: "mavi_user", text: "Good. Now add a test that a timeout is retried and a decline is not.", turn: 2, at: Date.now() - 40_000 },
    { type: "system", subtype: "init", session_id: "7c0e6c1a-4d1f-4c55-9b0e-2f8f5a1d3b21", model: "claude-opus-5-5", permissionMode: "manual", cwd: ROOT },
    assistantTool("m8", "t6", "Read", { file_path: `${ROOT}/src/charge.test.ts` }),
    result("t6", "1\timport { charge } from './charge';"),
    text("m9", "Adding two cases: a gateway that times out twice then succeeds, and a decline that must not be retried."),
    assistantTool("m10", "t7", "Edit", {
      file_path: `${ROOT}/src/charge.test.ts`,
      old_string: "describe('charge', () => {\n",
      new_string: "describe('charge', () => {\n  it('retries a timeout', async () => {\n    const gw = flakyGateway({ timeouts: 2 });\n    await expect(charge(req, gw)).resolves.toMatchObject({ ok: true });\n    expect(gw.calls).toBe(3);\n  });\n\n  it('does not retry a decline', async () => {\n    const gw = decliningGateway();\n    await expect(charge(req, gw)).rejects.toThrow('declined');\n    expect(gw.calls).toBe(1);\n  });\n",
    }),
    { type: "control_request", request_id: "r3", request: { subtype: "can_use_tool", tool_name: "Edit", tool_use_id: "t7", input: {}, description: "src/charge.test.ts" } },
    { type: "system", subtype: "task_summary", detail: "Waiting to edit charge.test.ts" },
  ];
  return lines.map((l) => JSON.stringify(l));
}

export function doneLines(): string[] {
  return [
    { type: "mavi_user", text: "Why is the nightly build slow?", turn: 1, at: Date.now() - 3_600_000 },
    text("d1", "Most of the time is the uncached Docker layer for `node_modules`."),
    { type: "result", subtype: "success", duration_ms: 31_000, total_cost_usd: 0.08 },
  ].map((l) => JSON.stringify(l));
}

export function summary(idText: string, phase: string, extra: Record<string, unknown> = {}) {
  return {
    id: idText,
    target: { kind: "local", root: ROOT },
    mode: "ask",
    model: null,
    phase,
    conversation_id: "7c0e6c1a-4d1f-4c55-9b0e-2f8f5a1d3b21",
    pending: phase === "approval" ? 1 : 0,
    turns: 2,
    cost_usd: 0.58,
    touched: ["src/charge.ts", "src/charge.test.ts"],
    background_tasks: 0,
    started_at_ms: Date.now() - 300_000,
    last_activity_ms: Date.now() - 20_000,
    exit: null,
    exit_ok: false,
    last_turn: "ok",
    ...extra,
  };
}

export const SNAPSHOT_PATCH = `diff --git a/src/__snapshots__/charge.test.ts.snap b/src/__snapshots__/charge.test.ts.snap
--- a/src/__snapshots__/charge.test.ts.snap
+++ b/src/__snapshots__/charge.test.ts.snap
@@ -3,5 +3,6 @@
 exports[\`charge snapshot 1\`] = \`
 {
-  "attempts": 1,
+  "attempts": 4,
+  "retryOn": "timeout",
   "status": "ok",
 }
`;

export { ROOT };
