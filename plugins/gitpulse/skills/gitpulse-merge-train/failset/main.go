// Command failset reads the log of a repository's full gate and answers one
// question for a merge train: which failures did the batch add that the base
// it was merged onto did not already have?
//
//	failset extract <log>            the failures one log reports (JSON)
//	failset compare <base> <batch>   the batch's failures that are new
//
// A gate's own exit status cannot answer it. Main is often already red, so a
// batch merged onto it fails too, whatever the batch did. What separates the
// two is the set of individual failures: test ids where the gate prints them,
// and the gate itself where it prints nothing finer.
//
// It reads four gate formats: the ci-local.mjs summary (GitPulse, DevCouncil),
// Manvi's verify.sh, and ScholarLM's local-ci.js. Inside any of them it
// recognises nextest, libtest, vitest, `go test`, pytest, Playwright and the
// GitPulse browser harness.
//
// Never reported as clean:
//   - a log with no verdict (killed, out of disk, or still running): "not-comparable"
//   - a gate that ran in only one of the two logs, or was skipped (could not
//     run) in both: "partial"
//   - a gate that failed on the base without per-test detail, so a new
//     failure inside it cannot be told apart from the old one: "partial"
//
// Exit status of compare: 0 clean, 1 new failures, 2 not comparable,
// 3 partial (no new failures where it could compare, but not everything
// could be compared), 64 usage.
package main

import (
	"bufio"
	"encoding/json/v2"
	"fmt"
	"io"
	"os"
	"regexp"
	"sort"
	"strings"
)

// Opaque is the id recorded for a gate that failed with no finer failure.
const Opaque = "*"

// Report is what one gate log says.
type Report struct {
	Format   string `json:"format"`
	Complete bool   `json:"complete"`
	Reason   string `json:"reason,omitempty"`
	// StoppedAt names the gate a stop-at-first-failure runner ended on.
	StoppedAt string `json:"stopped_at,omitempty"`
	// Incomplete names gates whose test runner stopped part way.
	Incomplete []string `json:"incomplete"`
	Gates      []Gate   `json:"gates"`
	Failures   []string `json:"failures"`
}

// Gate is one step of the runner, in the order the log first named it.
type Gate struct {
	Name string `json:"name"`
	// passed, failed, skipped (could not run), not-applicable (nothing to do
	// here), incomplete (its test runner stopped part way), or ran (outcome
	// not printed).
	Status string `json:"status"`
}

// notRun reports whether a step's status means it did not run.
func notRun(status string) bool { return status == "skipped" || status == "not-applicable" }

// Comparison is a batch's report read against its base's.
type Comparison struct {
	Verdict     string   `json:"verdict"` // clean, new-failures, partial, not-comparable
	Reasons     []string `json:"reasons,omitempty"`
	New         []string `json:"new"`
	Masked      []string `json:"masked"`
	NotCompared []string `json:"not_compared"`
	Fixed       []string `json:"fixed"`
}

const (
	FormatCILocal   = "ci-local"
	FormatVerifySh  = "verify-sh"
	FormatDCLocal   = "devcouncil-ci-local"
	FormatScholarLM = "scholarlm-local-ci"
	FormatUnknown   = "unknown"
)

