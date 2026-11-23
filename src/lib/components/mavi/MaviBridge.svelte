<script lang="ts">
  /**
   * Mavi's presence outside its own view. Mounted once by App:
   * - starts the store after the first paint, so the header badge, the dock
   *   pill, the quit question and Explorer know about running threads before
   *   anyone opens Mavi;
   * - forwards the files a thread's agent changes to the terminal dock, so
   *   open file panes reload and Follow agent works for Mavi too;
   * - announces a thread that starts waiting for an answer, or fails, unless
   *   that thread is on screen in a focused window. Only transitions this run
   *   saw are announced (`maviNotices`), so a reload is quiet.
   */
  import { onMount, untrack } from "svelte";
  import { maviStore } from "../../mavi/store.svelte";
  import { maviNotices } from "../../mavi/notify";
  import { showMaviThread } from "../../mavi/navigate";
  import type { ThreadStatus } from "../../mavi/threads";
  import { toastStore } from "../../stores/toastStore";
  import { repoStore } from "../../stores/repoStore";
  import { interfaceStore } from "../../stores/interfaceStore";
  import { agentEvents } from "../../terminal/agentEvents";

  let ready = $state(false);
  const seen = new Map<string, ThreadStatus>();
  const announced = new Map<string, number>();

  onMount(() => {
    // After first paint: starting Mavi must never delay opening a repository.
    const start = () => {
      void maviStore
        .init()
        .catch(() => {})
        .finally(() => (ready = true));
    };
    if (typeof requestIdleCallback === "function") {
      const handle = requestIdleCallback(start, { timeout: 2000 });
      return () => cancelIdleCallback(handle);
    }
    const timer = setTimeout(start, 300);
    return () => clearTimeout(timer);
  });

  // Files a Mavi agent changes reach the dock the way a terminal agent's do:
  // open file panes showing them reload, and Follow agent can show them.
  onMount(() =>
    maviStore.onFileChanged((record, path) => {
      if (record.target.kind !== "local") return;
      agentEvents.dispatchTouchedFile({ id: `mavi:${record.key}`, repo_path: record.target.root, path, artifact: false });
    }),
  );

  function onScreen(key: string): boolean {
    return (
      typeof document !== "undefined" &&
      document.visibilityState === "visible" &&
      document.hasFocus() &&
      $interfaceStore.globalSurface === "repository" &&
      $repoStore.activeTab === "mavi" &&
      // False while an expanded terminal dock covers the view.
      maviStore.viewing &&
      maviStore.openKey === key
    );
  }

  $effect(() => {
    if (!ready) return;
    const now = maviStore.records.map((record) => [record.key, maviStore.status(record)] as const);
    untrack(() => {
      for (const notice of maviNotices(seen, now, announced)) {
        if (onScreen(notice.key)) continue;
        const record = maviStore.record(notice.key);
        if (!record) continue;
        const open = { label: "Open", onClick: () => void showMaviThread(notice.key) };
        if (notice.kind === "needs_you") toastStore.warning(`“${record.title}” is waiting for your answer.`, open);
        else toastStore.warning(`“${record.title}” stopped with an error.`, open);
      }
    });
  });
</script>
