/**
 * The one way into History › Graph › Suspects from anywhere else.
 *
 * Blame's file, a diff hunk's symbol, a node on the code graph, the command
 * palette: each names a symptom (or none) and lands on the Suspects list with
 * the question asked. They all go through here so the landing is the same
 * everywhere: the repository surface, History, Graph, the Suspects list, and
 * either a run against the window already set or the cursor in the symptom
 * field when nothing was named.
 */
import { get } from "svelte/store";
import { currentRepoPath, repoStore } from "../stores/repoStore";
import { interfaceStore } from "../stores/interfaceStore";
import { suspectsQuery } from "./suspectsQuery";

export function openSuspectsFor(symptom?: string | null): void {
  interfaceStore.setGlobalSurface("repository");
  // The section first: naming Graph resets its list to the commit graph.
  repoStore.setActiveTab("history", "graph");
  repoStore.setHistoryList("suspects");
  const named = symptom?.trim();
  if (named) void suspectsQuery.askAbout(get(currentRepoPath), named);
  else suspectsQuery.requestFocus();
}