var (
	ansi = regexp.MustCompile(`\x1b\[[0-9;?]*[A-Za-z]`)

	// GitPulse ci-local.mjs
	ciGate = regexp.MustCompile(`^▶ (.+)$`)

	// The summary block a runner prints last: GitPulse's ci-local.mjs
	// ("── ci:local ───"), and DevCouncil's ci-local.mjs in --continue and
	// --only ("── DevCouncil ci:local ───"). DevCouncil's rust/verify.sh
	// --nested prints its own block inside its parent step's output, indented
	// two more spaces and with no totals line; only the outermost runner
	// prints totals, so only they make a log complete.
	//
	// A row is the mark, the name padded to 42, the seconds padded to 7 (blank
	// when the step did not run), then an optional "  (detail)". Step names
	// carry their own parentheses ("Browser regressions (headless Chrome)"), so
	// the tail is matched by its exact shape rather than by "the last
	// parenthesis".
	sumHeader  = regexp.MustCompile(`^( *)── (.+?) ─{3,}$`)
	sumRunRow  = regexp.MustCompile(`^ +(PASS|FAIL)  (.+?)\s+\d+\.\d+s(?:  \(.*\))?$`)
	sumIdleRow = regexp.MustCompile(`^ +(SKIP|N/A)\s+(.+?)\s{2,}\(.*\)$`)
	sumTotals  = regexp.MustCompile(`^\s+\d+ passed · \d+ failed · \d+ skipped(?: · \d+ not applicable)? of \d+ (?:gates|steps)`)

	// Manvi verify.sh
	vsGate       = regexp.MustCompile(`^==> (.+)$`)
	vsFail       = regexp.MustCompile(`^FAIL (.+)$`)
	vsIncomplete = regexp.MustCompile(`^INCOMPLETE verify\.sh exited`)
	vsPass       = regexp.MustCompile(`^PASS (all gates|with gates that did not run)`)

	// DevCouncil scripts/ci-local.mjs, and the rust/verify.sh it runs last.
	// Both stop at the first failing step.
	dcGate        = regexp.MustCompile(`^== (.+) ==$`)
	dcFail        = regexp.MustCompile(`^FAIL: (.+)$`)
	dcExit        = regexp.MustCompile(` \(exit \d+\)$`)
	dcNotRun      = regexp.MustCompile(`^NOT RUN: `) // nothing to do here (no local tag), not a pass
	dcCouldNotRun = regexp.MustCompile(`could not run|expects a .* checkout|is missing after`)
	dcOK          = regexp.MustCompile(`^OK: ci:local`)

	// ScholarLM local-ci.js
	slGate   = regexp.MustCompile(`^ STEP: .* \(([^()]+)\)$`)
	slRow    = regexp.MustCompile(`^\| (\S+)\s+\|.*\| (PASSED\*?|FAILED|CRASHED|SKIPPED)\s*\|`)
	slBanner = regexp.MustCompile(`^\s*(SUCCESS: All executed|PARTIAL SUCCESS:|FAILURE: Some local CI checks failed|REFUSED:)`)

	// Test runners, inside any gate.
	nextestFail = regexp.MustCompile(`^\s+(FAIL|SIGSEGV|SIGABRT|SIGKILL|SIGBUS|SIGILL|TIMEOUT|LEAK-FAIL|ABORT)\s+\[\s*[\d.]+m?s\]\s+(?:\(\s*\d+/\d+\)\s+)?(\S+)\s+(\S+)`)
	// The binary sits in target/<profile>/deps/ before Cargo 1.100 and in
	// target/<profile>/build/<pkg>/<hash>/out/ from it.
	libtestBin  = regexp.MustCompile(`^\s+Running (?:unittests )?(\S+)(?:\s+\((?:.*/)?(?:deps|out)/([A-Za-z0-9_]+)-[0-9a-f]+\))?`)
	libtestFail = regexp.MustCompile(`^test (\S+) \.\.\. FAILED$`)
	vitestFail  = regexp.MustCompile(`^\s*FAIL\s+(\S+\.(?:test|spec)\.[cm]?[jt]sx?)(?:\s+>\s+(.+?))?(?:\s+\[[^\]]*\])?\s*$`)
	goFail      = regexp.MustCompile(`^\s*--- FAIL: (\S+)`)
	goPkgFail   = regexp.MustCompile(`^FAIL\t(\S+)`)
	pytestFail  = regexp.MustCompile(`^FAILED (\S+)`)
	pwFail      = regexp.MustCompile(`^\s+✘\s+\d+\s+(.+?)\s+\(\d[\d.]*m?s\)\s*$`)
	harnessFail = regexp.MustCompile(`^Browser regressions failed in: (.+)$`)

	// A runner that started and never printed its summary stopped part way
	// (out of disk, killed): the tests after the stop neither passed nor
	// failed, so its gate cannot be compared.
	nextestStart = regexp.MustCompile(`^\s+(Starting \d+ tests? across|(PASS|FAIL|SKIP|SLOW)\s+\[)`)
	// A cancelled run still prints a Summary, as "5611/9253 tests run".
	nextestDone = regexp.MustCompile(`^\s+Summary \[[^\]]*\]\s+(?:(\d+)/(\d+) tests? run)?`)
	vitestStart = regexp.MustCompile(`^\s*RUN\s+v\d`)
	vitestDone  = regexp.MustCompile(`^\s+Test Files\s+`)
)

