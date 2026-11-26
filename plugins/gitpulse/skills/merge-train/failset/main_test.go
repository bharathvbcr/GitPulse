package main

import (
	"bytes"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
)

func extract(t *testing.T, log string) Report {
	t.Helper()
	r, err := Extract(strings.NewReader(log))
	if err != nil {
		t.Fatal(err)
	}
	return r
}

// Line shapes below are copied from real logs: tmp/consolidate-1111/*.log
// (GitPulse ci:local), Manvi tmp/env2/verify-*.log, and ScholarLM's
// scripts/local-ci.js printers.

// After its Summary nextest lists the failures again, in the run's own shape.
const nextestSummary = "     Summary [2227.146s] 9353 tests run: 9343 passed (19 slow, 1 leaky), 10 failed, 87 skipped\n" +
	"        FAIL [   3.762s] (   5/9353) gitpulse watcher::tests::old_flake\n"

const ciLocalBase = "\n▶ Rust format check\n  cargo fmt --check\n" +
	"\n▶ Frontend unit tests with coverage\n  bun run coverage\n" +
	" FAIL  scripts/managed-provider-parity-contract.test.ts > managed provider parity > agrees with the harness's adapter set\n" +
	"\n▶ Browser regressions (headless Chrome)\n  bun run test:browser:all\n" +
	"--- diagnostics ---\nBrowser regressions failed in: palette, tasks\n" +
	"\n▶ Rust tests with coverage\n  cargo llvm-cov nextest\n" +
	"        PASS [   0.737s] (   1/9353) gitpulse adv_w3gb2_tests::adv_pop\n" +
	"        FAIL [   3.762s] (   5/9353) gitpulse watcher::tests::old_flake\n" +
	nextestSummary +
	"\n▶ Coverage floor\n  bun run check:coverage\n" +
	"\n── ci:local ─────────────────────────────────────────────\n\n" +
	"  PASS  Rust format check                             4.2s\n" +
	"  FAIL  Frontend unit tests with coverage           145.7s  (exit 1)\n" +
	"  FAIL  Browser regressions (headless Chrome)       687.3s  (exit 1)\n" +
	"  FAIL  Rust tests with coverage                   1396.7s  (exit 100)\n" +
	"  FAIL  Coverage floor                                0.1s  (exit 2)\n" +
	"\n  1 passed · 4 failed · 0 skipped of 5 gates\n"

func TestCILocalExtract(t *testing.T) {
	r := extract(t, ciLocalBase)
	if r.Format != FormatCILocal || !r.Complete {
		t.Fatalf("format %q complete %v", r.Format, r.Complete)
	}
	want := []string{
		"Browser regressions (headless Chrome) :: harness:palette",
		"Browser regressions (headless Chrome) :: harness:tasks",
		"Coverage floor :: *",
		"Frontend unit tests with coverage :: vitest:scripts/managed-provider-parity-contract.test.ts > managed provider parity > agrees with the harness's adapter set",
		"Rust tests with coverage :: nextest:gitpulse watcher::tests::old_flake",
	}
	if !reflect.DeepEqual(r.Failures, want) {
		t.Fatalf("failures\n got %q\nwant %q", r.Failures, want)
	}
}

func TestCILocalWithoutSummaryIsNotComplete(t *testing.T) {
	cut := ciLocalBase[:strings.Index(ciLocalBase, "── ci:local")]
	r := extract(t, cut)
	if r.Complete || !strings.Contains(r.Reason, "before the runner's verdict") {
		t.Fatalf("a log cut before its summary must not be complete: %+v", r)
	}
	c := Compare(extract(t, ciLocalBase), r)
	if c.Verdict != "not-comparable" {
		t.Fatalf("verdict %q", c.Verdict)
	}
}

