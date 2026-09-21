<script lang="ts">
  /**
   * Notifications for agent sessions running in a terminal tab.
   *
   * Separate from `NativeNotificationSettings`, which governs the workbench's
   * *activity* inbox: that queue is durable, task-shaped and lives in the
   * profile database, while a terminal session is ephemeral and exists whether
   * or not a task ever did. They share the OS permission and nothing else.
   *
   * The counters are shown next to the switches on purpose. "Enabled, nothing
   * delivered" and "enabled, nine suppressed because you were watching" are
   * indistinguishable from the outside and mean opposite things, and the second
   * is the system working. A panel that showed only the switches would leave a
   * user with no way to tell a silent feature from a broken one.
   */
  import { onMount } from "svelte";
  import { isTauri } from "../platform";
  import { hostPlatform } from "../stores/platformStore";
  import { desktopNotificationsSupported, notificationUnavailableReason } from "../ui/platformCopy";
  import { explainError, nativeNotificationStatus, type NativeNotificationStatus } from "../workbench/client";
  import {
    loadSessionAlerts,
    refreshSessionAlerts,
    saveSessionAlerts,
    sessionAlerts,
    type SessionAlertSettings,
  } from "../stores/sessionAlertsStore";

  let { active = true }: { active?: boolean } = $props();

  let draft = $state<SessionAlertSettings | null>(null);
  let native = $state<NativeNotificationStatus | null>(null);
  let busy = $state(false);
  let error = $state("");
  let quiet = $state(false);
  let start = $state("22:00");
  let end = $state("07:00");
  let disposed = false;
  let loaded = false;

  const view = $derived($sessionAlerts);
  const supported = $derived(desktopNotificationsSupported(isTauri(), $hostPlatform.os, native));
  const unavailable = $derived(
    supported
      ? null
      : notificationUnavailableReason($hostPlatform.os, native?.available ?? false, native?.error ?? null),
  );

  const clock = (n: number) =>
    `${String(Math.floor(n / 60)).padStart(2, "0")}:${String(n % 60).padStart(2, "0")}`;
  const minute = (s: string) => {
    if (!/^([01][0-9]|2[0-3]):[0-5][0-9]$/.test(s)) throw new Error("Choose valid local quiet hours.");
    const [h, m] = s.split(":").map(Number);
    return h * 60 + m;
  };

  /**
   * Everything the notifier chose not to show, by reason.
   *
   * Derived rather than written out so a counter added to the backend cannot
   * be forgotten here — the total below is the sum of this list, and the two
   * cannot disagree.
   */
  const suppressed = $derived([
    { label: "you were watching the session", count: view.status.suppressed_attended },
    { label: "notifications are off", count: view.status.suppressed_disabled },
    { label: "it was a plain shell", count: view.status.suppressed_not_agent },
    { label: "quiet hours", count: view.status.suppressed_quiet },
    { label: "the rate limit", count: view.status.rate_limited },
    { label: "another notice replaced it", count: view.status.coalesced },
  ]);
  const suppressedTotal = $derived(suppressed.reduce((sum, row) => sum + row.count, 0));
  /**
   * Signals that never reached a decision at all. Distinct from the list above:
   * a suppressed notice was judged, a dropped one was lost, and reporting them
   * as one number would hide a fault behind a policy.
   */
  const lost = $derived(
    view.status.dropped_queue + view.status.dropped_scan + view.status.displaced,
  );

  async function load() {
    if (busy) return;
    busy = true;
    error = "";
    try {
      const fresh = await (loaded ? refreshSessionAlerts() : loadSessionAlerts());
      if (disposed) return;
      loaded = true;
      draft = { ...fresh.settings };
      quiet = fresh.settings.quiet_start !== null;
      if (fresh.settings.quiet_start !== null && fresh.settings.quiet_end !== null) {
        start = clock(fresh.settings.quiet_start);
        end = clock(fresh.settings.quiet_end);
      }
      if (isTauri()) {
        const status = await nativeNotificationStatus();
        if (!disposed) native = status;
      }
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) busy = false;
    }
  }

  async function save() {
    if (busy || !draft) return;
    busy = true;
    error = "";
    try {
      const next: SessionAlertSettings = {
        ...draft,
        quiet_start: quiet ? minute(start) : null,
        quiet_end: quiet ? minute(end) : null,
      };
      if (quiet && next.quiet_start === next.quiet_end) {
        throw new Error("Quiet-hour start and end must differ.");
      }
      if (next.enabled && isTauri() && !["authorized", "provisional"].includes(native?.authorization ?? "")) {
        // The OS permission is shared with activity notifications, so asking
        // here grants both. Asked only when turning the feature on: a prompt
        // for a permission the user has not opted into is a prompt they cannot
        // answer meaningfully.
        const status = await nativeNotificationStatus(true);
        if (disposed) return;
        native = status;
      }
      const saved = await saveSessionAlerts(next);
      if (disposed) return;
      draft = { ...saved.settings };
    } catch (cause) {
      if (!disposed) error = explainError(cause);
    } finally {
      if (!disposed) busy = false;
    }
  }

  onMount(() => () => {
    disposed = true;
  });

  $effect(() => {
    if (active && !loaded && !busy) void load();
  });