// clean strips colour codes and keeps only what a terminal would show after
// the last carriage return.
func clean(line string) string {
	if i := strings.LastIndexByte(line, '\r'); i >= 0 && i < len(line)-1 {
		line = line[i+1:]
	}
	return strings.TrimRight(ansi.ReplaceAllString(line, ""), "\r\n ")
}

type parser struct {
	format     string
	gate       string
	order      []string
	status     map[string]string
	ids        map[string]map[string]bool // gate -> ids
	complete   bool
	reason     string
	stoppedAt  string
	binary     string                       // libtest: the binary its results belong to
	goPending  []string                     // go: failed tests waiting for their package line
	open       map[string]map[string]string // gate -> runner -> why it is not complete
	summarised map[string]bool              // gate + runner that printed its summary

	// Summary blocks.
	inSummary  bool
	blockStart string            // a nested block: the gate running when it began
	blockRows  []string          // a nested block: the steps it names, so far
	totals     bool              // the outermost block's totals line was printed
	named      map[string]bool   // every step a summary row names
	prevOf     map[string]string // gate -> the gate running when its header appeared
	children   map[string][]string
}

func newParser() *parser {
	return &parser{format: FormatUnknown, gate: "(top)", status: map[string]string{}, ids: map[string]map[string]bool{}, open: map[string]map[string]string{}, summarised: map[string]bool{},
		named: map[string]bool{}, prevOf: map[string]string{}, children: map[string][]string{}}
}

var rowStatus = map[string]string{"PASS": "passed", "FAIL": "failed", "SKIP": "skipped", "N/A": "not-applicable"}

func (p *parser) row(mark, name string) {
	name = strings.TrimSpace(name)
	p.see(name)
	p.status[name] = rowStatus[mark]
	p.named[name] = true
	if p.blockStart != "" {
		p.blockRows = append(p.blockRows, name)
	}
}

// closeBlock ends a summary block. A nested block's steps are the children of
// the step whose output it was printed in: the nearest gate, walking back
// through the order headers appeared in, that the block does not itself name.
func (p *parser) closeBlock() {
	if p.blockStart != "" && len(p.blockRows) > 0 {
		inBlock := map[string]bool{}
		for _, r := range p.blockRows {
			inBlock[r] = true
		}
		g := p.blockStart
		for seen := 0; g != "" && inBlock[g] && seen <= len(p.order); seen++ {
			g = p.prevOf[g]
		}
		if g != "" && !inBlock[g] {
			p.children[g] = p.blockRows
		}
	}
	p.inSummary, p.blockStart, p.blockRows = false, "", nil
}

// childFailed reports whether a failed step's failure belongs to a step
// nested inside it, which then carries the failure's detail.
func (p *parser) childFailed(g string) bool {
	for _, c := range p.children[g] {
		if p.status[c] == "failed" {
			return true
		}
	}
	return false
}

