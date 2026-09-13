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
  it("mounts the production tour with existing actions and a persistent replay entry", () => {
    const app = read("src/App.svelte");
    expect(app).toContain('<ProductTour onOpenRepository={() => repoStore.pickAndOpenRepo()}');
    expect(app).toContain('onSettings={() => (isSettingsModalOpen = true)} onTools={() => openSetupWizard("devmap", "explain")}');
    expect(app).toContain('onclick={() => productTour.open()}>Walkthrough</button>');
    expect(app).toContain('onTasks={() => interfaceStore.setTasksOpen(true)}');
    expect(app).toContain('repoStore.setActiveTab(view)');
    expect(app).toContain('repositoryPath={$repoStore.currentPath}');
    expect(app).toContain('settingsOpen={isSettingsModalOpen}');
    expect(read("src/lib/components/HeaderRepoMenu.svelte")).toContain('data-tour="repository"');
    expect(read("src/lib/components/ViewTabBar.svelte")).toContain('data-tour="views"');
    expect(read("src/lib/components/RepoTabBar.svelte")).toContain('data-tour="tasks"');
  });
});
