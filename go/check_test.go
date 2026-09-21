//go:build gusset

package gussetcheck

import (
	"context"
	"testing"
	"time"

	"github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/gussetfn"
)

func TestRun(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := Run(ctx); err != nil {
		t.Fatal(err)
	}
}

func TestMatchAny(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	matched, err := gussetfn.MatchAny(ctx, []string{"*.go", "*.rs"}, "main.go")
	if err != nil {
		t.Fatalf("MatchAny: %v", err)
	}
	if !matched {
		t.Fatal("expected match for main.go")
	}
}