func TestCompareSeparatesNewFromOld(t *testing.T) {
	batch := strings.Replace(ciLocalBase,
		"        PASS [   0.737s] (   1/9353) gitpulse adv_w3gb2_tests::adv_pop\n",
		"        FAIL [   0.737s] (   1/9353) gitpulse adv_w3gb2_tests::adv_pop\n", 1)
	batch = strings.Replace(batch, "  PASS  Rust format check  ", "  FAIL  Rust format check  ", 1)
	c := Compare(extract(t, ciLocalBase), extract(t, batch))
	want := []string{
		"Rust format check :: *",
		"Rust tests with coverage :: nextest:gitpulse adv_w3gb2_tests::adv_pop",
	}
	if c.Verdict != "new-failures" || !reflect.DeepEqual(c.New, want) {
		t.Fatalf("verdict %q new %q", c.Verdict, c.New)
	}
	// The coverage floor failed on both with nothing finer: it can hide a
	// new failure, so it is named, not dropped.
	if len(c.Masked) != 1 || !strings.HasPrefix(c.Masked[0], "Coverage floor :: *") {
		t.Fatalf("masked %q", c.Masked)
	}
}

func TestCompareSameFailuresIsPartialWhileAGateIsOpaque(t *testing.T) {
	c := Compare(extract(t, ciLocalBase), extract(t, ciLocalBase))
	if c.Verdict != "partial" || len(c.New) != 0 {
		t.Fatalf("verdict %q new %q masked %q", c.Verdict, c.New, c.Masked)
	}
}

func TestCompareCleanWhenEveryFailureHasDetail(t *testing.T) {
	log := strings.Replace(ciLocalBase, "  FAIL  Coverage floor                                0.1s  (exit 2)", "  PASS  Coverage floor                                0.1s", 1)
	c := Compare(extract(t, log), extract(t, log))
	if c.Verdict != "clean" {
		t.Fatalf("verdict %q masked %q not compared %q", c.Verdict, c.Masked, c.NotCompared)
	}
}

func TestFixedFailuresAreReported(t *testing.T) {
	batch := strings.Replace(ciLocalBase, "Browser regressions failed in: palette, tasks", "Browser regressions failed in: tasks", 1)
	c := Compare(extract(t, ciLocalBase), extract(t, batch))
	if !reflect.DeepEqual(c.Fixed, []string{"Browser regressions (headless Chrome) :: harness:palette"}) {
		t.Fatalf("fixed %q", c.Fixed)
	}
}

const verifyStopped = "\x1b[36m==>\x1b[0m Go — format\n" +
	"\x1b[36m==>\x1b[0m Go — test\n" +
	"--- FAIL: TestAutomaticWake (570.00s)\n" +
	"    --- FAIL: TestAutomaticWake/saved_text (0.00s)\n" +
	"FAIL\n" +
	"FAIL\tgithub.com/bharathvbcr/Manvi/manvi/serve\t612.3s\n" +
	"ok  \tgithub.com/bharathvbcr/Manvi/manvi/gussetcheck\t0.109s\n" +
	"\x1b[31mFAIL\x1b[0m go test failed\n\n" +
	"INCOMPLETE verify.sh exited (status 1) before reaching a verdict.\n"

func TestVerifyShStopsAtANamedFailure(t *testing.T) {
	r := extract(t, verifyStopped)
	if r.Format != FormatVerifySh || !r.Complete || r.StoppedAt != "Go — test" {
		t.Fatalf("%+v", r)
	}
	want := []string{
		"Go — test :: go:github.com/bharathvbcr/Manvi/manvi/serve.TestAutomaticWake",
		"Go — test :: go:github.com/bharathvbcr/Manvi/manvi/serve.TestAutomaticWake/saved_text",
	}
	if !reflect.DeepEqual(r.Failures, want) {
		t.Fatalf("failures %q", r.Failures)
	}
	if r.Gates[0].Status != "passed" {
		t.Fatalf("a step followed by another step passed: %+v", r.Gates)
	}
}

