package gussetcheck

import "errors"

// ErrNotLinked is every answer from a build without -tags gusset. It is
// declared in both builds so a caller can tell "could not run" from "failed".
var ErrNotLinked = errors.New("gitpulse: gusset engine is not linked; rebuild with -tags gusset")
