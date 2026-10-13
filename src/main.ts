import "./app.css";
import App from "./App.svelte";
import { mount } from "svelte";
import { applyPlatformClass } from "./lib/platform";
import { loadHostPlatform } from "./lib/stores/platformStore";
import { installGlobalDiagnostics, diagnostics } from "./lib/diagnostics/diagnostics";
import { installResponsivenessDiagnostics } from "./lib/diagnostics/responsiveness";
import { installNavigationDiagnostics } from "./lib/diagnostics/navigation";
import { get } from "svelte/store";
import { interfaceStore } from "./lib/stores/interfaceStore";
import { repoStore } from "./lib/stores/repoStore";
import { configureSharedReads } from "./lib/ipc/invoke";
import { SHARED_READ_COMMANDS } from "./lib/ipc/sharedReads";

applyPlatformClass();

// Identical in-flight snapshot reads are joined in this window only; the
// status popover's bundle never carries the list (see `configureSharedReads`).
configureSharedReads(SHARED_READ_COMMANDS);

// Ask the backend which host this is before anything platform-gated renders.
// Fire-and-forget on purpose: the store starts at a conservative fallback that
// claims no native capability, so a slow or failed answer hides a
// platform-exclusive control rather than offering one that cannot work. Kicking
// it off here rather than on first subscribe keeps a gated control from popping
// into the settings page a moment after it opens.
void loadHostPlatform();

// Capture every failure channel (uncaught errors, unhandled rejections,
// console.error/warn) into the diagnostics ring buffer; the originals still
// reach devtools with the same prefixes as before. Retrieve via the
// Diagnostics panel (header bug icon or the command palette).
installGlobalDiagnostics(diagnostics);
// Late samples name the visible surface; a repository view also names its tab.
const stopResponsivenessDiagnostics = installResponsivenessDiagnostics(diagnostics, {
  view: () => {
    const surface = get(interfaceStore).globalSurface;
    return surface === "repository" ? `repository/${get(repoStore).activeTab}` : surface;
  },
});
import.meta.hot?.dispose(stopResponsivenessDiagnostics);
// Times each switch of surface, repository or view; the startup destination
// is the launch, not a navigation.
const navigation = installNavigationDiagnostics(diagnostics);
let destination: string | null = null;
let surface = get(interfaceStore).globalSurface;
let repo = get(repoStore);
const followDestination = () => {
  const key = surface === "repository" ? `repository\n${repo.currentPath ?? ""}\n${repo.activeTab}` : surface;
  if (key === destination) return;
  const first = destination === null;
  destination = key;
  if (!first) navigation.navigate(key, surface === "repository" ? `repository/${repo.activeTab}` : surface);
};
const stopNavigationSurface = interfaceStore.subscribe((state) => { surface = state.globalSurface; followDestination(); });
const stopNavigationRepo = repoStore.subscribe((state) => { repo = state; followDestination(); });
import.meta.hot?.dispose(() => {
  stopNavigationSurface();
  stopNavigationRepo();
  navigation.stop();
});

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
