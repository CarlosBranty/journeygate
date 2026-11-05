namespace Shop.App.Reports;

public static class Formatter
{
    // `T` is a type parameter, `Report` a declaration pattern, `OrderException`
    // the right-hand side of `as`.
    public static string Describe<T>(T value)
        where T : class
    {
        if (value is Report report)
        {
            return report.ToString() ?? string.Empty;
        }

        OrderException? failure = value as OrderException;
        return failure?.Message ?? string.Empty;
    }
}
