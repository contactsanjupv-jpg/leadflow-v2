//! Calendar-date plausibility, shared by every endpoint that accepts a date.
//!
//! Why this exists: a browser date control emits a valid ISO date on EVERY keystroke while the year is
//! being typed (`0002-…`, `0020-…`, `0202-…`), and one lost keystroke leaves a three-digit year such as
//! `0206`. `chrono::NaiveDate` and PostgreSQL `DATE` both accept any year 0001–9999, so such a value used
//! to be stored verbatim. Business dates in this product (invoices, manifests, weight tickets, contract
//! windows) are never outside this range; anything else is almost certainly a typing slip.
//!
//! The same bounds are enforced in PostgreSQL (migration 0005) and in the frontend `DateInput`.

use chrono::{Datelike, NaiveDate};

pub const MIN_YEAR: i32 = 1900;
pub const MAX_YEAR: i32 = 2100;

pub fn validate_date(field: &str, date: NaiveDate) -> Result<NaiveDate, String> {
    let year = date.year();
    if (MIN_YEAR..=MAX_YEAR).contains(&year) {
        Ok(date)
    } else {
        Err(format!(
            "{field}: the year {year:04} is outside the supported range {MIN_YEAR}-{MAX_YEAR}; check that the full 4-digit year was entered"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate { s.parse().unwrap() }

    #[test]
    fn accepts_normal_dates_and_the_boundaries() {
        for s in ["2026-09-12", "2026-12-09", "1900-01-01", "2100-12-31", "2024-02-29"] {
            assert_eq!(validate_date("f", d(s)).unwrap(), d(s), "{s}");
        }
    }

    #[test]
    fn rejects_partial_and_implausible_years_with_a_message_naming_the_field_and_year() {
        // 0206 is exactly what a browser date control produces when one year digit is lost.
        for s in ["0206-12-09", "0026-01-01", "0002-09-12", "0001-01-01", "1899-12-31", "2101-01-01", "9999-12-31"] {
            let e = validate_date("shipment_date", d(s)).unwrap_err();
            assert!(e.contains("shipment_date") && e.contains("4-digit year"), "{s}: {e}");
        }
        assert!(validate_date("x", d("0206-12-09")).unwrap_err().contains("0206"));
    }

    #[test]
    fn iso_round_trip_of_the_regression_dates_is_exact() {
        // 09/12/2026 is 12 September in month/day locales and 9 December in day/month locales.
        assert_eq!(validate_date("f", d("2026-09-12")).unwrap().to_string(), "2026-09-12");
        assert_eq!(validate_date("f", d("2026-12-09")).unwrap().to_string(), "2026-12-09");
    }
}