namespace Shop.App.Reports;

// `Order` comes from Shop.App's global using; Money and Currency are qualified.
public sealed class Report(Order order)
{
    public Shop.Domain.Billing.Money Total() => order.Total(Shop.Domain.Billing.Currency.Eur);
}
