package textutil

import "testing"

func TestTruncateKeepsShortText(t *testing.T) {
	if got := Truncate("hello", 10); got != "hello" {
		t.Fatalf("got %q", got)
	}
}

func TestTruncateCutsLongText(t *testing.T) {
	if got := Truncate("hello world", 6); got != "hello…" {
		t.Fatalf("got %q", got)
	}
}
