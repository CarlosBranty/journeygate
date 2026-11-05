namespace Shop.Domain.Orders;

// `Auditing.Audited` resolves through the parent namespace Shop.Domain.
[Auditing.Audited]
public static class Pricing
{
    private static readonly Dictionary<Currency, decimal> Rates = new()
    {
        [Currency.Usd] = 1m,
        [Currency.Eur] = 1.1m,
    };

    public static decimal Rate(Currency currency) => Rates[currency];
}
