package money

import "testing"

func TestFormat(t *testing.T) {
	if Format(1) != "$1" {
		t.Fatal("bad")
	}
}
