from decimal import Decimal

import pytest

from stockbook import Ledger, OutOfStock


def test_receive_adds_stock():
    led = Ledger()
    led.receive("bolt", 10, "0.25")
    assert led.on_hand("bolt") == 10
    assert led.value("bolt") == Decimal("2.50")


def test_receive_rejects_bad_quantities():
    led = Ledger()
    with pytest.raises(ValueError):
        led.receive("bolt", 0, "1")
    with pytest.raises(ValueError):
        led.receive("bolt", 3, "-1")


def test_receive_rejects_float_money():
    with pytest.raises(TypeError):
        Ledger().receive("bolt", 3, 0.1)


def test_ship_whole_lot():
    led = Ledger()
    led.receive("nut", 5, "1.00")
    assert led.ship("nut", 5) == Decimal("5.00")
    assert led.on_hand("nut") == 0
    assert led.skus() == []


def test_ship_part_of_a_lot():
    led = Ledger()
    led.receive("nut", 10, "1.00")
    assert led.ship("nut", 3) == Decimal("3.00")
    assert led.on_hand("nut") == 7


def test_ship_first_in_first_out():
    led = Ledger()
    led.receive("gear", 4, "2.00", day=1)
    led.receive("gear", 4, "3.00", day=2)
    assert led.ship("gear", 6, day=3) == Decimal("14.00")
    assert led.on_hand("gear") == 2
    assert led.value("gear") == Decimal("6.00")


def test_ship_twice_from_one_lot():
    led = Ledger()
    led.receive("pin", 100, "0.10")
    led.ship("pin", 30)
    led.ship("pin", 30)
    assert led.on_hand("pin") == 40
    assert led.value("pin") == Decimal("4.00")


def test_out_of_stock():
    led = Ledger()
    led.receive("cog", 2, "9.99")
    with pytest.raises(OutOfStock) as e:
        led.ship("cog", 3)
    assert e.value.on_hand == 2
    assert led.on_hand("cog") == 2


@pytest.mark.parametrize(
    "lots,ship,cost",
    [
        ([(1, "1.00")], 1, "1.00"),
        ([(2, "1.00"), (2, "2.00")], 3, "4.00"),
        ([(3, "0.333")], 3, "1.00"),
        ([(1, "0.005"), (1, "0.005")], 2, "0.01"),
        ([(5, "10"), (5, "20"), (5, "30")], 12, "210.00"),
        ([(7, "1.10"), (1, "100")], 8, "107.70"),
    ],
)
def test_cost_of_goods(lots, ship, cost):
    led = Ledger()
    for i, (q, c) in enumerate(lots):
        led.receive("x", q, c, day=i)
    assert led.ship("x", ship) == Decimal(cost)


def test_stock_after_many_movements():
    led = Ledger()
    for day in range(30):
        led.receive("w", 10, str(day), day=day)
        led.ship("w", 7, day=day)
    assert led.on_hand("w") == 90
    assert led.value("w") == Decimal(sum(range(21, 30)) * 10)
