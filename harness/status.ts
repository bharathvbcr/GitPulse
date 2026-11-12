import { mount } from "svelte";
// The window entry (src/status.ts) owns this import; the fixture mirrors it.
// status.css is not mirrored: it makes the page transparent and non-scrolling,
// which a fixture with its own backdrop and controls must not be.
import "../src/app.css";
import StatusPreview from "./StatusPreview.svelte";
const target = document.getElementById("preview");
if (!target) throw new Error("Preview root is missing");
mount(StatusPreview, { target });
if (new URLSearchParams(location.search).has("check")) {
  const { checkStatusPopover } = await import("./statusChecks");
  await checkStatusPopover();
}
