import "./app.css";
import App from "./App.svelte";
import { mount } from "svelte";
import { applyPlatformClass } from "./lib/platform";
import { installGlobalDiagnostics, diagnostics } from "./lib/diagnostics/diagnostics";
import { installResponsivenessDiagnostics } from "./lib/diagnostics/responsiveness";

applyPlatformClass();

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
