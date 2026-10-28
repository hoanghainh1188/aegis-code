// Package textutil has small helpers for display text.
package textutil

// Truncate shortens s to at most max characters, ending with "…" when it
// cut something.
func Truncate(s string, max int) string {
	if len(s) <= max {
		return s
	}
	return s[:max-1] + "…"
}