func TestVerifyShIncompleteWithoutAFailureIsNotAVerdict(t *testing.T) {
	r := extract(t, "==> Go — format\nINCOMPLETE verify.sh exited (status 137) before reaching a verdict.\n")
	if r.Complete {
		t.Fatalf("a killed run must not be complete: %+v", r)
	}
}

func TestVerifyShStepsAfterTheStopAreNotCompared(t *testing.T) {
	base := verifyStopped
	batch := "==> Go — format\n==> Go — test\n==> Go — lint\nPASS all gates\n"
	c := Compare(extract(t, base), extract(t, batch))
	if c.Verdict != "partial" {
		t.Fatalf("verdict %q", c.Verdict)
	}
	joined := strings.Join(c.NotCompared, "\n")
	if !strings.Contains(joined, "Go — lint (ran on batch only)") || !strings.Contains(joined, "after the first failure") {
		t.Fatalf("not compared %q", c.NotCompared)
	}
	if len(c.Fixed) != 2 {
		t.Fatalf("fixed %q", c.Fixed)
	}
}

// DevCouncil's runner (scripts/ci-local.mjs) and the rust/verify.sh it ends
// with, in their own printers' shapes.
const dcFailed = "\n== script tests ==\nok\n" +
	"\n== go orchestrator tests ==\n" +
	"--- FAIL: TestSelector (0.02s)\n" +
	"FAIL\n" +
	"FAIL\tgithub.com/bharathvbcr/devcouncil/backend/go_orchestrator/cmd/dctestselect\t1.2s\n" +
	"FAIL: go orchestrator tests (exit 1)\n"

func TestDevCouncilStopsAtANamedFailure(t *testing.T) {
	r := extract(t, dcFailed)
	if r.Format != FormatStepLocal || !r.Complete || r.StoppedAt != "go orchestrator tests" {
		t.Fatalf("%+v", r)
	}
	if !reflect.DeepEqual(r.Failures, []string{"go orchestrator tests :: go:github.com/bharathvbcr/devcouncil/backend/go_orchestrator/cmd/dctestselect.TestSelector"}) {
		t.Fatalf("failures %q", r.Failures)
	}
	if r.Gates[0].Status != "passed" {
		t.Fatalf("gates %+v", r.Gates)
	}
}

func TestDevCouncilGreenAndNested(t *testing.T) {
	log := "\n== script tests ==\n\n== rust verify --quick ==\n\x1b[1m== 1/8 format ==\x1b[0m\n\x1b[1m== 2/8 clippy (-D warnings) ==\x1b[0m\n\nOK: ci:local\n"
	r := extract(t, log)
	if !r.Complete || len(r.Failures) != 0 || len(r.Gates) != 4 {
		t.Fatalf("%+v", r)
	}
	c := Compare(r, r)
	if c.Verdict != "clean" {
		t.Fatalf("verdict %q %+v", c.Verdict, c)
	}
}

// A run that carries on past a failed step records each failure, and only a
// final verdict line makes it complete.
func TestDevCouncilContinuingPastAFailure(t *testing.T) {
	log := dcFailed + "\n== rust verify --quick ==\n\x1b[1m== 1/8 format ==\x1b[0m\nDiff in /x/lib.rs\nFAIL: rust verify --quick (exit 1)\n"
	r := extract(t, log)
	want := []string{
		"1/8 format :: *",
		"go orchestrator tests :: go:github.com/bharathvbcr/devcouncil/backend/go_orchestrator/cmd/dctestselect.TestSelector",
	}
	if !reflect.DeepEqual(r.Failures, want) || r.StoppedAt != "1/8 format" || !r.Complete {
		t.Fatalf("%+v", r)
	}
	cut := dcFailed + "\n== rust verify --quick ==\n"
	if r := extract(t, cut); r.Complete {
		t.Fatalf("a run that went on after a failure and printed no verdict is not complete: %+v", r)
	}
}

