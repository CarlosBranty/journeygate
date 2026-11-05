namespace Shop.Domain.Orders
{
    // One part of a partial class; `Money` comes from the project's global using.
    public partial class Order
    {
        private readonly List<Line> _lines = new();

        public sealed class Line
        {
            public Line(string sku, Money price)
            {
                Sku = sku;
                Price = price;
            }

            public string Sku { get; }

            public Money Price { get; }
        }
    }
}
