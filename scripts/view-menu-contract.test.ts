import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { nativeTabMenuId, REGISTERED_VIEWS } from "../src/lib/views/viewRegistry";

/**
 * Every registered view that belongs in a menu must be reachable from the
 * native menu, which needs three separate things on the Rust side: an id
 * constant, a parse arm so the id resolves to an action, and a menu item so
 * the id can ever be emitted.
 *
 * Nothing tied those together, and views went missing twice: `tab-manvi`
 * (noted in nativeActions.ts) and, when this was written, Storage — which had
 * no action at all — and Reflog, which had an action nothing could emit. Both
 * were reachable only through the command palette, which is not where a user
 * looks for a view.
 *
 * viewRegistry exports `nativeTabMenuId` to derive these ids "so a new view
 * cannot be missed", but nothing called it: the Rust ids are hand-written
 * constants. This asserts the correspondence the helper implied.
 */
const actions = readFileSync(new URL("../src-tauri/src/desktop/actions.rs", import.meta.url), "utf8");
const menu = readFileSync(new URL("../src-tauri/src/desktop/menu.rs", import.meta.url), "utf8");

/**
 * Every registered view: the View menu offers all of them.
 *
 * This used to filter on `menuGroup`, back when the header folded most views
 * into dropdowns and the field said which one. The header is four tabs now
 * and the field is gone, which makes the rule simpler and stronger — there is
 * no longer a way for a view to be registered and yet absent from the native
 * menu by construction.
 */
const MENU_VIEWS = REGISTERED_VIEWS;

describe("native menu covers every registered view", () => {
  it("has views to check", () => {
    // Guards the loop below against passing vacuously.
    expect(MENU_VIEWS.length).toBeGreaterThanOrEqual(4);
  });

  for (const view of MENU_VIEWS) {
    const constName = `TAB_${view.id.toUpperCase()}`;

    it(`${view.id} has an id constant, a parse arm and a menu item`, () => {
      // The id comes from the registry's own helper rather than being spelled
      // out again here, so the format has one owner. That helper existed to
      // make ids derivable "so a new view cannot be missed" and had no caller;
      // this is the caller.
      expect(actions, `${constName} constant missing`).toContain(
        `pub const ${constName}: &str = "${nativeTabMenuId(view.id)}"`,
      );
      expect(actions, `${constName} has no parse arm`).toContain(`${constName} => Self::`);
      // A constant with no menu item is an action nothing can emit — exactly
      // the state Reflog was in.
      expect(menu, `${constName} has no menu item`).toContain(`actions::${constName}`);
    });
  }
});

describe("every actionable native id has a frontend handler", () => {
  // The other direction of the same contract. A menu item whose id the
  // dispatcher does not recognise is inert: it appears, it can be clicked, and
  // nothing happens — with no error anywhere to notice.
  const dispatcher = readFileSync(
    new URL("../src/lib/desktop/nativeActions.ts", import.meta.url),
    "utf8",
  );

  const rustIds = [...actions.matchAll(/pub const [A-Z_]+: &str = "([a-z-]+)"/g)].map(
    (m) => m[1],
  );
  const handled = new Set(
    [...dispatcher.matchAll(/case "([a-z-]+)":/g)].map((m) => m[1]),
  );
  const viewIds = new Set(REGISTERED_VIEWS.map((view) => nativeTabMenuId(view.id)));

  /**
   * Ids that intentionally have no handler, with the reason. `recent-empty` is
   * the disabled "No Recent Repositories" placeholder — it is created with
   * enabled=false, so it cannot be clicked.
   */
  const INERT = new Set(["recent-empty"]);

  it("found both sides to compare", () => {
    expect(rustIds.length).toBeGreaterThan(20);
    expect(handled.size).toBeGreaterThan(15);
  });

  it("leaves no clickable id without a handler", () => {
    const orphans = rustIds.filter(
      (id) => !handled.has(id) && !viewIds.has(id) && !INERT.has(id),
    );
    expect(orphans).toEqual([]);
  });

  it("keeps the inert list honest", () => {
    // An id listed as inert that gained a handler should leave the list, and
    // one that no longer exists should not linger.
    for (const id of INERT) {
      expect(rustIds, `${id} is listed inert but no longer exists`).toContain(id);
      expect(handled.has(id), `${id} is listed inert but is now handled`).toBe(false);
    }
  });
});


/**
 * No two native menu items may claim the same accelerator.
 *
 * A collision is silent: muda binds one item and the other's shortcut simply
 * never fires, which looks like a broken feature rather than a menu bug, and
 * nothing in the build says a word. `build_native_menu` itself cannot be unit
 * tested to catch it — `muda::MenuChild` can only be constructed on the main
 * thread, and a Rust test harness runs cases on worker threads, so calling it
 * panics inside the platform layer (verified, not assumed). The structure it
 * builds is still checkable from source, which is what this does.
 *
 * cfg-aware on purpose: Settings deliberately carries `CmdOrCtrl+,` twice,
 * once under `cfg(target_os = "macos")` for the App menu and once under
 * `cfg(not(target_os = "macos"))` for the File menu. Those never coexist in one
 * binary, so treating them as a collision would be a false alarm — and a
 * contract that cries wolf gets deleted, which is how the real collision
 * eventually ships.
 */
