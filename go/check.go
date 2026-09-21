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

	"github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/gussetfn"
	"github.com/bharathvbcr/gusset"
)

// Run checks the linked engine and refuses a poisoned handle.
func Run(ctx context.Context) error {
	if gusset.MaxPoolSize < 4 {
		return fmt.Errorf("gusset: MaxPoolSize %d is below the bridge pool of 4", gusset.MaxPoolSize)
	}
	err := gussetfn.Check(ctx)
	if errors.Is(err, gusset.ErrPanic) || errors.Is(err, gusset.ErrPoisoned) {
		return fmt.Errorf("gitpulse: gusset engine poisoned the handle: %w", err)
	}
	return err
}
