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


@pytest.mark.parametrize(
    ("name", "expected"),
    [
        ("Кузов", "Kuzov"),  # the printer cannot encode it: transliterated, not rejected
        ("Wymiana 🔧 oleju", "Wymiana :wrench: oleju"),
        ("x\tnaInjected", "x naInjected"),  # tabs separate protocol fields
        ("Olej\u00ad", "Olej"),
        ("Olej\u00a05W30", "Olej 5W30"),
        ("x" * 100, "x" * 80),
    ],
)
def test_raw_name_is_sanitized_instead_of_rejected(name, expected):
    assert Line(name, Decimal(1), Decimal(10), 0).name == expected


@pytest.mark.parametrize("name", ["", " \t", "\u00ad"])
def test_name_that_sanitizes_to_nothing_is_rejected(name):
    with pytest.raises(ValueError):
        Line(name, Decimal(1), Decimal(10), 0)


def test_name_is_normalized_before_checking_printer_limit():
    line = Line(" o\u0301" + "ł" * 79 + " ", Decimal(1), Decimal(10), 0)
    assert line.name == "ó" + "ł" * 79


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


@pytest.mark.parametrize(
    ("index", "percent", "label"),
    [
        (0, "23.00", "A · 23%"),
        (1, "0", "B · 0%"),
        (2, "100", "C · zwolniona"),
        (6, "101", "G · nieaktywna"),
    ],
)
def test_vat_labels_name_the_slot_the_operator_picks(index, percent, label):
    rate = VatRate(index, Decimal(percent))
    assert rate.label == label
    assert rate.active is (percent != "101")
