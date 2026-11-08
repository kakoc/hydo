from decimal import Decimal

from stockbook import Ledger
from stockbook.money import format_money, round_cents
from stockbook.report import cost_of_goods, movements, stock_csv


def sample():
    led = Ledger()
    led.receive("b", 3, "1000", day=1)
    led.receive("a", 10, "0.5", day=1)
    led.ship("a", 4, day=2)
    led.receive("c", 1, "7", day=3)
    led.ship("c", 1, day=3)
    return led


def test_stock_csv():
    assert stock_csv(sample()) == "sku,on_hand,value\na,6,3.00\nb,3,\"3,000.00\"\n"


def test_movements_in_range():
    assert [m.sku for m in movements(sample(), 2, 3)] == ["a", "c", "c"]


def test_cost_of_goods():
    assert cost_of_goods(sample(), 1, 3) == Decimal("9.00")


def test_round_cents():
    assert round_cents("2.675") == Decimal("2.68")
    assert round_cents("-0.005") == Decimal("-0.01")


def test_format_money():
    assert format_money("1234567.891") == "1,234,567.89"
    assert format_money("-5") == "-5.00"
    assert format_money("0") == "0.00"
