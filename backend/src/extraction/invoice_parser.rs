//! Deterministic invoice CANDIDATE extraction from already-extracted text (Step 15a).
//!
//! What this is: a rule-based reader that turns the text layer of a born-digital invoice (as produced by
//! `pdftotext -layout`, or plain text) into *candidate* values for a person to review. Nothing here creates
//! a financial record, touches the database or feeds the reconciliation engine; the output is advice for a
//! human who must confirm every value.
//!
//! What this is not:
//! * It does not report a confidence score. A value that "looks right" is not proof it is right, so each
//!   value is only `extracted` (found, unambiguous, passes its own checks), `uncertain` (found but
//!   ambiguous, malformed, conflicting or failing a check - the reasons are listed) or `missing`.
//! * It never guesses. Fee codes are taken only from an explicit fee-code column, never from a description;
//!   the service date is never defaulted to the invoice date; contract rates are never involved; amounts and
//!   dates that cannot be read are reported as unreadable, not repaired.
//! * It never silently drops text it could not use: table rows and amount-bearing lines it cannot read are
//!   returned in `unparsed_lines` (bounded, with an explicit count of any not listed).
//!
//! Honesty about accuracy: the rules and the tests were written against SYNTHETIC invoice layouts. They show
//! the logic behaves as specified; they are not evidence of accuracy on any real vendor's invoices.
//!
//! Output is a pure function of the input text and `PARSER_VERSION` (no clocks, no randomness, ordered
//! collections), so the same text always gives byte-identical JSON. Bump `PARSER_VERSION` whenever a rule
//! changes in a way that can change output.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;
use std::collections::BTreeSet;
use std::str::FromStr;

pub const PARSER_VERSION: &str = "invoice-rules-3";

