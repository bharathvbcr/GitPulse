<script lang="ts">
  import { onMount } from "svelte";
  import { isTauri } from "../platform";
  import { explainError, getNotificationSettings, nativeNotificationStatus, newID, notificationDraft, putNotificationSettings, type NativeNotificationStatus, type NotificationDraft, type NotificationSettings, type NotificationWrite, type Scope } from "../workbench/client";
  let { scope, taskID }: { scope: Scope; taskID?: string } = $props();
  let saved = $state<NotificationSettings | null>(null), draft = $state<NotificationDraft | null>(null), native = $state<NativeNotificationStatus | null>(null);
  let pending = $state<NotificationWrite | null>(null), busy = $state(false), error = $state("");
  let quiet = $state(false), start = $state("22:00"), end = $state("07:00");
  let disposed = false, loaded = false;
  let expanded = $state(false);
  const muteField = $derived(taskID ? "muted_task_ids" : scope.kind === "workspace" ? "muted_workspace_ids" : "muted_repository_ids");
  const muted = $derived(!!draft?.[muteField].includes(taskID ?? (scope.kind !== "global" ? scope.id : "")));
  const muteCount = $derived(draft ? draft.muted_workspace_ids.length + draft.muted_repository_ids.length + draft.muted_task_ids.length : 0);
  const clock = (n: number) => `${String(Math.floor(n / 60)).padStart(2, "0")}:${String(n % 60).padStart(2, "0")}`;
  const minute = (s: string) => { if (!/^([01][0-9]|2[0-3]):[0-5][0-9]$/.test(s)) throw new Error("Choose valid local quiet hours."); const [h, m] = s.split(":").map(Number); return h * 60 + m; };
  async function refresh() {
    if (busy || pending) return; busy = true; error = "";
    try {
      const settings = await getNotificationSettings(); if (disposed) return;
      saved = settings; draft = notificationDraft(settings); quiet = settings.quiet_start !== null;
      if (settings.quiet_start !== null && settings.quiet_end !== null) { start = clock(settings.quiet_start); end = clock(settings.quiet_end); }
      if (isTauri()) { const status = await nativeNotificationStatus(); if (!disposed) native = status; }
    } catch (cause) { if (!disposed) error = explainError(cause); } finally { if (!disposed) busy = false; }
  }
  onMount(() => () => { disposed = true; });
  $effect(() => { if (expanded && !loaded) { loaded = true; void refresh(); } });
  function toggleMute() {
    if (!draft || !taskID && scope.kind === "global") return;
    const id = taskID ?? (scope.kind !== "global" ? scope.id : "");
    const ids = draft[muteField]; draft = { ...draft, [muteField]: muted ? ids.filter((value) => value !== id) : [...ids, id] };
  }
  async function save() {
    if (busy || !saved || !draft) return;
    busy = true; error = "";
    try {
      if (!pending) {
        const next = { ...draft, quiet_start: quiet ? minute(start) : null, quiet_end: quiet ? minute(end) : null };
        if (quiet && next.quiet_start === next.quiet_end) throw new Error("Quiet-hour start and end must differ.");
        if (next.enabled && isTauri() && (!native || !["authorized", "provisional"].includes(native.authorization))) {
          const status = await nativeNotificationStatus(true); if (disposed) return; native = status;
          if (!status.available || !["authorized", "provisional"].includes(status.authorization)) throw new Error(status.error ?? "Allow GitPulse notifications in macOS Settings, then try again.");
        }
        pending = { ...next, id: "profile", expected_revision: saved.revision, request_id: newID() };
      }
      const result = await putNotificationSettings(pending); if (disposed) return;
      saved = result; draft = notificationDraft(result); pending = null;
    } catch (cause) {
      if (!disposed) {
        error = explainError(cause);
        if (typeof cause === "object" && cause !== null && "code" in cause && !["transport_error", "worker_error", "protocol_error", "timeout"].includes(String(cause.code))) pending = null;
      }
    } finally { if (!disposed) busy = false; }
  }
</script>

<details class="native-settings" bind:open={expanded}>
  <summary>Desktop notifications{saved ? saved.enabled ? " · Enabled" : " · Off" : ""}</summary>
  {#if !isTauri()}<p>This preview can save settings. OS banners require the desktop app.</p>{/if}
  {#if native}<p>macOS permission: {native.authorization.replaceAll("_", " ")}{native.error ? ` · ${native.error}` : ""}</p>{/if}
  {#if draft}
    <fieldset disabled={busy || !!pending}>
      <label><input type="checkbox" bind:checked={draft.enabled} disabled={!draft.enabled && isTauri() && native?.available === false} /> Enable desktop notifications</label>
      <label><input type="checkbox" bind:checked={draft.sound} /> Play a sound</label>
      <label><input type="checkbox" bind:checked={draft.background} /> Notify while GitPulse is hidden or minimized</label>
      <label><input type="checkbox" bind:checked={quiet} /> Quiet hours in this Mac’s local time</label>
      {#if quiet}<div class="times"><label>From <input type="time" bind:value={start} /></label><label>Until <input type="time" bind:value={end} /></label></div>{/if}
      {#if taskID || scope.kind !== "global"}<label><input type="checkbox" checked={muted} onchange={toggleMute} /> Mute this {taskID ? "task" : scope.kind}</label>{/if}
      {#if draft && muteCount > 0}<div><p>{muteCount} saved scope mutes, including any deleted tasks or workspaces.</p><button type="button" onclick={() => { if (draft) draft = { ...draft, muted_workspace_ids: [], muted_repository_ids: [], muted_task_ids: [] }; }}>Clear saved scope mutes</button></div>{/if}
    </fieldset>
    <p>New activity only when enabled. Generic previews keep task content private. Submitted means macOS accepted the request; Focus and system settings can suppress display.</p>
    <button onclick={save} disabled={busy}>{pending ? "Retry saved settings" : "Save notification settings"}</button>
  {/if}
  <button onclick={refresh} disabled={busy || !!pending}>Recheck settings</button>
  {#if error}<p role="alert">{error}</p>{/if}
</details>

<style>
  .native-settings{margin:12px 0;padding:10px;border:1px solid rgb(var(--c-border));border-radius:8px;font-size:12px}summary{cursor:pointer}fieldset{border:0;padding:12px 0;display:grid;gap:9px}label{display:flex;align-items:center;gap:7px}.times{display:flex;gap:12px}p{color:rgb(var(--c-text-muted));line-height:1.5}button,input[type=time]{font:inherit;color:rgb(var(--c-text));background:rgb(var(--c-surface));border:1px solid rgb(var(--c-border));border-radius:6px;padding:6px 9px}button{margin-right:8px;cursor:pointer}button:disabled{opacity:.5}input{accent-color:rgb(var(--c-accent))}[role=alert]{color:#ef9a9a}
</style>
