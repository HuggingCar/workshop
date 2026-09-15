from dataclasses import dataclass
from decimal import ROUND_HALF_UP, Decimal

MAX_CENTS = 9_999_999_999


def money(cents: int) -> str:
    whole = f"{cents // 100:,}".replace(",", " ")
    return f"{whole},{cents % 100:02d} zł"


def printable(value: str) -> None:
    if any(ord(c) < 32 or ord(c) == 127 for c in value):
        raise ValueError("Tekst nie może zawierać znaków sterujących.")
    try:
        value.encode("cp1250")
    except UnicodeEncodeError as exc:
        raise ValueError("Drukarka nie obsługuje niektórych znaków w tekście.") from exc


@dataclass(frozen=True)
class Line:
    name: str
    quantity: Decimal
    unit_price: Decimal
    vat: int

    def __post_init__(self):
        printable(self.name)
        object.__setattr__(self, "name", self.name.strip())
        if not 1 <= len(self.name) <= 80:
            raise ValueError("Nazwa usługi musi mieć od 1 do 80 znaków.")
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
        if type(self.vat) is not int or not 0 <= self.vat <= 6:
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
        return self.percent == 100 or 0 <= self.percent <= Decimal("99.99")

    @property
    def rate_label(self) -> str:
        """The rate alone: '23%', 'zwolniona' or 'nieaktywna'."""
        if self.percent == 100:
            return "zwolniona"
        if not self.active:
            return "nieaktywna"
        return f"{self.percent.normalize():f}%".replace(".", ",")

    @property
    def label(self) -> str:
        return f"{chr(65 + self.index)} · {self.rate_label}"

    def tax_cents(self, gross_cents: int) -> int:
        """VAT contained in a gross amount, as the printer computes it (per rate, half-up)."""
        if self.percent == 100:
            return 0
        return int(
            (Decimal(gross_cents) * self.percent / (100 + self.percent)).quantize(
                Decimal(1), rounding=ROUND_HALF_UP
            )
        )
