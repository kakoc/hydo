"""The ledger: stock arrives in lots and leaves first in, first out."""

from dataclasses import dataclass, field
from decimal import Decimal
from typing import Dict, List

from .money import round_cents, to_money


class OutOfStock(Exception):
    def __init__(self, sku: str, wanted: int, on_hand: int):
        super().__init__(f"{sku}: {wanted} wanted, {on_hand} on hand")
        self.sku = sku
        self.wanted = wanted
        self.on_hand = on_hand


@dataclass
class Lot:
    qty: int
    unit_cost: Decimal
    day: int


@dataclass
class Movement:
    day: int
    sku: str
    qty: int  # positive: received, negative: shipped
    cost: Decimal


@dataclass
class Ledger:
    lots: Dict[str, List[Lot]] = field(default_factory=dict)
    movements: List[Movement] = field(default_factory=list)

    def receive(self, sku: str, qty: int, unit_cost, day: int = 0) -> None:
        if qty <= 0:
            raise ValueError("qty must be positive")
        cost = to_money(unit_cost)
        if cost < 0:
            raise ValueError("unit_cost must not be negative")
        self.lots.setdefault(sku, []).append(Lot(qty, cost, day))
        self.movements.append(Movement(day, sku, qty, round_cents(cost * qty)))

    def on_hand(self, sku: str) -> int:
        return sum(lot.qty for lot in self.lots.get(sku, []))

    def value(self, sku: str) -> Decimal:
        return round_cents(sum((lot.qty * lot.unit_cost for lot in self.lots.get(sku, [])), Decimal(0)))

    def ship(self, sku: str, qty: int, day: int = 0) -> Decimal:
        """Take `qty` out of the oldest lots first; returns the cost of the goods shipped."""
        if qty <= 0:
            raise ValueError("qty must be positive")
        have = self.on_hand(sku)
        if qty > have:
            raise OutOfStock(sku, qty, have)
        lots = self.lots[sku]
        cost = Decimal(0)
        left = qty
        while left:
            lot = lots[0]
            take = min(left, lot.qty)
            cost += take * lot.unit_cost
            lot.qty = take
            left -= take
            if lot.qty == 0:
                lots.pop(0)
        cost = round_cents(cost)
        self.movements.append(Movement(day, sku, -qty, cost))
        return cost

    def skus(self) -> List[str]:
        return sorted(sku for sku, lots in self.lots.items() if lots)