func TestDevCouncilCouldNotRunIsNoVerdict(t *testing.T) {
	r := extract(t, "\n== script tests ==\nFAIL: Go replace expects a Gusset checkout at /x/gusset. CI checks out it.\n")
	if r.Complete || !strings.Contains(r.Reason, "could not run") {
		t.Fatalf("a step that never ran is not a failure verdict: %+v", r)
	}
}

// DevCouncil's --continue mode (4bb53241), cut down from a real run: the
// script tests print node:test "▶ suite" lines, the selector step fails, and
// rust/verify.sh --continue --nested prints its own block, indented, with no
// totals, before the outer block.
const dcContinue = "\n> node scripts/ci-local.mjs --continue\n\n\n" +
	"== script tests ==\n" +
	"▶ check:workflows\n  ✔ finds the repository's workflow files (0.607334ms)\n" +
	"▶ ci:local --only\n  ✔ runs exactly the chain (0.2ms)\n\n" +
	"== release identity ==\nOK: product version 0.2.5 is consistent\n\n" +
	"== release identity (tag path) ==\nNOT RUN: no local tag v0.2.5\n\n" +
	"== test selector end-to-end ==\n" +
	"--- FAIL: TestE2EPresenceACrateWithNothingToRunIsNotHeldAgainstTheRun (2.10s)\n" +
	"FAIL\n" +
	"FAIL\tgithub.com/bharathvbcr/DevCouncil/backend/go_orchestrator/cmd/dctestselect\t25.363s\n" +
	"FAIL\n" +
	"FAIL: test selector end-to-end (exit 1)\n" +
	"✗ test selector end-to-end failed (exit 1); continuing so the rest still report\n\n" +
	"== go orchestrator tests ==\n" +
	"ok  \tgithub.com/bharathvbcr/DevCouncil/backend/go_orchestrator/policy\t25.564s\n\n" +
	"== rust verify --quick ==\n" +
	"\x1b[1m== 1/8 format ==\x1b[0m\n" +
	"\x1b[1m== 2/8 clippy (-D warnings) ==\x1b[0m\n" +
	"\x1b[1m== 3/8 tests — host fixture ==\x1b[0m\n" +
	"\x1b[1m== 3/8 tests ==\x1b[0m\n" +
	"     Running unittests src/lib.rs (target/debug/build/devmap-resolve/0a1b/out/devmap_resolve-0123456789abcdef)\n" +
	"test lsp::session_bounds::a_server_that_dies_at_start_up_reports_its_redacted_stderr ... FAILED\n" +
	"test result: FAILED. 91 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.01s\n\n" +
	"error: 1 target failed:\n    `-p devmap-resolve --lib`\n" +
	"✗ 3/8 tests failed (exit 101); continuing so the rest still report\n" +
	"NOT RUN (quick mode): determinism, memory ceiling, growth plateau, incremental equivalence, mutation testing\n\n" +
	"  ── rust/verify.sh ──────────────────────────────────────────\n\n" +
	"    PASS  1/8 format                                    3.3s\n" +
	"    PASS  2/8 clippy (-D warnings)                     42.4s\n" +
	"    PASS  3/8 tests — host fixture                     31.1s\n" +
	"    FAIL  3/8 tests                                  1059.1s  (exit 101)\n" +
	"    N/A   release worker panic recovery (executable, not test profile)          (not applicable: quick mode)\n" +
	"    N/A   4/8 determinism — two clean builds must produce identical graph digests          (not applicable: quick mode)\n\n" +
	"FAIL: rust verify --quick (exit 1)\n" +
	"✗ rust verify --quick failed (exit 1); continuing so the rest still report\n\n" +
	"── DevCouncil ci:local ─────────────────────────────────────\n\n" +
	"  PASS  script tests                                  3.0s\n" +
	"  PASS  release identity                              0.1s\n" +
	"  N/A   release identity (tag path)                         (not applicable: no local tag v0.2.5)\n" +
	"  FAIL  test selector end-to-end                     26.2s  (exit 1)\n" +
	"  PASS  go orchestrator tests                       333.1s\n" +
	"  FAIL  rust verify --quick                        1136.2s  (exit 1)\n\n" +
	"  3 passed · 2 failed · 0 skipped · 1 not applicable of 6 steps\n\nexit=1\n"

