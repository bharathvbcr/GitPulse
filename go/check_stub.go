//go:build !gusset

package gussetcheck

import (
	"context"
	"errors"
)

// Run stubs the Gusset engine check when built without -tags gusset.
func Run(_ context.Context) error {
	return errors.New("gitpulse: gusset engine is not linked; rebuild with -tags gusset")
}
