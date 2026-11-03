import { existsSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * GitPulse is a *consumer* of Manvi's serve seam; Manvi owns it.
 *
 * `contracts/serve.ops.schema.json` is generated from the Go wire types in
 * `manvi/serve` (and held to them by a Go test there). This file holds the
 * Rust side to that contract: what GitPulse sends must be declared, and the
 * ops GitPulse names must be served.
 *
 * It exists because `scope.allowed_commands` was sent by Rust for a release
 * while Go had no field for it. Go ignores unknown fields on purpose (host and
 * harness ship separately), so the drop was silent and every task-authorised
 * command was demoted to `command.not_allowed`. Runtime strictness is the
 * wrong cure; a build-time check against the shared description is the right
 * one. The Rust sources are read as text, the way the other contract tests do,
 * so no cargo build stands between a developer and the failure.
 */

const REPO = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const read = (...parts: string[]) => readFileSync(path.join(REPO, ...parts), "utf8");

interface Field {
  type: string;
  required: boolean;
  fields?: Record<string, Field>;
  items?: Field;
}
interface ServeContract {
  protocol: number;
  ops: { always: string[]; optional: string[] };
  types: Record<string, Field>;
}

const contract: ServeContract = JSON.parse(read("contracts", "serve.ops.schema.json"));
const policyRs = read("src-tauri", "src", "harness", "policy.rs");
const protocolRs = read("src-tauri", "src", "harness", "protocol.rs");
const sidecarRs = read("src-tauri", "src", "harness", "sidecar.rs");
const gateRs = read("src-tauri", "src", "workbench", "verification_gate.rs");
const proposalsRs = read("src-tauri", "src", "workbench", "intake_proposals.rs");
const workbenchRs = read("src-tauri", "src", "workbench.rs");

/**
 * The non-test text of every `src-tauri/src/harness/*.rs` file, by file name.
 * Test modules (an inline `#[cfg(test)] mod`, and `*_tests.rs` files) are cut
 * off: they name ops as literals on purpose, to talk to fakes.
 */
function harnessProduction(): Map<string, string> {
  const dir = path.join(REPO, "src-tauri", "src", "harness");
  const files = new Map<string, string>();
  for (const name of readdirSync(dir).filter((f) => f.endsWith(".rs") && !/_tests?\.rs$/.test(f))) {
    const text = readFileSync(path.join(dir, name), "utf8");
    const testMod = /^#\[cfg\((?:all\()?test[^\n]*\n(?:#\[[^\n]*\n)*(?:pub(?:\([^)]*\))? )?mod \w+/m.exec(text);
    files.set(name, testMod ? text.slice(0, testMod.index) : text);
  }
  return files;
}

/**
 * Op-shaped string literals passed as the op argument of a sidecar call:
 * `x.call("a.b", ..)`, `call_typed::<T>("a.b", ..)`, `call_in_slot(owner, "a.b", ..)`.
 * An op is `hello` or dotted lowercase words, which a path or message is not.
 */
const OP_LITERAL_ARG = /\bcall\w*(?:::<[^>]*>)?\(\s*(?:\w+\s*,\s*)?"(hello|[a-z][a-z0-9_]*(?:\.[a-z][a-z0-9_]*)+)"/g;

function literalOpCalls(source: string): string[] {
  return [...source.matchAll(OP_LITERAL_ARG)].map((m) => m[1]);
}

const declared = (type: string) => Object.keys(contract.types[type].fields ?? {});

/** Text from `start` to the first line that is exactly the closing brace. */
function block(source: string, start: RegExp): string {
  const at = source.search(start);
  expect(at, `${start} not found`).toBeGreaterThanOrEqual(0);
  const rest = source.slice(at);
  const end = rest.search(/\n\}\n/);
  expect(end, `${start} has no closing brace`).toBeGreaterThan(0);
  return rest.slice(0, end);
}

/** Field names of a Rust struct, skipping doc comments and attributes. */
function structFields(source: string, name: string): string[] {
  const body = block(source, new RegExp(`pub(?:\\(crate\\))? struct ${name} \\{`));
  return [...body.matchAll(/^\s+pub (\w+):/gm)].map((m) => m[1]);
}

/** Keys of the `json!({ ... })` literal plus every `params["key"] =` assignment in a function. */
function paramKeys(fn: string): string[] {
  const literal = /json!\(\{([^}]*)\}\)/.exec(fn);
  expect(literal, "params json! literal not found").not.toBeNull();
  const keys = [...literal![1].matchAll(/"(\w+)"\s*:/g)].map((m) => m[1]);
  keys.push(...[...fn.matchAll(/params\["(\w+)"\]\s*=/g)].map((m) => m[1]));
  return keys;
}

