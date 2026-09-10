import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import SettingsModal from "./SettingsModal.svelte";
import { SETTINGS_SECTIONS } from "../ui/settingsSections";
import { SETTINGS_CATALOG } from "../ui/settingsCatalog";
import { ACCENTS } from "../ui/accents";
import { TAB_WIDTHS } from "../ui/codeDisplay";
import { VIEW_TABS } from "../repos/persist";
import { viewNavItemFor } from "../views/viewNav";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "SettingsModal.svelte"),
  "utf8",
);

const appCss = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "..", "app.css"),
  "utf8",
);

const open = () => render(SettingsModal, { props: { isOpen: true } }).body;

/**
 * A label as it appears in rendered markup.
 *
 * Svelte escapes text, so a label carrying `&`, `<` or `>` never appears
 * literally in the body — "Diff & code" renders as "Diff &amp; code". Comparing
 * the raw catalog string against HTML would fail on the label rather than on
 * the missing panel it is supposed to be checking for.
 */
function asRendered(text: string): string {
  return text
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;");
}

/**
 * Panel markup with runs of whitespace collapsed.
 *
 * Template copy is wrapped for the 100-column source, so a sentence renders
 * with newlines and indentation inside it. An assertion on a phrase that
 * happens to straddle a wrap would fail on the formatting rather than on the
 * missing copy it is checking for.
 */
function prose(chunk: string): string {
  return chunk.replace(/\s+/g, " ");
}

/** The markup of one category panel, so an assertion cannot pass on a neighbour. */
function panel(body: string, id: string): string {
  const start = body.indexOf(`id="settings-panel-${id}"`);
  expect(start, `no panel for ${id}`).toBeGreaterThan(-1);
  const next = SETTINGS_SECTIONS.map((entry) => body.indexOf(`id="settings-panel-${entry.id}"`))
    .filter((index) => index > start)
    .sort((a, b) => a - b)[0];
  return body.slice(start, next ?? body.length);
}

describe("SettingsModal", () => {
  it("renders nothing while closed", () => {
    const { body } = render(SettingsModal, { props: { isOpen: false } });
    expect(body).not.toContain('role="dialog"');
  });

  it("exposes theme and density as labelled stateful controls", () => {
    const body = open();

    expect(body).toContain('role="dialog"');
    expect(body).toContain('aria-label="Theme appearance"');
    expect(body).toContain('aria-label="Branch spacing"');
    expect(body).toContain('aria-label="Graph width"');
    // Every option is a pressed/unpressed toggle.
    expect(body).toContain('aria-pressed="true"');
    expect(body).toContain('aria-pressed="false"');
    // Theme options: system is the default preference in a fresh store.
    expect(body).toContain("System");
    expect(body).toContain("Light");
    expect(body).toContain("Dark");
    // Density options (sole owner since the FilterBar control moved here).
    expect(body).toContain("Spacious");
    expect(body).toContain("Compact");
    expect(body).toContain("Balanced");
    expect(body).toContain("Wide");
    expect(body).toContain("Full");
  });

  it("owns the interface visibility switches", () => {
    const body = open();

    expect(body).toContain('role="switch"');
    expect(body).toContain('aria-label="Show the language mix in the status bar"');
    expect(body).toContain('aria-label="Show MANVI status badges"');
    // Defaults are on, so both switches render checked.
    expect(body).toContain('aria-checked="true"');
  });

  it("keeps every setting and the Done action reachable at the 900 by 600 minimum window", () => {
    // The rail and the panel scroll independently: a long panel must not be
    // able to push the categories or the footer buttons off a short window.
    expect(source).toContain("max-h-[calc(100vh-2rem)]");
    expect(source).toContain("min-h-0 flex-1 overflow-y-auto");
    expect(source).toContain("w-36 shrink-0 overflow-y-auto");
    expect(source).toContain("shrink-0");
  });
});

describe("SettingsModal category rail", () => {
  const body = open();

  it.each(SETTINGS_SECTIONS.map((entry) => [entry.id, entry.label] as const))(
    "gives %s a rail tab and a panel it controls",
    (id, label) => {
      // Derived from the catalog, so adding a category without its panel
      // fails here instead of rendering an empty pane.
      expect(body).toContain(`id="settings-tab-${id}"`);
      expect(body).toContain(`aria-controls="settings-panel-${id}"`);
      expect(body).toContain(`id="settings-panel-${id}"`);
      expect(body).toContain(`aria-labelledby="settings-tab-${id}"`);
      expect(body).toContain(asRendered(label));
    },
  );

  it("shows exactly one panel at a time", () => {
    const visible = SETTINGS_SECTIONS.filter((entry) => {
      const chunk = panel(body, entry.id);
      const tag = chunk.slice(0, chunk.indexOf(">"));
      return !/\shidden(\s|=|$)/.test(tag);
    });
    expect(visible.map((entry) => entry.id)).toEqual(["appearance"]);
  });

  it("names the dialog through its heading rather than a duplicate label", () => {
    expect(body).toContain('aria-labelledby="settings-modal-title"');
    expect(body).toContain('id="settings-modal-title"');
  });
});

