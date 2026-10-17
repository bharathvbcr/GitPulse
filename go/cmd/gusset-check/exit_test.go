package main

import (
	"errors"
	"fmt"
	"testing"

	gussetcheck "github.com/bharathvbcr/gitpulse/go"
)

func TestExitCodeSeparatesFailedFromCouldNotRun(t *testing.T) {
	for _, tc := range []struct {
		name string
		err  error
		want int
	}{
		{"not linked", gussetcheck.ErrNotLinked, 2},
		{"unsupported platform", gussetcheck.ErrUnsupported, 2},
		{"wrapped unsupported", fmt.Errorf("self-test: %w", gussetcheck.ErrUnsupported), 2},
		{"engine failure", errors.New("parity mismatch"), 1},
	} {
		if got := exitCode(tc.err); got != tc.want {
			t.Errorf("%s: exitCode = %d, want %d", tc.name, got, tc.want)
		}
	}
}