// runner tracks whether a test runner inside the current gate reached a full
// summary. why is empty for a complete summary, and says what is missing
// otherwise. Once a runner has summarised, later lines never reopen it:
// nextest lists the failures again after its Summary line, in the same shape
// as the run.
func (p *parser) runner(name string, started bool, why string) {
	key := p.gate + "\x00" + name
	if p.open[p.gate] == nil {
		p.open[p.gate] = map[string]string{}
	}
	if started {
		if !p.summarised[key] {
			p.open[p.gate][name] = name + " started and printed no summary"
		}
		return
	}
	p.summarised[key] = true
	if why != "" {
		p.open[p.gate][name] = why
	} else {
		delete(p.open[p.gate], name)
	}
}

func (p *parser) see(gate string) {
	if _, ok := p.status[gate]; !ok {
		p.status[gate] = "ran"
		p.order = append(p.order, gate)
	}
}

func (p *parser) enter(format, gate string) {
	p.flushGo("?")
	p.closeBlock()
	if _, ok := p.prevOf[gate]; !ok {
		p.prevOf[gate] = p.gate
	}
	if p.format == FormatUnknown {
		p.format = format
	}
	// verify.sh and DevCouncil's runner print nothing when a step passes and
	// stop at a failure, so reaching the next step is the pass.
	if (format == FormatVerifySh || format == FormatDCLocal) && p.status[p.gate] == "ran" {
		p.status[p.gate] = "passed"
	}
	// A runner that goes on after a failed step (a --continue mode) did not
	// stop there, and has not reached a verdict until it prints one.
	if format == FormatDCLocal && p.stoppedAt != "" {
		p.stoppedAt = ""
		p.complete = false
	}
	p.gate = gate
	p.see(gate)
}

func (p *parser) fail(id string) {
	p.see(p.gate)
	if p.ids[p.gate] == nil {
		p.ids[p.gate] = map[string]bool{}
	}
	p.ids[p.gate][id] = true
}

func (p *parser) flushGo(pkg string) {
	for _, t := range p.goPending {
		p.fail("go:" + pkg + "." + t)
	}
	p.goPending = nil
}