const dcLibtestFailure = "3/8 tests :: libtest:devmap_resolve src/lib.rs lsp::session_bounds::a_server_that_dies_at_start_up_reports_its_redacted_stderr"
const dcSelectorFailure = "test selector end-to-end :: go:github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/cmd/dctestselect.TestE2EPresenceACrateWithNothingToRunIsNotHeldAgainstTheRun"

func statusOf(r Report) map[string]string {
	m := map[string]string{}
	for _, g := range r.Gates {
		m[g.Name] = g.Status
	}
	return m
}

// The summary block is the verdict: every row's status comes from it, a step
// that went on past a failure did not stop, and a failed parent step whose
// failure is a nested step's is not an opaque failure of its own.
func TestDevCouncilContinueSummaryIsTheVerdict(t *testing.T) {
	r := extract(t, dcContinue)
	if r.Format != FormatStepLocal || !r.Complete || r.StoppedAt != "" || r.Reason != "" {
		t.Fatalf("%+v", r)
	}
	if want := []string{dcLibtestFailure, dcSelectorFailure}; !reflect.DeepEqual(r.Failures, want) {
		t.Fatalf("failures\n got %q\nwant %q", r.Failures, want)
	}
	s := statusOf(r)
	for name, want := range map[string]string{
		"rust verify --quick":         "failed",
		"3/8 tests":                   "failed",
		"3/8 tests — host fixture":    "passed",
		"release identity (tag path)": "not-applicable",
		"4/8 determinism — two clean builds must produce identical graph digests": "not-applicable",
		"test selector end-to-end": "failed",
		"go orchestrator tests":    "passed",
	} {
		if s[name] != want {
			t.Errorf("%s: %q, want %q", name, s[name], want)
		}
	}
	// node:test's "▶ suite" lines inside a step are not steps.
	for _, pseudo := range []string{"check:workflows", "ci:local --only"} {
		if _, ok := s[pseudo]; ok {
			t.Errorf("%q is a node:test suite inside script tests, not a step: %+v", pseudo, r.Gates)
		}
	}
	if c := Compare(r, r); c.Verdict != "clean" {
		t.Fatalf("the same continue log twice: %q %+v", c.Verdict, c)
	}
}

// The outer step's FAIL line comes after the nested block, while the last
// nested step is still the current one. If that step passed, its row stands.
func TestDevCouncilOuterFailDoesNotOverwriteANestedPass(t *testing.T) {
	log := strings.Replace(dcContinue, "    PASS  2/8 clippy (-D warnings)                     42.4s\n",
		"    FAIL  2/8 clippy (-D warnings)                     42.4s  (exit 101)\n", 1)
	log = strings.Replace(log, "    FAIL  3/8 tests                                  1059.1s  (exit 101)\n",
		"    PASS  3/8 tests                                  1059.1s\n", 1)
	log = strings.Replace(log, "test lsp::session_bounds::a_server_that_dies_at_start_up_reports_its_redacted_stderr ... FAILED\n", "", 1)
	r := extract(t, log)
	s := statusOf(r)
	if s["3/8 tests"] != "passed" || s["2/8 clippy (-D warnings)"] != "failed" || s["rust verify --quick"] != "failed" {
		t.Fatalf("%+v", r.Gates)
	}
	if want := []string{"2/8 clippy (-D warnings) :: *", dcSelectorFailure}; !reflect.DeepEqual(r.Failures, want) {
		t.Fatalf("failures\n got %q\nwant %q", r.Failures, want)
	}
}

