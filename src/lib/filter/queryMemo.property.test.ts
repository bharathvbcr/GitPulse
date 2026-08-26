import { describe, expect, it } from "vitest";
import { parseFilterQuery } from "./parseQuery";
import { filterRowsWithLanes, type FilterableRow } from "./queryMemo";

/**
 * Randomized property suite for client-side filtering over SOLVED graph rows.
 *
 * The lane solver bakes array-index arithmetic into every connection, so the
 * only acceptable outcome of filtering is: every surviving edge still names
 * its own declared parent at its recomputed offset, and every hidden parent
 * degrades to an honest dangling stub. These properties are checked across
 * hundreds of deterministic pseudo-random histories and filter needles.
 */

interface GraphRow extends FilterableRow {
  id: string;
  summary: string;
  author_name: string;
  author_email: string;
  lane: number;
  active_lanes: number[];
  parent_ids: string[];
  is_merge: boolean;
  is_root: boolean;
  connections: Array<{
    from_lane: number;
    to_lane: number;
    to_row_offset: number;
    is_merge: boolean;
    color_index: number;
    is_dangling?: boolean;
  }>;
}

/** Deterministic LCG; identical seeds must yield identical histories. */
function lcg(seed: number): () => number {
  let state = seed >>> 0 || 1;
  return () => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0;
    return state / 4294967296;
  };
}

const WORDS = ["feat:", "fix:", "chore:", "gate", "oauth", "login", "wip", "release"];

function makeHistory(seed: number): { commits: GraphRow[]; ids: string[] } {
  const rand = lcg(seed);
  const count = 2 + Math.floor(rand() * 60);
  const ids = Array.from({ length: count }, (_, i) => `c${seed}_${i}`);
  const commits: GraphRow[] = [];
  for (let i = 0; i < count; i++) {
    const parentCount = rand() < 0.72 ? 1 : rand() < 0.85 ? 2 : Math.ceil(rand() * 4) + 1;
    const parents: string[] = [];
    for (let p = 0; p < parentCount; p++) {
      // Children before parents: draw strictly from later indices.
      const idx = i + 1 + Math.floor(rand() * (count - i - 1));
      if (idx > i && idx < count) parents.push(ids[idx]);
    }
    // A window-cut tip may reference a commit that does not exist at all.
    const outside = i === 0 && rand() < 0.15 ? ["cut-off-parent"] : [];
    const parentIds = [...outside, ...parents];
    commits.push({
      id: ids[i],
      summary: `${WORDS[Math.floor(rand() * WORDS.length)]} ${WORDS[Math.floor(rand() * WORDS.length)]} body ${i}`,
      author_name: rand() < 0.5 ? "ada" : "grace",
      author_email: "dev@example.com",
      lane: Math.floor(rand() * 3),
      active_lanes: [0, 1],
      parent_ids: parentIds,
      connections: parentIds.map((_, k) => ({
        from_lane: 0,
        to_lane: k,
        // Solver arithmetic against THIS array; recomputed below for truth.
        to_row_offset: Math.max(1, Math.floor(rand() * 3)),
        is_merge: k > 0,
        color_index: k,
      })),
      is_merge: parentIds.length > 1,
      is_root: parentIds.length === 0,
    });
  }
  // Overwrite offsets with the TRUE solver arithmetic so the property's
  // "before" state mirrors production output.
  const indexOf = new Map(ids.map((id, i) => [id, i]));
  for (let i = 0; i < count; i++) {
    for (let k = 0; k < commits[i].parent_ids.length; k++) {
      const target = indexOf.get(commits[i].parent_ids[k]);
      commits[i].connections[k] = {
        ...commits[i].connections[k],
        to_row_offset: target !== undefined && target > i ? target - i : 1,
        is_dangling: target === undefined || target <= i,
      };
    }
  }
  return { commits, ids };
}

