package money

func Format(cents int) string {
	return symbol() + itoa(cents)
}