function undeclared(sent: string[], type: string): string[] {
  const known = new Set(declared(type));
  return sent.filter((key) => !known.has(key));
}

function missingRequired(sent: string[], type: string): string[] {
  const have = new Set(sent);
  return Object.entries(contract.types[type].fields ?? {})
    .filter(([name, field]) => field.required && !have.has(name))
    .map(([name]) => name);
}

describe("the serve contract", () => {
  it("is for the protocol this build speaks", () => {
    const pinned = /pub const PROTOCOL_VERSION: i64 = (\d+);/.exec(protocolRs);
    expect(pinned).not.toBeNull();
    expect(contract.protocol).toBe(Number(pinned![1]));
  });

  it("matches Manvi's copy when Manvi is checked out beside this repo", () => {
    const roots = [process.env.GITPULSE_MANVI_ROOT, path.resolve(REPO, "..", "Manvi"), path.resolve(REPO, "..", "..", "..", "..", "Manvi")];
    const manvi = roots.find((root) => root && existsSync(path.join(root, "contracts", "serve.ops.schema.json")));
    if (!manvi) return; // not checked out: nothing to compare, and the checks below still run
    expect(read("contracts", "serve.ops.schema.json")).toBe(readFileSync(path.join(manvi, "contracts", "serve.ops.schema.json"), "utf8"));
  });
});

