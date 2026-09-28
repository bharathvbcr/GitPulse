//go:build !gusset

package gussetcheck

import (
	"context"
	"io"
	"time"
)

// Run stubs the Gusset engine check when built without -tags gusset.
func Run(_ context.Context) error {
	return ErrNotLinked
}

// SelfTest stubs the panic-firewall self-test.
func SelfTest(_ context.Context) error {
	return ErrNotLinked
}

// Close has nothing to release without the engine.
func Close() error { return nil }

// DrainLogs has nothing to drain without the engine.
func DrainLogs(io.Writer) (int, error) { return 0, nil }

// Shutdown has nothing to release without the engine.
func Shutdown(time.Duration) error { return nil }
