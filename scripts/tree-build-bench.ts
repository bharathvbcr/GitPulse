// Cost of one `buildFileTree` on synthetic paths: `bun scripts/tree-build-bench.ts [runs]`.
//
// What a rebuild costs the Code view each time the array feeding it changes.
// Prints min and median over `runs` (default 9, at least 7) for 20,000 and
// 100,000 paths, with the 1-minute load average before and after. The timing is
// skipped when the load average is above 30 at the start (a number taken on a
// busy host is not a number), and marked contaminated when it is above 30 at the
// end. The path shape matches harness/stress.html's repo fixture.
import { loadavg } from "node:os";
import { buildFileTree } from "../src/lib/files/fileTree";

const LOAD_LIMIT = 30;
const runs = Math.max(7, Number(process.argv[2] ?? 9));

function paths(count: number): string[] {
  return Array.from({ length: count }, (_, i) => `src/d${Math.floor(i / 1000)}/m${Math.floor(i / 100) % 10}/f${i}.ts`);
}

function stats(samples: number[]) {
  const sorted = [...samples].sort((a, b) => a - b);
  return { n: sorted.length, minMs: +sorted[0].toFixed(2), medianMs: +sorted[Math.floor(sorted.length / 2)].toFixed(2) };
}

const loadBefore = loadavg()[0];
if (loadBefore > LOAD_LIMIT) {
  console.log(JSON.stringify({ skipped: true, reason: `1-minute load ${loadBefore.toFixed(1)} is above ${LOAD_LIMIT} at the start`, loadBefore }));
  process.exit(0);
}

const results: Record<string, unknown> = {};
for (const count of [20_000, 100_000]) {
  const input = paths(count);
  buildFileTree(input); // warm
  const samples: number[] = [];
  for (let i = 0; i < runs; i += 1) {
    const start = performance.now();
    buildFileTree(input);
    samples.push(performance.now() - start);
  }
  results[`paths_${count}`] = stats(samples);
}
const loadAfter = loadavg()[0];
console.log(JSON.stringify({ ...results, loadBefore, loadAfter, contaminated: loadAfter > LOAD_LIMIT }));
