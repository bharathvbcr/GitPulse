// Min-of-N bench for the first visible row of a large replacement block
// (ft-02b836b8, row for the ft-c0d9313f baseline).
//
//   bun scripts/bench-word-diff.ts [--pairs 2000] [--words 1000] [--runs 5] [--whole]
//
// `annotateAround` is the shipped path. `--whole` also times the pre-fix path
// (`annotateRange` over the entire block) for the before/after comparison; it
// is slow by construction, so it takes only 2 samples.
import { annotateAround, annotateRange, parseUnifiedDiff } from "../src/lib/diff/wordDiff";

function arg(name: string, fallback: number): number {
  const at = process.argv.indexOf(`--${name}`);
  return at >= 0 ? Number(process.argv[at + 1]) : fallback;
}

const pairs = arg("pairs", 2000);
const words = arg("words", 1000);
const runs = arg("runs", 5);
const whole = process.argv.includes("--whole");

function line(seed: number): string {
  const parts: string[] = [];
  for (let i = 0; i < words; i += 1) parts.push(`w${(seed * 7 + i * 13) % 97}`);
  return parts.join(" ");
}

const raw = [`@@ -1,${pairs} +1,${pairs} @@`];
for (let i = 0; i < pairs; i += 1) raw.push(`-${line(i)}`);
for (let i = 0; i < pairs; i += 1) raw.push(`+${line(i + 1)}`);
const text = raw.join("\n") + "\n";

function measure(label: string, n: number, run: (lines: ReturnType<typeof parseUnifiedDiff>) => void) {
  let best = Infinity;
  let annotated = 0;
  for (let i = 0; i < n; i += 1) {
    const lines = parseUnifiedDiff(text);
    const t0 = performance.now();
    run(lines);
    best = Math.min(best, performance.now() - t0);
    annotated = lines.filter((l) => l.segments).length / 2;
  }
  console.log(
    JSON.stringify({
      bench: label,
      pairs,
      line_chars: line(0).length,
      runs: n,
      min_ms: Number(best.toFixed(2)),
      pairs_annotated: annotated,
    })
  );
}

measure("annotateAround_first_row", runs, (lines) => annotateAround(lines, 1));
if (whole) {
  measure("annotateRange_whole_block_prefix", 2, (lines) => annotateRange(lines, 0, lines.length));
}
