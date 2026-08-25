import "./app.css";
import App from "./App.svelte";
import { mount } from "svelte";
import { applyPlatformClass } from "./lib/platform";
import { formatError } from "./lib/ui/formatError";

applyPlatformClass();

// Dev-aid only: surface async failures that would otherwise vanish silently
// (invoke rejections from fire-and-forget call sites). No UI, no reporting.
window.addEventListener("unhandledrejection", (event) => {
  console.error(
    `[gitpulse] unhandled promise rejection: ${formatError(event.reason)}`,
  );
});
window.addEventListener("error", (event) => {
  console.error(`[gitpulse] uncaught error: ${formatError(event.error ?? event.message)}`);
});

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
