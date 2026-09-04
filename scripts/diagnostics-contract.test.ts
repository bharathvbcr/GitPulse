import { readdirSync, readFileSync, statSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";

/**
 * The diagnostics subsystem is the thing you reach for when everything else
 * has already failed, so its wiring is exactly the wiring nothing else will
 * notice is broken. Two failures here are silent by construction:
 *
 *   1. Installing the panic hook BEFORE `logging::init()`. The hook captures
 *      its logger at install time, so an early install binds nothing and every
 *      subsequent panic is recorded into a logger no one can read. The code
 *      comment in logging.rs records that this already happened once.
 *   2. Adding a binary and forgetting it. A new `[[bin]]` inherits neither the
 *      logger nor the hook, and is missing from `LOGGED_BINARIES`, so it
 *      writes no durable log — and nothing about a silent binary looks wrong.
 *
 * Every list below is derived from the source that owns it. A hand-written
 * roster of binaries would pass forever while the real set grew past it.
 */
const repo = (relative: string): string =>
  readFileSync(fileURLToPath(new URL(`../${relative}`, import.meta.url)), "utf8");

const CARGO = repo("src-tauri/Cargo.toml");
const LOGGING = repo("src-tauri/src/logging.rs");
const LIB_RS = repo("src-tauri/src/lib.rs");
const REDACT_RS = repo("src-tauri/src/ledger/redact.rs");
const DIAGNOSTICS_TS = repo("src/lib/diagnostics/diagnostics.ts");

/**
 * Source with the test module removed, so fixtures never satisfy a check.
 *
 * Cut at `#[cfg(test)] mod tests`, not at the first `#[cfg(test)]`: logging.rs
 * uses a bare `#[cfg(test)]` block inside `init()`, and cutting there silently
 * hid every function below it — which is how a check passes by examining
 * nothing at all.
 */
function production(source: string): string {
  const marker = source.search(/#\[cfg\(test\)\]\s*\nmod tests/);
  return marker === -1 ? source : source.slice(0, marker);
}

/** Every `[[bin]]` in Cargo.toml, as declared — not as remembered here. */
function declaredBinaries(): Array<{ name: string; path: string }> {
  return [...CARGO.matchAll(/\[\[bin\]\]\s*\nname\s*=\s*"([^"]+)"\s*\npath\s*=\s*"([^"]+)"/g)].map(
    (match) => ({ name: match[1], path: match[2] }),
  );
}

describe("every shipped binary is wired for diagnostics", () => {
  const binaries = declaredBinaries();

  it("finds the binaries at all", () => {
    // Without this the loop below passes vacuously on a parser change.
    expect(binaries.map((b) => b.name).sort()).toEqual(["gitpulse", "gitpulse-mcp", "gitpulsed"]);
  });

  for (const { name, path } of binaries) {
    it(`${name} installs the logger and the panic hook`, () => {
      const entry = production(repo(`src-tauri/${path}`));
      // main.rs delegates to lib.rs's `run()`; follow that one hop rather
      // than demanding the calls sit in a file that only forwards.
      const source = /gitpulse_lib::run\(\)/.test(entry) ? production(LIB_RS) : entry;
      expect(source, `${name} never calls logging::init()`).toMatch(/logging::init\(\)/);
      expect(source, `${name} never installs a panic hook`).toMatch(
        /logging::install_panic_hook\(\)/,
      );
    });
  }

  it("gives every binary a durable log of its own", () => {
    // LOGGED_BINARIES gates which stems get a file without an explicit
    // GITPULSE_LOG_DIR. A binary missing from it logs to memory only.
    const listed = /const LOGGED_BINARIES:\s*\[&str;\s*\d+\]\s*=\s*\[([^\]]*)\]/.exec(LOGGING);
    expect(listed, "LOGGED_BINARIES must be findable").toBeTruthy();
    const stems = [...(listed?.[1] ?? "").matchAll(/"([^"]+)"/g)].map((m) => m[1]);
    expect(stems.sort()).toEqual(binaries.map((b) => b.name).sort());
  });
});

describe("the panic hook can actually record", () => {
  it("is installed after the logger it captures, never before", () => {
    const source = production(LIB_RS);
    const init = source.indexOf("logging::init()");
    const hook = source.indexOf("logging::install_panic_hook()");
    expect(init, "logging::init() must be called in run()").toBeGreaterThan(-1);
    expect(hook, "the panic hook must be installed in run()").toBeGreaterThan(-1);
    expect(init, "a hook installed before init() binds a logger nobody can read").toBeLessThan(
      hook,
    );
  });

  it("routes panics through the same seam as every other entry", () => {
    // The hook calling anything other than write_entry would give panics a
    // path that skips the durable sink — losing precisely the entry that
    // explains the crash, while every ordinary warning survived.
    const hook = /pub\(crate\) fn install_panic_hook_for[\s\S]*?\n}/.exec(production(LOGGING));
    expect(hook, "install_panic_hook_for must be findable").toBeTruthy();
    expect(hook?.[0]).toMatch(/logger\.write_entry\(/);
  });

  it("keeps write_entry writing to the durable sink", () => {
    const writeEntry = /fn write_entry\([\s\S]*?\n    }/.exec(production(LOGGING));
    expect(writeEntry, "write_entry must be findable").toBeTruthy();
    expect(writeEntry?.[0], "entries must reach the file, not only the ring").toMatch(
      /sink\.write_line\(/,
    );
  });

  it("never lets the sink panic while a panic is being handled", () => {
    // A panic raised inside the panic hook aborts the process on the spot,
    // destroying the evidence at the one moment it matters. The sink's write
    // path must therefore carry no unwrap/expect at all.
    const sink = /impl FileSink \{[\s\S]*?\n}/.exec(production(LOGGING));
    expect(sink, "the FileSink impl must be findable").toBeTruthy();
    const offenders = [...(sink?.[0] ?? "").matchAll(/\.(unwrap|expect)\(/g)].map((m) => m[0]);
    // `unwrap_or_else(PoisonError::into_inner)` is not one of these: it is the
    // deliberate recovery from a poisoned lock and cannot itself panic.
    expect(offenders).toEqual([]);
  });
});

describe("the frontend capture is installed before anything can fail", () => {
  it("installs global diagnostics before mounting the app", () => {
    const main = repo("src/main.ts");
    const install = main.indexOf("installGlobalDiagnostics(");
    const mount = main.indexOf("mount(App");
    expect(install).toBeGreaterThan(-1);
    expect(mount).toBeGreaterThan(-1);
    expect(install, "errors thrown during mount would go unrecorded").toBeLessThan(mount);
  });
});

describe("every declared panel source actually reports", () => {
  /**
   * `PanelSource` is a closed union, and a member nothing uses is a panel
   * whose failures never reach the diagnostics ring. `clone` and `rebase` sat
   * here unused: both modals caught their errors, showed a banner, and
   * recorded nothing — so a failed clone was invisible in the report a user
   * would send. Both sides are derived, so wiring a new panel needs no edit
   * here and forgetting one still fails.
   */
  const declared = (): string[] => {
    const union = /export type PanelSource =([\s\S]*?);/.exec(
      repo("src/lib/diagnostics/report.ts"),
    );
    expect(union, "PanelSource must be findable").toBeTruthy();
    return [...(union?.[1] ?? "").matchAll(/"([a-z-]+)"/g)].map((m) => m[1]);
  };

  /**
   * Walks all of `src/lib`, not just the flat component directory it started
   * as: a panel may own its fetches in a dedicated store (Pulse does) or nest
   * its components in a subdirectory, and neither placement makes its failures
   * any less reported. Test files are excluded on purpose — a source named
   * only from a test would satisfy this contract while the app stayed silent.
   */
  const used = (): Set<string> => {
    const root = fileURLToPath(new URL("../src/lib/", import.meta.url));
    const found = new Set<string>();
    const walk = (dir: string): void => {
      for (const name of readdirSync(dir)) {
        const full = `${dir}${name}`;
        if (statSync(full).isDirectory()) {
          if (name !== "__tests__") walk(`${full}/`);
          continue;
        }
        if (name.endsWith(".test.ts") || name.endsWith(".spec.ts")) continue;
        if (!name.endsWith(".svelte") && !name.endsWith(".ts")) continue;
        for (const call of readFileSync(full, "utf8").matchAll(
          /reportPanelError\(\s*"([a-z-]+)"/g,
        )) {
          found.add(call[1]);
        }
      }
    };
    walk(root);
    return found;
  };

  it("finds both sides of the comparison", () => {
    expect(declared().length).toBeGreaterThan(5);
    expect(used().size).toBeGreaterThan(5);
  });

  it("leaves no panel source declared but silent", () => {
    const silent = declared().filter((source) => !used().has(source));
    expect(silent, "these panels show an error banner and record nothing").toEqual([]);
  });

  it("leaves no reporter using a source the union never declared", () => {
    const undeclared = [...used()].filter((source) => !declared().includes(source));
    expect(undeclared).toEqual([]);
  });
});


/**
 * The two credential-name tables are the same table written twice — once for
 * the ledger's write path (Rust, protects what reaches disk) and once for the
 * diagnostics report (TypeScript, protects what reaches the clipboard). Their
 * own source comments name the failure this guards: two tables drift, and the
 * one that drifted is discovered by the leak.
 *
 * They drifted before this existed. The `Object` branch of both redactors
 * ignored its keys entirely, so `{"access_token": "<opaque>"}` — the shape of
 * every OAuth response — was written through in full on both sides.
 *
 * Parsed from the source that owns each list rather than restated here, since
 * a third hand-written copy is one more thing to drift.
 */
function rustList(name: string): string[] {
  const match = REDACT_RS.match(
    new RegExp(`const ${name}: \\[&str; (\\d+)\\] = \\[([^\\]]*)\\];`),
  );
  if (!match) return [];
  const entries = [...match[2].matchAll(/"([^"]+)"/g)].map((entry) => entry[1]);
  // A declared length that disagrees with the entries is its own drift: the
  // array would not compile, but the mismatch would be reported as a build
  // error far from here, so assert it where the list is read.
  expect(entries.length, `${name} declares ${match[1]} entries`).toBe(Number(match[1]));
  return entries;
}

function tsList(name: string): string[] {
  const match = DIAGNOSTICS_TS.match(
    new RegExp(`export const ${name}: readonly string\\[\\] = \\[([^\\]]*)\\];`),
  );
  return match ? [...match[1].matchAll(/"([^"]+)"/g)].map((entry) => entry[1]) : [];
}

describe("the two credential-name tables cannot drift apart", () => {
  for (const table of ["SECRET_FIELD_NAMES", "SECRET_FIELD_SUFFIXES"]) {
    const rust = rustList(table);
    const typescript = tsList(table);

    it(`finds ${table} on both sides`, () => {
      // Without this the comparison below passes vacuously the moment either
      // declaration is reshaped and the regex stops matching.
      expect(rust.length, `${table} not found in redact.rs`).toBeGreaterThan(0);
      expect(typescript.length, `${table} not found in diagnostics.ts`).toBeGreaterThan(0);
    });

    it(`keeps ${table} identical in Rust and TypeScript`, () => {
      expect([...typescript].sort()).toEqual([...rust].sort());
    });
  }

  it("keeps the bare word `key` out of the suffix table", () => {
    // `key` as a suffix swallows public_key, cache_key and primary_key, which
    // would redact the report down to nothing useful while adding no secrecy.
    expect(rustList("SECRET_FIELD_SUFFIXES")).not.toContain("key");
    expect(tsList("SECRET_FIELD_SUFFIXES")).not.toContain("key");
  });

  it("scans the object key itself on both sides of the seam", () => {
    // The other half of the same blindness: keys were neither consulted for
    // what they name nor scanned for what they contain, so a token used as a
    // map key was written through in full.
    expect(REDACT_RS, "redact.rs never scans its keys").toMatch(/fn redact_object_keys\(/);
    expect(DIAGNOSTICS_TS, "diagnostics.ts never scans its keys").toMatch(
      /function redactObjectKeys\(/,
    );
  });

  it("consults the object key on both sides of the seam", () => {
    // The defect was structural, not a missing table entry: both `Object`
    // branches iterated values and discarded keys.
    expect(REDACT_RS, "redact.rs object branch ignores its keys").toMatch(
      /for \(key, value\) in values\.iter_mut\(\)/,
    );
    expect(REDACT_RS).toMatch(/is_secret_field_name\(key\)/);
    expect(DIAGNOSTICS_TS, "diagnostics.ts object branch ignores its keys").toMatch(
      /isSecretFieldName\(key\)/,
    );
  });
});
