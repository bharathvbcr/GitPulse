import { mount } from "svelte";
import StatusPreview from "./StatusPreview.svelte";
const target = document.getElementById("preview");
if (!target) throw new Error("Preview root is missing");
mount(StatusPreview, { target });
