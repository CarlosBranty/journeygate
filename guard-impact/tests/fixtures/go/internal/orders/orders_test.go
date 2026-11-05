package orders

import (
	"testing"

	"example.com/shop/internal/money"
)

func TestPlace(t *testing.T) {
	if Place() != money.Format(100) {
		t.Fatal("mismatch")
	}
}
