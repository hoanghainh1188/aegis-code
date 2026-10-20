// Package cart is a small package with one of each item the Go profile's
// symbol query knows.
package cart

import "errors"

// MaxItems is the most a cart holds.
const MaxItems = 3

// ErrFull is returned when a fourth item is added.
var ErrFull = errors.New("cart is full")

// Total is a sum of prices.
type Total = int

// Priced has a total.
type Priced interface {
	Total() Total
}

// Cart holds prices.
type Cart struct {
	items []int
}

// Add puts a price in the cart.
func (c *Cart) Add(price int) error {
	if len(c.items) >= MaxItems {
		return ErrFull
	}
	c.items = append(c.items, price)
	return nil
}

// Total sums the prices.
func (c *Cart) Total() Total {
	sum := 0
	for _, p := range c.items {
		sum += p
	}
	return sum
}

// Of builds a cart from prices.
func Of(prices ...int) (*Cart, error) {
	c := &Cart{}
	for _, p := range prices {
		if err := c.Add(p); err != nil {
			return nil, err
		}
	}
	return c, nil
}
