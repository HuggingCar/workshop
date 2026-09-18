use encoding_rs::WINDOWS_1250;
use rust_decimal::prelude::*;
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use crate::error::{Error, Result};

pub const MAX_CENTS: i64 = 9_999_999_999;
pub const MAX_NAME_LENGTH: usize = 80;
pub const VAT_COUNT: usize = 7;

/// The rate the printer reserves for "zwolniona".
pub fn exempt_percent() -> Decimal {
    Decimal::from(100)
}

pub fn money(cents: i64) -> String {
    let (sign, cents) = if cents < 0 {
        ("-", cents.unsigned_abs())
    } else {
        ("", cents as u64)
    };
    let whole = cents / 100;
    let mut grouped = String::new();
    let digits = whole.to_string();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(' ');
        }
        grouped.push(digit);
    }
    format!("{sign}{grouped},{:02} zł", cents % 100)
}

fn encodable(text: &str) -> bool {
    !text
        .chars()
        .any(|ch| matches!(ch, '\u{81}' | '\u{83}' | '\u{88}' | '\u{90}' | '\u{98}'))
        && !WINDOWS_1250.encode(text).2
}

pub fn printable(value: &str) -> Result<()> {
    if value
        .chars()
        .any(|char| (char as u32) < 32 || char as u32 == 127)
    {
        return Err(Error::value("Tekst nie może zawierać znaków sterujących."));
    }
    if !encodable(value) {
        return Err(Error::value(
            "Drukarka nie obsługuje niektórych znaków w tekście.",
        ));
    }
    Ok(())
}

/// Printable Unicode categories, with ASCII space as the sole printable separator.
fn is_printable(char: char) -> bool {
    if char == ' ' {
        return true;
    }
    !matches!(
        get_general_category(char),
        GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::SpaceSeparator
    )
}

/// Whitespace includes the C1-adjacent file separators.
fn is_space(char: char) -> bool {
    char.is_whitespace() || matches!(char, '\u{1c}'..='\u{1f}')
}

/// Coerce raw user text into something a CP1250 device can print.
///
/// Characters the printer cannot encode are transliterated, invisible and control
/// characters (tabs separate protocol fields) are dropped, and the result is trimmed
/// to `max_length`.
pub fn sanitize(value: &str, max_length: usize) -> String {
    let mut out = String::with_capacity(value.len());
    for char in value.nfc() {
        if !is_printable(char) {
            if is_space(char) {
                out.push(' ');
            }
        } else if encodable(char.encode_utf8(&mut [0u8; 4])) {
            out.push(char);
        } else {
            out.push_str(any_ascii::any_ascii_char(char));
        }
    }
    out.trim_matches(is_space)
        .chars()
        .take(max_length)
        .collect()
}

pub fn validate_name(value: &str) -> Result<String> {
    let value = sanitize(value, MAX_NAME_LENGTH);
    if value.is_empty() {
        return Err(Error::value("Nazwa usługi nie może być pusta."));
    }
    Ok(value)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    name: String,
    quantity: Decimal,
    unit_price: Decimal,
    vat: usize,
}

impl Line {
    pub fn new(name: &str, quantity: Decimal, unit_price: Decimal, vat: usize) -> Result<Self> {
        let name = validate_name(name)?;
        for number in [quantity, unit_price] {
            if number <= Decimal::ZERO {
                return Err(Error::value("Ilość i cena muszą być dodatnimi liczbami."));
            }
        }
        if quantity > Decimal::from(9_999_999_999i64) || quantity.round_dp(8) != quantity {
            return Err(Error::value(
                "Ilość może mieć najwyżej 8 miejsc po przecinku.",
            ));
        }
        if unit_price > Decimal::from_str("99999999.99").expect("literal")
            || unit_price.round_dp(2) != unit_price
        {
            return Err(Error::value(
                "Cena może mieć najwyżej 2 miejsca po przecinku i wynosić do 99 999 999,99 zł.",
            ));
        }
        if vat >= VAT_COUNT {
            return Err(Error::value("Nieprawidłowa stawka VAT."));
        }
        let line = Self {
            name,
            quantity,
            unit_price,
            vat,
        };
        let total = line.total_cents();
        if !(1..=MAX_CENTS).contains(&total) {
            return Err(Error::value(
                "Wartość pozycji przekracza zakres drukarki lub zaokrągla się do zera.",
            ));
        }
        Ok(line)
    }

    /// The quantity exactly as the protocol carries it: plain notation, scale kept.
    pub fn quantity_text(&self) -> String {
        self.quantity.to_string()
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn vat(&self) -> usize {
        self.vat
    }

    pub fn total_cents(&self) -> i64 {
        half_up(self.quantity * self.unit_price * Decimal::ONE_HUNDRED)
    }

    pub fn price_cents(&self) -> i64 {
        (self.unit_price * Decimal::ONE_HUNDRED)
            .trunc()
            .to_i64()
            .unwrap_or(i64::MAX)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VatRate {
    pub index: usize,
    pub percent: Decimal,
}

impl VatRate {
    pub fn active(&self) -> bool {
        self.percent == exempt_percent()
            || (self.percent >= Decimal::ZERO
                && self.percent <= Decimal::from_str("99.99").expect("literal"))
    }

    /// The rate alone: '23%', 'zwolniona' or 'nieaktywna'.
    pub fn rate_label(&self) -> String {
        if self.percent == exempt_percent() {
            return "zwolniona".into();
        }
        if !self.active() {
            return "nieaktywna".into();
        }
        format!("{}%", self.percent.normalize()).replace('.', ",")
    }

    pub fn label(&self) -> String {
        let letter = char::from(b'A' + self.index as u8);
        format!("{letter} · {}", self.rate_label())
    }

    /// VAT contained in a gross amount, as the printer computes it (per rate, half-up).
    pub fn tax_cents(&self, gross_cents: i64) -> i64 {
        if self.percent == exempt_percent() {
            return 0;
        }
        half_up(Decimal::from(gross_cents) * self.percent / (Decimal::ONE_HUNDRED + self.percent))
    }
}

fn half_up(value: Decimal) -> i64 {
    value
        .round_dp_with_strategy(0, RoundingStrategy::MidpointAwayFromZero)
        .to_i64()
        .unwrap_or(i64::MAX)
}
