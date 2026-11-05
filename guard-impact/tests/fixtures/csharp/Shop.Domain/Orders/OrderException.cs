namespace Shop.Domain.Orders;

public class OrderException : Exception
{
    public OrderException(string message)
        : base(message)
    {
    }
}
