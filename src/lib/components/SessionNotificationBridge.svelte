<script lang="ts">
  /**
   * Turns a clicked session banner into the terminal that raised it.
   *
   * The native side has already shown, unminimised and focused the window by
   * the time this runs; all that is left is the part only the renderer can do,
   * which is to select the right tab and put the cursor in it.
   *
   * A banner can outlive its session — an agent finishes, the user closes the
   * tab, then clicks the notification. That is an ordinary outcome, not an
   * error: the tab is gone, nothing is revealed, and nothing is invented in
   * its place. It is reported quietly rather than silently so "clicking does
   * nothing" has an explanation on screen.
   */
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { isTauri } from "../platform";
  import { createListenerTracker } from "../dom/listenerTracker";
  import { sessionByNativeId, terminalSessions } from "../terminal/sessionRegistry";
  import { LAYERS } from "../ui/layers";

  let missed = $state<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;

  function open(sessionId: unknown) {
    if (typeof sessionId !== "string" || !sessionId) return;
    const record = sessionByNativeId($terminalSessions, sessionId);
    if (record?.reveal) {
      missed = null;
      record.reveal();
      return;
    }
    missed = record
      ? "That session is no longer on screen. Open its terminal from the Sessions list."
      : "That terminal session has ended.";
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => (missed = null), 6000);
  }

  onMount(() => {
    if (!isTauri()) return;
    const listeners = createListenerTracker();
    void listen<string>("gitpulse-session-notification-open", (event) => open(event.payload))
      .then((unlisten) => listeners.track(unlisten))
      .catch(() => {
        missed = "Session notification clicks cannot be delivered. Open the terminal directly.";
      });
    return () => {
      if (timer) clearTimeout(timer);
      listeners.dispose();
    };
  });
</script>

{#if missed}
  <div class="session-activation gp-card shadow-float" style:z-index={LAYERS.MODAL} role="status">{missed}</div>
{/if}

<style>
  .session-activation {
    position: fixed;
    bottom: 30px;
    right: 20px;
    max-width: 360px;
    padding: 12px;
    font-size: 12px;
    color: rgb(var(--c-text));
  }
</style>
