namespace Fx.Domain;

public static class OrderExtensions
{
    public static int Count(this Order order) => 1;
}

public class Box<T>
{
    public T? Value;
}
