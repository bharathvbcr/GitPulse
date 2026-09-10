// Runs against the production components and a disposable, real Manvi profile.
// The scripted model proves lifecycle behavior, not semantic rewrite quality.
export async function runWorkbenchChecks(): Promise<string[]> {
  const results: string[] = [];
  const assert = (condition: boolean, message: string) => { if (!condition) throw new Error(message); };
  const editor = () => {
    const element = document.querySelector<HTMLElement>('aside[aria-label="Task details"], aside[aria-label="New task"]');
    if (!element) throw new Error("Task editor is missing");
    return element;
  };
  const button = (name: string, root: ParentNode = document) => {
    const element = [...root.querySelectorAll("button")].find((b) => b.textContent?.trim() === name || b.getAttribute("aria-label") === name);
    if (!element) throw new Error(`Missing button: ${name}`);
    return element;
  };
  const field = (name: string, selector: string, root: ParentNode = editor()) => {
    const label = [...root.querySelectorAll("label")].find((label) => (label.textContent?.trim() === name || label.querySelector(selector)?.getAttribute("aria-label") === name) && label.querySelector(selector));
    const element = label?.querySelector(selector);
    if (!(element instanceof HTMLInputElement || element instanceof HTMLTextAreaElement)) throw new Error(`Missing field: ${name}`);
    return element;
  };
  const fill = (name: string, selector: string, value: string) => {
    const element = field(name, selector);
    element.value = value; element.dispatchEvent(new Event("input", { bubbles: true }));
  };
  const wait = async (condition: () => boolean) => {
    const deadline = Date.now() + 15000;
    let lastError = "";
    while (true) {
      try { if (condition()) return; }
      catch (error) { lastError = error instanceof Error ? error.message : String(error); }
      if (Date.now() >= deadline) throw new Error(`UI did not reach the expected state: ${lastError}; ${document.querySelector("aside")?.textContent}`);
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  };
  const review = () => {
    const element = editor().querySelector<HTMLElement>('article[aria-label="Enhancement review"]');
    if (!element) throw new Error("Enhancement review is missing");
    return element;
  };
  const state = (value: string) => editor().querySelector("article h3")?.textContent === value;
  const check = (name: string, root: ParentNode = editor()) => {
    const element = field(name, 'input[type="checkbox"]', root);
    if (!(element instanceof HTMLInputElement)) throw new Error("Expected a checkbox");
    return element;
  };
  const switchControl = (name: string, root: ParentNode = editor()) => {
    const element = [...root.querySelectorAll<HTMLButtonElement>('button[role="switch"]')].find((node) => node.getAttribute("aria-label") === name);
    if (!element) throw new Error(`Missing switch: ${name}`);
    return element;
  };
  const enhance = (root: ParentNode = editor()) => button("Improve with Manvi", root);
  const { harnessStore } = await import("../src/lib/stores/harnessStore");
  void harnessStore.selectModel({ base_url: "http://127.0.0.1:11434/v1", model: "fixture-model" });
  const close = [...document.querySelectorAll("button")].find((b) => b.getAttribute("aria-label") === "Close task details");
  close?.click();
  button("New task").click();
  await wait(() => Boolean(document.querySelector('aside[aria-label="New task"]')));
  const title = `Preserve E42 evidence ${Date.now()}`;
  const description = "Preserve the original repository evidence.";
  fill("Title", "input", title);
  fill("Description", "textarea", description);
  fill("Acceptance criteria", "textarea", "Both repository checks remain available");
  check("GitPulse").click();
  await wait(() => !button("Save task", editor()).disabled);
  button("Save task", editor()).click();
  await wait(() => editor().dataset.taskRevision === "1");
  assert(Boolean(editor().querySelector(".manvi-assist .change-link")), "Merged Manvi section is missing the Change link");
  assert(![...editor().querySelectorAll(".manvi-assist label")].some((label) => label.firstChild?.textContent?.trim() === "Model"), "Merged Manvi section must not expose a Model input");
  await wait(() => !enhance().matches(":disabled"));
  enhance().click();
  await wait(() => state("Ready for review") || Boolean(button("Use both", editor())) || Boolean(button("Use this title", editor())));
  if (!editor().querySelector('article[aria-label="Enhancement review"]')) {
    editor().querySelector<HTMLElement>(".history-drawer > summary")?.click();
    await wait(() => Boolean(editor().querySelector('article[aria-label="Enhancement review"]')));
  }
  assert(field("Title", "input").value === title, "Generation changed the task before acceptance");
  assert(field("Description", "textarea").value === description, "Generation changed description before acceptance");
  results.push("Generation leaves saved task fields unchanged");
  button("Edit suggestion", editor()).click();
  await wait(() => button("Accept selected fields", editor()).matches(":disabled"));
  assert(button("Close task details").disabled, "Suggestion draft can be closed without saving");
  const revisedTitle = `${title} after review`;
  fill("Revised title", "textarea", revisedTitle);
  await wait(() => !button("Save suggestion edits", editor()).matches(":disabled"));
  button("Save suggestion edits", editor()).click();
  await wait(() => editor().textContent?.includes("Suggestion saved. The task has not changed.") === true);
  assert(editor().dataset.taskRevision === "1", "Suggestion editing changed the task revision");
  assert(field("Title", "input").value === title, "Suggestion editing changed the task title");
  assert(review().textContent?.includes(`${title} (clarified)`) === true && review().textContent?.includes(revisedTitle) === true, "Original or edited suggestion was lost");
  results.push("Suggestion edits preserve original model text and await task acceptance");
  check("Description", review()).click();
  button("Lose next acceptance reply").click();
  button("Accept selected fields", editor()).click();
  await wait(() => editor().textContent?.includes("The result needs reconciliation") === true || editor().textContent?.includes("The result is uncertain") === true);
  assert(field("Title", "input").matches(":disabled"), "Uncertain acceptance allowed task edits");
  button("Retry pending action", editor()).click();
  await wait(() => editor().dataset.taskRevision === "2" && state("Accepted"));
  assert(field("Title", "input").value === revisedTitle, "Selected edited title was not applied");
  assert(field("Description", "textarea").value === description, "Unselected description changed");
  assert(field("Acceptance criteria", "textarea").value === "Both repository checks remain available", "Acceptance criteria changed");
  assert(check("Title", review()).checked && !check("Description", review()).checked, "Accepted review marks an unaccepted field as accepted");
  results.push("Lost acceptance reply reconciles once and displays only accepted fields");
  button("Undo accepted fields", editor()).click();
  await wait(() => editor().dataset.taskRevision === "3" && state("Undone"));
  assert(field("Title", "input").value === title && field("Description", "textarea").value === description, "Undo did not restore the selected field");
  results.push("Undo preserves unrelated description and task evidence");
  await wait(() => !field("Title", "input").matches(":disabled") && !button("Save task", editor()).matches(":disabled"));
  fill("Title", "input", `${title} after manual review`);
  await wait(() => editor().querySelector("header small")?.textContent === "Unsaved");
  results.push("Unsaved edits mark the sheet dirty before Manvi prepare");
  if (switchControl("Keep description").getAttribute("aria-checked") !== "true") switchControl("Keep description").click();
  button("Save task", editor()).click();
  await wait(() => editor().dataset.taskRevision === "4");
  assert(switchControl("Keep description").getAttribute("aria-checked") === "true", "Saved field lock was not applied");
  const descriptionPick = [...editor().querySelectorAll<HTMLButtonElement>(".field-picks button")].find((node) => node.textContent?.trim() === "Description");
  assert(Boolean(descriptionPick?.disabled) && descriptionPick?.getAttribute("aria-pressed") !== "true", "Locked description remained selectable for generation");
  enhance().click();
  await wait(() => state("Ready for review") && review().textContent?.includes("source revision 4") === true);
  assert(review().querySelectorAll('input[type="checkbox"]').length === 1, "Locked description entered generation");
  results.push("Saved field locks exclude locked content from proposed changes");
  button("Dismiss", editor()).click();
  await wait(() => state("Dismissed"));
  enhance().click();
  await wait(() => state("Generating"));
  button("Cancel", editor()).click();
  await wait(() => state("Cancelled"));
  assert(field("Title", "input").value === `${title} after manual review`, "Cancellation changed the task");
  results.push("Cancellation waits for worker acknowledgment and preserves the task");
  const automation = document.querySelector<HTMLElement>('section[aria-label="Automatic Manvi suggestions"]');
  if (!automation) throw new Error("Automatic suggestions controls are missing");
  const automaticHeading = automation.querySelector("button");
  if (!automaticHeading) throw new Error("Automatic suggestions heading is missing");
  if (automaticHeading.getAttribute("aria-expanded") !== "true") automaticHeading.click();
  await wait(() => Boolean(automation.querySelector("fieldset")) && !button("Save automatic settings", automation).disabled);
  const enable = check("Suggest clearer task titles and descriptions automatically", automation);
  if (!enable.checked) enable.click();
  button("Save automatic settings", automation).click();
  await wait(() => automation.textContent?.includes("Automatic suggestions are enabled for future text saves") === true);
  const automaticTitle = `${title} after saved evidence`;
  fill("Title", "input", automaticTitle);
  button("Save task", editor()).click();
  await wait(() => editor().dataset.taskRevision === "5");
  await wait(() => [...editor().querySelectorAll(".history button")].some((entry) => entry.textContent?.includes("Ready for review") && entry.textContent.includes("Task revision 5") && entry.textContent.includes("Automatic")));
  const automaticEntry = [...editor().querySelectorAll<HTMLButtonElement>(".history button")].find((entry) => entry.textContent?.includes("Task revision 5") && entry.textContent.includes("Automatic"));
  if (!automaticEntry) throw new Error("Automatic suggestion was not exposed for review");
  automaticEntry.click();
  await wait(() => state("Ready for review") && review().textContent?.includes("source revision 5") === true);
  assert(field("Title", "input").value === automaticTitle && field("Description", "textarea").value === description, "Automatic inference changed saved fields");
  assert(review().querySelectorAll('input[type="checkbox"]').length === 1, "Automatic generation ignored the saved field lock");
  results.push("Text saves automatically generate a reviewable suggestion using the saved field locks");
  button("Stop automatic suggestions", automation).click();
  await wait(() => automation.textContent?.includes("Automatic suggestions are off") === true);
  assert(state("Ready for review"), "Disabling automatic work discarded a ready suggestion");
  button("Reload automatic settings", automation).click();
  await wait(() => !check("Suggest clearer task titles and descriptions automatically", automation).checked && !button("Reload automatic settings", automation).disabled);
  results.push("Stopping automatic suggestions persists across settings reload and preserves ready reviews");
  assert(document.getElementById("errors")?.textContent === "Runtime errors: 0", "The browser reported runtime errors");
  return results;
}
