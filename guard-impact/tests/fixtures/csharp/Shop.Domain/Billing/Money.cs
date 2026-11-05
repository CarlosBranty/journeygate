namespace Shop.Domain.Billing;

// `Currency` lives in the same (file-scoped) namespace.
public record Money(decimal Amount, Currency Currency)
{
    public static Money Zero(Currency currency) => new(0m, currency);
}
