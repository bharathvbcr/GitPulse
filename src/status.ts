import { mount } from "svelte";
import StatusApp from "./lib/desktop/StatusApp.svelte";
// The popover wears the shared gp-btn / gp-chip / gp-icon-btn utilities and the
// --c-pop-* tokens, both of which are defined in app.css. status.css comes
// second and is unlayered, so its transparent html/body wins over app.css's
// layered body background.
import "./app.css";
import "./status.css";
const root = document.getElementById("status-root");
if (!root) throw new Error("Status window root is missing");
mount(StatusApp, { target: root });
