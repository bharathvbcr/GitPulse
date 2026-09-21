//go:build !gusset

package gussetcheck

import (
	"context"
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
}
