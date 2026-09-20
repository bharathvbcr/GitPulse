import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const read = (path: string) => readFileSync(new URL(`../${path}`, import.meta.url), "utf8");
describe("onboarding integration and native permission purposes", () => {
  it.each(["Desktop", "Documents", "Downloads"])("explains repository access to %s", folder => {
    const plist = read("src-tauri/Info.plist");
    const purpose = plist.match(new RegExp(`<key>NS${folder}FolderUsageDescription</key>\\s*<string>([^<]+)</string>`))?.[1];
    expect(purpose).toContain("repositories you open");
    expect(purpose).toContain("Git operations you request");
  });
  it("does not introduce unrelated privacy requests or broader webview scopes", () => {
    const plist = read("src-tauri/Info.plist");
    expect(plist).not.toMatch(/NS(Camera|Microphone|AppleEvents)UsageDescription/);
    const capability = JSON.parse(read("src-tauri/capabilities/default.json"));
    expect(capability.windows).toEqual(["main"]);
    expect(capability.remote).toBeUndefined();
  });
  it("mounts the production tour with existing actions", () => {
    const app = read("src/App.svelte");
    expect(app).toContain('<ProductTour onOpenRepository={() => repoStore.pickAndOpenRepo()}');
    expect(app).toContain('onSettings={() => (isSettingsModalOpen = true)} onTools={() => openSetupWizard("devmap", "explain")}');
    expect(app).toContain('onTasks={() => interfaceStore.setTasksOpen(true)}');
    expect(app).toContain('repoStore.setActiveTab(view)');
    expect(app).toContain('repositoryPath={$repoStore.currentPath}');
    expect(app).toContain('settingsOpen={isSettingsModalOpen}');
    expect(read("src/lib/components/HeaderRepoMenu.svelte")).toContain('data-tour="repository"');
    expect(read("src/lib/components/ViewTabBar.svelte")).toContain('data-tour="views"');
    expect(read("src/lib/components/RepoTabBar.svelte")).toContain('data-tour="tasks"');
  });

  it("keeps the title bar clear of the walkthrough pill and replays from Settings", () => {
    // These two halves are one fact. The pill was the only production caller
    // of productTour.open(), so dropping it without rehoming replay strands
    // the tour the first time anyone defers it.
    const app = read("src/App.svelte");
    expect(app).not.toContain('data-tour="replay"');
    expect(app).not.toContain("productTour.open()");
    expect(app).not.toContain('from "./lib/tools/productTour"');

    const settings = read("src/lib/components/SettingsModal.svelte");
    expect(settings).toContain('import { productTour } from "../tools/productTour";');
    expect(settings).toContain('data-setting="walkthrough-replay"');
    expect(settings).toContain("onclick={replayWalkthrough}");
    // Settings has to close before the guide opens: the tour renders below
    // this dialog by design, and its live steps point at controls it covers.
    expect(settings).toMatch(
      /function replayWalkthrough\(\) \{\s*onClose\?\.\(\);\s*productTour\.open\(\);\s*\}/,
    );
  });
});
