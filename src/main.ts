import "./app.css";
import App from "./App.svelte";
import { mount } from "svelte";
import { applyPlatformClass } from "./lib/platform";

applyPlatformClass();

const app = mount(App, {
  target: document.getElementById("app")!,
});

export default app;