func (p *parser) line(raw string) {
	l := clean(raw)

	// Runner structure. "▶ name" is a GitPulse gate, but node:test prints
	// suites the same way inside another runner's steps.
	if p.format == FormatUnknown || p.format == FormatCILocal {
		if m := ciGate.FindStringSubmatch(l); m != nil {
			p.enter(FormatCILocal, strings.TrimSpace(m[1]))
			return
		}
	}
	if m := sumHeader.FindStringSubmatch(l); m != nil {
		p.flushGo("?")
		p.closeBlock()
		p.inSummary = true
		if m[1] != "" {
			p.blockStart = p.gate
		}
		return
	}
	if p.inSummary {
		if l == "" {
			return
		}
		if m := sumRunRow.FindStringSubmatch(l); m != nil {
			p.row(m[1], m[2])
			return
		}
		if m := sumIdleRow.FindStringSubmatch(l); m != nil {
			p.row(m[1], m[2])
			return
		}
		if sumTotals.MatchString(l) {
			// The outermost runner's verdict on every step: one that went on
			// past a failure did not stop there, and one that could not
			// start is a SKIP row in it, not a missing verdict.
			p.closeBlock()
			p.totals, p.complete, p.stoppedAt, p.reason = true, true, "", ""
			return
		}
		p.closeBlock()
	}
	if m := vsGate.FindStringSubmatch(l); m != nil {
		p.enter(FormatVerifySh, strings.TrimSpace(m[1]))
		return
	}
	if p.format == FormatVerifySh {
		if m := vsFail.FindStringSubmatch(l); m != nil {
			p.status[p.gate] = "failed"
			p.stoppedAt = p.gate
			return
		}
		if vsIncomplete.MatchString(l) {
			p.flushGo("?")
			if p.stoppedAt != "" {
				p.complete = true // it stopped at a named failure: a verdict
			} else {
				p.reason = "verify.sh exited before a verdict and named no failing step"
			}
			return
		}
		if vsPass.MatchString(l) {
			p.flushGo("?")
			if p.status[p.gate] == "ran" {
				p.status[p.gate] = "passed"
			}
			p.complete = true
			return
		}
	}
	if p.format == FormatUnknown || p.format == FormatDCLocal {
		if m := dcGate.FindStringSubmatch(l); m != nil {
			p.enter(FormatDCLocal, strings.TrimSpace(m[1]))
			return
		}
	}
	if p.format == FormatDCLocal {
		if m := dcFail.FindStringSubmatch(l); m != nil {
			p.flushGo("?")
			// A step a summary row already gave a status keeps it: in a
			// --continue run the outer step's FAIL comes after its nested
			// block, while p.gate is still the last nested step, which may
			// have passed.
			if !p.named[p.gate] {
				p.status[p.gate] = "failed"
			}
			p.stoppedAt = p.gate
			// "FAIL: <step> (exit N)" can name an outer step whose own nested
			// steps (rust/verify.sh's) printed headers since it began: it was
			// marked passed when the first of them started. The nested step
			// running when it failed is where the failure is; a summary
			// block, when one follows, replaces this guess with its rows.
			if label := dcExit.ReplaceAllString(m[1], ""); label != p.gate {
				if _, ok := p.status[label]; ok {
					p.status[label] = "failed"
					if p.children[label] == nil {
						p.children[label] = []string{p.gate}
					}
				}
			}
			if dcCouldNotRun.MatchString(m[1]) {
				// Exit 2 in this runner: the step never ran, so there is
				// no verdict to compare, only a broken environment.
				p.reason = "step " + p.gate + " could not run: " + m[1]
				p.complete = false
			} else if p.reason == "" {
				p.complete = true // it stopped at a named failure: a verdict
			}
			return
		}
		if dcNotRun.MatchString(l) {
			p.status[p.gate] = "not-applicable"
			return
		}
		if dcOK.MatchString(l) {
			p.flushGo("?")
			if p.status[p.gate] == "ran" {
				p.status[p.gate] = "passed"
			}
			p.complete = true
			return
		}
	}
	if m := slGate.FindStringSubmatch(l); m != nil {
		p.enter(FormatScholarLM, strings.TrimSpace(m[1]))
		return
	}
	if p.format == FormatScholarLM {
		if m := slRow.FindStringSubmatch(l); m != nil {
			name := m[1]
			p.see(name)
			switch {
			case strings.HasPrefix(m[2], "PASSED"):
				p.status[name] = "passed"
			case m[2] == "SKIPPED":
				p.status[name] = "skipped"
			default:
				p.status[name] = "failed"
			}
			return
		}
		if slBanner.MatchString(l) {
			p.complete = true
			return
		}
	}

	// Test runners.
	if m := nextestDone.FindStringSubmatch(l); m != nil {
		why := ""
		if m[1] != "" && m[1] != m[2] {
			why = "nextest ran " + m[1] + " of " + m[2] + " tests and stopped"
		}
		p.runner("nextest", false, why)
	} else if nextestStart.MatchString(l) {
		p.runner("nextest", true, "")
	} else if vitestDone.MatchString(l) {
		p.runner("vitest", false, "")
	} else if vitestStart.MatchString(l) {
		p.runner("vitest", true, "")
	}
	if m := nextestFail.FindStringSubmatch(l); m != nil {
		p.fail("nextest:" + m[2] + " " + m[3])
		return
	}
	if m := libtestBin.FindStringSubmatch(l); m != nil {
		// "src/lib.rs" names no crate; the deps path after it does.
		p.binary = m[1]
		if m[2] != "" {
			p.binary = m[2] + " " + m[1]
		}
		return
	}
	if m := libtestFail.FindStringSubmatch(l); m != nil {
		p.fail("libtest:" + p.binary + " " + m[1])
		return
	}
	if m := vitestFail.FindStringSubmatch(l); m != nil {
		id := "vitest:" + m[1]
		if m[2] != "" {
			id += " > " + m[2]
		}
		p.fail(id)
		return
	}
	if m := goFail.FindStringSubmatch(l); m != nil {
		p.goPending = append(p.goPending, m[1])
		return
	}
	if m := goPkgFail.FindStringSubmatch(l); m != nil {
		if len(p.goPending) == 0 {
			p.fail("go:" + m[1]) // a package that failed to build, or failed outside a test
		}
		p.flushGo(m[1])
		return
	}
	if m := pytestFail.FindStringSubmatch(l); m != nil {
		p.fail("pytest:" + m[1])
		return
	}
	if m := pwFail.FindStringSubmatch(l); m != nil {
		p.fail("playwright:" + m[1])
		return
	}
	if m := harnessFail.FindStringSubmatch(l); m != nil {
		for h := range strings.SplitSeq(m[1], ",") {
			if h = strings.TrimSpace(h); h != "" {
				p.fail("harness:" + h)
			}
		}
	}
}

