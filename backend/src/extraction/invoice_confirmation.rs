//! Server-derived confirmation requirements for invoice candidates (Step 15b-3).
//!
//! Pure functions of what the parser returned plus whether the stored extraction was truncated. The CLIENT never
//! decides what must be acknowledged: the GET endpoint reports this set, and the future confirm endpoint
//! recomputes it and requires an exact match. The parser itself is untouched (no PARSER_VERSION change).
//!
//! Required keys, in this order, without duplicates:
//!   1. the parser's own `acknowledgments_required` (uncertain fields, blank-fee-code lines);
//!   2. `unparsed_lines[N]` for every LISTED unparsed line, N = its 1-based line number in the extracted text;
//!   3. `unparsed_not_listed` when more unparsed lines exist than the parser lists (the cap);
//!   4. `text_truncated` when the stored extraction was cut short;
//!   5. `line_sum_incomplete` when `checks.line_sum_complete` is false.
//! The original document must additionally be checked by the person (`original_document_check_required`)
//! whenever key 3, 4 or 5 applies.

use crate::extraction::invoice_parser::InvoiceCandidates;
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfirmationRequirements {
    pub acknowledgments_required: Vec<String>,
    pub original_document_check_required: bool,
}

pub fn confirmation_requirements(c: &InvoiceCandidates, text_truncated: bool) -> ConfirmationRequirements {
    let mut keys: Vec<String> = Vec::new();
    let mut push = |k: String| {
        if !keys.contains(&k) {
            keys.push(k);
        }
    };
    for k in &c.acknowledgments_required {
        push(k.clone());
    }
    for u in &c.unparsed_lines {
        push(format!("unparsed_lines[{}]", u.line));
    }
    let not_listed = c.unparsed_not_listed > 0;
    let incomplete = !c.checks.line_sum_complete;
    if not_listed {
        push("unparsed_not_listed".to_string());
    }
    if text_truncated {
        push("text_truncated".to_string());
    }
    if incomplete {
        push("line_sum_incomplete".to_string());
    }
    ConfirmationRequirements {
        acknowledgments_required: keys,
        original_document_check_required: not_listed || text_truncated || incomplete,
    }
}

#[cfg(test)]
mod tests {
    //! SYNTHETIC layouts only (same style as the parser's tests).
    use super::*;
    use crate::extraction::invoice_parser::{mark_text_truncated, parse_invoice_text};

    fn row(d: &str, f: &str, q: &str, u: &str, p: &str, a: &str) -> String {
        format!("{d:<28}{f:<13}{q:>6}  {u:<9}{p:>13}{a:>13}")
    }

    fn text(extra: &[&str], with_totals: bool) -> String {
        let mut v = vec![
            format!("{:<62}INVOICE", "Vendor: Evergreen Environmental"),
            "Invoice Number: INV-1              Invoice Date: 03/15/2026".to_string(),
            "Service Date:   03/20/2026".to_string(),
            "Waste Stream:   F003 Spent Solvents".to_string(),
            String::new(),
            row("Description", "Fee Code", "Qty", "Unit", "Unit Price", "Amount"),
            row("Drum disposal", "", "10", "drum", "400.00", "4,000.00"),
            row("Transportation", "TRANSPORT", "1", "flat", "500.00", "500.00"),
            String::new(),
        ];
        if with_totals {
            v.push("                                          Subtotal                        4,500.00".to_string());
            v.push("                                          Total Due                       4,500.00".to_string());
        }
        if !extra.is_empty() {
            v.push(String::new());
            v.push(String::new());
            v.extend(extra.iter().map(|s| s.to_string()));
        }
        v.join("\n")
    }

    fn req(t: &str, truncated: bool) -> (ConfirmationRequirements, InvoiceCandidates) {
        let mut c = parse_invoice_text(t);
        if truncated {
            mark_text_truncated(&mut c);
        }
        (confirmation_requirements(&c, truncated), c)
    }

    fn line_of(t: &str, needle: &str) -> usize {
        t.lines().position(|l| l.contains(needle)).unwrap() + 1
    }

    #[test]
    fn a_clean_complete_extraction_needs_only_the_blank_fee_code_acknowledgment() {
        let (r, _) = req(&text(&[], true), false);
        assert_eq!(r.acknowledgments_required, vec!["lines[0].fee_code"]);
        assert!(!r.original_document_check_required);
    }

    #[test]
    fn every_listed_unparsed_line_gets_its_own_key_with_its_text_line_number() {
        for (label, row_text) in [
            ("charge-like", "Emergency response fee                         150.00"),
            ("unreadable amount", "Emergency response fee                 $4,50.00"),
            ("tax", "Sales Tax 8.25%"),
        ] {
            let t = text(&[row_text], true);
            let n = line_of(&t, row_text);
            let (r, c) = req(&t, false);
            assert!(r.acknowledgments_required.contains(&format!("unparsed_lines[{n}]")), "{label}: {:?}", r.acknowledgments_required);
            assert!(r.acknowledgments_required.contains(&"line_sum_incomplete".to_string()), "{label}");
            assert!(r.original_document_check_required, "{label}");
            assert!(!c.checks.line_sum_complete, "{label}");
        }
    }

    #[test]
    fn an_uncorroborated_sum_needs_the_incomplete_key_and_the_original_check_but_no_unparsed_keys() {
        let (r, _) = req(&text(&[], false), false);
        assert!(r.acknowledgments_required.contains(&"line_sum_incomplete".to_string()));
        assert!(r.acknowledgments_required.iter().all(|k| !k.starts_with("unparsed_lines[")));
        assert!(r.original_document_check_required);
    }

    #[test]
    fn a_truncated_extraction_needs_its_own_key_and_can_never_be_complete() {
        let (r, c) = req(&text(&[], true), true);
        assert!(r.acknowledgments_required.contains(&"text_truncated".to_string()));
        assert!(r.acknowledgments_required.contains(&"line_sum_incomplete".to_string()));
        assert!(r.original_document_check_required);
        assert!(!c.checks.line_sum_complete);
        // the flag alone (without the parser being told) still adds its key
        let plain = parse_invoice_text(&text(&[], true));
        assert!(confirmation_requirements(&plain, true).acknowledgments_required.contains(&"text_truncated".to_string()));
    }

    #[test]
    fn unparsed_lines_beyond_the_listing_cap_add_the_not_listed_key() {
        let t: String = (0..620).map(|i| format!("Item {i}      1      {i}.50\n")).collect();
        let (r, c) = req(&t, false);
        assert_eq!(c.unparsed_not_listed, 120);
        assert!(r.acknowledgments_required.contains(&"unparsed_not_listed".to_string()));
        assert_eq!(r.acknowledgments_required.iter().filter(|k| k.starts_with("unparsed_lines[")).count(), 500);
        assert!(r.original_document_check_required);
    }

    #[test]
    fn the_set_is_deterministic_ordered_and_free_of_duplicates() {
        let t = text(&["Emergency response fee                         150.00", "Sales Tax 8.25%"], true);
        let (a, _) = req(&t, true);
        let (b, _) = req(&t, true);
        assert_eq!(a, b);
        let mut sorted = a.acknowledgments_required.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), a.acknowledgments_required.len(), "no duplicates");
        let pos = |k: &str| a.acknowledgments_required.iter().position(|x| x == k).unwrap();
        assert!(pos("lines[0].fee_code") < pos("text_truncated") && pos("text_truncated") < pos("line_sum_incomplete"));
    }
}