describe("SettingsModal layout options", () => {
  const body = open();

  it("offers the chrome the main window can drop", () => {
    const layout = panel(body, "layout");
    expect(layout).toContain('aria-label="Status bar detail"');
    expect(layout).toContain('aria-label="Diagnostics button visibility"');
    expect(layout).toContain('aria-label="Show labels on header action buttons"');
    expect(layout).toContain(
      'aria-label="Hide the repository tab strip while a single repository is open"',
    );
    expect(layout).toContain('aria-label="Show the language mix in the status bar"');
    expect(layout).toContain('aria-label="Show MANVI status badges"');
  });

  it("says a hidden status bar still comes back for anything that needs attention", () => {
    // The setting would otherwise read as "never show me this again", which
    // is not what it does — and must not be what a user believes it does.
    const layout = panel(body, "layout");
    expect(layout).toContain("parked merge or rebase");
    expect(layout).toContain("unresolved");
    expect(layout).toContain("watcher");
  });

  it("offers every status-bar and diagnostics choice, with the defaults selected", () => {
    const layout = panel(body, "layout");
    for (const label of ["Full", "Compact", "Hidden", "Always", "When recorded"]) {
      expect(layout, `missing option ${label}`).toContain(`>${label}`);
    }
    // A fresh store is full + always, so exactly two options read as pressed.
    expect(layout.match(/aria-pressed="true"/g)).toHaveLength(2);
  });
});

describe("SettingsModal view visibility", () => {
  const body = open();

  it("offers a checkbox for every registered view", () => {
    const views = panel(body, "views");
    for (const tab of VIEW_TABS) {
      const label = viewNavItemFor(tab)?.label;
      expect(label, `no nav item for ${tab}`).toBeTruthy();
      expect(views, `no checkbox for ${tab}`).toContain(
        `aria-label="Show ${label} in the header"`,
      );
    }
    // Nothing is hidden by default, so every box renders checked.
    expect(views).not.toContain('type="checkbox" checked={false}');
  });

  it("states that hiding a view is cosmetic, not a loss of access", () => {
    const views = panel(body, "views");
    expect(views).toContain("command palette");
    expect(views).toContain("View menu");
    expect(views).toContain("Work reappears");
  });
});

describe("SettingsModal automatic GitHub alerts", () => {
  it("checks GitHub alerts on launch by default and names the gh CLI cost", () => {
    const analysis = panel(open(), "analysis");
    expect(analysis).toContain(
      'aria-label="Automatically check GitHub Dependabot and code scanning alerts when a repository opens"',
    );
    expect(analysis).toContain("On by default");
    expect(analysis).toContain("GitHub CLI");
    expect(analysis).toContain("Critical and high");
  });
});

describe("SettingsModal automatic coverage toggle", () => {
  it("offers automatic coverage generation as an explicit opt-in, off by default", () => {
    const body = open();
    expect(body).toContain(
      'aria-label="Automatically generate coverage for repositories that have none"',
    );
    // Rendered from the stored preference, which defaults to off.
    expect(body).toContain('aria-checked="false"');
  });

  it("states the cost before the user turns it on", () => {
    // Running a repository's test suites and writing artifacts into its
    // working tree is not what a settings toggle is normally assumed to do.
    // Scoped to the Analysis panel: the Updates panel also opens "Off by
    // default", and an unscoped assertion passed with this copy deleted.
    const analysis = panel(open(), "analysis");
    expect(analysis).not.toBe("");
    expect(analysis).toContain("Off by default");
    expect(analysis).toContain("writes coverage artifacts into the working tree");
    expect(analysis).toContain("never reported as a clean result");
  });
});

describe("SettingsModal MCP / Agent Plugins", () => {
  it("names the plugin package, the protocol version and the read-only guarantee", () => {
    // Pinned on what the panel promises rather than the package's marketing
    // name: the manifest filenames and protocol come from the Rust side and
    // have been renamed before, but "read-only" is a claim about behaviour
    // and must never quietly disappear from the copy.
    const agents = panel(open(), "agents");
    expect(agents).toContain("plugin.json");
    expect(agents).toContain("mcp.json");
    expect(agents).toContain("MCP 2026-07-28");
    expect(agents).toContain("read-only");
  });

  it("loads installer facts through cmd_mcp_info rather than guessing a path", () => {
    expect(source).toContain("getMcpInfo");
    expect(source).toContain("plugin.json");
    expect(source).toContain("mcp.json");
    expect(source).toContain("gitpulse_insights");
  });
});

