/**
 * Runs one harness page that is not (yet) in BROWSER_HARNESSES, in headless
 * Chrome or the system WKWebView, and prints its verdict.
 *
 *   node harness/runPage.mjs <chrome|webkit> /harness/appearance.html [query]
 *
 * Run it from the checkout whose code should be tested: Vite serves cwd. Set
 * SHOW_ALL=1 to list every check, not only the failures.
 */
import { runHarnessPage } from "../scripts/harness-page-runner.mjs";

const [engine = "chrome", page = "/harness/appearance.html", query = ""] = process.argv.slice(2);
const { results, errors } = await runHarnessPage({ engine, page, query, deadlineSeconds: 150 });
const failed = results.filter((row) => !row.pass);
console.log(JSON.stringify({ engine, page, checks: results.length, failed: failed.map((row) => row.name), errors }));
if (process.env.SHOW_ALL) for (const row of results) console.log(row.pass ? "ok  " : "FAIL", row.name);
if (failed.length || errors.length) process.exitCode = 1;
