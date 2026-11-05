using Repo = Shop.Domain.Orders.IOrderRepository;
using static Shop.Domain.Billing.Currency;

namespace Shop.App;

public static class Program
{
    public static int Main(string[] args)
    {
        try
        {
            Console.WriteLine(nameof(Repo));
            Console.WriteLine(Pricing.Rate(Usd));
            Console.WriteLine(typeof(Order.Line).Name);
            return args.Length;
        }
        catch (OrderException e)
        {
            Console.Error.WriteLine(e.Message);
            return 1;
        }
    }
}