</script>

<div class="space-y-2 text-[11px]" data-testid="session-alerts">
  <h4 class="text-[10px] font-bold uppercase tracking-wider text-textMuted">Agent session notifications</h4>
  <p class="text-textMuted leading-snug">
    A native banner when an agent running in a terminal tab needs you or finishes. Suppressed while
    you are looking at that session.
  </p>
  {#if unavailable}<p class="text-textMuted" data-testid="session-alerts-unavailable">{unavailable}</p>{/if}
  {#if native && supported}
    <p class="text-textMuted">
      System permission: {native.authorization.replaceAll("_", " ")}{native.error ? ` · ${native.error}` : ""}
    </p>
  {/if}

  {#if draft}
    <fieldset class="grid gap-2 border-0 p-0" disabled={busy}>
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={draft.enabled} /> Notify me about agent sessions</label>
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={draft.sound} /> Play a sound</label>
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={draft.shell_bell} /> Also notify for a plain shell's bell</label>
      <label class="flex items-center gap-2">
        <input type="checkbox" bind:checked={draft.configure_agents} />
        Configure agent CLIs GitPulse launches
      </label>
      <p class="text-textMuted pl-6 leading-snug">
        Adds each CLI's own documented notification flag to the sessions GitPulse starts —
        <span class="font-mono">claude --settings</span> and <span class="font-mono">codex -c tui.notifications</span>.
        For that session only; no file of yours is written and no other setting is changed. Without
        it, neither CLI signals this terminal and a waiting agent stays silent.
      </p>
      <label class="flex items-center gap-2">
        <input type="checkbox" bind:checked={draft.hook_bridge} disabled={!view.bridge_supported} />
        Accept reports from agent hooks
      </label>
      <p class="text-textMuted pl-6 leading-snug">
        {#if view.bridge_supported}
          Opens a private socket in GitPulse's own folder, readable only by you, while the app is
          running. The GitPulse plugin's hooks use it to say <em>why</em> an agent stopped —
          permission, idle, finished — instead of only that it did. A report can raise a banner and
          nothing else.
          {#if view.status.bridge_path}<br /><span class="font-mono text-[10px]">{view.status.bridge_path}</span>{/if}
        {:else}
          Needs a Unix socket, which this platform does not offer.
        {/if}
      </p>
      <label class="flex items-center gap-2"><input type="checkbox" bind:checked={quiet} /> Quiet hours in this computer's local time</label>
      {#if quiet}
        <div class="flex gap-3 pl-6">
          <label class="flex items-center gap-1">From <input class="gp-field" type="time" bind:value={start} /></label>
          <label class="flex items-center gap-1">Until <input class="gp-field" type="time" bind:value={end} /></label>
        </div>
      {/if}
    </fieldset>

    <div class="text-textMuted leading-snug" data-testid="session-alerts-counts">
      {#if !view.status.running}
        <p role="alert">The notification worker is not running, so nothing can be delivered.</p>
      {:else}
        <p>{view.status.delivered} delivered · {suppressedTotal} suppressed{lost > 0 ? ` · ${lost} lost` : ""}{view.status.failed > 0 ? ` · ${view.status.failed} refused by the system` : ""}</p>
        {#if suppressedTotal > 0}
          <ul class="mt-1 space-y-0.5">
            {#each suppressed.filter((row) => row.count > 0) as row (row.label)}
              <li>{row.count} because {row.label}</li>
            {/each}
          </ul>
        {/if}
        {#if lost > 0}
          <p role="alert">
            {view.status.dropped_queue} could not be queued, {view.status.dropped_scan} were read but
            not carried, and {view.status.displaced} were pushed out by other sessions. These were
            never judged; they are a fault, not a preference.
          </p>
        {/if}
        {#if view.status.bridge_rejected > 0}
          <p>{view.status.bridge_rejected} hook reports were refused as malformed or not yours.</p>
        {/if}
      {/if}
      {#if view.status.last_error}<p role="alert">{view.status.last_error}</p>{/if}
    </div>

    <div class="flex gap-2">
      <button type="button" class="gp-btn text-[11px] px-2 py-0.5" onclick={save} disabled={busy}>Save</button>
      <button type="button" class="gp-btn text-[11px] px-2 py-0.5" onclick={load} disabled={busy}>Recheck</button>
    </div>
  {/if}
  {#if error}<p role="alert" class="text-danger">{error}</p>{/if}
</div>
