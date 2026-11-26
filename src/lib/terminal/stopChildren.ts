/**
 * The question before "Stop children" ends the sessions an agent spawned.
 * Always asked: each child is running by definition and nothing here has
 * looked at what it is doing; stopping ends it and cannot be undone.
 */

const NAMES_SHOWN = 8;

export function stopChildrenQuestion(labels: readonly string[]): { title: string; message: string } {
  const count = labels.length;
  const names = labels.map((label) => label.trim() || "Untitled session");
  const shown = names.slice(0, NAMES_SHOWN).join(", ");
  const rest = names.length > NAMES_SHOWN ? `, and ${names.length - NAMES_SHOWN} more` : "";
  return {
    title: `Stop ${count} child session${count === 1 ? "" : "s"}?`,
    message:
      `${shown}${rest} ${count === 1 ? "is" : "are"} still running. ` +
      `Stopping ends ${count === 1 ? "it" : "them"} and closes the tab${count === 1 ? "" : "s"}; ` +
      `whatever ${count === 1 ? "it is" : "they are"} doing is lost.`,
  };
}
