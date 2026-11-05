namespace Fx.App;

[Audited]
public class Use
{
    public void Run()
    {
        var order = new Order();
        var line = typeof(Order.Line);
        int answer = HostingExtensions.Answer;
        var made = Fx.Shared.Linked.Make();
        var count = order.Count();
        var proto = new Proto();
#if NET10_0_OR_GREATER
        Fx.Domain.Order.Line? current = null;
#else
        Fx.Other.Skipped? skipped = null;
#endif
    }
}
