//go:build gusset

package main

import (
	"bytes"
	"testing"
)

func TestWithTheEngineReportsOK(t *testing.T) {
	var out, errOut bytes.Buffer
	if code := run(&out, &errOut); code != 0 {
		t.Fatalf("run() = %d: %s", code, errOut.String())
	}
}