func TestDevCouncilContinueNewFailureInsideNestedStep(t *testing.T) {
	batch := strings.Replace(dcContinue,
		"test result: FAILED. 91 passed",
		"test lsp::fresh ... FAILED\ntest result: FAILED. 90 passed", 1)
	c := Compare(extract(t, dcContinue), extract(t, batch))
	if c.Verdict != "new-failures" || !reflect.DeepEqual(c.New, []string{"3/8 tests :: libtest:devmap_resolve src/lib.rs lsp::fresh"}) || len(c.Masked) != 0 {
		t.Fatalf("%+v", c)
	}
}

// Without a summary (the default mode), a FAIL line naming the outer step
// fails it too: it was marked passed when its first nested step began.
func TestDevCouncilDefaultModeNestedFailureFailsTheParent(t *testing.T) {
	log := "\n== script tests ==\n\n== release identity (tag path) ==\nNOT RUN: no local tag v0.2.5\n\n" +
		"== rust verify --quick ==\n\x1b[1m== 1/8 format ==\x1b[0m\n\x1b[1m== 2/8 clippy (-D warnings) ==\x1b[0m\nerror: unused import\n" +
		"FAIL: rust verify --quick (exit 1)\n"
	r := extract(t, log)
	s := statusOf(r)
	if s["rust verify --quick"] != "failed" || s["2/8 clippy (-D warnings)"] != "failed" || s["1/8 format"] != "passed" {
		t.Fatalf("%+v", r.Gates)
	}
	if s["release identity (tag path)"] != "not-applicable" {
		t.Fatalf("NOT RUN is not a pass: %+v", r.Gates)
	}
	if !reflect.DeepEqual(r.Failures, []string{"2/8 clippy (-D warnings) :: *"}) {
		t.Fatalf("failures %q", r.Failures)
	}
}

// A base from a tree without --continue stops at its first failure. A batch
// failure in a step the base never reached is not new: nothing compared it.
func TestBatchFailureInAStepTheBaseNeverReachedIsNotNew(t *testing.T) {
	base := dcContinue[:strings.Index(dcContinue, "✗ test selector end-to-end")]
	c := Compare(extract(t, base), extract(t, dcContinue))
	if c.Verdict != "partial" || len(c.New) != 0 {
		t.Fatalf("%+v", c)
	}
	if !strings.Contains(strings.Join(c.NotCompared, "\n"), "3/8 tests (failed on the batch; the base never ran it)") {
		t.Fatalf("not compared %q", c.NotCompared)
	}
	// A step the base has never had at all is the batch's own.
	added := strings.Replace(ciLocalBase, "\n── ci:local", "\n▶ New gate\n  x\n--- FAIL: TestNew (0.01s)\nFAIL\tex/new\t0.1s\n\n── ci:local", 1)
	added = strings.Replace(added, "  1 passed · 4 failed · 0 skipped of 5 gates", "  FAIL  New gate                                      0.1s  (exit 1)\n\n  1 passed · 5 failed · 0 skipped of 6 gates", 1)
	if c := Compare(extract(t, ciLocalBase), extract(t, added)); c.Verdict != "new-failures" || !reflect.DeepEqual(c.New, []string{"New gate :: go:ex/new.TestNew"}) {
		t.Fatalf("%+v", c)
	}
}

// A step that was skipped in both logs ran in neither: that is not clean.
func TestSkippedInBothIsNotCompared(t *testing.T) {
	dc := "\n== a ==\n\n== b ==\nSKIP: b (not run: needs devmap)\n\n" +
		"── DevCouncil ci:local ─────────────────────────────────────\n\n" +
		"  PASS  a                                             1.0s\n" +
		"  SKIP  b                                                   (not run: devmap not found)\n\n" +
		"  1 passed · 0 failed · 1 skipped · 0 not applicable of 2 steps\n"
	gp := "\n▶ Rust format check\n\n— SKIP WebKit (needs linux, this host is darwin)\n" +
		"\n── ci:local ─────────────────────────────────────────────\n\n" +
		"  PASS  Rust format check                             4.2s\n" +
		"  SKIP  WebKit                                               (needs linux, this host is darwin)\n" +
		"\n  1 passed · 0 failed · 1 skipped of 2 gates\n  not run here: WebKit\n"
	for name, log := range map[string]string{"devcouncil": dc, "gitpulse": gp} {
		r := extract(t, log)
		c := Compare(r, r)
		if !r.Complete || c.Verdict != "partial" || !strings.Contains(strings.Join(c.NotCompared, "\n"), "skipped") {
			t.Errorf("%s: %+v %+v", name, r, c)
		}
	}
}

