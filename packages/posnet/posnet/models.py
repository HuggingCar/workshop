from dataclasses import dataclass
from decimal import ROUND_HALF_UP, Decimal
from unicodedata import normalize

from anyascii import anyascii

MAX_CENTS = 9_999_999_999
MAX_NAME_LENGTH = 80
VAT_COUNT = 7
EXEMPT_PERCENT = Decimal(100)  # the rate the printer reserves for "zwolniona"
CONTROL_CHARS = frozenset(chr(code) for code in (*range(32), 127))


def money(cents: int) -> str:
    whole = f"{cents // 100:,}".replace(",", " ")
    return f"{whole},{cents % 100:02d} zł"


def printable(value: str) -> None:
    if not CONTROL_CHARS.isdisjoint(value):
        raise ValueError("Tekst nie może zawierać znaków sterujących.")
    try:
        value.encode("cp1250")
    except UnicodeEncodeError as exc:
        raise ValueError("Drukarka nie obsługuje niektórych znaków w tekście.") from exc


def sanitize(value: str, max_length: int) -> str:
    """Coerce raw user text into something a CP1250 device can print.

    Characters the printer cannot encode are transliterated, invisible and
    control characters (tabs separate protocol fields) are dropped, and the
    result is trimmed to `max_length`.
    """
    chars = []
    for char in normalize("NFC", value):
        if not char.isprintable():
            chars.append(" " if char.isspace() else "")
            continue
        try:
            char.encode("cp1250")
        except UnicodeEncodeError:
            chars.append(anyascii(char))
        else:
            chars.append(char)
    return "".join(chars).strip()[:max_length]


def validate_name(value: str) -> str:
    value = sanitize(value, MAX_NAME_LENGTH)
    if not value:
        raise ValueError("Nazwa usługi nie może być pusta.")
    return value


@dataclass(frozen=True)
class Line:
    name: str
    quantity: Decimal
    unit_price: Decimal
    vat: int

    def __post_init__(self):
        object.__setattr__(self, "name", validate_name(self.name))
        for field in ("quantity", "unit_price"):
            number = Decimal(str(getattr(self, field)))
            if not number.is_finite() or number <= 0:
                raise ValueError("Ilość i cena muszą być dodatnimi liczbami.")
            object.__setattr__(self, field, number)
        if self.quantity > Decimal(9999999999) or self.quantity != self.quantity.quantize(
            Decimal("0.00000001")
        ):
            raise ValueError("Ilość może mieć najwyżej 8 miejsc po przecinku.")
        if self.unit_price > Decimal("99999999.99") or self.unit_price != self.unit_price.quantize(
            Decimal("0.01")
        ):
            raise ValueError(
                "Cena może mieć najwyżej 2 miejsca po przecinku i wynosić do 99 999 999,99 zł."
            )
        if type(self.vat) is not int or not 0 <= self.vat < VAT_COUNT:
            raise ValueError("Nieprawidłowa stawka VAT.")
        if not 0 < self.total_cents <= MAX_CENTS:
            raise ValueError(
                "Wartość pozycji przekracza zakres drukarki lub zaokrągla się do zera."
            )

    @property
    def total_cents(self) -> int:
        return int(
            (self.quantity * self.unit_price * 100).quantize(Decimal(1), rounding=ROUND_HALF_UP)
        )

    @property
    def price_cents(self) -> int:
        return int(self.unit_price * 100)


@dataclass(frozen=True)
class VatRate:
    index: int
    percent: Decimal

    @property
    def active(self) -> bool:
        return self.percent == EXEMPT_PERCENT or 0 <= self.percent <= Decimal("99.99")

    @property
    def rate_label(self) -> str:
        """The rate alone: '23%', 'zwolniona' or 'nieaktywna'."""
        if self.percent == EXEMPT_PERCENT:
            return "zwolniona"
        if not self.active:
            return "nieaktywna"
        return f"{self.percent.normalize():f}%".replace(".", ",")

    @property
    def label(self) -> str:
        return f"{chr(65 + self.index)} · {self.rate_label}"

    def tax_cents(self, gross_cents: int) -> int:
        """VAT contained in a gross amount, as the printer computes it (per rate, half-up)."""
        if self.percent == EXEMPT_PERCENT:
            return 0
        return int(
            (Decimal(gross_cents) * self.percent / (100 + self.percent)).quantize(
                Decimal(1), rounding=ROUND_HALF_UP
            )
        )
