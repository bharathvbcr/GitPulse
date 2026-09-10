<script lang="ts">
  import { onMount } from "svelte";
  import { listen } from "@tauri-apps/api/event";
  import { isTauri } from "../platform";
  import { LAYERS } from "../ui/layers";
  import { trapFocus } from "../ui/focusTrap";
  import type { Attention, NotificationDelivery, Repository, Task, WorkspaceCard } from "../workbench/client";
  let current = $state<NotificationDelivery | null>(null), notice = $state<Attention | null>(null), task = $state<Task | null>(null);
  let repositories = $state<Repository[]>([]), workspaces = $state<WorkspaceCard[]>([]);
  let Editor = $state<typeof import("./TaskEditor.svelte").default | null>(null);
  let error = $state(""), busy = $state(false);
  let disposed = false, loading = false, again = false;
  async function receive() {
    if (disposed || current || !isTauri()) return;
    if (loading) { again = true; return; }
    loading = true;
    try {
      const client = await import("../workbench/client");
      const activation = await client.pendingNotificationActivation();
      if (!activation || disposed) return;
      current = activation; error = "";
      const latest = await client.getAttention(activation.id);
      if (disposed) return;
      if (latest.task_id !== activation.task_id) throw new Error("Notification target does not match its saved delivery.");
      notice = latest;
      if (latest.target_status === "task_deleted" || latest.target_status === "unavailable") return;
      const value = await client.getTask(latest.task_id);
      const [repos, workspace, editor] = await Promise.all([
        Promise.all(value.repository_ids.map((id) => client.getRepository(id))),
        value.home_workspace_id ? client.getWorkspace(value.home_workspace_id) : Promise.resolve(null),
        import("./TaskEditor.svelte"),
      ]);
      if (disposed) return;
      task = value; repositories = repos; workspaces = workspace ? [{ ...workspace, repository_count: workspace.repository_ids.length }] : []; Editor = editor.default;
    } catch (cause) { if (!disposed) error = cause instanceof Error ? cause.message : "Cannot open the saved notification. Open the activity inbox."; }
    finally { loading = false; if (again) { again = false; void receive(); } }
  }
  async function close() {
    if (!current || busy) return; busy = true; error = "";
    try {
      const { acknowledgeNotification } = await import("../workbench/client");
      await acknowledgeNotification(current.id);
      if (disposed) return;
      current = null; task = null; notice = null; Editor = null;
      await receive();
    } catch (cause) { if (!disposed) error = cause instanceof Error ? cause.message : "Could not acknowledge this notification. Try closing again."; }
    finally { if (!disposed) busy = false; }
  }
  onMount(() => {
    if (!isTauri()) return;
    void receive();
    let stop: (() => void) | undefined;
    void listen("workbench-notification-open", () => { void receive(); }).then((unlisten) => { if (disposed) unlisten(); else { stop = unlisten; void receive(); } }).catch(() => { if (!disposed) error = "Native activation updates are unavailable. Open the activity inbox."; });
    const focus = () => { void receive(); }; window.addEventListener("focus", focus);
    return () => { disposed = true; stop?.(); window.removeEventListener("focus", focus); };
  });
</script>

{#if current}
  <div class="native-activation" style:z-index={LAYERS.MODAL} role="dialog" aria-modal="true" aria-label="Notification review" tabindex="-1" use:trapFocus>
    <div class="receipt"><strong>{notice?.title ?? "GitPulse activity"}</strong><p>{notice?.target_status === "current" ? "Opened from a desktop notification. Review and task acceptance remain your decision." : notice ? `This notification’s target is ${notice.target_status.replaceAll("_", " ")}. Available task details show the current saved version.` : "Resolving the saved notification…"}</p>{#if error}<p role="alert">{error}</p>{/if}{#if !task}<button onclick={close} disabled={busy}>Close notification</button>{/if}</div>
    {#if Editor && task}<Editor value={task} {repositories} {workspaces} onSaved={(value) => { task = value; }} onClose={() => { void close(); }} />{/if}
  </div>
{:else if error}<div class="activation-error" style:z-index={LAYERS.MODAL} role="status">{error}</div>{/if}

<style>
  .native-activation{position:fixed;top:48px;bottom:24px;left:50%;transform:translateX(-50%);width:min(620px,calc(100vw - 48px));overflow:hidden;display:flex;flex-direction:column;background:rgb(var(--c-bg));border:1px solid rgb(var(--c-border));border-radius:12px;box-shadow:0 20px 80px #0008;color:rgb(var(--c-text))}.native-activation :global(.task-editor){width:100%;box-sizing:border-box;border-left:0;flex:1;min-height:0;overflow:hidden}.receipt{flex-shrink:0;padding:16px 20px;font-size:12px;border-bottom:1px solid rgb(var(--c-border))}.receipt p{color:rgb(var(--c-text-muted));line-height:1.5}button{font:inherit;padding:6px 10px;border-radius:6px;background:rgb(var(--c-surface));border:1px solid rgb(var(--c-border));color:inherit;cursor:pointer}.activation-error{position:fixed;bottom:30px;right:20px;max-width:360px;padding:12px;background:rgb(var(--c-surface));color:rgb(var(--c-text));border:1px solid rgb(var(--c-border));border-radius:8px;font-size:12px}[role=alert]{color:#ef9a9a}
</style>