// In the new modes a step that could not start is a SKIP row, and the
// summary is still a verdict for every other step.
func TestDevCouncilCouldNotStartUnderSummaryIsASkip(t *testing.T) {
	log := "\n== go host tests ==\nFAIL: Go replace expects a Gusset checkout at /x/gusset. CI checks out it.\n\n" +
		"── DevCouncil ci:local ─────────────────────────────────────\n\n" +
		"  SKIP  go host tests                                       (not run: Go replace expects a Gusset checkout at /x/gusset. CI checks out it.)\n\n" +
		"  0 passed · 0 failed · 1 skipped · 0 not applicable of 1 steps\n"
	r := extract(t, log)
	if !r.Complete || r.Reason != "" || statusOf(r)["go host tests"] != "skipped" || len(r.Failures) != 0 {
		t.Fatalf("%+v", r)
	}
}

const scholar = "================================================================================\n" +
	" STEP: FRONTEND TESTS (frontend)\n" +
	"================================================================================\n\n" +
	" FAIL  frontend/src/a.test.tsx > renders\n" +
	"================================================================================\n" +
	" STEP: GO SERVICES (go)\n" +
	"================================================================================\n\n" +
	"| Step Name    | Description                                    | Status     | Time (s) |\n" +
	"| frontend     | Frontend tests                                 | \x1b[31mFAILED\x1b[0m     | 12.00    |\n" +
	"| go           | Go services                                    | \x1b[32mPASSED\x1b[0m     | 3.00     |\n" +
	"\n\x1b[41m  FAILURE: Some local CI checks failed. Please fix them.  \x1b[0m\n"

func TestScholarLMSteps(t *testing.T) {
	r := extract(t, scholar)
	if r.Format != FormatLocalCIJS || !r.Complete {
		t.Fatalf("%+v", r)
	}
	if !reflect.DeepEqual(r.Failures, []string{"frontend :: vitest:frontend/src/a.test.tsx > renders"}) {
		t.Fatalf("failures %q", r.Failures)
	}
}

func TestOtherRunners(t *testing.T) {
	log := "▶ misc\n" +
		"     Running unittests src/lib.rs (target/debug/deps/devmap_store-0a1b)\n" +
		"test store::tests::opens ... FAILED\n" +
		"     Running unittests src/lib.rs (target/debug/build/devmap-resolve/197b107c0bd761d2/out/devmap_resolve-197b107c0bd761d2)\n" +
		"test lsp::session_bounds::dies ... FAILED\n" +
		"FAILED tests/test_api.py::test_search - AssertionError\n" +
		"  ✘  3 [chromium] › e2e/login.spec.ts:12:3 › signs in (1.2s)\n" +
		"FAIL\tgithub.com/x/broken [build failed]\n" +
		"── ci:local\n  FAIL  misc   1.0s  (exit 1)\n  0 passed · 1 failed · 0 skipped of 1 gates\n"
	r := extract(t, log)
	want := []string{
		"misc :: go:github.com/x/broken",
		"misc :: libtest:devmap_resolve src/lib.rs lsp::session_bounds::dies",
		"misc :: libtest:devmap_store src/lib.rs store::tests::opens",
		"misc :: playwright:[chromium] › e2e/login.spec.ts:12:3 › signs in",
		"misc :: pytest:tests/test_api.py::test_search",
	}
	if !reflect.DeepEqual(r.Failures, want) {
		t.Fatalf("failures\n got %q\nwant %q", r.Failures, want)
	}
}