describe("SettingsModal restore defaults", () => {
  it("confirms first, then resets every store this page writes to", () => {
    expect(open()).toContain("Restore defaults");
    const fn = source.slice(
      source.indexOf("async function restoreDefaults"),
      source.indexOf("const SECTION_ICONS"),
    );
    expect(fn).toContain("askConfirm");
    expect(fn).toContain("if (!confirmed) return;");
    // All three owners, or "defaults" would silently mean "some of them".
    expect(fn).toContain("interfaceStore.reset()");
    expect(fn).toContain("densityStore.setDensity");
    expect(fn).toContain("setTheme(");
  });
});


describe("SettingsModal is wired to the search catalog", () => {
  /** Every `data-setting` id the template stamps on a control row. */
  const stamped = [...source.matchAll(/data-setting="([^"]+)"/g)].map((m) => m[1]);

  it("stamps each id exactly once, so a filter cannot half-hide a row", () => {
    expect(new Set(stamped).size).toBe(stamped.length);
  });

  it("has a catalog entry for every control on the page", () => {
    // Without one the control is unfindable by search — present, but only to
    // someone who already knew which category to open.
    const known = new Set(SETTINGS_CATALOG.map((entry) => entry.id));
    for (const id of stamped) {
      expect(known, `no catalog entry for data-setting="${id}"`).toContain(id);
    }
  });

  it("has a control on the page for every catalog entry", () => {
    // The other direction: an entry with no control is a search result that
    // opens a panel and highlights nothing.
    for (const entry of SETTINGS_CATALOG) {
      expect(stamped, `catalog entry ${entry.id} has no control`).toContain(entry.id);
    }
  });

  it("puts each control in the panel its catalog entry names", () => {
    const body = open();
    for (const entry of SETTINGS_CATALOG) {
      expect(
        panel(body, entry.section),
        `${entry.id} is not inside the ${entry.section} panel`,
      ).toContain(`data-setting="${entry.id}"`);
    }
  });

  it("hides each row the filter drops, unless the whole category goes with it", () => {
    // A control that ignores the filter would sit in a panel of hits looking
    // like one of them. The exception is a category with a single control:
    // filtering removes the category from the rail outright, so there is no
    // panel left to render a stale row into.
    const soleControl = new Set(
      SETTINGS_CATALOG.filter(
        (entry) =>
          SETTINGS_CATALOG.filter((other) => other.section === entry.section).length === 1,
      ).map((entry) => entry.id),
    );
    for (const id of stamped) {
      if (soleControl.has(id)) continue;
      expect(
        prose(source),
        `data-setting="${id}" is not gated on the filter`,
      ).toContain(`data-setting="${id}" hidden={!shown("${id}")}`);
    }
  });

  it("makes the hidden attribute actually hide, whatever utilities a row carries", () => {
    // The UA sheet's `[hidden] { display: none }` loses to any author rule
    // that sets `display`, so a row with `flex` on it stayed on screen while
    // marked hidden. Two rows here carry `flex`; the filter is only as honest
    // as this rule.
    expect(appCss.replace(/\s+/g, " ")).toContain("[hidden] { display: none !important; }");
  });

  it("offers a labelled search box that can be cleared", () => {
    const body = open();
    expect(body).toContain('aria-label="Search settings"');
    // The clear button only exists while there is something to clear.
    expect(body).not.toContain('aria-label="Clear settings search"');
    expect(source).toContain('aria-label="Clear settings search"');
  });
});

describe("SettingsModal category rail keyboard support", () => {
  const body = open();

  it("is a single tab stop with the selected category focusable", () => {
    // `role="tab"` promises arrow-key movement. Before the roving tabindex
    // the rail was a tablist in name only: eight separate tab stops, none of
    // which answered an arrow key.
    expect(body.match(/tabindex="0"/g) ?? []).toHaveLength(1);
    expect(body.match(/tabindex="-1"/g) ?? []).toHaveLength(
      // Every unselected tab, plus the dialog container itself.
      SETTINGS_SECTIONS.length - 1 + 1,
    );
  });

  it("moves selection with the arrow keys, wrapping, plus Home and End", () => {
    const fn = source.slice(
      source.indexOf("function onRailKeydown"),
      source.indexOf("/** Result of the most recent manual check"),
    );
    for (const key of ["ArrowDown", "ArrowUp", "Home", "End"]) {
      expect(fn, `no handling for ${key}`).toContain(key);
    }
    expect(fn).toContain("preventDefault");
    // Selection follows focus, so the focus has to travel with it.
    expect(fn).toContain(".focus()");
  });
});

