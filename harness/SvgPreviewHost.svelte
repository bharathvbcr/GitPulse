<script lang="ts">
  /**
   * Mounts GitPulse's live SVG preview surfaces for `svg-preview.html`:
   * - `FileTreePanel` to drive real right-click context menu actions
   * - `SvgPreviewViewer` (simulating FileViewer's active tab)
   * - `MediaViewer` (simulating TerminalFilePane's LazyMount delegation)
   */
  import { untrack } from "svelte";
  import FileTreePanel from "../src/lib/components/files/FileTreePanel.svelte";
  import SvgPreviewViewer from "../src/lib/components/files/SvgPreviewViewer.svelte";
  import MediaViewer from "../src/lib/components/files/MediaViewer.svelte";

  interface ViewerProps {
    repoPath: string;
    filePath: string;
    text: string | null;
    base64?: string | null;
    isBinary?: boolean;
    diskBase?: string | null;
    draftContent?: string | null;
    revealTarget?: { line: number | null; column?: number | null; seq: number } | null;
  }

  let {
    initial,
    width = 920,
    height = 600,
    onTreeSelect,
  }: {
    initial: ViewerProps;
    width?: number;
    height?: number;
    onTreeSelect?: (path: string) => void;
  } = $props();

  let primary = $state<ViewerProps>(untrack(() => ({ ...initial })));
  let secondary = $state<ViewerProps>(untrack(() => ({ ...initial })));
  let hostWidth = $state(untrack(() => width));
  let hostHeight = $state(untrack(() => height));
  let mountSecondary = $state(true);

  export function updatePrimary(next: Partial<ViewerProps>) {
    primary = { ...primary, ...next };
  }

  export function updateSecondary(next: Partial<ViewerProps>) {
    secondary = { ...secondary, ...next };
  }

  export function setSecondaryMounted(mounted: boolean) {
    mountSecondary = mounted;
  }

  export function resize(nextWidth: number, nextHeight = hostHeight) {
    hostWidth = nextWidth;
    hostHeight = nextHeight;
  }
</script>

<div class="flex flex-col gap-3 p-2 bg-background text-textPrimary">
  <div class="flex gap-3">
    <div data-host="tree" class="shrink-0" style="width: 240px; height: {hostHeight}px; display: flex; flex-direction: column">
      <FileTreePanel
        repoPath={primary.repoPath}
        selectedFile={primary.filePath}
        onSelectFile={(path) => onTreeSelect?.(path)}
      />
    </div>

    <div data-host="primary" class="shrink-0" style="width: {hostWidth}px; height: {hostHeight}px; display: flex; flex-direction: column">
      <SvgPreviewViewer
        filePath={primary.filePath}
        repoPathOverride={primary.repoPath}
        blob={{
          path: primary.filePath,
          is_binary: primary.isBinary ?? false,
          is_image: true,
          mime: "image/svg+xml",
          text: primary.text,
          base64: primary.base64 ?? null,
          disk_base: primary.diskBase ?? null,
        }}
        draftContent={primary.draftContent ?? null}
        dirty={primary.draftContent !== null && primary.draftContent !== undefined && primary.draftContent !== primary.text}
        revealTarget={primary.revealTarget ?? null}
        onDraftChange={(next) => {
          primary = { ...primary, draftContent: next };
        }}
      />
    </div>
  </div>

  {#if mountSecondary}
    <div data-host="secondary" class="shrink-0" style="width: {hostWidth}px; height: {hostHeight}px; display: flex; flex-direction: column">
      <MediaViewer
        filePath={secondary.filePath}
        repoPath={secondary.repoPath}
        blob={{
          path: secondary.filePath,
          is_binary: secondary.isBinary ?? false,
          is_image: true,
          mime: "image/svg+xml",
          text: secondary.text,
          base64: secondary.base64 ?? null,
          disk_base: secondary.diskBase ?? null,
        }}
        draftContent={secondary.draftContent ?? null}
        dirty={secondary.draftContent !== null && secondary.draftContent !== undefined && secondary.draftContent !== secondary.text}
        revealTarget={secondary.revealTarget ?? null}
        onDraftChange={(next) => {
          secondary = { ...secondary, draftContent: next };
        }}
      />
    </div>
  {/if}
</div>