/// Same $0.01 tolerance the invoice API applies to line and invoice totals.
fn rounding_tolerance() -> Decimal {
    Decimal::new(1, 2)
}
const MAX_UNPARSED_LISTED: usize = 500;
const MAX_LINE_CANDIDATES: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Extracted,
    Missing,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Evidence {
    /// 1-based line number in the extracted text (as split by `str::lines`).
    pub line: usize,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Field {
    pub value: Option<String>,
    pub status: Status,
    /// The text the value was read from, when something was found (even if it could not be read).
    pub raw: Option<String>,
    pub evidence: Option<Evidence>,
    pub reasons: Vec<String>,
    /// Other readings of the same field (conflicting matches, or the other reading of an ambiguous date).
    pub alternatives: Vec<String>,
}

impl Field {
    fn missing(reason: &str) -> Field {
        Field {
            value: None,
            status: Status::Missing,
            raw: None,
            evidence: None,
            reasons: vec![reason.to_string()],
            alternatives: vec![],
        }
    }
    fn extracted(value: String, raw: &str, evidence: Evidence) -> Field {
        Field {
            value: Some(value),
            status: Status::Extracted,
            raw: Some(clip(raw, 100)),
            evidence: Some(evidence),
            reasons: vec![],
            alternatives: vec![],
        }
    }
    fn uncertain(
        value: Option<String>,
        raw: &str,
        evidence: Evidence,
        reasons: Vec<String>,
        alternatives: Vec<String>,
    ) -> Field {
        Field {
            value,
            status: Status::Uncertain,
            raw: Some(clip(raw, 100)),
            evidence: Some(evidence),
            reasons,
            alternatives,
        }
    }
    fn add_uncertainty(&mut self, reason: String) {
        if self.status == Status::Extracted {
            self.status = Status::Uncertain;
        }
        self.reasons.push(reason);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Arithmetic {
    /// quantity x unit price, exactly.
    pub computed_total: String,
    pub difference: String,
    pub within_tolerance: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineCandidate {
    /// 0-based position among the candidates; this is the `lines[i]` of an acknowledgment key.
    pub index: usize,
    pub description: Field,
    pub fee_code: Field,
    pub quantity: Field,
    pub unit: Field,
    pub unit_price: Field,
    pub line_total: Field,
    /// No fee code was read for this line. The invoice API prices a blank fee code as a DISPOSAL line,
    /// so a person must acknowledge every such line explicitly.
    pub blank_fee_code: bool,
    pub arithmetic: Option<Arithmetic>,
    pub status: Status,
    pub evidence: Evidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnparsedLine {
    pub line: usize,
    pub text: String,
    pub kind: UnparsedKind,
    pub reason: String,
}

/// Why a line is in `unparsed_lines`. Every kind except `subtotal` blocks `line_sum_complete`; a `subtotal`
/// blocks it too unless it was accounted for (and then it is not listed at all).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UnparsedKind {
    /// A table row whose cells could not be read or assigned reliably.
    UnreadableRow,
    /// Text ending in something that LOOKS like an amount but cannot be read (`$4,50.00`, `4.500,00`,
    /// `12O0.00`). It may be a charge, so it is kept with its original text and blocks `line_sum_complete`.
    UnreadableAmount,
    /// Text ending in a decimal, `$`-prefixed or negative amount that is not a recognised table row.
    ChargeLike,
    /// Text ending in a bare whole number in its own column. It may be a charge, or an address / phone /
    /// reference number; the parser cannot tell, so it says so instead of ignoring it.
    PossibleCharge,
    /// A tax row. Tax is never silently absorbed: it stays listed until a person represents it.
    Tax,
    /// A subtotal that could not be accepted against a complete, matching line sum.
    Subtotal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Checks {
    pub parsed_line_count: usize,
    /// Sum of the parsed line totals (only when every parsed line has a readable total).
    pub line_total_sum: Option<String>,
    /// True only when the parser has POSITIVE EVIDENCE that the parsed lines are every charge on the invoice:
    /// a table was recognised, every line total is readable, no unreadable row / charge-like line / tax row /
    /// unaccepted subtotal remains, AND the sum is corroborated by a single, matching invoice total or by an
    /// accepted subtotal. Readable table rows alone are never enough. When false, `incompleteness_reasons`
    /// says why; when true, `completeness_basis` says what the evidence was. Even when true this is
    /// evidence for a person reviewing the invoice, not a verdict that the invoice is correct.
    pub line_sum_complete: bool,
    pub completeness_basis: Option<String>,
    pub incompleteness_reasons: Vec<String>,
    /// Whether the invoice total equals the sum of the parsed line totals (within $0.01); `None` when either
    /// side is unavailable.
    pub total_matches_lines: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvoiceCandidates {
    pub parser_version: &'static str,
    pub vendor_name: Field,
    pub invoice_number: Field,
    pub invoice_date: Field,
    pub service_date: Field,
    pub waste_stream: Field,
    pub invoice_total: Field,
    pub lines: Vec<LineCandidate>,
    pub unparsed_lines: Vec<UnparsedLine>,
    /// Unparsed lines found beyond the listing cap (never hidden: the count is always reported).
    pub unparsed_not_listed: usize,
    pub checks: Checks,
    /// Keys a person must explicitly acknowledge before confirming: every `uncertain` field
    /// (`invoice_number`, `lines[2].unit_price`, ...) and every blank-fee-code line (`lines[2].fee_code`).
    pub acknowledgments_required: Vec<String>,
    pub warnings: Vec<String>,
}

// ------------------------------------------------------------------------------------------ text helpers

fn clip(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

fn evidence(line_no: usize, text: &str) -> Evidence {
    Evidence {
        line: line_no,
        text: clip(text, 200),
    }
}

fn normalize_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| {
            l.chars()
                .filter(|c| *c != '\u{0c}')
                .map(|c| match c {
                    '\t' | '\u{a0}' => ' ',
                    '\u{2212}' | '\u{2013}' => '-', // true minus / en dash used as a minus sign
                    other => other,
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// A run of text separated from its neighbours by two or more spaces (a layout column).
#[derive(Debug, Clone)]
struct Seg {
    start: usize,
    end: usize, // exclusive, in chars
    text: String,
}

fn segments(line: &str) -> Vec<Seg> {
    let chars: Vec<char> = line.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == ' ' {
            i += 1;
            continue;
        }
        let start = i;
        let mut last = i;
        let mut j = i;
        while j < chars.len() {
            if chars[j] == ' ' {
                let mut k = j;
                while k < chars.len() && chars[k] == ' ' {
                    k += 1;
                }
                if k - j >= 2 || k == chars.len() {
                    break;
                }
                j = k;
                continue;
            }
            last = j;
            j += 1;
        }
        out.push(Seg {
            start,
            end: last + 1,
            text: chars[start..=last].iter().collect(),
        });
        i = j;
    }
    out
}

// ---------------------------------------------------------------------------------------- value parsing

struct ParsedVal {
    /// The value as it would be entered in the invoice form.
    value: String,
    /// Identity used to decide whether two matches are "the same value".
    key: String,
    reasons: Vec<String>,
    alternatives: Vec<String>,
}

fn is_currency_word(t: &str) -> bool {
    matches!(t.to_ascii_lowercase().as_str(), "usd" | "us" | "$")
}

/// An exact decimal amount. Accepts `$`, a leading `-` or surrounding parentheses (negative), and thousands
/// commas ONLY in well-formed groups of three. Anything else (`4,50.00`, `12O0.00`, `4.500,00`, more than
/// four decimals, no digits) is malformed and is reported, never repaired. Returns (value, was_negative).
fn parse_amount(raw: &str, allow_dollar: bool) -> Result<(Decimal, bool), String> {
    let original = raw.trim();
    let mut s = original;
    let mut negative = false;
    if s.starts_with('(') && s.ends_with(')') && s.len() >= 3 {
        negative = true;
        s = s[1..s.len() - 1].trim();
    }
    if let Some(r) = s.strip_prefix('-') {
        if negative {
            return Err(format!("'{original}' is not a valid amount"));
        }
        negative = true;
        s = r.trim_start();
    }
    if let Some(r) = s.strip_prefix('$') {
        if !allow_dollar {
            return Err(format!("'{original}' is not a plain number"));
        }
        s = r.trim_start();
    }
    if s.is_empty() {
        return Err(format!("'{original}' is not a valid amount"));
    }
    let (int_part, frac) = match s.split_once('.') {
        Some((i, f)) => (i, Some(f)),
        None => (s, None),
    };
    let digits = |x: &str| !x.is_empty() && x.chars().all(|c| c.is_ascii_digit());
    let int_ok = if int_part.contains(',') {
        let groups: Vec<&str> = int_part.split(',').collect();
        digits(groups[0])
            && groups[0].len() <= 3
            && groups[1..].iter().all(|g| g.len() == 3 && digits(g))
    } else {
        digits(int_part)
    };
    if !int_ok {
        return Err(format!("'{original}' is not a valid amount"));
    }
    if let Some(f) = frac {
        if !digits(f) {
            return Err(format!("'{original}' is not a valid amount"));
        }
        if f.len() > 4 {
            return Err(format!(
                "'{original}' has more than 4 decimal places, which the invoice API does not accept"
            ));
        }
    }
    let cleaned = format!(
        "{}{}",
        int_part.replace(',', ""),
        frac.map(|f| format!(".{f}")).unwrap_or_default()
    );
    let d =
        Decimal::from_str(&cleaned).map_err(|_| format!("'{original}' is not a valid amount"))?;
    if d >= Decimal::from(10_000_000_000i64) {
        return Err(format!("'{original}' is too large"));
    }
    Ok((if negative { -d } else { d }, negative))
}

fn month_from_name(s: &str) -> Option<u32> {
    let l = s.to_ascii_lowercase();
    const NAMES: [&str; 12] = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    for (i, n) in NAMES.iter().enumerate() {
        if l == *n || (l.len() == 3 && n.starts_with(&l)) || (i == 8 && l == "sept") {
            return Some(i as u32 + 1);
        }
    }
    None
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_digit())
}

fn iso(y: i32, m: u32, d: u32) -> Option<String> {
    let date = NaiveDate::from_ymd_opt(y, m, d)?;
    crate::common::dates::validate_date("date", date).ok()?;
    Some(date.format("%Y-%m-%d").to_string())
}

/// Parses exactly one date expression. US month-first is assumed for numeric dates with two readings, but
/// that reading is REPORTED as ambiguous (with both readings as alternatives), never presented as certain.
fn parse_date_one(s: &str) -> Result<ParsedVal, String> {
    let s = s.trim().trim_end_matches([',', '.', ';']);
    let bad = || format!("'{s}' is not a valid calendar date (or its year is outside 1900-2100)");
    let mk = |iso: String, reasons: Vec<String>, alts: Vec<String>| ParsedVal {
        key: iso.clone(),
        value: iso,
        reasons,
        alternatives: alts,
    };
    // numeric: YYYY-MM-DD, YYYY/MM/DD, M/D/YYYY, M-D-YY, M.D.YYYY
    let parts: Vec<&str> = s.split(['/', '-', '.']).collect();
    if parts.len() == 3 && parts.iter().all(|p| all_digits(p)) {
        if parts[0].len() == 4 {
            let (y, m, d) = (parts[0], parts[1], parts[2]);
            if m.len() > 2 || d.len() > 2 {
                return Err(bad());
            }
            let v =
                iso(y.parse().unwrap(), m.parse().unwrap(), d.parse().unwrap()).ok_or_else(bad)?;
            return Ok(mk(v, vec![], vec![]));
        }
        if parts[0].len() > 2 || parts[1].len() > 2 || !(parts[2].len() == 4 || parts[2].len() == 2)
        {
            return Err(bad());
        }
        let (a, b): (u32, u32) = (parts[0].parse().unwrap(), parts[1].parse().unwrap());
        let mut reasons = Vec::new();
        let year: i32 = if parts[2].len() == 2 {
            reasons.push("two-digit year read as 20yy".to_string());
            2000 + parts[2].parse::<i32>().unwrap()
        } else {
            parts[2].parse().unwrap()
        };
        let mut alts = Vec::new();
        let (month, day) = if a <= 12 && b <= 12 && a != b {
            let primary = iso(year, a, b).ok_or_else(bad)?;
            let other = iso(year, b, a).ok_or_else(bad)?;
            reasons.push(
                "ambiguous numeric date: it could be month/day or day/month; read as month/day"
                    .to_string(),
            );
            alts = vec![primary, other];
            (a, b)
        } else if a <= 12 {
            (a, b)
        } else if b <= 12 {
            reasons
                .push("read as day/month because the first number is greater than 12".to_string());
            (b, a)
        } else {
            return Err(bad());
        };
        let v = iso(year, month, day).ok_or_else(bad)?;
        return Ok(mk(v, reasons, alts));
    }
    // month name: "Mar 9, 2026", "March 9 2026", "9 Mar 2026", "09-Mar-2026"
    let toks: Vec<&str> = s
        .split([' ', ',', '-', '/', '.'])
        .filter(|t| !t.is_empty())
        .collect();
    if toks.len() == 3 {
        let (mi, others): (usize, Vec<&str>) =
            match (month_from_name(toks[0]), month_from_name(toks[1])) {
                (Some(_), None) => (0, vec![toks[1], toks[2]]),
                (None, Some(_)) => (1, vec![toks[0], toks[2]]),
                _ => return Err(format!("'{s}' is not a recognised date")),
            };
        let month = month_from_name(toks[mi]).unwrap();
        let (day, year) = (others[0], others[1]);
        if all_digits(day)
            && day.len() <= 2
            && all_digits(year)
            && (year.len() == 4 || year.len() == 2)
        {
            let mut reasons = Vec::new();
            let y: i32 = if year.len() == 2 {
                reasons.push("two-digit year read as 20yy".to_string());
                2000 + year.parse::<i32>().unwrap()
            } else {
                year.parse().unwrap()
            };
            let v = iso(y, month, day.parse().unwrap()).ok_or_else(bad)?;
            return Ok(mk(v, reasons, vec![]));
        }
    }
    Err(format!("'{s}' is not a recognised date"))
}

fn parse_date_text(rest: &str) -> Result<ParsedVal, String> {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    if toks.is_empty() {
        return Err("no date follows the label".to_string());
    }
    let mut first_err = None;
    for n in [3usize, 2, 1] {
        if n > toks.len() {
            continue;
        }
        match parse_date_one(&toks[..n].join(" ")) {
            Ok(v) => return Ok(v),
            Err(e) => {
                if n == 1 || first_err.is_none() {
                    first_err = Some(e);
                }
            }
        }
    }
    Err(first_err.unwrap_or_else(|| "not a recognised date".to_string()))
}

fn parse_invoice_number(rest: &str) -> Result<ParsedVal, String> {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let Some(first) = toks.first() else {
        return Err("no invoice number follows the label".to_string());
    };
    let tok = first.trim_end_matches([',', '.', ';']);
    let ok = !tok.is_empty()
        && tok.chars().count() <= 40
        && tok
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && tok
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '/' | '.'));
    if !ok {
        return Err(format!(
            "'{}' does not look like an invoice number",
            clip(first, 40)
        ));
    }
    let mut reasons = Vec::new();
    if toks.len() > 1 {
        reasons.push(
            "more text follows the number on the same line; confirm the number is complete"
                .to_string(),
        );
    }
    Ok(ParsedVal {
        value: tok.to_string(),
        key: tok.to_string(),
        reasons,
        alternatives: vec![],
    })
}

fn parse_free_text(rest: &str, max: usize) -> Result<ParsedVal, String> {
    let t = rest.trim();
    if t.is_empty() {
        return Err("nothing follows the label".to_string());
    }
    let mut reasons = Vec::new();
    if t.chars().count() > max {
        reasons.push(format!(
            "longer than {max} characters; confirm it is the value only"
        ));
    }
    Ok(ParsedVal {
        value: clip(t, max),
        key: t.to_ascii_lowercase(),
        reasons,
        alternatives: vec![],
    })
}

/// `None` = this label hit is not an amount at all and is ignored (only for the bare label `total`).
fn parse_total_text(rest: &str, bare: bool) -> Option<Result<ParsedVal, String>> {
    let toks: Vec<&str> = rest.split_whitespace().collect();
    let first = *toks.first()?;
    let amount_like = first
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_digit() || matches!(c, '$' | '-' | '('));
    let trailing_ok = toks[1..].iter().all(|t| is_currency_word(t));
    if bare && (!amount_like || !trailing_ok) {
        return None;
    }
    Some(parse_amount(first, true).map(|(d, neg)| {
        let mut reasons = Vec::new();
        if neg {
            reasons.push("negative amount (a credit?)".to_string());
        }
        if !trailing_ok {
            reasons.push("more text follows the amount on the same line".to_string());
        }
        ParsedVal {
            value: d.to_string(),
            key: d.normalize().to_string(),
            reasons,
            alternatives: vec![],
        }
    }))
}

// ------------------------------------------------------------------------------------- label scanning

struct Hit {
    label: String,
    rest: String,
    line_no: usize,
    line: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pickup {
    AfterColon,
    Always,
}

/// Finds `Label: value` / `Label value` pairs. A label must START a layout column, so a label that merely
/// appears inside a sentence is not read. `(label, needs_separator)`: a generic label such as `invoice` or
/// `date` only counts when followed by `:` or `#`. When nothing follows the label in its own column, the next
/// column is used only as `pickup` allows.
fn find_labeled(lines: &[String], labels: &[(&str, bool)], pickup: Pickup) -> Vec<Hit> {
    let mut hits = Vec::new();
    for (idx, line) in lines.iter().enumerate() {
        let segs = segments(line);
        for (i, seg) in segs.iter().enumerate() {
            let lower = seg.text.to_ascii_lowercase();
            let mut best: Option<(&str, usize)> = None;
            for (label, needs_sep) in labels {
                let Some(after) = lower.strip_prefix(label) else {
                    continue;
                };
                let alnum_end = label
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric());
                if alnum_end
                    && after
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    continue;
                }
                if *needs_sep {
                    let t = after.trim_start();
                    if !(t.starts_with(':') || t.starts_with('#')) {
                        continue;
                    }
                }
                if !best.is_some_and(|(_, l)| label.len() <= l) {
                    best = Some((label, label.len()));
                }
            }
            let Some((label, len)) = best else {
                continue;
            };
            let after_label = &seg.text[len..];
            let rest = after_label
                .trim_start_matches([':', '#', '.', ' '])
                .trim()
                .to_string();
            let ends_colon = seg.text.trim_end().ends_with(':');
            let rest = if rest.is_empty() {
                let allowed =
                    pickup == Pickup::Always || (pickup == Pickup::AfterColon && ends_colon);
                match (allowed, segs.get(i + 1)) {
                    (true, Some(next)) => next.text.clone(),
                    _ => continue,
                }
            } else {
                rest
            };
            hits.push(Hit {
                label: label.to_string(),
                rest,
                line_no: idx + 1,
                line: line.clone(),
            });
        }
    }
    hits
}

/// Turns every hit of one field into a single `Field`, keeping conflicting and unreadable matches visible.
fn build_field(
    hits: &[Hit],
    parse: impl Fn(&Hit) -> Option<Result<ParsedVal, String>>,
    absent_reason: &str,
) -> Field {
    let mut oks: Vec<(ParsedVal, &Hit)> = Vec::new();
    let mut bad: Vec<(String, &Hit)> = Vec::new();
    for h in hits {
        match parse(h) {
            None => {}
            Some(Ok(v)) => oks.push((v, h)),
            Some(Err(e)) => bad.push((e, h)),
        }
    }
    if oks.is_empty() && bad.is_empty() {
        return Field::missing(absent_reason);
    }
    if oks.is_empty() {
        let (_, h) = &bad[0];
        let mut reasons: Vec<String> = Vec::new();
        for (e, _) in &bad {
            if !reasons.contains(e) {
                reasons.push(e.clone());
            }
        }
        return Field::uncertain(None, &h.rest, evidence(h.line_no, &h.line), reasons, vec![]);
    }
    let mut distinct: Vec<&str> = Vec::new();
    let mut distinct_vals: Vec<String> = Vec::new();
    for (v, _) in &oks {
        if !distinct.contains(&v.key.as_str()) {
            distinct.push(&v.key);
            distinct_vals.push(v.value.clone());
        }
    }
    let (first, hit) = &oks[0];
    let mut reasons = first.reasons.clone();
    let mut alternatives = first.alternatives.clone();
    if distinct.len() > 1 {
        reasons.push(format!(
            "{} different values were found in the document",
            distinct.len()
        ));
        alternatives = distinct_vals;
    }
    for (e, _) in &bad {
        let r = format!("another occurrence could not be read: {e}");
        if !reasons.contains(&r) {
            reasons.push(r);
        }
    }
    let ev = evidence(hit.line_no, &hit.line);
    if reasons.is_empty() {
        Field::extracted(first.value.clone(), &hit.rest, ev)
    } else {
        Field::uncertain(
            Some(first.value.clone()),
            &hit.rest,
            ev,
            reasons,
            alternatives,
        )
    }
}

// ------------------------------------------------------------------------------------------------- table

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Col {
    Description,
    FeeCode,
    Quantity,
    Unit,
    UnitPrice,
    LineTotal,
    Other,
}

fn col_kind(name: &str) -> Col {
    let n = name.trim().trim_end_matches(':').to_ascii_lowercase();
    match n.as_str() {
        "description"
        | "item"
        | "item description"
        | "service"
        | "service description"
        | "charge description" => Col::Description,
        "fee code" | "charge code" => Col::FeeCode,
        "qty" | "qty." | "quantity" => Col::Quantity,
        "unit" | "uom" | "unit of measure" | "u/m" => Col::Unit,
        "unit price" | "price" | "rate" | "unit rate" | "unit cost" => Col::UnitPrice,
        "amount" | "line total" | "total" | "extended" | "extended amount" | "ext amount"
        | "ext. amount" | "line amount" => Col::LineTotal,
        _ => Col::Other,
    }
}

const TOTAL_ROWS: [&str; 8] = [
    "total",
    "total due",
    "amount due",
    "balance due",
    "grand total",
    "invoice total",
    "total amount",
    "total amount due",
];

fn is_subtotal_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    let l = l.trim();
    l.starts_with("subtotal") || l.starts_with("sub-total") || l.starts_with("sub total")
}

/// `tax` as a whole word anywhere in the label ("Sales Tax (8.25%)", "State tax", "Tax 8.25%").
fn is_tax_label(label: &str) -> bool {
    label
        .to_ascii_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|w| w == "tax" || w == "taxes")
}

/// A line that ends in something amount-like: its label text and the amount. Recognising an amount-like
/// trailing token and successfully parsing it are DIFFERENT questions: a malformed amount (`$4,50.00`) is still
/// recognised, carried as `Err(reason)` and kept visible, never dropped because it failed to parse.
/// `strong` = the token has a decimal point, a `$`, parentheses or a minus sign (hard to mistake for an
/// address or phone number); a bare whole number is weak.
struct Tail {
    label: String,
    amount: Result<Decimal, String>,
    strong: bool,
}

/// Could this whitespace-free token be an attempt at writing an amount? It needs a digit, must start like an
/// amount, may contain only amount characters (and letters, so `12O0.00` is caught), allows `-` only as a sign,
/// and needs a money marker (`.` `,` `$` `(`). Dates (`03/15/2026`), phone numbers (`713-555-0100`), percentages
/// (`8.25%`) and plain words are NOT amount-like.
fn amount_like(tok: &str) -> bool {
    let chars: Vec<char> = tok.chars().collect();
    if chars.is_empty() || chars.len() > 25 || !chars.iter().any(|c| c.is_ascii_digit()) {
        return false;
    }
    if !matches!(chars[0], '0'..='9' | '$' | '(' | '-' | '.') {
        return false;
    }
    for (i, c) in chars.iter().enumerate() {
        match c {
            '0'..='9' | 'a'..='z' | 'A'..='Z' | '.' | ',' | '$' | '(' | ')' => {}
            '-' if i == 0 || matches!(chars[i - 1], '(' | '$') => {}
            _ => return false,
        }
    }
    chars.iter().any(|c| matches!(c, '.' | ',' | '$' | '('))
}

/// `Some((amount-or-reason, strong))` when the token is an amount or an attempt at one.
fn trailing_amount(tok: &str) -> Option<(Result<Decimal, String>, bool)> {
    let strong =
        tok.contains('.') || tok.contains('$') || tok.starts_with('(') || tok.starts_with('-');
    match parse_amount(tok, true) {
        Ok((d, _)) => Some((Ok(d), strong)),
        Err(e) if amount_like(tok) => Some((Err(e), true)),
        Err(_) => None,
    }
}

fn amount_tail(segs: &[Seg]) -> Option<Tail> {
    let has_letters = |t: &str| t.chars().any(|c| c.is_alphabetic());
    if segs.len() >= 2 {
        let last = &segs[segs.len() - 1].text;
        let label = segs[..segs.len() - 1]
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        if !has_letters(&label) {
            return None;
        }
        if let Some((amount, strong)) = trailing_amount(last) {
            return Some(Tail {
                label,
                amount,
                strong,
            });
        }
        // A tax or subtotal row whose last column is not an amount at all ("Sales Tax    8.25%") must still
        // be seen: tax may never disappear.
        let first = &segs[0].text;
        if (is_tax_label(first) || is_subtotal_label(first))
            && last.chars().any(|c| c.is_ascii_digit())
        {
            return Some(Tail {
                label,
                amount: Err(format!("'{}' is not an amount", clip(last, 30))),
                strong: true,
            });
        }
        return None;
    }
    let toks: Vec<&str> = segs.first()?.text.split_whitespace().collect();
    if toks.len() < 2 {
        return None;
    }
    // single-spaced text: only a strong amount (or strong-looking attempt) counts; a bare trailing number is
    // far too often an address
    let last = toks[toks.len() - 1];
    let label = toks[..toks.len() - 1].join(" ");
    if let Some((amount, strong)) = trailing_amount(last).filter(|(_, strong)| *strong) {
        return has_letters(&label).then_some(Tail {
            label,
            amount,
            strong,
        });
    }
    // A short single-spaced tax or subtotal row whose last token is not an amount ("Sales Tax 8.25%") must
    // still be seen, exactly like the multi-column case above. The label is limited to a few words so that a
    // sentence that merely mentions tax is not swept in, and a non-tax label ("Discount 10%") never matches.
    if toks.len() - 1 <= 4
        && (is_tax_label(&label) || is_subtotal_label(&label))
        && last.chars().any(|c| c.is_ascii_digit())
    {
        return Some(Tail {
            label,
            amount: Err(format!("'{}' is not an amount", clip(last, 30))),
            strong: true,
        });
    }
    None
}

/// Tell the caller the text it parsed was cut short, so completeness can never be claimed.
pub fn mark_text_truncated(c: &mut InvoiceCandidates) {
    c.warnings.push(
        "the extracted text was truncated at the size limit, so lines and totals near the end of the document may be missing".to_string(),
    );
    c.checks.line_sum_complete = false;
    c.checks.completeness_basis = None;
    c.checks.incompleteness_reasons.push(
        "the extracted text was truncated, so charges near the end may be missing".to_string(),
    );
}

fn row_label(first: &Seg) -> String {
    first
        .text
        .to_ascii_lowercase()
        .trim_end_matches(':')
        .trim()
        .to_string()
}

struct Header {
    line_idx: usize, // 0-based
    segs: Vec<Seg>,
    kinds: Vec<Col>,
    problem: Option<String>,
}

fn find_header(lines: &[String]) -> Option<Header> {
    for (i, l) in lines.iter().enumerate() {
        let segs = segments(l);
        if segs.len() < 3 {
            continue;
        }
        let kinds: Vec<Col> = segs.iter().map(|s| col_kind(&s.text)).collect();
        let numeric = [Col::Quantity, Col::UnitPrice, Col::LineTotal]
            .iter()
            .filter(|k| kinds.contains(k))
            .count();
        if !(kinds.contains(&Col::Description) && numeric >= 2) {
            continue;
        }
        let mut problem = None;
        for k in [
            Col::Description,
            Col::FeeCode,
            Col::Quantity,
            Col::Unit,
            Col::UnitPrice,
            Col::LineTotal,
        ] {
            if kinds.iter().filter(|x| **x == k).count() > 1 {
                problem = Some(format!("the table header has more than one {k:?} column, so cells cannot be assigned reliably"));
            }
        }
        if problem.is_none() {
            let lacking: Vec<&str> = [
                (Col::Quantity, "quantity"),
                (Col::UnitPrice, "unit price"),
                (Col::LineTotal, "line total"),
            ]
            .iter()
            .filter(|(k, _)| !kinds.contains(k))
            .map(|(_, n)| *n)
            .collect();
            if !lacking.is_empty() {
                problem = Some(format!(
                    "the table header has no {} column, so lines cannot be read reliably",
                    lacking.join(" / ")
                ));
            }
        }
        return Some(Header {
            line_idx: i,
            segs,
            kinds,
            problem,
        });
    }
    None
}

/// Assigns each row cell to the header column it overlaps most. An empty cell therefore stays empty instead of
/// shifting the others left. A cell overlapping no column, two cells in one column, or a tie is an error: the
/// row is returned as unparsed rather than guessed.
fn assign_cells(h: &Header, row: &[Seg]) -> Result<Vec<Option<String>>, String> {
    let mut cells: Vec<Option<String>> = vec![None; h.segs.len()];
    for s in row {
        let overlaps: Vec<usize> = h
            .segs
            .iter()
            .map(|c| s.end.min(c.end).saturating_sub(s.start.max(c.start)))
            .collect();
        let best = *overlaps.iter().max().unwrap_or(&0);
        if best == 0 {
            return Err(format!(
                "'{}' does not line up with any column of the table header",
                clip(&s.text, 40)
            ));
        }
        let winners: Vec<usize> = overlaps
            .iter()
            .enumerate()
            .filter(|(_, o)| **o == best)
            .map(|(i, _)| i)
            .collect();
        if winners.len() != 1 {
            return Err(format!(
                "'{}' lines up with more than one column",
                clip(&s.text, 40)
            ));
        }
        let i = winners[0];
        if cells[i].is_some() {
            return Err(format!(
                "more than one piece of text falls in the '{}' column",
                h.segs[i].text
            ));
        }
        cells[i] = Some(s.text.clone());
    }
    Ok(cells)
}

fn amount_field(cell: Option<&String>, ev: &Evidence, allow_dollar: bool, what: &str) -> Field {
    let Some(raw) = cell else {
        return Field::missing(&format!("the {what} cell is empty"));
    };
    match parse_amount(raw, allow_dollar) {
        Ok((d, neg)) => {
            if neg {
                Field::uncertain(
                    Some(d.to_string()),
                    raw,
                    ev.clone(),
                    vec![format!("negative {what} (a credit?)")],
                    vec![],
                )
            } else {
                Field::extracted(d.to_string(), raw, ev.clone())
            }
        }
        Err(e) => Field::uncertain(None, raw, ev.clone(), vec![e], vec![]),
    }
}

fn parse_row(
    index: usize,
    line_no: usize,
    line: &str,
    h: &Header,
) -> Result<LineCandidate, String> {
    if let Some(p) = &h.problem {
        return Err(p.clone());
    }
    let row = segments(line);
    let cells = assign_cells(h, &row)?;
    let ev = evidence(line_no, line);
    let cell = |k: Col| -> Option<&String> {
        h.kinds
            .iter()
            .position(|x| *x == k)
            .and_then(|i| cells[i].as_ref())
    };
    let has_col = |k: Col| h.kinds.contains(&k);

    let description = match cell(Col::Description) {
        None => return Err("the description cell is empty".to_string()),
        Some(d) if d.chars().count() > 500 => Field::uncertain(
            Some(clip(d, 500)),
            d,
            ev.clone(),
            vec!["description is longer than 500 characters".to_string()],
            vec![],
        ),
        Some(d) => Field::extracted(d.trim().to_string(), d, ev.clone()),
    };

    // Fee code: ONLY from an explicit fee-code column. A blank or absent code is reported as such (and the
    // line must be acknowledged: a blank fee code is priced as a disposal line). It is never inferred.
    let mut blank_fee_code = true;
    let fee_code = if !has_col(Col::FeeCode) {
        Field::missing(
            "this table has no fee-code column; the fee code is not inferred from the description",
        )
    } else {
        match cell(Col::FeeCode) {
            None => Field::missing("the fee-code cell is blank"),
            Some(c)
                if matches!(
                    c.trim().to_ascii_lowercase().as_str(),
                    "-" | "--" | "n/a" | "na" | "none"
                ) =>
            {
                Field::missing("the fee-code cell has no code")
            }
            Some(c) => {
                blank_fee_code = false;
                let t = c.trim();
                let well_formed = t.chars().count() <= 50
                    && t.chars().all(|ch| {
                        ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '/' | '.')
                    });
                if well_formed {
                    Field::extracted(t.to_string(), c, ev.clone())
                } else {
                    Field::uncertain(
                        Some(clip(t, 50)),
                        c,
                        ev.clone(),
                        vec!["the fee code contains spaces or unusual characters".to_string()],
                        vec![],
                    )
                }
            }
        }
    };

    let quantity = amount_field(cell(Col::Quantity), &ev, false, "quantity");
    let unit_price = amount_field(cell(Col::UnitPrice), &ev, true, "unit price");
    let line_total = amount_field(cell(Col::LineTotal), &ev, true, "line total");
    let unit = if !has_col(Col::Unit) {
        Field::missing("this table has no unit column; the unit is not inferred")
    } else {
        match cell(Col::Unit) {
            None => Field::missing("the unit cell is empty"),
            Some(u) if u.chars().count() > 50 => Field::uncertain(
                Some(clip(u, 50)),
                u,
                ev.clone(),
                vec!["the unit is longer than 50 characters".to_string()],
                vec![],
            ),
            Some(u) => Field::extracted(u.trim().to_string(), u, ev.clone()),
        }
    };

    let mut line_total = line_total;
    let mut arithmetic = None;
    if let (Some(q), Some(p), Some(t)) = (&quantity.value, &unit_price.value, &line_total.value) {
        if let (Ok(q), Ok(p), Ok(t)) = (
            Decimal::from_str(q),
            Decimal::from_str(p),
            Decimal::from_str(t),
        ) {
            let computed = q * p;
            let diff = (t - computed).abs();
            let within = diff <= rounding_tolerance();
            if !within {
                line_total.add_uncertainty(format!(
                    "quantity {q} x unit price {p} = {computed}, but the line total is {t} (difference {diff}; tolerance 0.01)"
                ));
            }
            arithmetic = Some(Arithmetic {
                computed_total: computed.to_string(),
                difference: diff.to_string(),
                within_tolerance: within,
            });
        }
    }

    let fields = [
        &description,
        &quantity,
        &unit,
        &unit_price,
        &line_total,
        &fee_code,
    ];
    let status = if fields.iter().any(|f| f.status == Status::Uncertain) {
        Status::Uncertain
    } else if [&description, &quantity, &unit, &unit_price, &line_total]
        .iter()
        .any(|f| f.status == Status::Missing)
    {
        Status::Missing
    } else {
        Status::Extracted
    };
    Ok(LineCandidate {
        index,
        description,
        fee_code,
        quantity,
        unit,
        unit_price,
        line_total,
        blank_fee_code,
        arithmetic,
        status,
        evidence: ev,
    })
}

struct TableResult {
    lines: Vec<LineCandidate>,
    /// 1-based numbers of lines that are table structure or parsed rows (not scanned for labelled fields).
    structural: BTreeSet<usize>,
    /// (1-based line, reason) of in-table lines that could not be read as rows.
    unread: Vec<(usize, String)>,
    /// 1-based inclusive range of the detected table, if any.
    range: Option<(usize, usize)>,
    /// Subtotal rows (line, readable amount): accounted for when they equal the sum of the parsed lines.
    summaries: Vec<Summary>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SummaryKind {
    Subtotal,
    Tax,
}

/// A subtotal or tax row (anywhere in the document) with its readable amount.
struct Summary {
    line: usize,
    kind: SummaryKind,
    /// `Err(reason)` when the amount could not be read: the row is still reported.
    amount: Result<Decimal, String>,
}

fn parse_table(lines: &[String]) -> TableResult {
    let mut out = TableResult {
        lines: vec![],
        structural: BTreeSet::new(),
        unread: vec![],
        range: None,
        summaries: vec![],
    };
    let Some(h) = find_header(lines) else {
        return out;
    };
    out.structural.insert(h.line_idx + 1);
    let header_key: Vec<String> = h.segs.iter().map(|s| s.text.to_ascii_lowercase()).collect();
    let mut blank_run = 0;
    let mut j = h.line_idx + 1;
    while j < lines.len() {
        let l = &lines[j];
        if l.trim().is_empty() {
            blank_run += 1;
            if blank_run >= 2 {
                break;
            }
            j += 1;
            continue;
        }
        blank_run = 0;
        let segs = segments(l);
        let key: Vec<String> = segs.iter().map(|s| s.text.to_ascii_lowercase()).collect();
        if key == header_key {
            out.structural.insert(j + 1); // the same header repeated on a following page
            j += 1;
            continue;
        }
        let label = row_label(&segs[0]);
        if TOTAL_ROWS.contains(&label.as_str()) {
            break; // a totals row ends the table; it is read as the invoice total below
        }
        // A subtotal or tax row has few columns (label, optional rate, amount). A row with quantity, unit and
        // price cells that merely mentions tax in its description is a real line item and is parsed as one.
        if segs.len() <= 3 {
            if let Some(t) = amount_tail(&segs) {
                let kind = if is_subtotal_label(&t.label) {
                    Some(SummaryKind::Subtotal)
                } else if is_tax_label(&t.label) {
                    Some(SummaryKind::Tax)
                } else {
                    None
                };
                if let Some(kind) = kind {
                    out.summaries.push(Summary {
                        line: j + 1,
                        kind,
                        amount: t.amount,
                    });
                    j += 1;
                    continue;
                }
            }
        }
        match parse_row(out.lines.len(), j + 1, l, &h) {
            Ok(c) => {
                out.structural.insert(j + 1);
                out.lines.push(c);
            }
            Err(reason) => out.unread.push((j + 1, reason)),
        }
        j += 1;
    }
    out.range = Some((h.line_idx + 1, j.max(h.line_idx + 1)));
    out
}

// ---------------------------------------------------------------------------------------------- top level

pub fn parse_invoice_text(text: &str) -> InvoiceCandidates {
    let lines = normalize_lines(text);
    let mut warnings: Vec<String> = Vec::new();
    if lines.iter().all(|l| l.trim().is_empty()) {
        warnings.push("the extracted text is empty".to_string());
    }
    let mut table = parse_table(&lines);
    if table.range.is_none() {
        warnings.push("no line-item table (a header with Description and at least two of Qty / Unit Price / Amount) was recognised; every line must be entered by hand".to_string());
    }
    // Labelled fields are read from everything except the table's structure and the rows already parsed.
    let scan: Vec<String> = lines
        .iter()
        .enumerate()
        .map(|(i, l)| {
            if table.structural.contains(&(i + 1)) {
                String::new()
            } else {
                l.clone()
            }
        })
        .collect();
    let mut consumed: BTreeSet<usize> = BTreeSet::new();
    let mut take = |hits: Vec<Hit>| -> Vec<Hit> {
        consumed.extend(hits.iter().map(|h| h.line_no));
        hits
    };

    let number_hits = take(find_labeled(
        &scan,
        &[
            ("invoice number", false),
            ("invoice no", false),
            ("invoice #", false),
            ("invoice num", false),
            ("inv number", false),
            ("inv no", false),
            ("inv #", false),
            ("invoice", true),
        ],
        Pickup::AfterColon,
    ));
    let invoice_number = build_field(
        &number_hits,
        |h| Some(parse_invoice_number(&h.rest)),
        "no invoice number label was found",
    );

    let date_hits = take(find_labeled(
        &scan,
        &[
            ("invoice date", false),
            ("date of invoice", false),
            ("inv date", false),
            ("inv. date", false),
        ],
        Pickup::AfterColon,
    ));
    let mut invoice_date = if date_hits.is_empty() {
        let bare = take(find_labeled(&scan, &[("date", true)], Pickup::AfterColon));
        let mut f = build_field(
            &bare,
            |h| Some(parse_date_text(&h.rest)),
            "no invoice date label was found",
        );
        if f.value.is_some() {
            f.add_uncertainty(
                "the label is just 'Date', not 'Invoice Date'; confirm it is the invoice date"
                    .to_string(),
            );
        }
        f
    } else {
        build_field(
            &date_hits,
            |h| Some(parse_date_text(&h.rest)),
            "no invoice date label was found",
        )
    };
    if invoice_date.status == Status::Missing {
        invoice_date.reasons = vec!["no invoice date label was found".to_string()];
    }

    // The service date is NEVER taken from the invoice date or any other field.
    let service_hits = take(find_labeled(
        &scan,
        &[
            ("service date", false),
            ("date of service", false),
            ("date serviced", false),
            ("svc date", false),
            ("service dt", false),
        ],
        Pickup::AfterColon,
    ));
    let service_date = build_field(
        &service_hits,
        |h| Some(parse_date_text(&h.rest)),
        "no service date label was found; it is not defaulted from any other date",
    );

    let stream_hits = take(find_labeled(
        &scan,
        &[
            ("waste stream name", false),
            ("waste stream", false),
            ("wastestream", false),
        ],
        Pickup::AfterColon,
    ));
    let waste_stream = build_field(
        &stream_hits,
        |h| Some(parse_free_text(&h.rest, 100)),
        "no waste stream label was found",
    );

    let vendor_hits = take(find_labeled(
        &scan,
        &[
            ("vendor name", false),
            ("vendor", false),
            ("supplier", false),
            ("remit to", false),
        ],
        Pickup::AfterColon,
    ));
    let vendor_name = if vendor_hits.is_empty() {
        letterhead_vendor(&lines)
    } else {
        build_field(
            &vendor_hits,
            |h| Some(parse_free_text(&h.rest, 100)),
            "no vendor label was found",
        )
    };

    let total_hits = take(find_labeled(
        &scan,
        &[
            ("total amount due", false),
            ("total due", false),
            ("amount due", false),
            ("balance due", false),
            ("grand total", false),
            ("invoice total", false),
            ("total amount", false),
            ("total", false),
        ],
        Pickup::Always,
    ));
    let mut invoice_total = build_field(
        &total_hits,
        |h| parse_total_text(&h.rest, h.label == "total"),
        "no invoice total (Total / Amount Due / Balance Due ...) was found",
    );

    // Cross-checks between the lines and the invoice total.
    let line_totals: Vec<Option<Decimal>> = table
        .lines
        .iter()
        .map(|c| {
            c.line_total
                .value
                .as_deref()
                .and_then(|v| Decimal::from_str(v).ok())
        })
        .collect();
    let all_readable = !table.lines.is_empty() && line_totals.iter().all(|t| t.is_some());
    let sum: Option<Decimal> = if all_readable {
        Some(line_totals.iter().flatten().sum())
    } else {
        None
    };
    // Everything that is not a parsed row, a labelled field or a recognised total ends up either accounted for
    // or listed. Unreadable table rows first, then lines OUTSIDE the table that end in an amount.
    let mut unparsed: Vec<UnparsedLine> = table
        .unread
        .iter()
        .filter(|(n, _)| !consumed.contains(n))
        .map(|(n, why)| UnparsedLine {
            line: *n,
            text: clip(&lines[n - 1], 200),
            kind: UnparsedKind::UnreadableRow,
            reason: why.clone(),
        })
        .collect();
    let mut summaries: Vec<Summary> = std::mem::take(&mut table.summaries);
    for (i, l) in lines.iter().enumerate() {
        let n = i + 1;
        if l.trim().is_empty()
            || table.structural.contains(&n)
            || consumed.contains(&n)
            || table.range.is_some_and(|(a, b)| n >= a && n <= b)
        {
            continue;
        }
        let Some(t) = amount_tail(&segments(l)) else {
            continue;
        };
        let label = t.label.to_ascii_lowercase();
        let label = label.trim().trim_end_matches(':').trim();
        if TOTAL_ROWS.contains(&label) {
            continue; // an invoice-total row not read as the total above: it is a total, not a charge
        }
        if is_subtotal_label(label) || is_tax_label(label) {
            let kind = if is_subtotal_label(label) {
                SummaryKind::Subtotal
            } else {
                SummaryKind::Tax
            };
            summaries.push(Summary {
                line: n,
                kind,
                amount: t.amount,
            });
            continue;
        }
        let place = if table.range.is_some() {
            "is outside the detected table, so it is NOT included in the line sum"
        } else {
            "no table header was recognised, so it is NOT included in the line sum"
        };
        let (kind, why) = match (&t.amount, t.strong) {
            (Err(e), _) => (
                UnparsedKind::UnreadableAmount,
                format!("ends in something that looks like an amount but cannot be read ({e}); it may be a charge and {place}"),
            ),
            (Ok(_), true) => (UnparsedKind::ChargeLike, format!("ends in an amount and looks like a charge, but {place}")),
            (Ok(_), false) => (
                UnparsedKind::PossibleCharge,
                format!("ends in a whole number and may be a charge (or an address, phone or reference number); {place}"),
            ),
        };
        unparsed.push(UnparsedLine {
            line: n,
            text: clip(l, 200),
            kind,
            reason: why,
        });
    }
    // A subtotal is accepted ONLY against a complete line sum: every parsed line total readable and nothing
    // charge-like left over. Tax is never accepted; it stays listed so it cannot vanish or look reconciled.
    let sum_is_complete_basis = all_readable && unparsed.is_empty();
    let mut accepted_subtotals = 0usize;
    for sm in &summaries {
        let text = clip(&lines[sm.line - 1], 200);
        match (sm.kind, &sm.amount) {
            (SummaryKind::Tax, Ok(a)) => unparsed.push(UnparsedLine {
                line: sm.line,
                text,
                kind: UnparsedKind::Tax,
                reason: format!(
                    "tax row ({a}): tax is not a line item here and is NOT included in the line sum; if it is billed as a charge, add it as a line item yourself"
                ),
            }),
            (SummaryKind::Tax, Err(e)) => unparsed.push(UnparsedLine {
                line: sm.line,
                text,
                kind: UnparsedKind::Tax,
                reason: format!("tax row whose amount could not be read ({e}); tax is NOT included in the line sum and must be handled by a person"),
            }),
            (SummaryKind::Subtotal, Err(e)) => unparsed.push(UnparsedLine {
                line: sm.line,
                text,
                kind: UnparsedKind::Subtotal,
                reason: format!("subtotal row whose amount could not be read ({e}); it cannot be checked against the line sum"),
            }),
            (SummaryKind::Subtotal, Ok(amount)) => {
                let matches = sum.is_some_and(|total| (*amount - total).abs() <= rounding_tolerance());
                if sum_is_complete_basis && matches {
                    accepted_subtotals += 1;
                } else {
                    let why = match sum {
                        _ if !sum_is_complete_basis => format!(
                            "subtotal {amount} cannot be accepted: the parsed lines are incomplete (an unreadable line total, an unreadable row, or a charge-like line that is not in the sum)"
                        ),
                        Some(total) => format!("subtotal {amount} does not equal the sum of the parsed line totals {total}"),
                        None => format!("subtotal {amount} could not be checked because there is no complete line sum"),
                    };
                    unparsed.push(UnparsedLine { line: sm.line, text, kind: UnparsedKind::Subtotal, reason: why });
                }
            }
        }
    }
    unparsed.sort_by_key(|u| u.line);
    let unparsed_not_listed = unparsed.len().saturating_sub(MAX_UNPARSED_LISTED);
    let unparsed_total = unparsed.len();
    let blocking_charges = unparsed
        .iter()
        .filter(|u| {
            matches!(
                u.kind,
                UnparsedKind::UnreadableRow
                    | UnparsedKind::UnreadableAmount
                    | UnparsedKind::ChargeLike
                    | UnparsedKind::PossibleCharge
            )
        })
        .count();
    let tax_rows = unparsed
        .iter()
        .filter(|u| u.kind == UnparsedKind::Tax)
        .count();
    let rejected_subtotals = unparsed
        .iter()
        .filter(|u| u.kind == UnparsedKind::Subtotal)
        .count();
    unparsed.truncate(MAX_UNPARSED_LISTED);

    let mut total_matches_lines = None;
    if let (Some(sum), Some(Ok(total))) =
        (sum, invoice_total.value.as_deref().map(Decimal::from_str))
    {
        let matches = (total - sum).abs() <= rounding_tolerance();
        total_matches_lines = Some(matches);
        if !matches {
            let mut why = format!(
                "the invoice total {total} differs from the sum of the {} parsed line totals {sum} (difference {})",
                table.lines.len(),
                (total - sum).abs()
            );
            if unparsed_total > 0 {
                why.push_str(&format!(
                    "; {unparsed_total} unparsed line(s) exist and are not included"
                ));
            }
            invoice_total.add_uncertainty(why);
        }
    }
    // Completeness: positive evidence only (see `Checks::line_sum_complete`).
    let total_corroborates =
        total_matches_lines == Some(true) && invoice_total.alternatives.is_empty();
    let mut incomplete: Vec<String> = Vec::new();
    if table.range.is_none() {
        incomplete.push("no line-item table was recognised".to_string());
    } else if table.lines.is_empty() {
        incomplete.push("the table has no readable line".to_string());
    }
    let unreadable_totals = line_totals.iter().filter(|t| t.is_none()).count();
    if unreadable_totals > 0 {
        incomplete.push(format!(
            "{unreadable_totals} line total(s) could not be read"
        ));
    }
    if blocking_charges > 0 {
        incomplete.push(format!(
            "{blocking_charges} unparsed line(s) that may be charges are not included in the sum"
        ));
    }
    if tax_rows > 0 {
        incomplete.push(format!(
            "{tax_rows} tax row(s) are not represented as line items"
        ));
    }
    if rejected_subtotals > 0 {
        incomplete.push(format!(
            "{rejected_subtotals} subtotal row(s) could not be accepted against the line sum"
        ));
    }
    if !total_corroborates && accepted_subtotals == 0 {
        incomplete.push(match total_matches_lines {
            Some(false) => "the invoice total does not equal the sum of the lines".to_string(),
            _ if !invoice_total.alternatives.is_empty() => "several different invoice totals were found, so none can corroborate the line sum".to_string(),
            _ => "neither a readable invoice total nor a subtotal confirms that the lines are the whole invoice".to_string(),
        });
    }
    let line_sum_complete = incomplete.is_empty();
    let completeness_basis =
        line_sum_complete.then(|| match (total_corroborates, accepted_subtotals > 0) {
            (true, true) => {
                "the invoice total and a subtotal both equal the sum of the parsed line totals"
                    .to_string()
            }
            (true, false) => {
                "the invoice total equals the sum of the parsed line totals".to_string()
            }
            _ => "a subtotal equals the sum of the parsed line totals".to_string(),
        });
    if table.lines.len() > MAX_LINE_CANDIDATES {
        warnings.push(format!(
            "{} line candidates were found; the invoice API accepts at most {MAX_LINE_CANDIDATES} lines",
            table.lines.len()
        ));
    }

    let mut acks: Vec<String> = Vec::new();
    for (name, f) in [
        ("vendor_name", &vendor_name),
        ("invoice_number", &invoice_number),
        ("invoice_date", &invoice_date),
        ("service_date", &service_date),
        ("waste_stream", &waste_stream),
        ("invoice_total", &invoice_total),
    ] {
        if f.status == Status::Uncertain {
            acks.push(name.to_string());
        }
    }
    for c in &table.lines {
        for (name, f) in [
            ("description", &c.description),
            ("quantity", &c.quantity),
            ("unit", &c.unit),
            ("unit_price", &c.unit_price),
            ("line_total", &c.line_total),
        ] {
            if f.status == Status::Uncertain {
                acks.push(format!("lines[{}].{name}", c.index));
            }
        }
        if c.blank_fee_code || c.fee_code.status == Status::Uncertain {
            acks.push(format!("lines[{}].fee_code", c.index));
        }
    }

    InvoiceCandidates {
        parser_version: PARSER_VERSION,
        vendor_name,
        invoice_number,
        invoice_date,
        service_date,
        waste_stream,
        invoice_total,
        checks: Checks {
            parsed_line_count: table.lines.len(),
            line_total_sum: sum.map(|s| s.to_string()),
            line_sum_complete,
            completeness_basis,
            incompleteness_reasons: incomplete,
            total_matches_lines,
        },
        lines: table.lines,
        unparsed_lines: unparsed,
        unparsed_not_listed,
        acknowledgments_required: acks,
        warnings,
    }
}

/// With no vendor label, the first line of the document (the letterhead) is offered as a candidate, always
/// flagged `uncertain`: a letterhead is a convention, not a label.
fn letterhead_vendor(lines: &[String]) -> Field {
    let Some((idx, line)) = lines.iter().enumerate().find(|(_, l)| !l.trim().is_empty()) else {
        return Field::missing("the document has no text");
    };
    let segs = segments(line);
    let first = segs.first().map(|s| s.text.clone()).unwrap_or_default();
    let lower = first.to_ascii_lowercase();
    let letters = first.chars().filter(|c| c.is_alphabetic()).count();
    let digits = first.chars().filter(|c| c.is_ascii_digit()).count();
    let banned = [
        "invoice",
        "statement",
        "page ",
        "bill to",
        "ship to",
        "date",
        "remit",
        "total",
    ];
    if letters >= 3
        && digits <= letters / 2
        && first.chars().count() <= 80
        && !banned.iter().any(|b| lower.contains(b))
    {
        Field::uncertain(
            Some(first.clone()),
            &first,
            evidence(idx + 1, line),
            vec!["taken from the first line of the document (the letterhead); confirm that it is the vendor".to_string()],
            vec![],
        )
    } else {
        Field::missing(
            "no vendor label was found and the first line does not look like a company name",
        )
    }
}

#[cfg(test)]
mod tests {
    //! SYNTHETIC layouts only. These tests prove the rules behave as specified; they say nothing about how
    //! accurate the parser is on any real vendor's invoices.
    use super::*;

    /// One table row laid out in fixed-width columns, as `pdftotext -layout` lays out a PDF table.
    fn row(d: &str, f: &str, q: &str, u: &str, p: &str, a: &str) -> String {
        format!("{d:<28}{f:<13}{q:>6}  {u:<9}{p:>13}{a:>13}")
    }
    fn header() -> String {
        row(
            "Description",
            "Fee Code",
            "Qty",
            "Unit",
            "Unit Price",
            "Amount",
        )
    }
    fn invoice(top: &[&str], rows: &[String], bottom: &[&str]) -> String {
        let mut v: Vec<String> = top.iter().map(|s| s.to_string()).collect();
        v.push(String::new());
        v.push(header());
        v.extend(rows.iter().cloned());
        v.push(String::new());
        v.extend(bottom.iter().map(|s| s.to_string()));
        v.join("\n")
    }
    const TOP: [&str; 5] = [
        "Evergreen Environmental Services, Inc.                       INVOICE",
        "1200 Industrial Way, Houston, TX 77001",
        "Invoice Number: INV-2026-0451              Invoice Date: 03/15/2026",
        "Service Date:   03/20/2026                 Terms: Net 30",
        "Waste Stream:   F003 Spent Solvents",
    ];
    fn rows_ok() -> Vec<String> {
        vec![
            row("Drum disposal", "", "10", "drum", "400.00", "4,000.00"),
            row(
                "Transportation",
                "TRANSPORT",
                "1",
                "flat",
                "500.00",
                "500.00",
            ),
        ]
    }
    const BOTTOM_OK: [&str; 2] = [
        "                                          Subtotal                        4,500.00",
        "                                          Total Due                       4,500.00",
    ];
    fn clean() -> InvoiceCandidates {
        parse_invoice_text(&invoice(&TOP, &rows_ok(), &BOTTOM_OK))
    }
    fn parse_with(top: &[&str], rows: &[String], bottom: &[&str]) -> InvoiceCandidates {
        parse_invoice_text(&invoice(top, rows, bottom))
    }

    #[test]
    fn a_clean_invoice_yields_exact_candidates() {
        let c = clean();
        assert_eq!(c.parser_version, "invoice-rules-3");
        assert_eq!(c.invoice_number.value.as_deref(), Some("INV-2026-0451"));
        assert_eq!(c.invoice_number.status, Status::Extracted);
        assert_eq!(c.invoice_date.value.as_deref(), Some("2026-03-15"));
        assert_eq!(c.invoice_date.status, Status::Extracted);
        assert_eq!(c.service_date.value.as_deref(), Some("2026-03-20"));
        assert_eq!(c.waste_stream.value.as_deref(), Some("F003 Spent Solvents"));
        assert_eq!(c.invoice_total.value.as_deref(), Some("4500.00"));
        assert_eq!(c.invoice_total.status, Status::Extracted);
        assert_eq!(c.lines.len(), 2);
        let l1 = &c.lines[1];
        assert_eq!(
            (
                l1.description.value.as_deref(),
                l1.fee_code.value.as_deref(),
                l1.quantity.value.as_deref(),
                l1.unit.value.as_deref(),
                l1.unit_price.value.as_deref(),
                l1.line_total.value.as_deref()
            ),
            (
                Some("Transportation"),
                Some("TRANSPORT"),
                Some("1"),
                Some("flat"),
                Some("500.00"),
                Some("500.00")
            )
        );
        assert_eq!(l1.status, Status::Extracted);
        assert!(!l1.blank_fee_code);
        assert!(c.unparsed_lines.is_empty(), "{:?}", c.unparsed_lines);
        assert_eq!(c.checks.line_total_sum.as_deref(), Some("4500.00"));
        assert!(c.checks.line_sum_complete);
        assert_eq!(c.checks.total_matches_lines, Some(true));
        assert_eq!(
            c.invoice_total.evidence.as_ref().unwrap().text,
            "Total Due                       4,500.00"
        );
    }

    #[test]
    fn only_the_letterhead_vendor_and_the_blank_fee_code_line_need_acknowledgment_on_a_clean_invoice(
    ) {
        let c = clean();
        assert_eq!(
            c.vendor_name.value.as_deref(),
            Some("Evergreen Environmental Services, Inc.")
        );
        assert_eq!(
            c.vendor_name.status,
            Status::Uncertain,
            "a letterhead is never presented as certain"
        );
        assert_eq!(
            c.acknowledgments_required,
            vec!["vendor_name", "lines[0].fee_code"]
        );
    }

    // ---- vendor ----
    #[test]
    fn an_explicit_vendor_label_is_extracted_and_a_missing_one_falls_back_to_the_flagged_letterhead(
    ) {
        let top = ["Vendor: Acme Hazmat LLC", "Invoice Number: 7"];
        let c = parse_with(&top, &rows_ok(), &BOTTOM_OK);
        assert_eq!(
            (c.vendor_name.value.as_deref(), c.vendor_name.status),
            (Some("Acme Hazmat LLC"), Status::Extracted)
        );
        let none = parse_invoice_text("INVOICE\nInvoice Number: 7");
        assert_eq!(
            none.vendor_name.status,
            Status::Missing,
            "'INVOICE' is not a company name"
        );
        assert_eq!(none.vendor_name.value, None);
    }

    // ---- multiple matches ----
    #[test]
    fn conflicting_invoice_numbers_are_flagged_with_every_alternative_and_never_resolved_silently()
    {
        let top = ["Invoice Number: INV-1", "Invoice No: INV-2"];
        let c = parse_with(&top, &rows_ok(), &BOTTOM_OK);
        assert_eq!(c.invoice_number.status, Status::Uncertain);
        assert_eq!(c.invoice_number.alternatives, vec!["INV-1", "INV-2"]);
        assert!(c
            .invoice_number
            .reasons
            .iter()
            .any(|r| r.contains("2 different values")));
        assert!(c
            .acknowledgments_required
            .contains(&"invoice_number".to_string()));
    }

    #[test]
    fn the_same_value_repeated_in_a_header_and_a_footer_is_not_a_conflict() {
        let top = ["Invoice Number: INV-9", "Invoice #: INV-9"];
        let c = parse_with(&top, &rows_ok(), &BOTTOM_OK);
        assert_eq!(
            (c.invoice_number.value.as_deref(), c.invoice_number.status),
            (Some("INV-9"), Status::Extracted)
        );
    }

    #[test]
    fn conflicting_totals_are_flagged_and_equal_totals_in_different_formats_are_not() {
        let bottom_conflict = [
            "                                          Total Due                       4,500.00",
            "                                          Amount Due                      4,000.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom_conflict);
        assert_eq!(c.invoice_total.status, Status::Uncertain);
        assert_eq!(c.invoice_total.alternatives, vec!["4500.00", "4000.00"]);
        let bottom_same = [
            "                                          Total Due                       $4,500.00",
            "                                          Balance Due                     4500.0",
        ];
        let d = parse_with(&TOP, &rows_ok(), &bottom_same);
        assert_eq!(
            (d.invoice_total.value.as_deref(), d.invoice_total.status),
            (Some("4500.00"), Status::Extracted)
        );
    }

    #[test]
    fn conflicting_dates_are_flagged() {
        let top = ["Invoice Date: 03/15/2026", "Invoice Date: 03/16/2026"];
        let c = parse_with(&top, &rows_ok(), &BOTTOM_OK);
        assert_eq!(c.invoice_date.status, Status::Uncertain);
        assert_eq!(
            c.invoice_date.alternatives,
            vec!["2026-03-15", "2026-03-16"]
        );
    }

    // ---- dates ----
    fn date_of(s: &str) -> Field {
        let line = format!("Invoice Date: {s}");
        parse_with(&[line.as_str()], &rows_ok(), &BOTTOM_OK).invoice_date
    }

    #[test]
    fn ambiguous_numeric_dates_are_uncertain_and_carry_both_readings() {
        let f = date_of("03/09/2026");
        assert_eq!(f.status, Status::Uncertain);
        assert_eq!(f.value.as_deref(), Some("2026-03-09"));
        assert_eq!(f.alternatives, vec!["2026-03-09", "2026-09-03"]);
        assert!(f.reasons.iter().any(|r| r.contains("ambiguous")));
        // equal day and month is not ambiguous
        assert_eq!(date_of("03/03/2026").status, Status::Extracted);
    }

    #[test]
    fn unambiguous_dates_are_extracted_in_each_supported_format() {
        for (raw, want) in [
            ("03/15/2026", "2026-03-15"),
            ("2026-03-09", "2026-03-09"),
            ("2026/03/09", "2026-03-09"),
            ("Mar 9, 2026", "2026-03-09"),
            ("March 9 2026", "2026-03-09"),
            ("9 Mar 2026", "2026-03-09"),
            ("09-Mar-2026", "2026-03-09"),
            ("12/31/2026", "2026-12-31"),
        ] {
            let f = date_of(raw);
            assert_eq!(
                (f.value.as_deref(), f.status),
                (Some(want), Status::Extracted),
                "{raw}"
            );
        }
    }

    #[test]
    fn day_first_and_two_digit_year_readings_are_flagged() {
        let f = date_of("15/03/2026");
        assert_eq!(
            (f.value.as_deref(), f.status),
            (Some("2026-03-15"), Status::Uncertain)
        );
        let g = date_of("03/15/26");
        assert_eq!(
            (g.value.as_deref(), g.status),
            (Some("2026-03-15"), Status::Uncertain)
        );
        assert!(g.reasons.iter().any(|r| r.contains("two-digit year")));
    }

    #[test]
    fn invalid_or_implausible_dates_are_unreadable_not_repaired() {
        for raw in [
            "02/30/2026",
            "13/45/2026",
            "03/09/1850",
            "00/10/2026",
            "TBD",
            "March 2026",
            "2026-13-01",
        ] {
            let f = date_of(raw);
            assert_eq!(f.status, Status::Uncertain, "{raw}");
            assert_eq!(f.value, None, "{raw} must not produce a value");
            assert!(f.raw.is_some() && !f.reasons.is_empty(), "{raw}");
        }
    }

    #[test]
    fn a_bare_date_label_is_only_a_flagged_fallback_for_the_invoice_date() {
        let c = parse_invoice_text("Date: 03/15/2026\n");
        assert_eq!(
            (c.invoice_date.value.as_deref(), c.invoice_date.status),
            (Some("2026-03-15"), Status::Uncertain)
        );
        assert!(c
            .invoice_date
            .reasons
            .iter()
            .any(|r| r.contains("just 'Date'")));
        let both = parse_invoice_text("Date: 01/02/2026\nInvoice Date: 03/15/2026\n");
        assert_eq!(
            (both.invoice_date.value.as_deref(), both.invoice_date.status),
            (Some("2026-03-15"), Status::Extracted)
        );
    }

    #[test]
    fn the_service_date_is_never_defaulted_from_the_invoice_date() {
        let top = ["Invoice Number: 1", "Invoice Date: 03/15/2026"];
        let c = parse_with(&top, &rows_ok(), &BOTTOM_OK);
        assert_eq!(c.service_date.status, Status::Missing);
        assert_eq!(c.service_date.value, None);
        assert!(c.service_date.reasons[0].contains("not defaulted"));
    }

    // ---- malformed amounts ----
    #[test]
    fn malformed_amounts_are_unreadable_and_never_repaired() {
        for bad in [
            "4,50.00",
            "12O0.00",
            "4.500,00",
            "1.23456",
            "4,500.",
            ".50",
            "1,2345.00",
            "--5",
            "4 500.00",
        ] {
            let rows = vec![row("Drum disposal", "DISP", "10", "drum", bad, "4,000.00")];
            let c = parse_with(&TOP, &rows, &BOTTOM_OK);
            if c.lines.is_empty() {
                // a value that splits into two columns cannot be placed: it must be listed, not dropped
                assert!(
                    c.unparsed_lines
                        .iter()
                        .any(|u| u.text.contains("Drum disposal")),
                    "{bad}: {:?}",
                    c.unparsed_lines
                );
                continue;
            }
            let f = &c.lines[0].unit_price;
            assert_eq!(f.status, Status::Uncertain, "{bad}");
            assert_eq!(f.value, None, "{bad} must not produce a value");
            assert!(
                c.acknowledgments_required
                    .contains(&"lines[0].unit_price".to_string()),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_malformed_invoice_total_is_unreadable_with_no_value() {
        let bottom =
            ["                                          Total Due                       4,50.00"];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert_eq!(c.invoice_total.status, Status::Uncertain);
        assert_eq!(c.invoice_total.value, None);
        assert_eq!(
            c.checks.total_matches_lines, None,
            "no comparison against an unreadable total"
        );
    }

    #[test]
    fn negative_amounts_are_flagged_as_possible_credits() {
        for neg in ["-450.00", "(450.00)", "-$450.00"] {
            let rows = vec![row("Credit", "CR", "1", "flat", neg, neg)];
            let c = parse_with(&TOP, &rows, &BOTTOM_OK);
            assert_eq!(
                c.lines[0].unit_price.value.as_deref(),
                Some("-450.00"),
                "{neg}"
            );
            assert_eq!(c.lines[0].unit_price.status, Status::Uncertain, "{neg}");
        }
    }

    #[test]
    fn money_stays_exact_decimal_text() {
        let rows = vec![row("Odd", "X", "0.3", "kg", "0.1", "0.03")];
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        assert_eq!(
            c.lines[0].arithmetic.as_ref().unwrap().computed_total,
            "0.03"
        );
        assert!(c.lines[0].arithmetic.as_ref().unwrap().within_tolerance);
        let four = vec![row("Four places", "X", "3", "ea", "3.3333", "9.9999")];
        let l = &parse_with(&TOP, &four, &BOTTOM_OK).lines[0].clone();
        assert_eq!(
            (l.unit_price.value.as_deref(), l.line_total.value.as_deref()),
            (Some("3.3333"), Some("9.9999"))
        );
        assert!(l.arithmetic.as_ref().unwrap().within_tolerance);
        // bounds of the invoice API: at most 4 decimals, below 10^10
        assert_eq!(
            parse_amount("9999999999.9999", true).unwrap().0.to_string(),
            "9999999999.9999"
        );
        assert!(parse_amount("10000000000", true).is_err());
        assert!(parse_amount("1.00001", true).is_err());
    }

    // ---- fee codes ----
    #[test]
    fn fee_codes_are_never_inferred_from_a_description() {
        let rows = vec![
            row("Fuel Surcharge", "", "1", "flat", "25.00", "25.00"),
            row("TRANSPORT fee", "", "1", "flat", "500.00", "500.00"),
        ];
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        for l in &c.lines {
            assert_eq!(l.fee_code.value, None);
            assert_eq!(l.fee_code.status, Status::Missing);
            assert!(l.blank_fee_code);
        }
        assert!(c
            .acknowledgments_required
            .contains(&"lines[0].fee_code".to_string()));
        assert!(c
            .acknowledgments_required
            .contains(&"lines[1].fee_code".to_string()));
    }

    #[test]
    fn a_table_without_a_fee_code_column_blanks_every_line_and_says_why() {
        let hdr = format!(
            "{:<28}{:>6}  {:<9}{:>13}{:>13}",
            "Description", "Qty", "Unit", "Unit Price", "Amount"
        );
        let r = |d: &str, q: &str, u: &str, p: &str, a: &str| {
            format!("{d:<28}{q:>6}  {u:<9}{p:>13}{a:>13}")
        };
        let text = [
            TOP.join("\n"),
            String::new(),
            hdr,
            r("Drum disposal", "10", "drum", "400.00", "4,000.00"),
            r("Transportation", "1", "flat", "500.00", "500.00"),
            String::new(),
            BOTTOM_OK.join("\n"),
        ]
        .join("\n");
        let c = parse_invoice_text(&text);
        assert_eq!(c.lines.len(), 2);
        assert!(c
            .lines
            .iter()
            .all(|l| l.blank_fee_code && l.fee_code.status == Status::Missing));
        assert!(c.lines[0].fee_code.reasons[0].contains("no fee-code column"));
        assert!(c
            .acknowledgments_required
            .contains(&"lines[1].fee_code".to_string()));
    }

    #[test]
    fn placeholder_and_odd_fee_code_cells() {
        let rows = vec![
            row("A", "-", "1", "ea", "1.00", "1.00"),
            row("B", "N/A", "1", "ea", "1.00", "1.00"),
            row("C", "FUEL SURCH", "1", "ea", "1.00", "1.00"),
        ];
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        assert!(c.lines[0].blank_fee_code && c.lines[1].blank_fee_code);
        assert!(!c.lines[2].blank_fee_code);
        assert_eq!(
            (
                c.lines[2].fee_code.value.as_deref(),
                c.lines[2].fee_code.status
            ),
            (Some("FUEL SURCH"), Status::Uncertain)
        );
        assert!(c
            .acknowledgments_required
            .contains(&"lines[2].fee_code".to_string()));
    }

    #[test]
    fn a_missing_unit_column_leaves_the_unit_missing() {
        let hdr = format!(
            "{:<28}{:<13}{:>6}{:>13}{:>13}",
            "Description", "Fee Code", "Qty", "Unit Price", "Amount"
        );
        let r = format!(
            "{:<28}{:<13}{:>6}{:>13}{:>13}",
            "Drum disposal", "DISP", "10", "400.00", "4,000.00"
        );
        let c = parse_invoice_text(&[TOP.join("\n"), String::new(), hdr, r].join("\n"));
        assert_eq!(c.lines[0].unit.status, Status::Missing);
        assert_eq!(c.lines[0].unit.value, None);
        assert_eq!(c.lines[0].status, Status::Missing);
    }

    // ---- arithmetic ----
    #[test]
    fn a_wrong_line_total_is_flagged_with_the_arithmetic() {
        let rows = vec![row(
            "Drum disposal",
            "DISP",
            "10",
            "drum",
            "400.00",
            "4,100.00",
        )];
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        let l = &c.lines[0];
        assert_eq!(l.line_total.status, Status::Uncertain);
        assert_eq!(
            l.line_total.value.as_deref(),
            Some("4100.00"),
            "the document's number is kept, not corrected"
        );
        let a = l.arithmetic.as_ref().unwrap();
        assert_eq!(
            (
                a.computed_total.as_str(),
                a.difference.as_str(),
                a.within_tolerance
            ),
            ("4000.00", "100.00", false)
        );
        assert!(c
            .acknowledgments_required
            .contains(&"lines[0].line_total".to_string()));
    }

    #[test]
    fn rounding_within_one_cent_is_not_flagged_but_two_cents_is() {
        let ok = parse_with(
            &TOP,
            &[row("A", "X", "3", "ea", "0.333", "1.00")],
            &BOTTOM_OK,
        );
        assert_eq!(ok.lines[0].line_total.status, Status::Extracted);
        let off = parse_with(
            &TOP,
            &[row("A", "X", "3", "ea", "0.333", "1.02")],
            &BOTTOM_OK,
        );
        assert_eq!(off.lines[0].line_total.status, Status::Uncertain);
    }

    #[test]
    fn a_total_that_does_not_equal_the_lines_is_flagged_even_when_lines_were_left_unparsed() {
        let bottom = [
            "                                          Total Due                       4,600.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert_eq!(c.invoice_total.status, Status::Uncertain);
        assert_eq!(c.checks.total_matches_lines, Some(false));
        assert!(c
            .invoice_total
            .reasons
            .iter()
            .any(|r| r.contains("differs from the sum")));
        // with a line the parser could not read, the sum is flagged as incomplete
        let mut rows = rows_ok();
        rows.push("   wrapped continuation of the description above".to_string());
        let d = parse_with(&TOP, &rows, &BOTTOM_OK);
        // the unreadable row, and the subtotal, which is no longer accepted against an incomplete line sum
        let kinds: Vec<UnparsedKind> = d.unparsed_lines.iter().map(|u| u.kind).collect();
        assert_eq!(
            kinds,
            vec![UnparsedKind::UnreadableRow, UnparsedKind::Subtotal]
        );
        assert!(!d.checks.line_sum_complete);
    }

    // ---- unparsed lines ----
    #[test]
    fn unreadable_rows_are_preserved_with_a_reason_and_never_dropped() {
        let mut rows = rows_ok();
        rows.insert(
            1,
            "   (wrapped) continuation of the previous description".to_string(),
        );
        rows.push(format!(
            "{:<28}{:<13}{:>6}  {:<9}",
            "Orphan description", "X", "2", "ea"
        ));
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        // the partly blank row is kept as a candidate whose missing price and total must be entered by hand
        assert_eq!(c.lines.len(), 3);
        let partial = &c.lines[2];
        assert_eq!(
            (
                partial.unit_price.status,
                partial.line_total.status,
                partial.status
            ),
            (Status::Missing, Status::Missing, Status::Missing)
        );
        assert_eq!(
            (
                partial.unit_price.value.as_deref(),
                partial.line_total.value.as_deref()
            ),
            (None, None)
        );
        let texts: Vec<&str> = c.unparsed_lines.iter().map(|u| u.text.as_str()).collect();
        assert!(texts.iter().any(|t| t.contains("wrapped")), "{texts:?}");
        assert!(c.unparsed_lines.iter().all(|u| !u.reason.is_empty()));
        assert!(
            c.unparsed_lines.windows(2).all(|w| w[0].line < w[1].line),
            "ordered by line"
        );
    }

    #[test]
    fn tax_rows_are_listed_and_a_subtotal_is_only_accounted_for_when_it_matches() {
        let bottom = [
            "                                          Subtotal                        4,500.00",
            "                                          Sales Tax                         360.00",
            "                                          Total Due                       4,860.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert_eq!(c.unparsed_lines.len(), 1, "{:?}", c.unparsed_lines);
        assert!(c.unparsed_lines[0].text.contains("Sales Tax"));
        assert!(
            c.invoice_total
                .reasons
                .iter()
                .any(|r| r.contains("differs from the sum")),
            "tax makes the total differ, visibly"
        );
        let bad_sub = [
            "                                          Subtotal                        4,400.00",
            "                                          Total Due                       4,500.00",
        ];
        let d = parse_with(&TOP, &rows_ok(), &bad_sub);
        assert!(d
            .unparsed_lines
            .iter()
            .any(|u| u.reason.contains("does not equal the sum")));
    }

    #[test]
    fn amount_bearing_lines_outside_the_table_or_with_no_table_are_listed() {
        let mut v = invoice(&TOP, &rows_ok(), &BOTTOM_OK);
        v.push_str("\n\n\nFuel surcharge adjustment             1       25.00\n");
        let c = parse_invoice_text(&v);
        assert!(c
            .unparsed_lines
            .iter()
            .any(|u| u.text.contains("Fuel surcharge")
                && u.reason.contains("outside the detected table")));
        let none = parse_invoice_text(
            "Acme Co\nInvoice Number: 5\nDrum disposal     10     400.00     4,000.00\n",
        );
        assert!(none.lines.is_empty());
        assert!(none
            .warnings
            .iter()
            .any(|w| w.contains("no line-item table")));
        assert!(none
            .unparsed_lines
            .iter()
            .any(|u| u.text.contains("Drum disposal") && u.reason.contains("no table header")));
    }

    #[test]
    fn every_amount_bearing_line_is_either_parsed_used_as_evidence_or_listed() {
        let mut rows = rows_ok();
        rows.insert(1, "   continuation line".to_string());
        rows.push(row("Weird", "X", "ten", "ea", "1.00", "1.00"));
        let text = invoice(&TOP, &rows, &BOTTOM_OK);
        let c = parse_invoice_text(&text);
        let accounted: BTreeSet<usize> = c
            .lines
            .iter()
            .map(|l| l.evidence.line)
            .chain(c.unparsed_lines.iter().map(|u| u.line))
            .chain(
                [
                    &c.invoice_number,
                    &c.invoice_date,
                    &c.service_date,
                    &c.waste_stream,
                    &c.invoice_total,
                ]
                .iter()
                .filter_map(|f| f.evidence.as_ref().map(|e| e.line)),
            )
            .collect();
        for (i, l) in text.lines().enumerate() {
            let has_amount = l
                .split_whitespace()
                .any(|t| t.contains('.') && parse_amount(t, true).is_ok());
            let is_subtotal = l.trim_start().starts_with("Subtotal");
            if has_amount && !is_subtotal && !l.contains("Description") {
                assert!(
                    accounted.contains(&(i + 1)),
                    "line {} dropped silently: {l:?}",
                    i + 1
                );
            }
        }
    }

    #[test]
    fn a_header_that_cannot_be_trusted_sends_every_row_to_unparsed() {
        let dup = format!(
            "{:<28}{:>6}{:>10}{:>13}{:>13}",
            "Description", "Qty", "Qty", "Unit Price", "Amount"
        );
        let c = parse_invoice_text(
            &[
                dup,
                format!(
                    "{:<28}{:>6}{:>10}{:>13}{:>13}",
                    "Drum", "1", "2", "3.00", "3.00"
                ),
            ]
            .join("\n"),
        );
        assert!(c.lines.is_empty());
        assert!(c.unparsed_lines[0].reason.contains("more than one"));
        let lacking = format!("{:<28}{:>6}{:>13}", "Description", "Qty", "Amount");
        let d = parse_invoice_text(
            &[lacking, format!("{:<28}{:>6}{:>13}", "Drum", "1", "3.00")].join("\n"),
        );
        assert!(d.lines.is_empty());
        assert!(d.unparsed_lines[0].reason.contains("no unit price"));
    }

    #[test]
    fn a_header_repeated_on_a_later_page_is_not_treated_as_data() {
        let mut rows = rows_ok();
        rows.insert(1, header());
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        assert_eq!(c.lines.len(), 2);
        assert!(c.unparsed_lines.is_empty(), "{:?}", c.unparsed_lines);
    }

    #[test]
    fn the_unparsed_list_is_capped_with_an_explicit_count_of_what_was_not_listed() {
        let text: String = (0..620)
            .map(|i| format!("Item {i}      1      {i}.50\n"))
            .collect();
        let c = parse_invoice_text(&text);
        assert_eq!(c.unparsed_lines.len(), 500);
        assert_eq!(c.unparsed_not_listed, 120);
    }

    // ---- determinism and robustness ----
    #[test]
    fn output_is_deterministic_and_versioned() {
        let t = invoice(&TOP, &rows_ok(), &BOTTOM_OK);
        let a = serde_json::to_string(&parse_invoice_text(&t)).unwrap();
        for _ in 0..5 {
            assert_eq!(a, serde_json::to_string(&parse_invoice_text(&t)).unwrap());
        }
        assert!(a.contains("\"parser_version\":\"invoice-rules-3\""));
        assert!(
            a.contains("\"status\":\"extracted\"")
                && a.contains("\"status\":\"uncertain\"")
                && a.contains("\"status\":\"missing\"")
        );
        assert!(
            !a.contains("confidence"),
            "no confidence score is ever reported"
        );
        // CRLF, form feeds and tabs do not change the result
        let noisy = t.replace('\n', "\r\n").replacen("Terms", "\u{0c}Terms", 1);
        assert_eq!(
            parse_invoice_text(&noisy).invoice_total.value,
            parse_invoice_text(&t).invoice_total.value
        );
    }

    #[test]
    fn empty_garbage_and_hostile_text_never_panics_and_reports_everything_missing() {
        for t in [
            "",
            "   \n\n",
            "\u{0c}",
            "just some prose with no structure at all.",
            "Invoice Number:",
            "Total",
            "Total Due:",
            "(\u{e9})",
            "Invoice Date: \u{1F4A5}\u{1F4A5}",
            &"x".repeat(200_000),
            &"A  ".repeat(50_000),
            "Invoice #\u{a0}\u{a0}\u{a0}77",
            "Date: -",
            "Total: (",
            "Total: $",
            "Invoice Date: 13/13/13",
            "Service Date: 99-99-9999",
        ] {
            let c = parse_invoice_text(t);
            assert_eq!(c.parser_version, PARSER_VERSION);
            assert!(c.lines.is_empty());
        }
        let c = parse_invoice_text("");
        assert!(c.warnings.iter().any(|w| w.contains("empty")));
        for f in [
            &c.invoice_number,
            &c.invoice_date,
            &c.service_date,
            &c.waste_stream,
            &c.invoice_total,
            &c.vendor_name,
        ] {
            assert_eq!((f.status, f.value.as_deref()), (Status::Missing, None));
        }
        assert!(
            c.acknowledgments_required.is_empty(),
            "missing fields need entry, not acknowledgment"
        );
    }

    #[test]
    fn labels_inside_sentences_and_table_rows_are_not_read_as_fields() {
        let rows = vec![row(
            "Invoice number correction fee",
            "FEE",
            "1",
            "ea",
            "5.00",
            "5.00",
        )];
        let top = [
            "Please quote the invoice number: shown above when paying.",
            "Invoice Number: INV-5",
        ];
        let c = parse_with(&top, &rows, &BOTTOM_OK);
        assert_eq!(
            c.invoice_number.value.as_deref(),
            Some("INV-5"),
            "a parsed table row is not scanned for labels"
        );
    }

    // ---- 15a.1: outside-table charges, subtotal/tax handling, completeness ----
    fn listed(c: &InvoiceCandidates, needle: &str) -> bool {
        c.unparsed_lines.iter().any(|u| u.text.contains(needle))
    }

    #[test]
    fn charge_like_rows_after_the_table_are_preserved_in_every_amount_shape_and_block_completeness()
    {
        for (label, tail) in [
            (
                "two-column decimal",
                "Emergency response fee                         150.00",
            ),
            (
                "two-column whole number",
                "Emergency response fee                         150",
            ),
            (
                "two-column thousands",
                "Emergency response fee                       1,150",
            ),
            (
                "currency prefixed",
                "Emergency response fee                        $150.00",
            ),
            (
                "currency prefixed whole",
                "Emergency response fee                        $150",
            ),
            ("single-spaced currency", "Emergency response fee $150.00"),
            ("single-spaced decimal", "Emergency response fee 150.00"),
            (
                "three columns",
                "Emergency response fee        1       150.00",
            ),
            (
                "negative credit",
                "Emergency response credit                    (150.00)",
            ),
        ] {
            let bottom = [BOTTOM_OK[0], BOTTOM_OK[1], "", "", tail];
            let c = parse_with(&TOP, &rows_ok(), &bottom);
            assert!(
                listed(&c, "Emergency response"),
                "{label}: charge-like row after the table was dropped: {:?}",
                c.unparsed_lines
            );
            assert!(
                !c.checks.line_sum_complete,
                "{label}: the line sum must not be called complete"
            );
        }
    }

    #[test]
    fn charge_like_rows_before_the_table_are_preserved_and_block_completeness() {
        for (label, head) in [
            (
                "two-column",
                "Emergency response fee                         150.00",
            ),
            (
                "whole number",
                "Emergency response fee                         150",
            ),
            (
                "currency",
                "Emergency response fee                        $150.00",
            ),
        ] {
            let top = [TOP[0], TOP[1], TOP[2], TOP[3], TOP[4], head];
            let c = parse_with(&top, &rows_ok(), &BOTTOM_OK);
            assert!(
                listed(&c, "Emergency response"),
                "{label}: {:?}",
                c.unparsed_lines
            );
            assert!(!c.checks.line_sum_complete, "{label}");
        }
    }

    #[test]
    fn a_two_column_row_inside_the_table_is_kept_and_visible() {
        let mut rows = rows_ok();
        rows.push(format!("{:<28}{:>57}", "Fuel surcharge", "25.00"));
        let c = parse_with(&TOP, &rows, &BOTTOM_OK);
        let kept = c
            .lines
            .iter()
            .any(|l| l.description.value.as_deref() == Some("Fuel surcharge"))
            || listed(&c, "Fuel surcharge");
        assert!(kept, "an in-table charge row vanished");
        assert!(!c.checks.line_sum_complete, "the sum cannot be complete: this row is not a fully read line (or the total no longer matches)");
    }

    #[test]
    fn tax_rows_never_disappear_wherever_they_are() {
        let inside = [
            "                                          Sales Tax                         360.00",
            "                                          Total Due                       4,860.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &inside);
        assert!(
            listed(&c, "Sales Tax"),
            "tax inside the table region: {:?}",
            c.unparsed_lines
        );
        assert!(!c.checks.line_sum_complete);
        // far below the table, after a gap, and with the tax rate shown
        let outside = [
            BOTTOM_OK[0],
            "",
            "",
            "Sales Tax (8.25%)                                       360.00",
            "Total Due                                            4,860.00",
        ];
        let d = parse_with(&TOP, &rows_ok(), &outside);
        assert!(
            listed(&d, "Sales Tax (8.25%)"),
            "tax outside the table: {:?}",
            d.unparsed_lines
        );
        assert!(!d.checks.line_sum_complete);
        let rate = [
            "                                          Tax 8.25%            360.00",
            "                                          Total Due                       4,860.00",
        ];
        let e = parse_with(&TOP, &rows_ok(), &rate);
        assert!(listed(&e, "Tax 8.25%"), "{:?}", e.unparsed_lines);
    }

    #[test]
    fn tax_never_makes_the_invoice_look_reconciled_even_when_the_total_equals_the_lines() {
        // tax-inclusive total: the total equals the line sum, but a tax row exists and has not been accounted for
        let bottom = [
            "                                          Subtotal                        4,500.00",
            "                                          Sales Tax (included)              360.00",
            "                                          Total Due                       4,500.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert!(listed(&c, "Sales Tax (included)"));
        assert!(
            !c.checks.line_sum_complete,
            "a visible tax row must keep the sum from being called complete"
        );
    }

    #[test]
    fn a_subtotal_is_accounted_for_only_when_it_matches_a_complete_line_sum() {
        // matches and the lines are complete: accounted for (not listed), inside the table region
        let c = clean();
        assert!(!listed(&c, "Subtotal"));
        // outside the table region (after a gap) it is accounted for the same way
        let outside = [
            BOTTOM_OK[1],
            "",
            "",
            "Subtotal                                              4,500.00",
        ];
        let d = parse_with(&TOP, &rows_ok(), &outside);
        assert!(!listed(&d, "Subtotal"), "{:?}", d.unparsed_lines);
        // a subtotal that does not match is listed, inside and outside
        let wrong_in = [
            "                                          Subtotal                        4,400.00",
            BOTTOM_OK[1],
        ];
        assert!(listed(&parse_with(&TOP, &rows_ok(), &wrong_in), "Subtotal"));
        let wrong_out = [
            BOTTOM_OK[1],
            "",
            "",
            "Subtotal                                              4,400.00",
        ];
        assert!(listed(
            &parse_with(&TOP, &rows_ok(), &wrong_out),
            "Subtotal"
        ));
        // a subtotal that equals a PARTIAL sum is not accounted for: a charge-like row was left unparsed
        let mut rows = rows_ok();
        rows.push("   (wrapped) another charge continues here                 25.00".to_string());
        let partial = parse_with(&TOP, &rows, &BOTTOM_OK);
        assert!(
            listed(&partial, "Subtotal"),
            "a subtotal must not be accepted against an incomplete sum: {:?}",
            partial.unparsed_lines
        );
        assert!(!partial.checks.line_sum_complete);
    }

    #[test]
    fn completeness_needs_evidence_not_just_readable_table_rows() {
        // readable rows but nothing corroborates the sum (no total, no subtotal)
        let none = parse_with(&TOP, &rows_ok(), &[]);
        assert_eq!(none.checks.parsed_line_count, 2);
        assert!(
            !none.checks.line_sum_complete,
            "readable rows alone are not evidence that every charge was found"
        );
        // total present but different
        let wrong = parse_with(&TOP, &rows_ok(), &["                                          Total Due                       4,600.00"]);
        assert!(!wrong.checks.line_sum_complete);
        // conflicting totals, one of which matches
        let conflict = parse_with(&TOP, &rows_ok(), &["                                          Total Due                       4,500.00", "                                          Amount Due                      4,000.00"]);
        assert!(!conflict.checks.line_sum_complete);
        // a line total that cannot be read
        let rows = vec![
            row("Drum disposal", "DISP", "10", "drum", "400.00", "4,00.00"),
            row(
                "Transportation",
                "TRANSPORT",
                "1",
                "flat",
                "500.00",
                "500.00",
            ),
        ];
        assert!(!parse_with(&TOP, &rows, &BOTTOM_OK).checks.line_sum_complete);
        // no table at all
        assert!(
            !parse_invoice_text("Acme Co\nInvoice Number: 5\nTotal Due     4,500.00\n")
                .checks
                .line_sum_complete
        );
        // and the clean invoice, corroborated by its total, is complete
        assert!(clean().checks.line_sum_complete);
    }

    fn kinds_of(c: &InvoiceCandidates) -> Vec<UnparsedKind> {
        c.unparsed_lines.iter().map(|u| u.kind).collect()
    }
    fn reasons_of(c: &InvoiceCandidates) -> String {
        c.checks.incompleteness_reasons.join(" | ")
    }

    #[test]
    fn unparsed_lines_carry_a_kind_and_strong_versus_weak_amounts_are_told_apart() {
        let bottom = [
            BOTTOM_OK[1],
            "",
            "",
            "Emergency response fee                        $150.00",
            "Houston, TX                                           77001",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        let by_text = |needle: &str| {
            c.unparsed_lines
                .iter()
                .find(|u| u.text.contains(needle))
                .unwrap_or_else(|| panic!("{needle} missing"))
        };
        assert_eq!(by_text("Emergency response").kind, UnparsedKind::ChargeLike);
        assert_eq!(
            by_text("Houston").kind,
            UnparsedKind::PossibleCharge,
            "a bare whole number in its own column is flagged, not ignored"
        );
        assert!(by_text("Houston")
            .reason
            .contains("address, phone or reference number"));
        // a single-spaced address ending in a whole number is NOT flagged (too ambiguous to be a useful signal)
        let single = parse_with(
            &TOP,
            &rows_ok(),
            &[
                BOTTOM_OK[1],
                "",
                "",
                "1200 Industrial Way, Houston, TX 77001",
            ],
        );
        assert!(
            single.unparsed_lines.is_empty(),
            "{:?}",
            single.unparsed_lines
        );
        assert!(single.checks.line_sum_complete);
        let json = serde_json::to_string(&c).unwrap();
        assert!(
            json.contains("\"kind\":\"charge_like\"")
                && json.contains("\"kind\":\"possible_charge\"")
        );
    }

    #[test]
    fn a_charge_like_row_after_the_totals_is_listed_and_the_total_still_cannot_vouch_for_completeness(
    ) {
        // the total equals the parsed lines, but a late fee is printed AFTER the total: completeness is refused
        let bottom = [
            BOTTOM_OK[0],
            BOTTOM_OK[1],
            "Late payment fee                                        25.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert_eq!(c.checks.total_matches_lines, Some(true));
        // the late fee is listed, and the (otherwise matching) subtotal is rejected because the sum is incomplete
        assert_eq!(
            kinds_of(&c),
            vec![UnparsedKind::Subtotal, UnparsedKind::ChargeLike]
        );
        assert!(!c.checks.line_sum_complete);
        assert!(
            reasons_of(&c).contains("unparsed line(s) that may be charges"),
            "{}",
            reasons_of(&c)
        );
        assert_eq!(c.checks.completeness_basis, None);
    }

    #[test]
    fn tax_is_listed_with_its_amount_and_blocks_completeness_in_every_position() {
        let in_table = [
            "                                          Sales Tax                         360.00",
            "                                          Total Due                       4,860.00",
        ];
        let out_of_table = [
            BOTTOM_OK[0],
            "",
            "",
            "State Tax                                              360.00",
            "Total Due                                            4,860.00",
        ];
        let with_rate = [
            "                                          Sales Tax    8.25%         360.00",
            "                                          Total Due                       4,860.00",
        ];
        for (label, bottom) in [
            ("inside", &in_table[..]),
            ("outside", &out_of_table[..]),
            ("rate column", &with_rate[..]),
        ] {
            let c = parse_with(&TOP, &rows_ok(), bottom);
            let tax: Vec<&UnparsedLine> = c
                .unparsed_lines
                .iter()
                .filter(|u| u.kind == UnparsedKind::Tax)
                .collect();
            assert_eq!(tax.len(), 1, "{label}: {:?}", c.unparsed_lines);
            assert!(
                tax[0].reason.contains("360.00") && tax[0].reason.contains("NOT included"),
                "{label}"
            );
            assert!(!c.checks.line_sum_complete, "{label}");
            assert!(
                reasons_of(&c).contains("tax row(s)"),
                "{label}: {}",
                reasons_of(&c)
            );
            // the total no longer equals the lines, and that is flagged on the total itself
            assert_eq!(c.invoice_total.status, Status::Uncertain, "{label}");
            assert_eq!(c.checks.total_matches_lines, Some(false), "{label}");
        }
    }

    #[test]
    fn a_line_item_that_merely_mentions_tax_in_its_description_stays_a_line_item() {
        let rows = vec![
            row("Drum disposal", "DISP", "10", "drum", "400.00", "4,000.00"),
            row(
                "State hazardous waste tax",
                "HWTAX",
                "1",
                "flat",
                "120.00",
                "120.00",
            ),
        ];
        let bottom = [
            "                                          Total Due                       4,120.00",
        ];
        let c = parse_with(&TOP, &rows, &bottom);
        assert_eq!(c.lines.len(), 2, "{:?}", c.unparsed_lines);
        assert_eq!(
            c.lines[1].description.value.as_deref(),
            Some("State hazardous waste tax")
        );
        assert!(c.unparsed_lines.is_empty());
        assert!(c.checks.line_sum_complete, "{}", reasons_of(&c));
    }

    #[test]
    fn subtotals_are_accepted_only_against_complete_matching_sums_in_every_position() {
        // accepted: complete lines, matching subtotal, inside and outside the table region
        let inside = parse_with(&TOP, &rows_ok(), &[BOTTOM_OK[0]]);
        assert!(inside.unparsed_lines.is_empty());
        assert!(
            inside.checks.line_sum_complete,
            "a matching subtotal is positive evidence on its own: {}",
            reasons_of(&inside)
        );
        assert!(inside
            .checks
            .completeness_basis
            .as_deref()
            .unwrap()
            .contains("subtotal"));
        let outside = parse_with(
            &TOP,
            &rows_ok(),
            &[
                BOTTOM_OK[1],
                "",
                "",
                "Subtotal                                              4,500.00",
            ],
        );
        assert!(
            outside.unparsed_lines.is_empty(),
            "{:?}",
            outside.unparsed_lines
        );
        assert!(outside
            .checks
            .completeness_basis
            .as_deref()
            .unwrap()
            .contains("both"));
        // rejected: a subtotal that matches the parsed lines while an unreadable row is left over
        let mut rows = rows_ok();
        rows.push("   (wrapped) a charge whose amount is on the next line".to_string());
        let partial = parse_with(&TOP, &rows, &[BOTTOM_OK[0], BOTTOM_OK[1]]);
        assert_eq!(
            kinds_of(&partial),
            vec![UnparsedKind::UnreadableRow, UnparsedKind::Subtotal]
        );
        assert!(
            partial.unparsed_lines[1]
                .reason
                .contains("cannot be accepted"),
            "{}",
            partial.unparsed_lines[1].reason
        );
        assert!(!partial.checks.line_sum_complete);
        // rejected: a subtotal that matches while a charge-like row sits outside the table
        let late = parse_with(
            &TOP,
            &rows_ok(),
            &[
                BOTTOM_OK[0],
                BOTTOM_OK[1],
                "",
                "",
                "Late payment fee                                        25.00",
            ],
        );
        assert!(late
            .unparsed_lines
            .iter()
            .any(|u| u.kind == UnparsedKind::Subtotal && u.reason.contains("cannot be accepted")));
        // rejected: an unreadable line total makes the sum unusable
        let bad = vec![
            row("Drum disposal", "DISP", "10", "drum", "400.00", "4,00.00"),
            row(
                "Transportation",
                "TRANSPORT",
                "1",
                "flat",
                "500.00",
                "500.00",
            ),
        ];
        let c = parse_with(&TOP, &bad, &BOTTOM_OK);
        assert!(c
            .unparsed_lines
            .iter()
            .any(|u| u.kind == UnparsedKind::Subtotal));
        assert!(!c.checks.line_sum_complete);
        // rejected: a plain mismatch
        let wrong = parse_with(&TOP, &rows_ok(), &["                                          Subtotal                        4,400.00", BOTTOM_OK[1]]);
        assert!(wrong
            .unparsed_lines
            .iter()
            .any(|u| u.kind == UnparsedKind::Subtotal && u.reason.contains("does not equal")));
        assert!(!wrong.checks.line_sum_complete);
    }

    #[test]
    fn completeness_reports_why_it_is_false_and_what_the_evidence_was_when_true() {
        let c = clean();
        assert!(c.checks.line_sum_complete);
        assert!(c.checks.incompleteness_reasons.is_empty());
        assert_eq!(
            c.checks.completeness_basis.as_deref(),
            Some("the invoice total and a subtotal both equal the sum of the parsed line totals")
        );
        let none = parse_with(&TOP, &rows_ok(), &[]);
        assert!(
            reasons_of(&none).contains("neither a readable invoice total nor a subtotal"),
            "{}",
            reasons_of(&none)
        );
        let conflict = parse_with(&TOP, &rows_ok(), &["                                          Total Due                       4,500.00", "                                          Amount Due                      4,000.00"]);
        assert!(
            reasons_of(&conflict).contains("several different invoice totals"),
            "{}",
            reasons_of(&conflict)
        );
        let wrong = parse_with(&TOP, &rows_ok(), &["                                          Total Due                       4,600.00"]);
        assert!(
            reasons_of(&wrong).contains("does not equal the sum of the lines"),
            "{}",
            reasons_of(&wrong)
        );
        let no_table = parse_invoice_text("Acme\nTotal Due     4,500.00\n");
        assert!(
            reasons_of(&no_table).contains("no line-item table"),
            "{}",
            reasons_of(&no_table)
        );
        // a total that matches only ONE of two readings is never enough on its own to bless a partial sum
        let mut rows = rows_ok();
        rows.push("   (wrapped) continuation without an amount".to_string());
        let partial = parse_with(&TOP, &rows, &[BOTTOM_OK[1]]);
        assert_eq!(partial.checks.total_matches_lines, Some(true));
        assert!(
            !partial.checks.line_sum_complete,
            "a matching total does not excuse an unread row"
        );
    }

    #[test]
    fn a_truncated_extraction_can_never_be_complete() {
        let mut c = clean();
        assert!(c.checks.line_sum_complete);
        mark_text_truncated(&mut c);
        assert!(!c.checks.line_sum_complete);
        assert_eq!(c.checks.completeness_basis, None);
        assert!(c
            .checks
            .incompleteness_reasons
            .iter()
            .any(|r| r.contains("truncated")));
        assert!(c.warnings.iter().any(|w| w.contains("truncated")));
    }

    #[test]
    fn two_column_and_whole_number_rows_inside_the_table_are_kept_and_block_completeness() {
        for tail in [
            format!("{:<28}{:>57}", "Fuel surcharge", "25.00"),
            format!("{:<28}{:>57}", "Fuel surcharge", "25"),
        ] {
            let mut rows = rows_ok();
            rows.push(tail.clone());
            let c = parse_with(&TOP, &rows, &BOTTOM_OK);
            let as_line = c
                .lines
                .iter()
                .any(|l| l.description.value.as_deref() == Some("Fuel surcharge"));
            let as_unparsed = c
                .unparsed_lines
                .iter()
                .any(|u| u.text.contains("Fuel surcharge"));
            assert!(as_line || as_unparsed, "row vanished: {tail:?}");
            assert!(!c.checks.line_sum_complete, "{tail:?}: {}", reasons_of(&c));
        }
    }

    // ---- 15a.2: malformed amounts must stay visible ----
    const MALFORMED: [&str; 6] = [
        "$4,50.00",
        "4.500,00",
        "12O0.00",
        "1.23456",
        "$4,500.",
        "(4,50.00)",
    ];

    #[test]
    fn malformed_outside_table_charge_amounts_are_preserved_and_block_completeness() {
        for bad in MALFORMED {
            for (label, row) in [
                (
                    "two-column after",
                    format!("Emergency response fee                 {bad}"),
                ),
                (
                    "single-spaced after",
                    format!("Emergency response fee {bad}"),
                ),
                (
                    "three columns after",
                    format!("Emergency response fee        1       {bad}"),
                ),
            ] {
                let bottom = [BOTTOM_OK[0], BOTTOM_OK[1], "", "", row.as_str()];
                let c = parse_with(&TOP, &rows_ok(), &bottom);
                assert!(
                    listed(&c, "Emergency response"),
                    "{label} {bad}: the row disappeared: {:?}",
                    c.unparsed_lines
                );
                assert!(!c.checks.line_sum_complete, "{label} {bad}");
            }
            let top = [
                TOP[0],
                TOP[1],
                TOP[2],
                TOP[3],
                TOP[4],
                &format!("Emergency response fee                 {bad}"),
            ];
            let before = parse_with(&top, &rows_ok(), &BOTTOM_OK);
            assert!(
                listed(&before, "Emergency response"),
                "before the table {bad}: {:?}",
                before.unparsed_lines
            );
            assert!(!before.checks.line_sum_complete, "before the table {bad}");
        }
    }

    #[test]
    fn a_malformed_charge_does_not_get_blessed_by_matching_lines_and_total() {
        let bottom = [
            BOTTOM_OK[0],
            BOTTOM_OK[1],
            "",
            "",
            "Emergency response fee                 $4,50.00",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert_eq!(
            c.checks.total_matches_lines,
            Some(true),
            "the lines and the total do match"
        );
        assert!(
            listed(&c, "$4,50.00"),
            "original text preserved: {:?}",
            c.unparsed_lines
        );
        assert!(
            !c.checks.line_sum_complete,
            "a row that could not even be read must stop completeness"
        );
    }

    #[test]
    fn malformed_tax_rows_outside_and_inside_the_table_stay_visible_and_block_completeness() {
        for (label, bottom) in [
            ("outside", vec![BOTTOM_OK[0], "", "", "Sales Tax (8.25%)                                    $36,0.00", "Total Due                                            4,860.00"]),
            ("inside", vec!["                                          Sales Tax                       $36,0.00", "                                          Total Due                       4,860.00"]),
            ("rate only", vec![BOTTOM_OK[0], "", "", "Sales Tax                                                8.25%", "Total Due                                            4,500.00"]),
        ] {
            let c = parse_with(&TOP, &rows_ok(), &bottom);
            assert!(listed(&c, "Sales Tax"), "{label}: {:?}", c.unparsed_lines);
            assert!(!c.checks.line_sum_complete, "{label}");
        }
    }

    #[test]
    fn malformed_subtotal_rows_outside_and_inside_the_table_stay_visible_and_block_completeness() {
        for (label, bottom) in [
            ("outside", vec![BOTTOM_OK[1], "", "", "Subtotal                                               4,50.00"]),
            ("inside", vec!["                                          Subtotal                        4,50.00", BOTTOM_OK[1]]),
        ] {
            let c = parse_with(&TOP, &rows_ok(), &bottom);
            assert!(listed(&c, "Subtotal"), "{label}: {:?}", c.unparsed_lines);
            assert!(!c.checks.line_sum_complete, "{label}");
        }
    }

    #[test]
    fn ordinary_non_amount_text_is_not_mistaken_for_a_malformed_amount() {
        let bottom = [
            BOTTOM_OK[0],
            BOTTOM_OK[1],
            "",
            "Phone:   713-555-0100",
            "Due Date   04/14/2026",
            "Terms   Net 30 days",
            "Remit to the address above, e.g. Houston.",
            "Rate    8.25%",
            "Ref   A-123",
            "Thank you for your business.",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert!(c.unparsed_lines.is_empty(), "{:?}", c.unparsed_lines);
        assert!(c.checks.line_sum_complete, "{}", reasons_of(&c));
    }

    #[test]
    fn malformed_rows_keep_their_original_text_a_specific_kind_and_an_explicit_reason() {
        for bad in MALFORMED {
            let row = format!("Emergency response fee                 {bad}");
            let c = parse_with(
                &TOP,
                &rows_ok(),
                &[BOTTOM_OK[0], BOTTOM_OK[1], "", "", row.as_str()],
            );
            let u = c
                .unparsed_lines
                .iter()
                .find(|u| u.text.contains("Emergency response"))
                .unwrap_or_else(|| panic!("{bad} vanished"));
            assert_eq!(u.kind, UnparsedKind::UnreadableAmount, "{bad}");
            assert!(
                u.text.contains(bad),
                "{bad}: original text must be preserved, got {:?}",
                u.text
            );
            assert!(
                u.reason.contains("cannot be read") && u.reason.contains("NOT included"),
                "{bad}: {}",
                u.reason
            );
            assert!(
                reasons_of(&c).contains("may be charges"),
                "{bad}: {}",
                reasons_of(&c)
            );
            assert_eq!(c.checks.completeness_basis, None, "{bad}");
        }
        let json = serde_json::to_string(&parse_with(
            &TOP,
            &rows_ok(),
            &[BOTTOM_OK[1], "", "", "Fee   $4,50.00"],
        ))
        .unwrap();
        assert!(json.contains("\"kind\":\"unreadable_amount\""));
    }

    #[test]
    fn malformed_tax_and_subtotal_rows_get_their_own_kind_and_a_reason_that_names_the_problem() {
        let tax_out = parse_with(
            &TOP,
            &rows_ok(),
            &[
                BOTTOM_OK[0],
                "",
                "",
                "Sales Tax (8.25%)                                    $36,0.00",
            ],
        );
        let t = tax_out
            .unparsed_lines
            .iter()
            .find(|u| u.text.contains("Sales Tax"))
            .unwrap();
        assert_eq!(t.kind, UnparsedKind::Tax);
        assert!(
            t.text.contains("$36,0.00")
                && t.reason.contains("could not be read")
                && t.reason.contains("NOT included"),
            "{t:?}"
        );
        let tax_in = parse_with(&TOP, &rows_ok(), &["                                          Sales Tax                       $36,0.00", BOTTOM_OK[1]]);
        assert!(
            tax_in
                .unparsed_lines
                .iter()
                .any(|u| u.kind == UnparsedKind::Tax && u.reason.contains("could not be read")),
            "{:?}",
            tax_in.unparsed_lines
        );
        assert!(
            tax_in
                .lines
                .iter()
                .all(|l| l.description.value.as_deref() != Some("Sales Tax")),
            "tax is never a line item"
        );
        let rate = parse_with(
            &TOP,
            &rows_ok(),
            &[
                BOTTOM_OK[0],
                "",
                "",
                "Sales Tax                                                8.25%",
            ],
        );
        assert!(
            rate.unparsed_lines
                .iter()
                .any(|u| u.kind == UnparsedKind::Tax && u.reason.contains("is not an amount")),
            "{:?}",
            rate.unparsed_lines
        );
        for bottom in [
            vec![
                BOTTOM_OK[1],
                "",
                "",
                "Subtotal                                               4,50.00",
            ],
            vec![
                "                                          Subtotal                        4,50.00",
                BOTTOM_OK[1],
            ],
        ] {
            let c = parse_with(&TOP, &rows_ok(), &bottom);
            let u = c
                .unparsed_lines
                .iter()
                .find(|u| u.kind == UnparsedKind::Subtotal)
                .unwrap_or_else(|| panic!("{:?}", c.unparsed_lines));
            assert!(
                u.text.contains("4,50.00") && u.reason.contains("could not be read"),
                "{u:?}"
            );
            assert!(
                reasons_of(&c).contains("subtotal row(s)"),
                "{}",
                reasons_of(&c)
            );
        }
    }

    #[test]
    fn amount_recognition_is_separate_from_amount_parsing() {
        for tok in [
            "$4,50.00",
            "4.500,00",
            "12O0.00",
            "1.23456",
            "(4,50.00)",
            "$4,500.",
            ".50",
            "-$4,50.00",
        ] {
            assert!(
                amount_like(tok),
                "{tok} should be recognised as an amount attempt"
            );
            assert!(parse_amount(tok, true).is_err(), "{tok} should not parse");
        }
        for tok in [
            "03/15/2026",
            "713-555-0100",
            "12-34.56",
            "03-15.2026",
            "8.25%",
            "Net",
            "A-123",
            "e.g.",
            "Houston,",
            "2026-03-15",
            "10:30",
            "1st",
            "",
        ] {
            assert!(
                !amount_like(tok),
                "{tok} must not be treated as an amount attempt"
            );
        }
        for tok in ["150", "1,150", "$150.00", "(150.00)", "-150.00"] {
            assert!(parse_amount(tok, true).is_ok(), "{tok}");
        }
    }

    #[test]
    fn the_parser_version_reflects_the_rule_changes() {
        assert_eq!(PARSER_VERSION, "invoice-rules-3");
        assert_eq!(parse_invoice_text("").parser_version, "invoice-rules-3");
    }

    // ---- 15a.3: single-spaced rate-only tax / subtotal rows ----
    const SINGLE_SPACED_TAX: [&str; 6] = [
        "Sales Tax 8.25%",
        "Tax 8.25%",
        "State Tax 8.25%",
        "Sales Tax: 8.25%",
        "Tax (8.25%)",
        "Sales Tax 8.25% included",
    ];

    #[test]
    fn single_spaced_rate_only_tax_rows_are_listed_as_tax_and_block_completeness() {
        for row in SINGLE_SPACED_TAX {
            if row.ends_with("included") {
                continue; // prose after the rate: covered by the guard test below
            }
            for (label, bottom) in [
                ("outside", vec![BOTTOM_OK[0], BOTTOM_OK[1], "", "", row]),
                ("inside", vec![row, BOTTOM_OK[0], BOTTOM_OK[1]]),
            ] {
                let c = parse_with(&TOP, &rows_ok(), &bottom);
                let u = c
                    .unparsed_lines
                    .iter()
                    .find(|u| u.text.contains(row))
                    .unwrap_or_else(|| panic!("{label} {row:?} vanished: {:?}", c.unparsed_lines));
                assert_eq!(u.kind, UnparsedKind::Tax, "{label} {row:?}");
                assert!(
                    u.reason.contains("is not an amount")
                        && u.reason.contains("could not be read")
                        && u.reason.contains("NOT included"),
                    "{label} {row:?}: {}",
                    u.reason
                );
                assert!(!c.checks.line_sum_complete, "{label} {row:?}");
                assert!(
                    reasons_of(&c).contains("tax row(s)"),
                    "{label} {row:?}: {}",
                    reasons_of(&c)
                );
                assert!(
                    c.lines.iter().all(|l| !l
                        .description
                        .value
                        .as_deref()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains("tax")),
                    "tax is never a line item"
                );
            }
        }
    }

    #[test]
    fn single_spaced_rate_only_subtotal_rows_stay_visible_as_unaccepted_subtotals() {
        for row in ["Subtotal 8.25%", "Subtotal: 8.25%", "Sub-total 8.25%"] {
            for (label, bottom) in [
                ("outside", vec![BOTTOM_OK[0], BOTTOM_OK[1], "", "", row]),
                ("inside", vec![row, BOTTOM_OK[1]]),
            ] {
                let c = parse_with(&TOP, &rows_ok(), &bottom);
                let u = c
                    .unparsed_lines
                    .iter()
                    .find(|u| u.text.contains(row))
                    .unwrap_or_else(|| panic!("{label} {row:?} vanished: {:?}", c.unparsed_lines));
                assert_eq!(u.kind, UnparsedKind::Subtotal, "{label} {row:?}");
                assert!(
                    u.reason.contains("could not be read") && u.reason.contains("is not an amount"),
                    "{label} {row:?}: {}",
                    u.reason
                );
                assert!(!c.checks.line_sum_complete, "{label} {row:?}");
                assert!(
                    reasons_of(&c).contains("subtotal row(s)"),
                    "{label} {row:?}: {}",
                    reasons_of(&c)
                );
            }
        }
    }

    #[test]
    fn ordinary_percentage_lines_are_not_classified_as_tax_or_subtotal() {
        let bottom = [
            BOTTOM_OK[0],
            BOTTOM_OK[1],
            "",
            "Discount 10%",
            "Rate 8.25%",
            "Fuel surcharge 5%",
            "Interest 1.5% per month",
            "Taxable amount 8.25%",
            "Discount        10%",
            "Rate            8.25%",
            "Sales tax applies to all taxable goods and services listed above 2",
        ];
        let c = parse_with(&TOP, &rows_ok(), &bottom);
        assert!(
            c.unparsed_lines
                .iter()
                .all(|u| u.kind != UnparsedKind::Tax && u.kind != UnparsedKind::Subtotal),
            "{:?}",
            c.unparsed_lines
        );
        assert!(c.unparsed_lines.is_empty(), "{:?}", c.unparsed_lines);
        assert!(c.checks.line_sum_complete, "{}", reasons_of(&c));
    }
}
