<script lang="ts">
  import { onMount } from "svelte";
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import StatusPopover from "./StatusPopover.svelte";
  import type { MenuState } from "./menuState";
  import { createMenuSync } from "./menuSync";
  import { createStatusConnection } from "./statusConnection";
  import { isTauri } from "../platform";
  import { formatError } from "../ui/formatError";
  let snapshot = $state<MenuState | null>(null);
  let error = $state<string | null>(null);
  let pending = $state(false);
  let host: HTMLDivElement;
  let disposed = false;
  function apply(next: MenuState) {
    snapshot = next; error = null;
    document.documentElement.classList.toggle("dark", next.checked.includes("theme-dark"));
    document.documentElement.classList.toggle("light", next.checked.includes("theme-light"));
  }
  const connection = createStatusConnection({
    read: () => invoke<MenuState>("cmd_get_status_state"),
    subscribe: (receive) => listen<MenuState>("gitpulse-status-state", ({ payload }) => receive(payload)),
    apply,
    failed: (cause) => { error = formatError(cause); },
  });
  async function action(id: string) {
    if (id === "retry") { await connection.connect(); return; }
    if (pending && id !== "dismiss") return;
    pending = true; error = null;
    try { await invoke("cmd_status_action", { id, repoPath: snapshot?.activePath ?? null }); }
    catch (cause) { if (!disposed) error = formatError(cause); }
    finally { pending = false; }
  }
  onMount(() => {
    if (!isTauri()) { error = "Open this panel from the GitPulse menu bar icon."; return; }
    void connection.connect();
    const resize = createMenuSync<number>((height) => invoke("cmd_resize_status", { height }),
      (cause) => { if (!disposed) error = formatError(cause); });
    const observer = new ResizeObserver(() => resize.update(Math.min(640, Math.max(100, Math.ceil(host.getBoundingClientRect().height)))));
    observer.observe(host);
    return () => { disposed = true; connection.dispose(); observer.disconnect(); resize.dispose(); };
  });
</script>
<svelte:window onkeydown={(event) => { if (event.key === "Escape") { event.preventDefault(); void action("dismiss"); } }} />
<div bind:this={host}><StatusPopover {snapshot} {error} {pending} onaction={(id) => void action(id)} /></div>
