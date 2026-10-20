package cart

import (
	"errors"
	"testing"
)

func TestAddsUpPrices(t *testing.T) {
	c, err := Of(1, 2)
	if err != nil || c.Total() != 3 {
		t.Fatalf("got %v, %v", c, err)
	}
}

func TestRefusesAFourthItem(t *testing.T) {
	c, _ := Of(1, 2, 3)
	if err := c.Add(4); !errors.Is(err, ErrFull) {
		t.Fatalf("got %v", err)
	}
}
