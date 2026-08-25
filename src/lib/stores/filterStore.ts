import { writable } from "svelte/store";

export interface FilterState {
  searchQuery: string;
  selectedBranch: string | null;
}

function createFilterStore() {
  const { subscribe, set, update } = writable<FilterState>({
    searchQuery: "",
    selectedBranch: null,
  });

  return {
    subscribe,
    setSearch: (query: string) => update((s) => ({ ...s, searchQuery: query })),
    selectBranch: (branch: string | null) => update((s) => ({ ...s, selectedBranch: branch })),
    clear: () => set({ searchQuery: "", selectedBranch: null }),
  };
}

export const filterStore = createFilterStore();
