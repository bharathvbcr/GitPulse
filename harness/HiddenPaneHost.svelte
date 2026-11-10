<script lang="ts">
  import Sidebar from "../src/lib/components/Sidebar.svelte";
  import TerminalDock from "../src/lib/components/TerminalDock.svelte";
  import VirtualList from "../src/lib/components/VirtualList.svelte";

  /**
   * The row App.svelte builds around the sidebar and the dock: a relative row
   * holding the sidebar and a `.gp-workspace` pane, with the dock under the
   * view inside that pane. The view is a `VirtualList`, the component that
   * renders only an overscan band when its pane has been measured at zero.
   * The page drives the overlays and resizes the shell; this component owns
   * only what is mounted and whether the pane is shown.
   */
  const load = () => import("./PanelStub.svelte");
  const items = Array.from({ length: 4000 }, (_, index) => index);

  let dockOpen = $state(false);
  let paneHidden = $state(false);
  let shellHeight = $state(520);
  let shellWidth = $state(1000);

  export function setDockOpen(next: boolean) {
    dockOpen = next;
  }
  export function setPaneHidden(next: boolean) {
    paneHidden = next;
  }
  export function setShellSize(width: number, height: number) {
    shellWidth = width;
    shellHeight = height;
  }
</script>

<div
  id="row"
  class="relative flex overflow-hidden bg-background text-textPrimary"
  style="width:{shellWidth}px; height:{shellHeight}px"
>
  <Sidebar />
  <main
    id="pane"
    class="gp-workspace flex-1 flex flex-col min-w-0 bg-background gp-pane relative"
    style:display={paneHidden ? "none" : null}
  >
    <div class="gp-view flex-1 flex flex-col min-h-0">
      <VirtualList {items} rowHeight={24} class="flex-1 min-h-0">
        {#snippet row(item, index)}
          <div data-row={index} class="px-3 text-xs leading-6 truncate" style="height:24px">Row {item ?? index}</div>
        {/snippet}
      </VirtualList>
    </div>
    <TerminalDock open={dockOpen} onClose={() => (dockOpen = false)} {load} />
  </main>
</div>
