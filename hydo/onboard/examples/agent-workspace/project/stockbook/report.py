"""Reports: the stock on hand as CSV, and the movements of a day range."""

import csv
import io

from .ledger import Ledger
from .money import format_money


def stock_csv(ledger: Ledger) -> str:
    out = io.StringIO()
    w = csv.writer(out, lineterminator="\n")
    w.writerow(["sku", "on_hand", "value"])
    for sku in ledger.skus():
        w.writerow([sku, ledger.on_hand(sku), format_money(ledger.value(sku))])
    return out.getvalue()


def movements(ledger: Ledger, first_day: int, last_day: int):
    return [m for m in ledger.movements if first_day <= m.day <= last_day]


def cost_of_goods(ledger: Ledger, first_day: int, last_day: int):
    return sum((m.cost for m in movements(ledger, first_day, last_day) if m.qty < 0), 0)