// foldUnnamed moves every gate a complete summary does not name into the
// named step it appeared inside. Such a "gate" was a header printed by
// something running inside a step (node:test's "▶ suite"), so the failures
// under it are that step's.
func (p *parser) foldUnnamed() {
	cur := ""
	var kept []string
	for _, g := range p.order {
		if p.named[g] || cur == "" {
			if p.named[g] {
				cur = g
			}
			kept = append(kept, g)
			continue
		}
		for id := range p.ids[g] {
			if p.ids[cur] == nil {
				p.ids[cur] = map[string]bool{}
			}
			p.ids[cur][id] = true
		}
		for runner, why := range p.open[g] {
			if p.open[cur] == nil {
				p.open[cur] = map[string]string{}
			}
			p.open[cur][runner] = why
		}
		delete(p.status, g)
	}
	p.order = kept
}

func (p *parser) report() Report {
	p.flushGo("?")
	p.closeBlock()
	if p.totals {
		p.foldUnnamed()
	}
	r := Report{Format: p.format, Complete: p.complete, Reason: p.reason, StoppedAt: p.stoppedAt, Incomplete: []string{}, Gates: []Gate{}, Failures: []string{}}
	if p.format == FormatUnknown {
		r.Complete = false
		r.Reason = "no gate runner this tool reads (GitPulse or DevCouncil ci-local.mjs, Manvi verify.sh, ScholarLM local-ci.js) was found in the log"
	} else if !r.Complete && r.Reason == "" {
		r.Reason = "the log ends before the runner's verdict: killed, out of disk, or still running"
	}
	for _, g := range p.order {
		if len(p.open[g]) > 0 {
			var whys []string
			for _, why := range p.open[g] {
				whys = append(whys, why)
			}
			sort.Strings(whys)
			p.status[g] = "incomplete"
			r.Incomplete = append(r.Incomplete, g+" ("+strings.Join(whys, "; ")+")")
		}
		r.Gates = append(r.Gates, Gate{Name: g, Status: p.status[g]})
		ids := p.ids[g]
		if p.status[g] == "failed" && len(ids) == 0 && !p.childFailed(g) {
			r.Failures = append(r.Failures, g+" :: "+Opaque)
		}
		for id := range ids {
			r.Failures = append(r.Failures, g+" :: "+id)
		}
	}
	sort.Strings(r.Failures)
	return r
}

// Extract reads one gate log.
func Extract(in io.Reader) (Report, error) {
	p := newParser()
	sc := bufio.NewScanner(in)
	sc.Buffer(make([]byte, 1<<20), 16<<20)
	for sc.Scan() {
		p.line(sc.Text())
	}
	if err := sc.Err(); err != nil {
		return Report{}, err
	}
	return p.report(), nil
}

func gateOf(failure string) string {
	g, _, _ := strings.Cut(failure, " :: ")
	return g
}