type CfgBucket = "macos" | "not-macos" | "always";

interface Accelerator {
  value: string;
  bucket: CfgBucket;
}

/** Menu source with its `#[cfg(test)] mod tests` block removed. */
function menuProduction(): string {
  const marker = menu.search(/#\[cfg\(test\)\]\s*\nmod tests/);
  return marker === -1 ? menu : menu.slice(0, marker);
}

/**
 * Spans guarded by a platform cfg, as [start, end) offsets.
 *
 * An attribute guards the item or block that follows it, so the span runs to
 * the end of that statement — a brace-matched block when one opens before the
 * next `;`, otherwise the statement itself.
 */
function platformSpans(source: string): Array<{ bucket: CfgBucket; start: number; end: number }> {
  const spans: Array<{ bucket: CfgBucket; start: number; end: number }> = [];
  for (const match of source.matchAll(/#\[cfg\((not\()?target_os = "macos"\)?\)\]/g)) {
    const bucket: CfgBucket = match[1] ? "not-macos" : "macos";
    const from = match.index + match[0].length;
    const brace = source.indexOf("{", from);
    const semi = source.indexOf(";", from);
    if (brace !== -1 && (semi === -1 || brace < semi)) {
      let depth = 0;
      let end = source.length;
      for (let cursor = brace; cursor < source.length; cursor += 1) {
        if (source[cursor] === "{") depth += 1;
        else if (source[cursor] === "}") {
          depth -= 1;
          if (depth === 0) {
            end = cursor;
            break;
          }
        }
      }
      spans.push({ bucket, start: from, end });
    } else if (semi !== -1) {
      spans.push({ bucket, start: from, end: semi });
    }
  }
  return spans;
}

function declaredAccelerators(): Accelerator[] {
  const source = menuProduction();
  const spans = platformSpans(source);
  const constants = new Map<string, string>();
  for (const declaration of source.matchAll(/const\s+(\w+)\s*:\s*&str\s*=\s*"([^"]+)"\s*;/g)) {
    constants.set(declaration[1], declaration[2]);
  }
  const bucketAt = (offset: number): CfgBucket => {
    const enclosing = spans.filter((span) => offset >= span.start && offset < span.end);
    return enclosing.length > 0 ? enclosing[enclosing.length - 1].bucket : "always";
  };
  const found: Accelerator[] = [];
  for (const match of source.matchAll(/Some\(\s*(?:"([^"]+)"|([A-Z][A-Z0-9_]+))\s*\)/g)) {
    // `PredefinedMenuItem::…(app, Some("Close Window"))` takes a TITLE in this
    // position, not an accelerator. Counting it would compare labels to
    // shortcuts and report nonsense.
    const preceding = source.slice(Math.max(0, match.index - 160), match.index);
    if (/PredefinedMenuItem::\w+\(\s*app\s*,\s*$/.test(preceding)) continue;
    const value = match[1] ?? constants.get(match[2] ?? "");
    if (!value) continue;
    // Accelerators always name a key with a modifier or are a function key.
    if (!/^(CmdOrCtrl|Cmd|Ctrl|Alt|Shift|F\d)/.test(value)) continue;
    found.push({ value, bucket: bucketAt(match.index) });
  }
  return found;
}

describe("no two native menu items claim the same accelerator", () => {
  const accelerators = declaredAccelerators();

  it("finds the accelerators at all", () => {
    // A parser that silently stopped matching would make the check below pass
    // while examining nothing.
    expect(accelerators.length, "no accelerators parsed from menu.rs").toBeGreaterThanOrEqual(12);
    const values = accelerators.map((a) => a.value);
    expect(values, "the Open Repository accelerator went missing").toContain("CmdOrCtrl+O");
    expect(values, "a constant-declared accelerator was not resolved").toContain(
      "CmdOrCtrl+Shift+W",
    );
  });

  it("sees both sides of the deliberate platform split", () => {
    // Settings is the one accelerator declared twice on purpose. If cfg
    // tracking broke, both uses would land in "always" and the assertion below
    // would fail on a false alarm — or, worse, be "fixed" by weakening it.
    const settings = accelerators.filter((a) => a.value === "CmdOrCtrl+,");
    expect(settings.map((a) => a.bucket).sort()).toEqual(["macos", "not-macos"]);
  });

  it("leaves no accelerator claimed twice in one binary", () => {
    const collisions: string[] = [];
    const byValue = new Map<string, CfgBucket[]>();
    for (const { value, bucket } of accelerators) {
      byValue.set(value, [...(byValue.get(value) ?? []), bucket]);
    }
    for (const [value, buckets] of byValue) {
      if (buckets.length < 2) continue;
      // Two items collide unless every pair is on opposite platforms.
      const macos = buckets.filter((b) => b !== "not-macos").length;
      const others = buckets.filter((b) => b !== "macos").length;
      if (macos > 1 || others > 1) collisions.push(`${value} (${buckets.join(", ")})`);
    }
    expect(
      collisions,
      `these accelerators are bound by more than one menu item, so one of them ` +
        `silently never fires: ${collisions.join("; ")}`,
    ).toEqual([]);
  });
});