describe("SettingsModal theme segment", () => {
  it("re-reads the preference on open rather than trusting a mount snapshot", () => {
    // ⌘-shortcuts and the native View menu set the theme from outside this
    // modal. A segment left on its mount-time snapshot reads as selected
    // while something else is in force, which is the same failure as a
    // setting that does nothing.
    expect(source).toContain("if (isOpen) themePreference = themeStore.preference();");
  });
});

describe("SettingsModal appearance additions", () => {
  const body = open();
  const appearance = panel(body, "appearance");

  it("offers every accent as a checkable radio, with the default selected", () => {
    for (const accent of ACCENTS) {
      expect(appearance, `no swatch for ${accent.id}`).toContain(
        `aria-label="${accent.label} accent"`,
      );
    }
    // Same shape as every other single-choice control here: a labelled group
    // of aria-pressed buttons, exactly one of them pressed.
    expect(appearance).toContain('role="group" aria-label="Accent colour"');
    const swatches = appearance.slice(
      appearance.indexOf('aria-label="Accent colour"'),
      appearance.indexOf('data-setting="ui-scale"'),
    );
    expect(swatches.match(/aria-pressed="true"/g) ?? []).toHaveLength(1);
    expect(swatches).toContain('aria-pressed="true" aria-label="Blue accent"');
  });

  it("says an accent stays legible in both themes rather than leaving it implied", () => {
    expect(prose(appearance)).toContain("light and dark");
    expect(prose(appearance)).toContain("WCAG AA");
  });

  it("offers reduce motion as an opt-in that cannot overrule the system", () => {
    expect(appearance).toContain('aria-label="Reduce interface motion"');
    // The copy has to say which direction the setting works in: "off" means
    // follow the system, not "animate anyway".
    expect(prose(appearance)).toContain("follows the system setting");
    expect(prose(appearance)).toContain("It can only add reduction");
  });

  it("offers both timestamp styles with a preview of the one in force", () => {
    expect(appearance).toContain('aria-label="Timestamp style"');
    expect(appearance).toContain(">Relative");
    expect(appearance).toContain(">Date");
    // A fresh store is relative, and the preview is pinned to a fixed instant
    // three days back so it reads the same on every render.
    expect(appearance).toContain("3d ago");
  });
});

describe("SettingsModal diff and code panel", () => {
  const body = open();
  const diff = panel(body, "diff");

  it("says these are defaults, not a lock on the toolbar", () => {
    // Otherwise the panel reads as "force every diff unified", which would be
    // a different and much worse setting.
    expect(prose(diff)).toContain("opens");
    expect(prose(diff)).toContain("toolbar");
  });

  it("offers the default layout, with unified selected", () => {
    expect(diff).toContain('aria-label="Default diff layout"');
    expect(diff).toContain(">Unified");
    expect(diff).toContain(">Split");
  });

  it("offers wrap, syntax and whitespace defaults in their shipped states", () => {
    expect(diff).toContain('aria-label="Open diffs with word wrap on"');
    expect(diff).toContain('aria-label="Open diffs with syntax highlighting on"');
    expect(diff).toContain(
      'aria-label="Open repositories with whitespace-only changes ignored"',
    );
    // Wrap off, syntax on, whitespace off — one switch checked of the three.
    const switches = diff.match(/role="switch"[^>]*aria-checked="(true|false)"/g) ?? [];
    expect(switches).toHaveLength(3);
    expect(switches.filter((s) => s.includes('"true"'))).toHaveLength(1);
  });

  it("names the wrap ceiling instead of letting the switch look unconditional", () => {
    // Wrapping is refused above WRAP_MAX_LINES in DiffViewer; a default that
    // silently does not apply is exactly the kind of quiet failure to avoid.
    expect(prose(diff)).toContain("4,000 rows");
  });

  it("says ignoring whitespace hides it from reading, never from the commit", () => {
    expect(prose(diff)).toContain("real bytes");
  });

  it("offers every tab width, with the CSS initial value selected", () => {
    expect(diff).toContain('aria-label="Tab width"');
    for (const width of TAB_WIDTHS) {
      expect(diff, `missing tab width ${width}`).toContain(`>${width}`);
    }
    expect(prose(diff)).toContain("Display only");
  });
});
