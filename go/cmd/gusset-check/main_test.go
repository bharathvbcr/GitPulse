//go:build !gusset

package main

import (
	"bytes"
	"strings"
	"testing"
)

func TestWithoutTheEngineExitsTwo(t *testing.T) {
	var out, errOut bytes.Buffer
	if code := run(&out, &errOut); code != 2 {
		t.Fatalf("run() = %d, want 2", code)
	}
	if !strings.Contains(errOut.String(), "not linked") || out.Len() != 0 {
		t.Fatalf("stdout %q stderr %q", out.String(), errOut.String())
	}
}
