//go:build gusset

// Package gussetcheck is GitPulse's Go import of the shared Gusset engine.
//
// GitPulse's Tauri binary is already a staticlib. Linking Gusset into it
// would be a second Rust staticlib in one process (R14). This module is a
// separate program: one archive, the DevCouncil umbrella, the same dc-glob
// engine Manvi calls.
package gussetcheck

import (
	"context"
	"errors"
	"fmt"
	"io"
	"time"

	"github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/gussetfn"
	"github.com/bharathvbcr/gusset"
)

// Run checks the linked engine and refuses a poisoned handle.
func Run(ctx context.Context) error {
	if gusset.MaxPoolSize < 4 {
		return fmt.Errorf("gusset: MaxPoolSize %d is below the bridge pool of 4", gusset.MaxPoolSize)
	}
	return classify(gussetfn.Check(ctx))
}

// SelfTest is Run plus a deliberate panic inside the engine archive on a
// throwaway handle, proving the panic firewall (I2) against the archive this
// program links. Rust prints the induced panic to stderr; that is the proof.
func SelfTest(ctx context.Context) error {
	if gusset.MaxPoolSize < 4 {
		return fmt.Errorf("gusset: MaxPoolSize %d is below the bridge pool of 4", gusset.MaxPoolSize)
	}
	return classify(gussetfn.SelfTest(ctx))
}

// Close releases the engine's shared handle, joining its workers without a
// bound. At process exit use Shutdown.
func Close() error {
	return gussetfn.Close()
}

// DrainLogs writes the engine's Rust log ring to w; nothing else reads it,
// and it evicts its oldest line.
func DrainLogs(w io.Writer) (int, error) {
	return gussetfn.DrainLogs(w)
}

// Shutdown releases the engine within drain: jobs are cancelled and the join
// happens only if they finished, so a stuck engine cannot hang exit.
func Shutdown(drain time.Duration) error {
	return gussetfn.Shutdown(drain)
}

func classify(err error) error {
	if errors.Is(err, gusset.ErrPanic) || errors.Is(err, gusset.ErrPoisoned) {
		return fmt.Errorf("gitpulse: gusset engine poisoned the handle: %w", err)
	}
	return err
}
