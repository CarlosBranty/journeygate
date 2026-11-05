using System.Threading.Tasks;
using Shop.Domain.Auditing;

namespace Shop.Domain.Orders;

// `[Audited]` is `AuditedAttribute`; `Task`, `Guid` and `IReadOnlyList` are BCL.
[Audited]
public interface IOrderRepository
{
    Task<Order?> FindAsync(Guid id);

    IReadOnlyList<Order> All();
}