// The 2026-11-11 ci-282f run: nextest stopped on a full disk after 5,626
// tests and printed no Summary, yet ci:local went on and printed its table.
// The tests it never reached are not passes.
func TestARunnerThatStoppedPartWayIsNotCompared(t *testing.T) {
	partway := strings.Replace(ciLocalBase, "        FAIL [   3.762s] (   5/9353) gitpulse watcher::tests::old_flake\n"+nextestSummary, "", 1)
	full := ciLocalBase
	if len(extract(t, full).Incomplete) != 0 {
		t.Fatal("a runner that printed its Summary is complete")
	}
	r := extract(t, partway)
	if len(r.Incomplete) != 1 || !strings.HasPrefix(r.Incomplete[0], "Rust tests with coverage") {
		t.Fatalf("incomplete %q", r.Incomplete)
	}
	c := Compare(extract(t, full), r)
	if len(c.Fixed) != 0 {
		t.Fatalf("a failure the runner never reached is not fixed: %q", c.Fixed)
	}
	if !strings.Contains(strings.Join(c.NotCompared, "\n"), "Rust tests with coverage (its test runner stopped part way") {
		t.Fatalf("not compared %q", c.NotCompared)
	}

	// ci-282f's actual shape: the cancelled run printed a Summary, as a fraction.
	cancelled := strings.Replace(ciLocalBase, "9353 tests run: 9343 passed", "5611/9253 tests run: 5587 passed", 1)
	r = extract(t, cancelled)
	if len(r.Incomplete) != 1 || !strings.Contains(r.Incomplete[0], "nextest ran 5611 of 9253 tests and stopped") {
		t.Fatalf("a cancelled Summary is not a complete run: %q", r.Incomplete)
	}
	whole := strings.Replace(ciLocalBase, "9353 tests run: 9343 passed", "9253/9253 tests run: 9243 passed", 1)
	if r := extract(t, whole); len(r.Incomplete) != 0 {
		t.Fatalf("N of N is complete: %q", r.Incomplete)
	}
}

func TestUnknownLogIsNotComparable(t *testing.T) {
	r := extract(t, "hello\nworld\n")
	if r.Complete || r.Format != FormatUnknown {
		t.Fatalf("%+v", r)
	}
}

func TestCarriageReturnProgressKeepsTheLastFrame(t *testing.T) {
	if got := clean("50%\r\x1b[2K100%"); got != "100%" {
		t.Fatalf("got %q", got)
	}
}

func TestRunExitCodes(t *testing.T) {
	dir := t.TempDir()
	write := func(name, body string) string {
		p := filepath.Join(dir, name)
		if err := os.WriteFile(p, []byte(body), 0o600); err != nil {
			t.Fatal(err)
		}
		return p
	}
	base := write("base.log", ciLocalBase)
	same := write("same.log", ciLocalBase)
	cut := write("cut.log", ciLocalBase[:40])
	worse := write("worse.log", strings.Replace(ciLocalBase, "Browser regressions failed in: palette, tasks", "Browser regressions failed in: palette, tasks, graph", 1))
	var out, errb bytes.Buffer
	cases := []struct {
		args []string
		code int
	}{
		{[]string{"compare", base, worse}, 1},
		{[]string{"compare", base, same}, 3},
		{[]string{"compare", base, cut}, 2},
		{[]string{"compare", "--json", base, worse}, 1},
		{[]string{"extract", base}, 0},
		{[]string{"extract", cut}, 2},
		{[]string{"compare", base}, 64},
		{[]string{}, 64},
	}
	for _, c := range cases {
		out.Reset()
		errb.Reset()
		if got := run(c.args, &out, &errb); got != c.code {
			t.Errorf("%v: exit %d want %d\n%s%s", c.args, got, c.code, out.String(), errb.String())
		}
	}
}
