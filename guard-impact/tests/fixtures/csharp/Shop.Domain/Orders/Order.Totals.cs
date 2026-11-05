namespace Shop.Domain.Orders
{
    // The other part: `Line` is a nested type of the enclosing `Order`,
    // declared in Order.Lines.cs.
    public partial class Order
    {
        public Money Total(Currency currency)
        {
            var total = Money.Zero(currency);
            foreach (Line line in _lines)
            {
                total = total with { Amount = total.Amount + line.Price.Amount };
            }

            return total;
        }
    }
}
