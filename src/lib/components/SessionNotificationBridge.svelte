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
  import { adoptDetachedSessions } from "../terminal/detachedSessions";
  import { repoStore } from "../stores/repoStore";
  import { focusTerminalSession } from "../terminal/sessionFocus";
  import { bindAttention, sessionActivity } from "../terminal/sessionActivity";
  import { standingAttention } from "../stores/sessionAlertsStore";
  import { LAYERS } from "../ui/layers";

  let missed = $state<string | null>(null);
  let timer: ReturnType<typeof setTimeout> | null = null;

  /**
   * Through the same owner as the Sessions list's Go to: switch to the
   * session's repository, open its dock, then reveal. Calling `reveal`
   * directly selected the tab in a panel that might be another repository's,
   * hidden — and for a session adopted after a reload, queued its tab for a
   * dock nobody opened.
   */
  async function open(sessionId: unknown) {
    if (typeof sessionId !== "string" || !sessionId) return;
    const record = sessionByNativeId($terminalSessions, sessionId);
    const outcome = await focusTerminalSession(record);
    if (outcome.ok) {
      missed = null;
      return;
    }
    say(
      outcome.reason === "no-session"
        ? "That terminal session has ended."
        : "That session is no longer on screen. Open its terminal from the Sessions list.",
      6000,
    );
  }

  function say(message: string, ms: number) {
    missed = message;
    if (timer) clearTimeout(timer);
    timer = setTimeout(() => (missed = null), ms);
  }

  onMount(() => {
    if (!isTauri()) return;
    // Once per page. After a reload the host still runs what the previous
    // page started; without this they would be invisible and uncounted.
    void adoptDetachedSessions({ familyOf: (path) => repoStore.familyOf(path) }).then(
      (count) => {
        if (count) say(`${count === 1 ? "A terminal session is" : `${count} terminal sessions are`} still running from before the window reloaded. Find ${count === 1 ? "it" : "them"} under Sessions in the terminal dock.`, 10000);
      },
      (error: unknown) => say(`Terminal sessions left running before the window reloaded could not be listed: ${String(error)}`, 15000),
    );
    const listeners = createListenerTracker();
    void listen<string>("gitpulse-session-notification-open", (event) => void open(event.payload))
      .then((unlisten) => listeners.track(unlisten))
      .catch(() => {
        missed = "Session notification clicks cannot be delivered. Open the terminal directly.";
      });
    // What each agent last asked for, for a task's Agents pane — and, once
    // listening, what stood while this page was not (start-up, a reload).
    void bindAttention(listen, sessionActivity, standingAttention, (error: unknown) =>
      say(`Agents that asked for you before the window loaded cannot be shown: ${String(error)}`, 15000))
      .then((unlisten) => listeners.track(unlisten))
      .catch((error: unknown) => say(`Agents asking for you cannot be shown in their task: ${String(error)}`, 15000));
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
