import { mount } from "svelte";
import StatusApp from "./lib/desktop/StatusApp.svelte";
import "./status.css";
const root = document.getElementById("status-root");
if (!root) throw new Error("Status window root is missing");
mount(StatusApp, { target: root });
