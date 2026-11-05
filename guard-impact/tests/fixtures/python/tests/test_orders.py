from shop.orders.service import place_order


def test_place_order():
    assert place_order() is not None
