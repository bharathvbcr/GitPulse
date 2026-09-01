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

/** Views that appear in a menu group are the ones the menu must offer. */
const MENU_VIEWS = REGISTERED_VIEWS.filter((view) => Boolean(view.menuGroup));

describe("native menu covers every registered view", () => {
  it("has views to check", () => {
    expect(MENU_VIEWS.length).toBeGreaterThan(8);
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
