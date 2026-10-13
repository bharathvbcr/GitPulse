//go:build gusset

package gussetcheck

import (
	"context"
	"errors"
	"testing"
	"time"

	"github.com/bharathvbcr/DevCouncil/backend/go_orchestrator/gussetfn"
)

func TestClassifyMapsUnsupportedToCouldNotRun(t *testing.T) {
	err := classify(gussetfn.ErrUnsupported)
	if !errors.Is(err, ErrUnsupported) || !errors.Is(err, gussetfn.ErrUnsupported) {
		t.Fatalf("classify(ErrUnsupported) = %v, want ErrUnsupported wrapping the engine's", err)
	}
	if classify(nil) != nil {
		t.Fatal("classify(nil) must stay nil")
	}
}

func TestSelfTestProvesTheFirewall(t *testing.T) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	if err := SelfTest(ctx); err != nil {
		t.Fatal(err)
	}
	if err := classify(gussetfn.Check(ctx)); err != nil {
		t.Fatalf("Check after SelfTest: %v", err)
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
