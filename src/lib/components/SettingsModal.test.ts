import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { render } from "svelte/server";
import SettingsModal from "./SettingsModal.svelte";
import { SETTINGS_SECTIONS } from "../ui/settingsSections";
import { VIEW_TABS } from "../repos/persist";
import { viewNavItemFor } from "../views/viewNav";

const source = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "SettingsModal.svelte"),
  "utf8",
);

const open = () => render(SettingsModal, { props: { isOpen: true } }).body;

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
      expect(body).toContain(label);
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
