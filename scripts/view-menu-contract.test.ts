import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { REGISTERED_VIEWS } from "../src/lib/views/viewRegistry";

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
      expect(actions, `${constName} constant missing`).toContain(
        `pub const ${constName}: &str = "tab-${view.id}"`,
      );
      expect(actions, `${constName} has no parse arm`).toContain(`${constName} => Self::`);
      // A constant with no menu item is an action nothing can emit — exactly
      // the state Reflog was in.
      expect(menu, `${constName} has no menu item`).toContain(`actions::${constName}`);
    });
  }
});