// Compare reads a batch's report against its base's.
func Compare(base, batch Report) Comparison {
	c := Comparison{New: []string{}, Masked: []string{}, NotCompared: []string{}, Fixed: []string{}}
	if !base.Complete {
		c.Reasons = append(c.Reasons, "base: "+base.Reason)
	}
	if !batch.Complete {
		c.Reasons = append(c.Reasons, "batch: "+batch.Reason)
	}
	if base.Format != batch.Format {
		c.Reasons = append(c.Reasons, fmt.Sprintf("the logs come from different runners (%s, %s)", base.Format, batch.Format))
	}
	if len(c.Reasons) > 0 {
		c.Verdict = "not-comparable"
		return c
	}

	inBase := map[string]bool{}
	for _, f := range base.Failures {
		inBase[f] = true
	}
	inBatch := map[string]bool{}
	for _, f := range batch.Failures {
		inBatch[f] = true
	}
	opaqueBase := map[string]bool{}
	for _, f := range base.Failures {
		if strings.HasSuffix(f, " :: "+Opaque) {
			opaqueBase[gateOf(f)] = true
		}
	}
	ran := func(r Report) map[string]string {
		m := map[string]string{}
		for _, g := range r.Gates {
			if !notRun(g.Status) {
				m[g.Name] = g.Status
			}
		}
		return m
	}
	baseRan, batchRan := ran(base), ran(batch)
	// A step that could not run in either log was compared nowhere. One that
	// was not applicable (nothing to do here) is left out: it ran in neither
	// because there was nothing to run.
	skippedIn := func(r Report) map[string]bool {
		m := map[string]bool{}
		for _, g := range r.Gates {
			if g.Status == "skipped" {
				m[g.Name] = true
			}
		}
		return m
	}
	baseSkipped, batchSkipped := skippedIn(base), skippedIn(batch)
	for g := range baseSkipped {
		if _, ran := batchRan[g]; !ran {
			c.NotCompared = append(c.NotCompared, g+" (skipped: it ran in neither log)")
		}
	}
	for g := range batchSkipped {
		if _, ran := baseRan[g]; !ran && !baseSkipped[g] {
			c.NotCompared = append(c.NotCompared, g+" (skipped: it ran in neither log)")
		}
	}
	partway := map[string]bool{}
	for g, s := range baseRan {
		if s == "incomplete" {
			partway[g] = true
		}
	}
	for g, s := range batchRan {
		if s == "incomplete" {
			partway[g] = true
		}
	}
	for g := range partway {
		c.NotCompared = append(c.NotCompared, g+" (its test runner stopped part way in one of the logs)")
	}

	// A batch failure in a step the base never reached, because the base
	// stopped first or could not run it, has nothing to be compared with. A
	// step the base does not have at all is the batch's own, so its failures
	// are new.
	unreached := map[string]bool{}
	for _, f := range batch.Failures {
		g := gateOf(f)
		if _, ran := baseRan[g]; !ran && (base.StoppedAt != "" || baseSkipped[g]) && !unreached[g] {
			unreached[g] = true
			c.NotCompared = append(c.NotCompared, g+" (failed on the batch; the base never ran it)")
		}
	}
	for _, f := range batch.Failures {
		if inBase[f] || partway[gateOf(f)] || unreached[gateOf(f)] {
			continue
		}
		if opaqueBase[gateOf(f)] {
			c.Masked = append(c.Masked, f)
		} else {
			c.New = append(c.New, f)
		}
	}
	for g := range opaqueBase {
		if batchRan[g] == "failed" && inBatch[g+" :: "+Opaque] {
			c.Masked = append(c.Masked, g+" :: "+Opaque+" (failed on both with no per-test detail)")
		}
	}
	for _, f := range base.Failures {
		if !inBatch[f] && !partway[gateOf(f)] {
			if _, ranInBatch := batchRan[gateOf(f)]; ranInBatch {
				c.Fixed = append(c.Fixed, f)
			}
		}
	}
	for g := range baseRan {
		if _, ok := batchRan[g]; !ok {
			c.NotCompared = append(c.NotCompared, g+" (ran on base only)")
		}
	}
	for g := range batchRan {
		if _, ok := baseRan[g]; !ok && batchRan[g] != "failed" {
			c.NotCompared = append(c.NotCompared, g+" (ran on batch only)")
		}
	}
	if base.StoppedAt != "" || batch.StoppedAt != "" {
		c.NotCompared = append(c.NotCompared, fmt.Sprintf("every step after the first failure (base stopped at %q, batch at %q)", base.StoppedAt, batch.StoppedAt))
	}
	for _, s := range [][]string{c.New, c.Masked, c.NotCompared, c.Fixed} {
		sort.Strings(s)
	}

	switch {
	case len(c.New) > 0:
		c.Verdict = "new-failures"
	case len(c.Masked) > 0 || len(c.NotCompared) > 0:
		c.Verdict = "partial"
	default:
		c.Verdict = "clean"
	}
	return c
}

