//go:build !gusset

package gussetcheck

import (
	"context"
	"io"
	"time"
)

// SelfTest stubs the panic-firewall self-test.
func SelfTest(_ context.Context) error {
	return ErrNotLinked
}

// DrainLogs has nothing to drain without the engine.
func DrainLogs(io.Writer) (int, error) { return 0, nil }

// Shutdown has nothing to release without the engine.
func Shutdown(time.Duration) error { return nil }
