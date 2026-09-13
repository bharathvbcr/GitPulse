import "./app.css";
import App from "./App.svelte";
import { mount } from "svelte";
import { applyPlatformClass } from "./lib/platform";
import { loadHostPlatform } from "./lib/stores/platformStore";
import { installGlobalDiagnostics, diagnostics } from "./lib/diagnostics/diagnostics";
import { installResponsivenessDiagnostics } from "./lib/diagnostics/responsiveness";

applyPlatformClass();

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
const stopResponsivenessDiagnostics = installResponsivenessDiagnostics(diagnostics);
import.meta.hot?.dispose(stopResponsivenessDiagnostics);

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
