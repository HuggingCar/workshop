from decimal import Decimal

import pytest
from posnet.models import Line, VatRate, money


def test_fractional_quantity_rounds_line_half_up():
    line = Line("Naprawa", Decimal("0.5"), Decimal("19.99"), 0)
    assert line.total_cents == 1000
    assert money(3468) == "34,68 zł"
    assert money(123456789) == "1 234 567,89 zł"


@pytest.mark.parametrize("quantity", ["0", "-1", "NaN", "Infinity", "0.000000001"])
def test_invalid_quantity_rejected(quantity):
    with pytest.raises(ValueError):
        Line("Naprawa", Decimal(quantity), Decimal(10), 0)


@pytest.mark.parametrize("price", ["0", "-1", "NaN", "Infinity", "1.001", "100000000"])
def test_invalid_unit_price_rejected(price):
    with pytest.raises(ValueError):
        Line("Naprawa", Decimal(1), Decimal(price), 0)


@pytest.mark.parametrize("name", ["", " \t", "x\tnaInjected", "x\n", "🔧", "x" * 81])
def test_unsafe_or_unprintable_name_rejected(name):
    with pytest.raises(ValueError):
        Line(name, Decimal(1), Decimal(10), 0)


def test_line_value_must_fit_wire_amount():
    with pytest.raises(ValueError):
        Line("Naprawa", Decimal(9999999999), Decimal(10), 0)


def test_vat_slot_must_be_valid():
    with pytest.raises(ValueError):
        Line("Naprawa", Decimal(1), Decimal(10), 7)


@pytest.mark.parametrize(
    ("percent", "gross", "tax"),
    [("23", 10000, 1870), ("8", 10000, 741), ("0", 10000, 0), ("100", 10000, 0), ("23", 1, 0)],
)
def test_vat_contained_in_gross_matches_printer_rounding(percent, gross, tax):
    assert VatRate(0, Decimal(percent)).tax_cents(gross) == tax
