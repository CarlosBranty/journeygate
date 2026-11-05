namespace Shop.Domain.Auditing;

// `AttributeUsage`, `AttributeTargets` and `Attribute` are BCL types.
[AttributeUsage(AttributeTargets.Class | AttributeTargets.Interface)]
public sealed class AuditedAttribute : Attribute
{
}
