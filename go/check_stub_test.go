//go:build !gusset

package gussetcheck

import (
	"context"
	"errors"
	"strings"
	"testing"
)

func TestStubSelfTest(t *testing.T) {
	err := SelfTest(context.Background())
	if !errors.Is(err, ErrNotLinked) {
		t.Fatalf("SelfTest() = %v, want ErrNotLinked", err)
	}
	if !strings.Contains(err.Error(), "not linked") {
		t.Fatalf("unexpected error message: %v", err)
	}
}
