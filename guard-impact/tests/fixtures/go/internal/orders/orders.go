package orders

import "example.com/shop/internal/money"

func Place() string {
	return money.Format(100)
}
