package gussetcheck

import "errors"

// ErrNotLinked is every answer from a build without -tags gusset. It is
// declared in both builds so a caller can tell "could not run" from "failed".
var ErrNotLinked = errors.New("gitpulse: gusset engine is not linked; rebuild with -tags gusset")

// ErrUnsupported is every answer on a platform the engine is not built for
// (Windows). Like ErrNotLinked it means the check could not run, not that the
// engine failed.
var ErrUnsupported = errors.New("gitpulse: gusset engine is unsupported on this platform")
