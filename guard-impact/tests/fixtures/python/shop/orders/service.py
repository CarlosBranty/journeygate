from ..models import Order
import shop.payments.gateway as gw


def place_order() -> Order:
    gw.charge()
    return Order()
