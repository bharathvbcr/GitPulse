// Command gusset-check proves GitPulse's Go import of the shared Gusset
// engine: CPython fnmatch parity, the batched match-any path, and a real
// panic inside the linked archive caught at the boundary (I2).
//
// It is the only caller of package gussetcheck. Build it with cgo on and
// -tags gusset, against DevCouncil's umbrella archive:
//
//	eval "$(../../DevCouncil/rust/gusset-engine/cgo-env.sh --export)"
//	go run -tags gusset ./cmd/gusset-check
//
// or `bun run gusset:check` from the repository root. Without the tag it
// exits 2 and says the engine is not linked: a check that could not run is
// not a check that passed.
package main

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"time"

	gussetcheck "github.com/bharathvbcr/gitpulse/go"
)

func main() {
	os.Exit(run(os.Stdout, os.Stderr))
}

func run(out, errOut io.Writer) int {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	err := gussetcheck.SelfTest(ctx)
	if closeErr := gussetcheck.Shutdown(2 * time.Second); err == nil {
		err = closeErr
	}
	_, _ = gussetcheck.DrainLogs(errOut)
	if err != nil {
		fmt.Fprintf(errOut, "gusset-check: %v\n", err)
		return exitCode(err)
	}
	fmt.Fprintln(out, "gusset-check: ok (parity, match-any, panic firewall)")
	return 0
}

// exitCode separates "the engine failed" (1) from "the check could not run"
// (2): an engine that is not linked or not built for this platform proved
// nothing either way.
func exitCode(err error) int {
	if errors.Is(err, gussetcheck.ErrNotLinked) || errors.Is(err, gussetcheck.ErrUnsupported) {
		return 2
	}
	return 1
}
