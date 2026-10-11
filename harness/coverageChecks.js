// Behaviour checks for CoverageViewer's run, copy, selection and honesty paths.
//
// These replaced a test file that read CoverageViewer.svelte and asserted
// `toContain` on its text: deleting a handler left every one of them green.
// Each check here drives the rendered panel through the fixture IPC and reads
// what a person (or the backend) would see: the status rows, the copied text,
// the argv that reached `cmd_manvi_run_action`, the journal, the diagnostics
// ring and the number of scans.
//
// Called from coverage.html after its own flow, with the page's fixture
// knobs. Every scenario leaves the page in the state it found it.
import { tick } from "svelte";
import { get } from "svelte/store";
import { repoStore } from "../src/lib/stores/repoStore";
import { harnessStore } from "../src/lib/stores/harnessStore";
import { interfaceStore } from "../src/lib/stores/interfaceStore";
import { diagnostics } from "../src/lib/diagnostics/diagnostics";
import { coverageMetric } from "../src/lib/metrics/repoMetrics";
import { promptState, completePrompt } from "../src/lib/stores/modalStore";

const REPO = "/fixture/GitPulse";

export async function runCoverageChecks(ctx) {
  const { wait, until, button, byText, region, card, runs, aiCalls, ipcLog, knobs, setMode, setRunDelay, scans, check } = ctx;

  // `check` from the page takes a boolean evaluated at the call site, so a
  // throwing probe would be attributed to nothing. This evaluates here and
  // names the check (and what it saw) on failure.
  const ok = (name, assertion, detail) => {
    let pass = false;
    let suffix = "";
    try {
      pass = !!assertion();
    } catch (error) {
      suffix = ` — threw ${error}`;
    }
    if (!pass && detail) {
      try { suffix += ` — saw ${JSON.stringify(detail())}`; } catch { /* the probe is best effort */ }
    }
    check(`${name}${suffix}`, pass);
  };

  const flat = node => (node?.textContent ?? "").replace(/\s+/g, " ").trim();
  const strip = () => document.querySelector("[data-coverage-family-strip]");
  const header = () => flat(document.querySelector("[data-coverage-header]"));
  const rowOf = label => [...(strip()?.querySelectorAll("span[title]") ?? [])].find(node => node.title === label)?.parentElement ?? null;
  const rowText = label => flat(rowOf(label));
  const argvs = () => runs.map(run => run.args.join(" "));
  // The page never mounts App, which is what points the journal view at the
  // open repository; do that here, then read what each action filed.
  const journal = kind => {
    harnessStore.activateRepository(REPO);
    return get(harnessStore).actions.filter(action => action.kind === kind);
  };
  const idle = () => !button("Rescan coverage artifacts")?.disabled
    && !region().textContent.includes("Running…")
    && !document.querySelector("[data-coverage-family-strip] .animate-spin");
  const settle = async () => {
    await until(idle);
    await wait();
    await until(idle);
  };
  const copied = [];
  const setClipboard = writeText => Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
  const captureClipboard = () => setClipboard(async text => { copied.push(text); });
  const originalExec = document.execCommand;
  const resetKnobs = () => {
    Object.assign(knobs, { customReport: null, runHandler: null, aiHandler: null, detailHandler: null, contentHandler: null, scanDelay: 0 });
    setRunDelay(0);
    runs.length = 0;
    aiCalls.length = 0;
  };
  const switchTo = async name => { button(`Repository ${name}`).click(); await wait(); await wait(); };
  const done = result => ({ command: "", gated: true, policy: null, timed_out: false, truncated: false, truncation_reason: null, duration_ms: 5, exit_code: 0, stdout_tail: "", stderr_tail: "", ...result });
  const aiResult = text => ({
    text, reasoning: "", model: "fixture-model", base_url: "", context_window: 0, context_source: "fixture", prompt_tokens: 1, completion_tokens: 1,
    truncated: false, diff_truncated: false, diff_used_bytes: 0, diff_total_bytes: 0, elapsed_ms: 1,
    budget: { planned_by_harness: false, before_tokens: 0, threshold_tokens: 0, insufficient: false, calibration_samples: 0 }, warnings: [],
  });
  const family = (name, label, over = {}) => ({
    family: name, languages: [label], color_hex: "#888888", expected_formats: ["lcov"], expected_paths: [`${name}/lcov.info`],
    found: false, suggested_commands: [], setup_commands: [], tool_ready: true, tool_detail: "", duration_hint: "", ...over,
  });
  const empty = (families, over = {}) => ({
    families, files: [], languages: [], artifacts: [], overall: { lines_found: 0, lines_hit: 0, percentage: 0 }, truncated: false, ...over,
  });
  // A scan whose families turn `found` when the scenario says a command
  // produced something, and which can be made to fail.
  const world = { families: [], produced: new Set(), scanFail: false, extra: {} };
  const enter = async (families, extra = {}) => {
    world.families = families;
    world.produced = new Set();
    world.scanFail = false;
    world.extra = extra;
    knobs.customReport = () => {
      if (world.scanFail) throw new Error("Fixture scan failed");
      return empty(world.families.map(item => ({ ...item, found: world.produced.has(item.family) })), world.extra);
    };
    setMode("custom");
    runs.length = 0;
    aiCalls.length = 0;
    // A round trip remounts the panel: run statuses live until it does.
    await switchTo("Other");
    await switchTo("GitPulse");
    await until(() => !!document.querySelector("[data-coverage-empty]") && idle());
  };
  const scripted = table => (argv) => {
    const cmd = argv.join(" ");
    const row = table[cmd] ?? {};
    if (row.produces) world.produced.add(row.produces);
    if (row.after) row.after();
    return done({ command: cmd, exit_code: row.exit ?? 0, stdout_tail: row.out ?? "", stderr_tail: row.err ?? "", ...(row.raw ?? {}) });
  };
  const flatText = node => flat(node);
  const stripRows = () => [...(strip()?.querySelectorAll("span[title]") ?? [])].map(node => node.title);
  const leave = async () => {
    resetKnobs();
    setMode("measured");
    button("Rescan coverage artifacts").click();
    await until(() => !!button("Next missed block") || !!document.querySelector('[data-coverage-bar]'));
    await settle();
  };

  captureClipboard();
  try {
    // ---- Copying the report ----------------------------------------------
    // coverage.html leaves "Other" active; these scenarios run on GitPulse and
    // hand the page back on "Other".
    await switchTo("GitPulse");
    resetKnobs();
    setMode("measured");
    button("Rescan coverage artifacts").click();
    await until(() => header().includes("62.5%") && idle());
    button("Copy coverage report").click();
    await until(() => !!button("Coverage report copied"));
    ok("a copied report is the full snapshot for this repository",
      () => copied.at(-1).includes(REPO) && copied.at(-1).includes("10/16"), () => copied.at(-1)?.slice(0, 300));
    await until(() => !!button("Copy coverage report"));
    setClipboard(async () => { throw new Error("fixture denied"); });
    document.execCommand = () => false;
    button("Copy coverage report").click();
    await wait(); await wait();
    ok("a refused copy is never announced as copied", () => !!button("Copy coverage report") && !button("Coverage report copied"));
    captureClipboard();
    document.execCommand = originalExec;

    // ---- MANVI analysis: the report it is sent, and the journal ------------
    button("MANVI: analyze coverage with the local model").click(); await tick();
    ok("the generate button is usable with no model server discovered",
      () => region().textContent.includes("No local model server found. Start Ollama, LM Studio, llama.cpp, vLLM or Jan.")
        && !byText("Generate AI report").disabled);
    knobs.aiHandler = () => aiResult("1. Run the first: `npm run cov-one`\n2. Run the second: `npm run cov-two`");
    const reportJournal = journal("coverage-report").length;
    byText("Generate AI report").click();
    await until(() => !!button("Execute this command step directly"));
    ok("the model is sent the rendered report for this repository", () => aiCalls.length === 1 && aiCalls[0].repoPath === REPO && aiCalls[0].report.includes("10/16"));
    ok("a completed analysis is journaled as a success",
      () => journal("coverage-report").length === reportJournal + 1 && journal("coverage-report").at(-1).ok === true
        && journal("coverage-report").at(-1).label === "AI coverage analysis");
    knobs.aiHandler = () => { throw new Error("Fixture model down"); };
    byText("Generate AI report").click();
    await until(() => region().textContent.includes("Analysis failed"));
    ok("a failed analysis says why and is journaled as a failure",
      () => region().textContent.includes("Fixture model down") && journal("coverage-report").at(-1).ok === false);
    // The analysis settles after the user left: the journal still gets it.
    knobs.aiHandler = () => new Promise(resolve => setTimeout(() => resolve(aiResult("1. Run: `npm run cov-one`")), 400));
    const beforeStale = journal("coverage-report").length;
    byText("Generate AI report").click(); await tick();
    await switchTo("Other");
    await new Promise(resolve => setTimeout(resolve, 700));
    await switchTo("GitPulse");
    ok("an analysis that settles after a repository switch is still journaled",
      () => journal("coverage-report").length === beforeStale + 1 && journal("coverage-report").at(-1).ok === true);

    // ---- MANVI plan steps --------------------------------------------------
    button("MANVI: analyze coverage with the local model").click(); await tick();
    knobs.aiHandler = () => aiResult("1. Run the first: `npm run cov-one`\n2. Run the second: `npm run cov-two`");
    knobs.runHandler = (argv) => done({
      command: argv.join(" "),
      ...(argv.at(-1) === "cov-one" ? { exit_code: 1, stderr_tail: "ERR-LINE", stdout_tail: "OUT-LINE" } : { stdout_tail: "second ok" }),
    });
    byText("Generate AI report").click();
    await until(() => !!button("Execute this command step directly"));
    const scansBeforeSteps = scans();
    const stepJournal = journal("coverage-step").length;
    const diagnosticsBefore = get(diagnostics).length;
    // Run all is the very first command action of this panel: it must start
    // from a fresh guard rather than require one left by an earlier action.
    byText("Run all").click();
    await until(() => region().textContent.includes("Retry") && idle());
    ok("run all stops at the first failing step", () => argvs().join("|") === "npm run cov-one", argvs);
    ok("a plan step reaches the backend under the read-only coverage kind with its own argv",
      () => runs[0].actionKind === "coverage" && JSON.stringify(runs[0].args) === JSON.stringify(["npm", "run", "cov-one"]));
    ok("a plan step is journaled as a coverage-step failure",
      () => journal("coverage-step").length === stepJournal + 1 && journal("coverage-step").at(-1).ok === false);
    ok("a failed step keeps both streams, labelled",
      () => ["stderr:", "ERR-LINE", "stdout:", "OUT-LINE"].every(part => region().textContent.includes(part)));
    ok("a failed step is recorded to the diagnostics ring with its exit status",
      () => get(diagnostics).length > diagnosticsBefore
        && get(diagnostics).some(entry => entry.source === "coverage" && entry.message.includes('Coverage command "npm run cov-one" failed (exit 1)')));
    await wait(); await wait();
    ok("a settled step rescans so fresh artifacts appear", () => scans() > scansBeforeSteps);
    button("Copy step diagnostics").click(); await wait();
    ok("step diagnostics copy the command and both streams",
      () => copied.at(-1).startsWith("$ npm run cov-one") && copied.at(-1).includes("ERR-LINE") && copied.at(-1).includes("OUT-LINE"), () => copied.at(-1));
    runs.length = 0;
    knobs.runHandler = (argv) => done({ command: argv.join(" "), stdout_tail: "fine" });
    byText("Run all").click();
    await until(() => argvs().length === 2 && idle());
    ok("run all runs every step in order when they pass", () => argvs().join("|") === "npm run cov-one|npm run cov-two", argvs);
    // A step that settles after the user left is still journaled.
    setRunDelay(400);
    runs.length = 0;
    const stepJournalStale = journal("coverage-step").length;
    button("Execute this command step directly").click(); await tick();
    await switchTo("Other");
    await new Promise(resolve => setTimeout(resolve, 700));
    await switchTo("GitPulse");
    setRunDelay(0);
    ok("a step that settles after a repository switch is still journaled", () => journal("coverage-step").length === stepJournalStale + 1);
    await settle();

    // ---- Generating a missing report: setup, alternatives, verification -----
    const js = family("javascript", "JavaScript", { tool_ready: false, tool_detail: "npm is not installed", setup_commands: ["npm install"], suggested_commands: ["npm run cov-a", "npm run cov-b"] });
    const go = family("go", "Go", { suggested_commands: ["go -C a test ./...", "go -C b test ./..."] });
    const generatorJournal = () => journal("coverage-script");

    await enter([js, go]);
    ok("a family whose toolchain is missing still offers the install-then-generate run",
      () => !!byText("Run JavaScript coverage") && flat(card("javascript")).includes("First installs: npm install"));
    ok("and offers no bare command chip that would skip the install",
      () => ![...card("javascript").querySelectorAll("button")].some(node => node.title.startsWith("Run with MANVI:")));
    ok("a ready toolchain offers its commands as chips beside the family run",
      () => [...card("go").querySelectorAll("button")].some(node => node.title === "Run with MANVI: go -C a test ./..."));
    knobs.runHandler = scripted({ "npm run cov-b": { produces: "javascript", out: "b measured" } });
    let journaled = generatorJournal().length;
    let scansBefore = scans();
    byText("Run JavaScript coverage").click();
    await until(() => argvs().length === 3);
    await settle();
    ok("setup runs before any generate step, then each alternative in turn",
      () => argvs().join("|") === "npm install|npm run cov-a|npm run cov-b", argvs);
    ok("generation is routed through the generator action kind",
      () => runs.every(run => run.actionKind === "coverage_generator"));
    ok("a clean run that produced no report is no_data, never passed",
      () => rowText("npm run cov-a").includes("no coverage produced") && !rowText("npm run cov-a").includes("passed"), () => rowText("npm run cov-a"));
    ok("the alternative that did produce coverage is the one reported passed",
      () => rowText("npm run cov-b").includes("passed") && rowText("setup: npm install").includes("passed"));
    ok("the no_data row says why, in its own words",
      () => rowText("npm run cov-a").includes("Exited 0, but rescanning found no javascript coverage report."));
    ok("every generator command is journaled", () => generatorJournal().length === journaled + 3 && generatorJournal().slice(-3).every(entry => entry.ok === true));
    ok("the pipeline verifies by rescanning, then rescans when it settles", () => scans() >= scansBefore + 3, () => [scans(), scansBefore]);
    ok("a no_data run is listed with the unsuccessful ones", () => flat(strip()).includes("Unsuccessful coverage"));
    button("Copy failure diagnostics for npm run cov-a").click(); await wait();
    ok("a no_data run is copyable, and says it exited 0 with nothing",
      () => copied.at(-1).includes("Command: npm run cov-a") && copied.at(-1).includes("Status: exited 0 but produced no coverage data"), () => copied.at(-1));

    await enter([js, go]);
    knobs.runHandler = scripted({ "npm install": { exit: 1, err: "npm ERR! boom" } });
    byText("Run JavaScript coverage").click();
    await settle();
    ok("a failed setup step stops the pipeline before any generate step", () => argvs().join("|") === "npm install" && rowText("setup: npm install").includes("failed"), argvs);

    await enter([js, go]);
    knobs.runHandler = scripted({ "npm run cov-a": { produces: "javascript" } });
    byText("Run JavaScript coverage").click();
    await settle();
    ok("an alternative that produced coverage ends the run", () => argvs().join("|") === "npm install|npm run cov-a" && rowText("npm run cov-a").includes("passed"), argvs);

    await enter([js, go]);
    knobs.runHandler = scripted({ "npm run cov-a": { exit: 1, err: "a broke" }, "npm run cov-b": { produces: "javascript" } });
    byText("Run JavaScript coverage").click();
    await settle();
    ok("a failing alternative falls through to the next one",
      () => argvs().join("|") === "npm install|npm run cov-a|npm run cov-b" && rowText("npm run cov-a").includes("failed") && rowText("npm run cov-b").includes("passed"), argvs);

    await enter([js, go]);
    knobs.runHandler = scripted({ "npm run cov-a": { after: () => { world.scanFail = true; } } });
    byText("Run JavaScript coverage").click();
    await settle();
    world.scanFail = false;
    ok("a verification that cannot be answered neither continues nor condemns the run",
      () => argvs().join("|") === "npm install|npm run cov-a" && !rowText("npm run cov-a").includes("no coverage produced"), argvs);

    // Go and Rust are cumulative: every module runs, and verification is once.
    await enter([go]);
    knobs.runHandler = scripted({});
    byText("Run Go coverage").click();
    await settle();
    ok("a cumulative run executes every module, then attributes an empty result to all of them",
      () => argvs().join("|") === "go -C a test ./...|go -C b test ./..."
        && rowText("go -C a test ./...").includes("no coverage produced") && rowText("go -C b test ./...").includes("no coverage produced"), argvs);
    await enter([go]);
    knobs.runHandler = scripted({ "go -C a test ./...": { exit: 1, err: "a broke" } });
    byText("Run Go coverage").click();
    await settle();
    ok("a failing module stops a cumulative run", () => argvs().join("|") === "go -C a test ./..." && rowText("go -C a test ./...").includes("failed"), argvs);
    await enter([go]);
    knobs.runHandler = scripted({ "go -C b test ./...": { produces: "go" } });
    byText("Run Go coverage").click();
    await settle();
    ok("a cumulative run is verified once at the end, not after the first module",
      () => rowText("go -C a test ./...").includes("passed") && rowText("go -C b test ./...").includes("passed"), () => [rowText("go -C a test ./..."), rowText("go -C b test ./...")]);

    // The batch: one language failing does not skip the others, and nothing
    // else may start while it runs.
    await enter([js, go]);
    knobs.runHandler = scripted({ "npm install": { exit: 1, err: "npm ERR! offline" }, "go -C b test ./...": { produces: "go" } });
    setRunDelay(120);
    const diagBatch = get(diagnostics).length;
    byText("Run with MANVI").click(); await tick(); await wait();
    ok("while a batch runs, every other run control is disabled",
      () => {
        const runControls = [...region().querySelectorAll("button")].filter(node => /^Run .* coverage$/.test(node.textContent.trim()) || node.title.startsWith("Run with MANVI:"));
        return runControls.length > 0 && runControls.every(node => node.disabled) && !!byText("Running…") && !byText("Run with MANVI");
      });
    await until(() => idle() && argvs().length >= 3);
    await settle();
    setRunDelay(0);
    ok("a language that failed does not skip the languages after it",
      () => argvs().join("|") === "npm install|go -C a test ./...|go -C b test ./..." && rowText("go -C b test ./...").includes("passed"), argvs);
    ok("a failed command is filed to the diagnostics ring with its hint-bearing header",
      () => get(diagnostics).length > diagBatch && get(diagnostics).some(entry => entry.source === "coverage" && entry.message.includes('Coverage command "npm install" failed (exit 1)')));

    // A real repository's failing pair (ojas): two failures, one copy.
    setMode("ojas");
    await switchTo("Other"); await switchTo("GitPulse");
    await until(() => !!card("go") && idle());
    runs.length = 0;
    knobs.runHandler = null;
    journaled = generatorJournal().length;
    byText("Run with MANVI").click();
    await until(() => argvs().length === 2 && idle());
    await settle();
    ok("both failing languages ran and were journaled as failures",
      () => argvs().length === 2 && generatorJournal().length === journaled + 2 && generatorJournal().slice(-2).every(entry => entry.ok === false), argvs);
    button("Copy failure diagnostics for go -C go test ./... -coverprofile=coverage.out").click(); await wait();
    ok("a single failure copies its command, status, hint and full output",
      () => ["Command: go -C go test ./... -coverprofile=coverage.out", "Status: failed", "ld: symbol(s) not found", "FAIL\tgithub.com/bharathvbcr/ojas/go [build failed]"].every(part => copied.at(-1).includes(part)), () => copied.at(-1));
    ok("two failures offer one batch copy", () => !!byText("Copy failure diagnostics (2)"));
    byText("Copy failure diagnostics (2)").click(); await wait();
    ok("the batch copy carries every unsuccessful command",
      () => copied.at(-1).includes("Command: go -C go test") && copied.at(-1).includes("Command: cargo llvm-cov") && copied.at(-1).includes("could not parse `ojas-metal`"), () => copied.at(-1));
    ok("copying the batch is acknowledged", () => flat(strip()).includes("Copied"));

    // ---- Automatic recovery -------------------------------------------------
    const py = family("python", "Python", { suggested_commands: ["pytest --cov=. --cov-report=xml"] });
    const PYTEST = "pytest --cov=. --cov-report=xml";
    const ABORT = `INTERNALERROR> Traceback (most recent call last):\nINTERNALERROR>   File "${REPO}/bench/my stress.py", line 9, in <module>\nINTERNALERROR> SystemExit: 0\n\nno tests ran`;
    const IGNORE = "--ignore=bench/my stress.py";

    await enter([py]);
    knobs.runHandler = (argv) => {
      if (argv.includes(IGNORE)) { world.produced.add("python"); return done({ command: argv.join(" "), stdout_tail: "retry ok" }); }
      return done({ command: argv.join(" "), exit_code: 2, stdout_tail: ABORT });
    };
    journaled = generatorJournal().length;
    byText("Run Python coverage").click();
    await until(() => argvs().length === 2);
    await settle();
    ok("a recoverable failure is retried once, with the excluded path as one verbatim argument",
      () => runs.length === 2 && JSON.stringify(runs[1].args) === JSON.stringify(["pytest", "--cov=.", "--cov-report=xml", IGNORE]), () => runs.map(run => run.args));
    ok("the retry crosses IPC through the same gated kind as the first attempt",
      () => runs.every(run => run.actionKind === "coverage_generator" && run.repoPath === REPO));
    ok("both attempts are journaled", () => generatorJournal().length === journaled + 2);
    ok("a recovered pass is not drawn as a plain pass",
      () => rowText(PYTEST).includes("passed (recovered)") && rowText(PYTEST).includes("Retried without bench/my stress.py")
        && !!rowOf(PYTEST).querySelector(".text-amber-400\\/90") && !rowOf(PYTEST).querySelector(".text-emerald-400\\/90"), () => rowText(PYTEST));
    button("Copy coverage report").click(); await wait();
    ok("the copied report says what the recovery excluded", () => copied.at(-1).includes("bench/my stress.py") && copied.at(-1).includes("excluded from measurement"), () => copied.at(-1));
    button("MANVI: analyze coverage with the local model").click(); await tick();
    knobs.aiHandler = () => aiResult("No steps.");
    byText("Generate AI report").click();
    await until(() => aiCalls.length === 1);
    ok("the model is told what the recovery excluded", () => aiCalls[0].report.includes("bench/my stress.py") && aiCalls[0].report.includes("excluded from measurement"));
    await settle();

    await enter([py]);
    knobs.runHandler = (argv) => argv.includes(IGNORE)
      ? done({ command: argv.join(" "), exit_code: 1, stderr_tail: "retry broke" })
      : done({ command: argv.join(" "), exit_code: 2, stdout_tail: ABORT });
    byText("Run Python coverage").click();
    await settle();
    ok("a failed retry is not retried again", () => runs.length === 2, argvs);
    button(`Copy failure diagnostics for ${PYTEST}`).click(); await wait();
    ok("both attempts' output survive a failed retry",
      () => rowText(PYTEST).includes("failed") && copied.at(-1).includes("INTERNALERROR") && copied.at(-1).includes("also failed:") && copied.at(-1).includes("retry broke"), () => copied.at(-1));

    await enter([py]);
    knobs.runHandler = (argv) => done({ command: argv.join(" "), exit_code: 1, stderr_tail: "plain failure" });
    byText("Run Python coverage").click();
    await settle();
    ok("a failure GitPulse cannot name is not retried", () => runs.length === 1 && rowText(PYTEST).includes("failed"), argvs);

    const gofam = family("go", "Go", { suggested_commands: ["go test ./... -coverprofile=coverage.out"] });
    const GO = "go test ./... -coverprofile=coverage.out";
    const noModule = "go: directory prefix . does not contain main module or its selected dependencies";
    const perModule = outcomes => (argv) => {
      const dir = argv[1] === "-C" ? argv[2] : null;
      if (!dir) return done({ command: argv.join(" "), exit_code: 1, stderr_tail: noModule });
      if (dir === "go/b" && !outcomes.b) return done({ command: argv.join(" "), exit_code: 1, stderr_tail: "b broke" });
      if (dir === "go/b") world.produced.add("go");
      return done({ command: argv.join(" "), stdout_tail: `${dir}-out` });
    };
    await enter([gofam], { go_modules: ["go/a", "go/b"], go_modules_partial: true });
    knobs.runHandler = perModule({ b: true });
    byText("Run Go coverage").click();
    await settle();
    ok("Go is retried per module, from the modules the scan discovered, one run each",
      () => argvs().join("|") === `${GO}|go -C go/a test ./... -coverprofile=coverage.out|go -C go/b test ./... -coverprofile=coverage.out`, argvs);
    ok("a module-scoped pass is marked recovered and says the module list was capped",
      () => rowText(GO).includes("passed (recovered)") && rowText(GO).includes("Retried per Go module (go/a, go/b)") && rowText(GO).includes("capped"), () => rowText(GO));
    await enter([gofam], { go_modules: ["go/a", "go/b"] });
    knobs.runHandler = perModule({ b: false });
    byText("Run Go coverage").click();
    await settle();
    ok("a cumulative recovery needs every module to pass", () => rowText(GO).includes("failed") && !rowText(GO).includes("recovered"), () => rowText(GO));
    button(`Copy failure diagnostics for ${GO}`).click(); await wait();
    ok("a cumulative recovery that fails midway keeps every module's output",
      () => ["go -C go/a test ./... -coverprofile=coverage.out:\ngo/a-out", "go -C go/b test ./... -coverprofile=coverage.out:\nb broke", "also failed:"].every(part => copied.at(-1).includes(part)), () => copied.at(-1));

    // ---- Automatic generation (opt-in) --------------------------------------
    const autoRuns = repo => runs.filter(run => run.repoPath === repo).length;
    const openFresh = async path => { await repoStore.openRepo(path); await wait(); };
    knobs.runHandler = scripted({});
    world.families = [js, go];
    world.produced = new Set();
    knobs.customReport = () => empty(world.families.map(item => ({ ...item, found: world.produced.has(item.family) })));
    setMode("custom");
    runs.length = 0;
    interfaceStore.setAutoRunCoverage(true);
    await openFresh("/fixture/Auto1");
    await until(() => autoRuns("/fixture/Auto1") >= 5);
    await settle();
    const firstBatch = autoRuns("/fixture/Auto1");
    ok("opting in starts every missing family through the batch runner, once",
      () => argvs().join("|") === "npm install|npm run cov-a|npm run cov-b|go -C a test ./...|go -C b test ./...", argvs);
    button("Rescan coverage artifacts").click(); await settle();
    button("Rescan coverage artifacts").click(); await settle();
    await wait(); await wait();
    ok("the fresh report from its own run cannot start the suites again", () => autoRuns("/fixture/Auto1") === firstBatch, () => [autoRuns("/fixture/Auto1"), firstBatch]);

    interfaceStore.setAutoRunCoverage(false);
    await openFresh("/fixture/Auto2");
    await until(() => !!card("javascript") && idle());
    await new Promise(resolve => setTimeout(resolve, 500));
    ok("without the opt-in nothing starts by itself", () => autoRuns("/fixture/Auto2") === 0);

    interfaceStore.setAutoRunCoverage(true);
    setMode("measured");
    await openFresh("/fixture/Auto3");
    await until(() => header().includes("62.5%") && idle());
    await new Promise(resolve => setTimeout(resolve, 500));
    ok("with nothing missing there is nothing to start", () => autoRuns("/fixture/Auto3") === 0);

    setMode("custom");
    knobs.scanDelay = 500;
    await openFresh("/fixture/Auto4");
    await switchTo("GitPulse");
    await new Promise(resolve => setTimeout(resolve, 1000));
    knobs.scanDelay = 0;
    ok("a scan that lands after the user left does not start suites in that repository", () => autoRuns("/fixture/Auto4") === 0);
    interfaceStore.setAutoRunCoverage(false);
    runs.length = 0;

    // ---- Run output: no captured stream is dropped --------------------------
    const quick = family("javascript", "JavaScript", { suggested_commands: ["npm run cov-a"] });
    await enter([quick]);
    knobs.runHandler = scripted({ "npm run cov-a": { raw: { timed_out: true, exit_code: null, duration_ms: 2000, stdout_tail: "partial log", stderr_tail: "warn line", truncated: true } } });
    byText("Run JavaScript coverage").click();
    await settle();
    ok("a timed-out run's row says it was killed, not a stale first line",
      () => rowText("npm run cov-a").includes("Timed out after 2.0s and was killed.") && rowText("npm run cov-a").includes("failed"), () => rowText("npm run cov-a"));
    button("Copy failure diagnostics for npm run cov-a").click(); await wait();
    ok("a timed-out run copies both streams, the timeout and the clip marker",
      () => ["Timed out after 2.0s", "stderr:\nwarn line", "stdout:\npartial log", "(output clipped)"].every(part => copied.at(-1).includes(part)), () => copied.at(-1));
    ok("the row shows the one-line summary while the copy holds the whole detail",
      () => !rowText("npm run cov-a").includes("partial log"));

    // ---- Scan lifecycle: cache, one scan per action, external adoption -------
    resetKnobs();
    setMode("measured");
    await switchTo("GitPulse");
    button("Rescan coverage artifacts").click();
    await until(() => header().includes("62.5%") && idle());
    knobs.scanDelay = 600;
    await switchTo("Other");
    await switchTo("GitPulse");
    ok("a revisit shows the cached report while the fresh scan is still running",
      () => header().includes("62.5%") && button("Rescan coverage artifacts").disabled && !region().textContent.includes("Scanning coverage…"));
    await until(() => idle());
    await repoStore.openRepo("/fixture/Fresh"); await wait();
    ok("a repository with no cached report shows the scan placeholder, not stale numbers",
      () => region().textContent.includes("Scanning coverage…") && !header().includes("62.5%"));
    await until(() => idle());
    knobs.scanDelay = 0;
    await switchTo("GitPulse");
    await until(() => header().includes("62.5%") && idle());

    let scanBase = scans();
    let measuredBase = coverageMetric.snapshot(REPO).measuredAt;
    button("Rescan coverage artifacts").click();
    await settle();
    await new Promise(resolve => setTimeout(resolve, 400));
    ok("one rescan is exactly one scan, which it does not adopt and repeat", () => scans() - scanBase === 1, () => scans() - scanBase);
    ok("the rescan goes through the shared coverage metric", () => coverageMetric.snapshot(REPO).measuredAt > measuredBase);
    const tsFile = { path: "src/example.ts", language: "TypeScript", color_hex: "#3178c6", lines_found: 10, lines_hit: 5, percentage: 50 };
    setMode("custom");
    knobs.customReport = () => ({
      ...empty([family("javascript", "JavaScript", { found: true })]),
      files: [tsFile], artifacts: [{ path: "coverage/lcov.info", format: "lcov", family: "javascript", skipped: false, totals: { lines_found: 10, lines_hit: 5, percentage: 50 } }],
      languages: [], overall: { lines_found: 10, lines_hit: 5, percentage: 50 },
    });
    scanBase = scans();
    await coverageMetric.refresh(REPO, { force: true });
    await until(() => header().includes("50.0%"));
    await new Promise(resolve => setTimeout(resolve, 400));
    ok("a measurement the panel did not ask for is adopted once", () => scans() - scanBase === 1 && header().includes("50.0%"), () => scans() - scanBase);
    setMode("measured");
    button("Rescan coverage artifacts").click();
    await until(() => header().includes("62.5%") && idle());

    // ---- Selection -----------------------------------------------------------
    // The open file's path in the pane header (file rows share the title attribute).
    const paneTitle = () => region().querySelector("span.min-w-0.flex-1.truncate.text-textPrimary:not(.font-mono)")?.title ?? null;
    repoStore.selectFilePath("LIB/RUST.RS"); await tick();
    button("Rescan coverage artifacts").click(); await settle();
    ok("a selection that differs only in case survives a rescan",
      () => paneTitle() === "LIB/RUST.RS" && get(repoStore).selectedFilePath === "LIB/RUST.RS", () => [paneTitle(), get(repoStore).selectedFilePath]);
    repoStore.selectFilePath("nope/missing.ts"); await tick();
    button("Rescan coverage artifacts").click(); await settle();
    ok("a selection no longer in the report falls to the first file and is echoed to the session",
      () => paneTitle() === "src/example.ts" && get(repoStore).selectedFilePath === "src/example.ts", () => [paneTitle(), get(repoStore).selectedFilePath]);
    button("lib/rust.rs, 25.0%, 3 missed lines").click(); await tick();
    await switchTo("Other"); await switchTo("GitPulse");
    await until(() => header().includes("62.5%") && idle());
    ok("the persisted selection is seeded on remount, not wiped by the first scan", () => paneTitle() === "lib/rust.rs", paneTitle);

    // ---- The file list keeps its place, and the file scope follows the file ----
    const promptText = () => document.querySelector('textarea[aria-label="Coverage agent prompt"]')?.value ?? "";
    const many = Array.from({ length: 300 }, (_, i) => ({ path: `src/f${i}.ts`, language: "TypeScript", color_hex: "#3178c6", lines_found: 10, lines_hit: 5, percentage: 50 }));
    setMode("custom");
    knobs.customReport = () => ({
      ...empty([family("javascript", "JavaScript", { found: true })]), files: many,
      artifacts: [{ path: "coverage/lcov.info", format: "lcov", family: "javascript", skipped: false, totals: { lines_found: 3000, lines_hit: 1500, percentage: 50 } }],
      overall: { lines_found: 3000, lines_hit: 1500, percentage: 50 },
    });
    button("Rescan coverage artifacts").click();
    await until(() => header().includes("50.0%") && idle());
    const listScroller = () => [...region().querySelectorAll(".gp-scroll")].find(node => node.querySelector('button[aria-label$="missed lines"]'));
    listScroller().scrollTop = 600; await wait(); await wait();
    ok("scrolling the file list is not undone by the panel", () => listScroller().scrollTop > 300, () => listScroller().scrollTop);
    const query = document.querySelector('input[aria-label="Search coverage files"]');
    query.value = "f1"; query.dispatchEvent(new Event("input", { bubbles: true })); await wait(); await wait();
    ok("changing the filter returns the list to the top", () => listScroller().scrollTop === 0, () => listScroller().scrollTop);
    button("Clear filters").click(); await tick();
    button("Improve this file’s coverage").click(); await tick();
    ok("the file scope names the selected file", () => promptText().includes("Selected file: src/f0.ts"), promptText);
    setMode("ojas");
    button("Rescan coverage artifacts").click();
    await until(() => !!card("python") && idle());
    ok("the file scope is dropped when the selected file leaves the report", () => !promptText().includes("Selected file:"), promptText);
    setMode("measured");
    button("Rescan coverage artifacts").click();
    await until(() => header().includes("62.5%") && idle());

    // ---- File details: no stale gutters, truncation, missing source ----------
    button("src/example.ts, 70.0%, 3 missed lines").click(); await until(() => !!document.querySelector("[data-coverage-line]"));
    knobs.contentHandler = () => new Promise(resolve => setTimeout(() => resolve("export const x = 1;\n"), 500));
    button("lib/rust.rs, 25.0%, 3 missed lines").click(); await tick(); await wait();
    ok("while another file's details load, the report stays and no previous gutters linger",
      () => header().includes("62.5%") && !document.querySelector("[data-coverage-line]") && region().textContent.includes("Loading lib/rust.rs…"));
    await until(() => !!document.querySelector("[data-coverage-line]"));
    knobs.contentHandler = null;

    const detail = over => args => ({ path: args.filePath, language: "TypeScript", color_hex: "#3178c6", lines: [{ line_no: 1, hits: 1 }, { line_no: 2, hits: 0 }],
      totals: { lines_found: 10, lines_hit: 7, percentage: 70 }, truncated: false, lines_truncated: false, ...over });
    knobs.detailHandler = detail({ truncated: true });
    button("src/full.ts, 100.0%, 0 missed lines").click();
    await until(() => region().textContent.includes("missing gutters here mean unknown, not uncovered"));
    ok("a capped scan's missing gutters are disclosed as unknown, without naming a cause",
      () => region().textContent.includes("The scan did not read every artifact") && !region().textContent.includes("hit a cap"));
    knobs.detailHandler = detail({ lines_truncated: true });
    button("src/example.ts, 70.0%, 3 missed lines").click();
    await until(() => region().textContent.includes("Gutter view capped for display; totals still reflect the full file."));
    knobs.detailHandler = null;
    button("src/full.ts, 100.0%, 0 missed lines").click();
    await until(() => !region().textContent.includes("Gutter view capped"));
    ok("the capped-view notices go away with the file they qualified", () => !region().textContent.includes("did not read every artifact"));

    const diagBefore = get(diagnostics).length;
    knobs.contentHandler = () => { throw new Error("File not found: lib/rust.rs"); };
    button("lib/rust.rs, 25.0%, 3 missed lines").click();
    await until(() => region().textContent.includes("No source available"));
    await wait();
    ok("a source file that is simply absent says so without a diagnostics warning", () => get(diagnostics).length === diagBefore && region().textContent.includes("File not found: lib/rust.rs"));
    knobs.contentHandler = () => { throw new Error("disk on fire"); };
    button("src/example.ts, 70.0%, 3 missed lines").click();
    await until(() => region().textContent.includes("disk on fire"));
    ok("any other source failure is reported to the diagnostics ring",
      () => get(diagnostics).some(entry => entry.source === "coverage" && entry.message.includes("disk on fire")));
    knobs.contentHandler = null;

    // ---- A failed rescan ------------------------------------------------------
    setMode("failed");
    button("Rescan coverage artifacts").click();
    await until(() => region().textContent.includes("Rescan failed: Fixture scan failed") && idle());
    ok("a failed rescan keeps the previous results and offers a retry", () => header().includes("62.5%") && !!byText("Retry scan"));
    button("Copy scan error").click(); await wait();
    ok("the scan error is copyable, and the copy is acknowledged", () => copied.at(-1).includes("Fixture scan failed") && !!byText("Copied"), () => copied.at(-1));
    setMode("measured");
    byText("Retry scan").click();
    await until(() => !region().textContent.includes("Rescan failed") && idle());

    // ---- Honesty about what was measured --------------------------------------
    setMode("ojas");
    button("Rescan coverage artifacts").click();
    await until(() => !!card("python") && idle());
    ok("a scan that measured nothing is 'No coverage data', never a red 0.0%", () => header().includes("No coverage data") && !header().includes("0.0%"), header);
    ok("a family with no planned command still states why, and offers no Run button",
      () => flat(card("python")).includes("No Python tests found") && !card("python").querySelector("button"));
    ok("a family with a planned command carries its chip in the wide pane",
      () => [...card("go").querySelectorAll("button")].some(node => node.title === "Run with MANVI: go -C go test ./... -coverprofile=coverage.out"));
    setMode("ojas-mixed");
    button("Rescan coverage artifacts").click();
    await until(() => document.querySelectorAll("[data-coverage-family]").length === 3 && idle());
    const inStrip = name => document.querySelector(`[data-coverage-family="${name}"]`);
    ok("in the strip the same family states why and offers no Run button",
      () => flat(inStrip("python")).includes("No Python tests found") && !inStrip("python").querySelector("button"));
    ok("in the strip the planned command is the same chip",
      () => [...inStrip("go").querySelectorAll("button")].some(node => node.title === "Run with MANVI: go -C go test ./... -coverprofile=coverage.out"));
    const notices = [{ resource: "covered files", kept: 4000, total: 12873, inventory_only: false }, { resource: "manifests", kept: 5, total: 5, inventory_only: false }];
    await enter([family("javascript", "JavaScript", { suggested_commands: ["npm run cov-a"] })], { truncated: true, limit_notices: notices });
    ok("the capped chip names exact counts, and only for caps that dropped rows",
      () => flat(strip()).includes("scan capped: 4000 of 12873 covered files") && !flat(strip()).includes("5 of 5"), () => flat(strip()));
    await enter([family("javascript", "JavaScript", { suggested_commands: ["npm run cov-a"] })], { truncated: true, limit_notices: [] });
    ok("with no published counts the chip says only that the scan was capped", () => /scan capped(?!:)/.test(flat(strip())) && !flat(strip()).includes("scan capped:"));

    // ---- Commands the panel will not run, and what it says about the rest ----
    const bad = family("javascript", "JavaScript", { suggested_commands: ['npm run "unclosed'], duration_hint: "Takes a few minutes." });
    await enter([bad]);
    ok("a family's duration hint is the title of its run button", () => byText("Run JavaScript coverage").title === "Takes a few minutes.");
    ok("the batch button names what it needs and how long it can take",
      () => { const title = region().querySelector('button[title^="Generate each missing language"]')?.title ?? ""; return title.includes("cargo-llvm-cov") && title.includes("several minutes"); });
    journaled = generatorJournal().length;
    byText("Run JavaScript coverage").click();
    await settle();
    ok("a command that cannot be tokenized never reaches IPC, and is named, journaled and reported as failed",
      () => runs.length === 0 && rowText('npm run "unclosed').includes("failed") && generatorJournal().length === journaled + 1
        && generatorJournal().at(-1).ok === false && get(diagnostics).some(entry => entry.source === "coverage" && entry.message.includes("Invalid coverage command")),
      () => [runs.length, rowText('npm run "unclosed')]);
    await enter([go]);
    knobs.runHandler = scripted({});
    scanBase = scans();
    [...card("go").querySelectorAll("button")].find(node => node.title === "Run with MANVI: go -C a test ./...").click();
    await settle();
    ok("running one command by its chip runs only that command, then rescans", () => argvs().join("|") === "go -C a test ./..." && scans() > scanBase, () => [argvs(), scans() - scanBase]);

    // ---- Filing a coverage issue -----------------------------------------------
    resetKnobs();
    setMode("measured");
    await switchTo("Other"); await switchTo("GitPulse");
    await until(() => header().includes("62.5%") && idle());
    const issueButton = () => button("Create a guarded GitHub issue from this coverage snapshot");
    const created = () => ipcLog.filter(entry => entry.cmd === "cmd_github_create_issue");
    issueButton().click(); await wait();
    ok("filing an issue asks first, and promises no local path",
      () => get(promptState)?.options.title === "Create coverage issue" && get(promptState).options.message.includes("The draft excludes the local checkout path and command output."));
    completePrompt(false); await wait();
    ok("declining files nothing", () => created().length === 0);
    issueButton().click(); await wait(); completePrompt(true);
    await until(() => region().textContent.includes("Coverage issue created"));
    ok("confirming files a redacted snapshot, with no label requirement, for this repository",
      () => created().length === 1 && created()[0].args.repoPath === REPO && !created()[0].args.body.includes(REPO)
        && created()[0].args.title.startsWith("test(coverage): address 62.5%") && created()[0].args.labels.length === 0, () => created());
    ok("the created issue can be opened from the panel", () => !!byText("Open issue"));
    issueButton().click(); await wait();
    await switchTo("Other");
    completePrompt(true); await wait(); await wait();
    ok("an answer that arrives after a repository switch files nothing", () => created().length === 1);
    await switchTo("GitPulse");
    await until(() => header().includes("62.5%") && idle());
    knobs.runHandler = scripted({});
    setRunDelay(300);
    await enter([quick]);
    // The chip runs one script without the batch flag, so only the script's own
    // running state can be what disables the issue button.
    [...card("javascript").querySelectorAll("button")].find(node => node.title === "Run with MANVI: npm run cov-a").click(); await tick(); await wait();
    ok("a coverage command in flight disables filing an issue", () => issueButton().disabled && !byText("Running…"));
    await settle();
    setRunDelay(0);
    ok("and it is available again once the run settles", () => !issueButton().disabled);
  } finally {
    // Leaves the page for the checks that follow in coverage.html.
  }

  await leave();
  await switchTo("Other");
}