describe("what GitPulse sends is declared by the contract", () => {
  it("HostScope declares every field Rust serializes", () => {
    const fields = structFields(policyRs, "HostScope");
    expect(fields).toEqual(expect.arrayContaining(["task_id", "allowed_commands"]));
    expect(undeclared(fields, "HostScope")).toEqual([]);
  });

  it("HostScope sends every field the contract requires", () => {
    const fields = structFields(policyRs, "HostScope");
    expect(missingRequired(fields, "HostScope")).toEqual([]);
  });

  it("policy.check.command params are declared and complete", () => {
    // `check_command_allowing` delegates to `check_command_on`, which builds
    // the params for every lane; that is the function that sends them.
    const keys = paramKeys(block(policyRs, /pub\(crate\) fn check_command_on\(/));
    expect(keys).toEqual(expect.arrayContaining(["command", "root", "scope"]));
    expect(undeclared(keys, "CommandCheckParams")).toEqual([]);
    expect(missingRequired(keys, "CommandCheckParams")).toEqual([]);
  });

  it("policy.check.file params are declared and complete", () => {
    const keys = paramKeys(block(policyRs, /pub fn check_file\(/));
    expect(keys).toEqual(expect.arrayContaining(["root", "path", "op", "scope"]));
    expect(undeclared(keys, "FileCheckParams")).toEqual([]);
    expect(missingRequired(keys, "FileCheckParams")).toEqual([]);
  });

  it("hello params are declared and complete", () => {
    const call = /OP_HELLO,\s*Some\(serde_json::json!\(\{([^}]*)\}\)\)/.exec(sidecarRs);
    expect(call, "hello call not found").not.toBeNull();
    const keys = [...call![1].matchAll(/"(\w+)"\s*:/g)].map((m) => m[1]);
    expect(keys).toEqual(expect.arrayContaining(["protocol"]));
    expect(undeclared(keys, "HelloParams")).toEqual([]);
    expect(missingRequired(keys, "HelloParams")).toEqual([]);
  });

  it("HelloResult reads only fields Go sends", () => {
    const fields = structFields(protocolRs, "HelloResult");
    expect(fields).toEqual(expect.arrayContaining(["protocol", "ops", "posture"]));
    expect(undeclared(fields, "HelloResult")).toEqual([]);
  });

  it("HelloResult reads the per-op deadlines Manvi advertises, and only fields Go sends", () => {
    expect(structFields(protocolRs, "HelloResult")).toContain("op_limits");
    const limit = structFields(protocolRs, "OpLimit");
    expect(limit).toEqual(expect.arrayContaining(["lane", "deadline_ms"]));
    expect(undeclared(limit, "OpLimit")).toEqual([]);
  });

  it("the sidecar sizes its timeouts from the advertised deadlines and says when it falls back", () => {
    expect(sidecarRs).toMatch(/fn advertised_timeout\(/);
    expect(sidecarRs).toMatch(/Budget::PerOp => match advertised_timeout\(/);
    expect(sidecarRs).toMatch(/does not advertise op deadlines/);
  });
});

describe("repository reads are bound to a root the request names", () => {
  // Manvi refuses these with E_BAD_ROOT when the root is missing, so a host
  // that builds one of these requests must send it; the contract pins which.
  it.each([
    ["DevmapStatusParams", "root"],
    ["DevmapQueryParams", "root"],
    ["RefineParams", "repo_root"],
    ["GapScanParams", "repo_root"],
  ])("%s requires %s", (type, field) => {
    expect(contract.types[type].fields?.[field]?.required).toBe(true);
  });
});

describe("the ops GitPulse names are served", () => {
  const served = new Set([...contract.ops.always, ...contract.ops.optional]);

  it("every OP_* constant is a served op", () => {
    const names = [...protocolRs.matchAll(/pub const OP_\w+: &str = "([^"]+)";/g)].map((m) => m[1]);
    expect(names.length).toBeGreaterThan(0);
    expect(names.filter((name) => !served.has(name))).toEqual([]);
  });

  it("the profile worker allowlist names only served ops", () => {
    const allow = /matches!\(\s*op,([\s\S]*?)\)\s*\{\s*return Err\(HarnessError::Protocol\(\s*"unsupported profile worker operation"/.exec(sidecarRs);
    expect(allow, "profile worker allowlist not found").not.toBeNull();
    const ops = [...allow![1].matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    expect(ops.filter((op) => !served.has(op))).toEqual([]);
    expect(ops).toHaveLength(15);
    expect(ops).toEqual(expect.arrayContaining(["work.verify.task", "work.verify.get", "work.scan.gaps", "work.scan.get", "work.scan.cancel", "work.enhancements.refine", "work.tasks.suggest", "work.runs.managed.continue"]));
  });

  it("what GitPulse sends work.verify.task and work.verify.get is declared, and nothing required is missing", () => {
    const fnBody = (name: string) => {
      const at = gateRs.indexOf(`pub(crate) fn ${name}(`);
      expect(at, `${name} not found`).toBeGreaterThanOrEqual(0);
      const rest = gateRs.slice(at);
      return rest.slice(0, rest.search(/\n\}\n/));
    };
    const start = paramKeys(fnBody("start_params"));
    expect(undeclared(start, "VerifyTaskParams")).toEqual([]);
    expect(missingRequired(start, "VerifyTaskParams")).toEqual([]);
    const get = paramKeys(fnBody("get_params"));
    expect(undeclared(get, "VerifyGetParams")).toEqual([]);
    expect(missingRequired(get, "VerifyGetParams")).toEqual([]);
  });

  it("what GitPulse sends work.runs.managed.continue is declared, and the op it names is the served one", () => {
    const managedRs = read("src-tauri", "src", "workbench", "managed_run.rs");
    const at = managedRs.indexOf("pub(crate) fn continue_params(");
    expect(at, "continue_params not found").toBeGreaterThanOrEqual(0);
    const rest = managedRs.slice(at);
    const keys = paramKeys(rest.slice(0, rest.search(/\n\}\n/)));
    expect(keys).toEqual(expect.arrayContaining(["id", "request_id", "message"]));
    expect(undeclared(keys, "ManagedContinueParams")).toEqual([]);
    expect(missingRequired(keys, "ManagedContinueParams")).toEqual([]);
    const op = /pub\(crate\) const OP_CONTINUE: &str = "([^"]+)";/.exec(managedRs);
    expect(op).not.toBeNull();
    expect(contract.ops.optional).toContain(op![1]);
    // What GitPulse reads back is a subset of what Manvi answers.
    expect(declared("ManagedContinueResult")).toEqual(expect.arrayContaining(["state", "request_id", "turn", "max_turns", "idle_deadline"]));
  });

  it("what GitPulse sends work.scan.* and work.enhancements.refine is declared, and the root is always sent", () => {
    const fnBody = (name: string) => {
      const at = proposalsRs.indexOf(`pub(crate) fn ${name}(`);
      expect(at, `${name} not found`).toBeGreaterThanOrEqual(0);
      const rest = proposalsRs.slice(at);
      return rest.slice(0, rest.search(/\n\}\n/));
    };
    const start = paramKeys(fnBody("scan_start_params"));
    expect(undeclared(start, "GapScanParams")).toEqual([]);
    expect(missingRequired(start, "GapScanParams")).toEqual([]);
    const ref = paramKeys(fnBody("scan_ref_params"));
    expect(undeclared(ref, "GapScanReadParams")).toEqual([]);
    expect(missingRequired(ref, "GapScanReadParams")).toEqual([]);
    // refine: id, request_id and expected_revision come from the generation request; the root is added by the host.
    expect(workbenchRs).toMatch(/params\["repo_root"\] = json!\(root\);\s*return self\.worker_call\("work\.enhancements\.refine"/);
    expect(declared("RefineParams")).toEqual(expect.arrayContaining(["id", "request_id", "expected_revision", "repo_root"]));
  });

  it("the evidence type the completion gate reads is the one Manvi stores a verification under", () => {
    const type = /pub\(crate\) const EVIDENCE_TYPE: &str = "([^"]+)";/.exec(gateRs);
    expect(type).not.toBeNull();
    expect(contract.ops.optional).toContain(type![1]);
  });

  it("the profile worker ops are optional: a build without the module does not serve them", () => {
    for (const op of ["work.enhancements.generate", "work.runs.managed.prepare"]) {
      expect(contract.ops.optional).toContain(op);
      expect(contract.ops.always).not.toContain(op);
    }
  });
});

describe("every op the harness sends is a named, contract-checked constant", () => {
  const served = new Set([...contract.ops.always, ...contract.ops.optional]);

  it("sidecar calls in the harness name an OP_* constant, never an op literal", () => {
    // What `every OP_* constant is a served op` cannot see: a literal handed
    // straight to `call` is in no list, so the hosted `serve.status` and
    // `serve.stop_all` went out unchecked against the contract.
    const offenders: string[] = [];
    for (const [file, text] of harnessProduction()) {
      for (const op of literalOpCalls(text)) offenders.push(`${file}: ${op}`);
    }
    expect(offenders).toEqual([]);
  });

  it("no serve.* op is spelled as a literal outside protocol.rs", () => {
    // `HOSTED_OPS` lists the hosted server's ops; it is not a call argument.
    const offenders: string[] = [];
    for (const [file, text] of harnessProduction()) {
      if (file === "protocol.rs") continue;
      // `serve.sock` and `serve.log` are file names; only the contract's ops count.
      for (const m of text.matchAll(/"(serve\.[a-z_]+)"/g)) if (served.has(m[1])) offenders.push(`${file}: ${m[1]}`);
    }
    expect(offenders).toEqual([]);
  });

  it("the literal-op scan sees a literal in a call (it is not vacuous)", () => {
    expect(literalOpCalls('let raw = self.call("serve.stop_all", None, t)?;')).toEqual(["serve.stop_all"]);
    expect(literalOpCalls('call_in_slot(\n owner,\n "policy.check",\n None)')).toEqual(["policy.check"]);
    expect(literalOpCalls("sidecar::call_typed::<PrepareResult>(OP_CHAT_PREPARE, params, t)")).toEqual([]);
    expect(literalOpCalls("self.call(OP_SERVE_STATUS, None, t)")).toEqual([]);
    expect(literalOpCalls('self.call("not an op", None, t)')).toEqual([]);
  });

  it("the serve ops the hosted lane sends are constants, served, and the shapes Rust reads are declared", () => {
    const ops = Object.fromEntries([...protocolRs.matchAll(/pub const (OP_\w+): &str = "([^"]+)";/g)].map((m) => [m[1], m[2]]));
    expect(ops.OP_SERVE_STATUS).toBe("serve.status");
    expect(ops.OP_SERVE_STOP_ALL).toBe("serve.stop_all");
    expect(served.has("serve.status") && served.has("serve.stop_all")).toBe(true);
    // What GitPulse sends with serve.stop_all.
    expect(declared("ServeStopAllParams")).toContain("wait_ms");
    expect(sidecarRs).toMatch(/OP_SERVE_STOP_ALL,\s*Some\(serde_json::json!\(\{"wait_ms":/);
    // What it reads back of serve.status ...
    const hostedRs = read("src-tauri", "src", "harness", "hosted.rs");
    const status = structFields(hostedRs, "ServeStatus");
    expect(status).toEqual(expect.arrayContaining(["protocol", "owner_id", "process_id", "controller", "jobs"]));
    expect(undeclared(status, "ServeStatusResult")).toEqual([]);
    const job = structFields(hostedRs, "ServeJob");
    expect(job).toEqual(expect.arrayContaining(["id", "phase"]));
    const jobFields = contract.types.ServeStatusResult.fields?.jobs.items?.fields ?? {};
    expect(job.filter((name) => !(name in jobFields))).toEqual([]);
    // ... and of serve.stop_all (the `pending` ids sidecar.rs decodes).
    expect(declared("ServeStopAllResult")).toContain("pending");
  });
});
