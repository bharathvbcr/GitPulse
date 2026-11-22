<script lang="ts">
  /**
   * Which editor "Open in editor" hands a file to. Auto-detect is the first of
   * Visual Studio Code, Cursor and Zed whose command-line launcher is found.
   * The choice is one of these four names, never a program: the backend maps
   * it to a fixed launcher.
   */
  import SettingSegment from "./SettingSegment.svelte";
  import { EXTERNAL_EDITOR_LABELS, interfaceStore, type EditorChoice } from "../stores/interfaceStore";

  const OPTIONS: readonly { value: EditorChoice; label: string }[] = (
    Object.keys(EXTERNAL_EDITOR_LABELS) as EditorChoice[]
  ).map((value) => ({ value, label: EXTERNAL_EDITOR_LABELS[value] }));
</script>

<div class="space-y-1 text-meta" data-testid="external-editor-setting">
  <div class="text-textPrimary text-meta font-medium mb-2">Open in editor</div>
  <SettingSegment
    ariaLabel="Editor for Open in editor"
    options={OPTIONS}
    value={$interfaceStore.externalEditor}
    onselect={(editor) => interfaceStore.setExternalEditor(editor)}
  />
  <p class="text-textMuted text-micro mt-1.5">
    Opens a file at its line in your own editor, so your extensions keep working there. Auto-detect
    uses the first of Visual Studio Code, Cursor and Zed that is installed.
  </p>
</div>
