//go:build !gusset

package gussetcheck

import (
	"context"
	"errors"
	"strings"
	"testing"
)

func TestStubRun(t *testing.T) {
	err := Run(context.Background())
	if err == nil {
		t.Fatal("expected error when gusset engine is not linked")
	}
	if !strings.Contains(err.Error(), "not linked") {
		t.Fatalf("unexpected error message: %v", err)
	}
	if err := SelfTest(context.Background()); !errors.Is(err, ErrNotLinked) {
		t.Fatalf("SelfTest() = %v, want ErrNotLinked", err)
	}
	if err := Close(); err != nil {
		t.Fatalf("Close() = %v", err)
	}
}