describe("filterRowsWithLanes randomized endpoint truth", () => {
  const NEEDLES = [
    "",
    "feat:",
    "author:ada",
    "author:nobody-matches",
    "gate",
    "oauth login",
    "type:fix",
    "sha:c7",
  ];

  it("survivor edges name their own parent at the recomputed offset", () => {
    for (let seed = 1; seed <= 240; seed++) {
      const { commits } = makeHistory(seed);
      const needle = NEEDLES[seed % NEEDLES.length];
      const parsed = parseFilterQuery(needle);
      const result = filterRowsWithLanes(commits, parsed);

      const newIndex = new Map(result.rows.map((r, i) => [r.id, i]));
      for (let i = 0; i < result.rows.length; i++) {
        const row = result.rows[i];
        expect(row.connections.length).toBe(row.parent_ids.length);
        for (let k = 0; k < row.connections.length; k++) {
          const conn = row.connections[k] as GraphRow["connections"][number];
          const parentId = row.parent_ids[k];
          const parentNewIdx = newIndex.get(parentId);
          if (conn.is_dangling) {
            // Dangling is honest exactly when the parent is absent from
            // the visible set.
            expect(
              parentNewIdx,
              `seed ${seed} row ${row.id} conn ${k}: dangled but parent visible`,
            ).toBeUndefined();
            if (result.rows.length !== commits.length) {
              expect(
                conn.to_lane,
                `seed ${seed} row ${row.id} conn ${k}: dangling to_lane must not keep a ghost column`,
              ).toBe(conn.from_lane);
            }
          } else {
            expect(
              parentNewIdx,
              `seed ${seed} row ${row.id} conn ${k}: live edge to invisible parent`,
            ).toBeDefined();
            expect(parentNewIdx).toBe(i + conn.to_row_offset);
            expect(result.rows[i + conn.to_row_offset].id).toBe(parentId);
          }
        }
      }
    }
  });

  it("never loses or duplicates a row and preserves topological order", () => {
    for (let seed = 1; seed <= 120; seed++) {
      const { commits, ids } = makeHistory(seed);
      const parsed = parseFilterQuery(NEEDLES[seed % NEEDLES.length]);
      const expected = commits.filter((c) => {
        // Independent oracle mirroring CommitFilter::matches_commit exactly
        // for these generated shapes: author substring over name+email,
        // sha prefix on the id, type over the four conventional headers,
        // word-AND text over summary+name+email+id.
        const name = c.author_name.toLowerCase();
        const email = c.author_email.toLowerCase();
        const id = c.id.toLowerCase();
        if (parsed.author && !`${name} ${email}`.includes(parsed.author)) return false;
        if (parsed.sha && !id.startsWith(parsed.sha)) return false;
        if (parsed.commitType) {
          const header = c.summary.toLowerCase();
          const kind = parsed.commitType;
          const shapes = [`${kind}:`, `${kind}(`, `${kind}!:`, `${kind}!(`];
          if (!shapes.some((s) => header.startsWith(s))) return false;
        }
        if (parsed.text) {
          const hay = `${c.summary} ${name} ${email} ${id}`.toLowerCase();
          if (!parsed.text.split(/\s+/).every((w) => hay.includes(w))) return false;
        }
        return true;
      });
      const result = filterRowsWithLanes(commits, parsed);
      expect(result.rows.map((r) => r.id)).toEqual(expected.map((r) => r.id));
      // Order preservation: survivor ids appear in original history order.
      const rank = new Map(ids.map((id, i) => [id, i]));
      for (let i = 1; i < result.rows.length; i++) {
        expect(rank.get(result.rows[i].id)!).toBeGreaterThan(rank.get(result.rows[i - 1].id)!);
      }
    }
  });

  it("densifies surviving lane indices onto 0..k-1 after a real filter", () => {
    for (let seed = 1; seed <= 240; seed++) {
      const { commits } = makeHistory(seed);
      for (let i = 0; i < commits.length; i++) {
        commits[i].lane = i % 2 === 0 ? 0 : 12;
        commits[i].active_lanes = [commits[i].lane];
        commits[i].connections = commits[i].connections.map((c, k) => ({
          ...c,
          from_lane: commits[i].lane,
          to_lane: k === 0 ? 0 : 12,
        }));
      }
      const needle = NEEDLES[seed % NEEDLES.length];
      if (needle === "") continue;
      const result = filterRowsWithLanes(commits, parseFilterQuery(needle));
      if (result.rows.length === commits.length || result.rows.length === 0) continue;
      const used = new Set<number>();
      const add = (n: unknown) => {
        if (typeof n === "number" && Number.isFinite(n) && n >= 0) used.add(n);
      };
      for (const row of result.rows) {
        add(row.lane);
        for (const lane of row.active_lanes ?? []) add(lane);
        for (const conn of row.connections) {
          const typed = conn as { from_lane?: unknown; to_lane?: unknown };
          add(typed.from_lane);
          add(typed.to_lane);
        }
      }
      const max = Math.max(...used);
      expect(result.maxActiveLane, `seed ${seed}`).toBe(max);
      expect(max, `seed ${seed} left a hole`).toBe(used.size - 1);
      for (let i = 0; i <= max; i++) {
        expect(used.has(i), `seed ${seed} missing densified lane ${i}`).toBe(true);
      }
    }
  });

  it("keeps endpoint truth after dropping about 90 percent of a history", () => {
    for (let seed = 1; seed <= 80; seed++) {
      const { commits } = makeHistory(seed);
      for (let i = 0; i < commits.length; i++) {
        if (i % 10 === 0) commits[i].summary = `KEEPME ${commits[i].summary}`;
      }
      const result = filterRowsWithLanes(commits, parseFilterQuery("KEEPME"));
      expect(result.rows.length, `seed ${seed}`).toBeGreaterThan(0);
      expect(result.rows.length, `seed ${seed}`).toBeLessThan(commits.length);
      const newIndex = new Map(result.rows.map((r, i) => [r.id, i]));
      for (let i = 0; i < result.rows.length; i++) {
        const row = result.rows[i];
        expect(row.connections.length).toBe(row.parent_ids.length);
        for (let k = 0; k < row.connections.length; k++) {
          const conn = row.connections[k] as GraphRow["connections"][number];
          const parentNewIdx = newIndex.get(row.parent_ids[k]);
          if (conn.is_dangling) {
            expect(parentNewIdx).toBeUndefined();
            expect(conn.to_lane).toBe(conn.from_lane);
          } else {
            expect(result.rows[i + conn.to_row_offset].id).toBe(row.parent_ids[k]);
          }
        }
      }
    }
  });

  it("is deterministic for identical inputs", () => {
    for (let seed = 1; seed <= 40; seed++) {
      const { commits } = makeHistory(seed);
      const parsed = parseFilterQuery("gate");
      expect(filterRowsWithLanes(commits, parsed)).toEqual(
        filterRowsWithLanes(commits, parseFilterQuery("gate")),
      );
    }
  });
});