func extractFile(path string) (Report, error) {
	f, err := os.Open(path)
	if err != nil {
		return Report{}, err
	}
	defer f.Close()
	return Extract(f)
}

func printList(w io.Writer, title string, items []string) {
	if len(items) == 0 {
		return
	}
	fmt.Fprintf(w, "%s (%d):\n", title, len(items))
	for _, it := range items {
		fmt.Fprintf(w, "  %s\n", it)
	}
}

func run(args []string, stdout, stderr io.Writer) int {
	usage := func() int {
		fmt.Fprintln(stderr, "usage: failset extract <log> | failset compare [--json] <base-log> <batch-log>")
		return 64
	}
	if len(args) < 1 {
		return usage()
	}
	switch args[0] {
	case "extract":
		if len(args) != 2 {
			return usage()
		}
		r, err := extractFile(args[1])
		if err != nil {
			fmt.Fprintln(stderr, "failset:", err)
			return 2
		}
		out, err := json.Marshal(r, json.Deterministic(true))
		if err != nil {
			fmt.Fprintln(stderr, "failset:", err)
			return 2
		}
		fmt.Fprintln(stdout, string(out))
		if !r.Complete {
			return 2
		}
		return 0
	case "compare":
		rest := args[1:]
		asJSON := len(rest) > 0 && rest[0] == "--json"
		if asJSON {
			rest = rest[1:]
		}
		if len(rest) != 2 {
			return usage()
		}
		base, err := extractFile(rest[0])
		if err != nil {
			fmt.Fprintln(stderr, "failset: base:", err)
			return 2
		}
		batch, err := extractFile(rest[1])
		if err != nil {
			fmt.Fprintln(stderr, "failset: batch:", err)
			return 2
		}
		c := Compare(base, batch)
		if asJSON {
			out, err := json.Marshal(c, json.Deterministic(true))
			if err != nil {
				fmt.Fprintln(stderr, "failset:", err)
				return 2
			}
			fmt.Fprintln(stdout, string(out))
		} else {
			fmt.Fprintf(stdout, "base: %s, %d failure(s) · batch: %s, %d failure(s)\n", base.Format, len(base.Failures), batch.Format, len(batch.Failures))
			for _, r := range c.Reasons {
				fmt.Fprintln(stdout, "cannot compare:", r)
			}
			printList(stdout, "NEW FAILURES", c.New)
			printList(stdout, "MASKED: the gate already failed on base with no per-test detail, so a new failure inside it cannot be told apart", c.Masked)
			printList(stdout, "NOT COMPARED: ran in only one log, in neither, or stopped part way", c.NotCompared)
			printList(stdout, "FIXED on the batch", c.Fixed)
			fmt.Fprintln(stdout, "VERDICT:", c.Verdict)
		}
		switch c.Verdict {
		case "clean":
			return 0
		case "new-failures":
			return 1
		case "partial":
			return 3
		default:
			return 2
		}
	default:
		return usage()
	}
}

func main() {
	os.Exit(run(os.Args[1:], os.Stdout, os.Stderr))
}
