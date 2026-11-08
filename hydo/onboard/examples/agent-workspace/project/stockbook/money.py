"""Money helpers: every amount is a Decimal rounded half up to cents."""

from decimal import ROUND_HALF_UP, Decimal

CENT = Decimal("0.01")


def to_money(value) -> Decimal:
    """A Decimal from an int, a string or a Decimal (never a float: floats are not exact)."""
    if isinstance(value, float):
        raise TypeError("use a string or a Decimal for money, not a float")
    return Decimal(value)


def round_cents(value) -> Decimal:
    return to_money(value).quantize(CENT, rounding=ROUND_HALF_UP)


def format_money(value) -> str:
    v = round_cents(value)
    sign = "-" if v < 0 else ""
    whole, _, cents = f"{abs(v):.2f}".partition(".")
    groups = []
    while len(whole) > 3:
        groups.insert(0, whole[-3:])
        whole = whole[:-3]
    groups.insert(0, whole)
    return f"{sign}{','.join(groups)}.{cents}"
