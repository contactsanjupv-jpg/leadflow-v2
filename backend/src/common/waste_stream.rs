//! Canonical waste-stream comparison key.
//!
//! Waste-stream identifiers (EPA codes such as `F003`, `D001`, state codes) are case-insensitive
//! labels. The text a user entered is always stored and displayed unchanged; only COMPARISON uses
//! this key: trim, collapse every run of whitespace to one space, upper-case. Nothing else is
//! altered. Internal whitespace is collapsed, never removed: `F 003` and `F   003` match each
//! other, but `F 003` is a different identifier from `F003`. `F003`, `F03`, `F0030` never merge.
//!
//! This must stay in step with the SQL expression behind the `waste_stream_key` generated column
//! on `contract_rates`/`invoices` (migration 0006); the integration test `rust_and_sql_keys_agree`
//! evaluates that expression directly and asserts both produce identical results.

/// Canonical key, or `None` when the identifier is empty/blank (a blank stream matches nothing).
pub fn key(raw: &str) -> Option<String> {
    let folded = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if folded.is_empty() {
        None
    } else {
        Some(folded.to_uppercase())
    }
}

/// True when both identifiers are non-blank and share the same canonical key.
pub fn same(a: &str, b: &str) -> bool {
    match (key(a), key(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_and_surrounding_whitespace_fold_to_one_key() {
        for v in ["F003", "f003", " F003 ", "\tf003\n", "F003  "] {
            assert_eq!(key(v).as_deref(), Some("F003"), "{v:?}");
        }
    }

    #[test]
    fn internal_whitespace_runs_collapse_but_are_not_removed() {
        assert_eq!(
            key("hazardous   waste\tstream").as_deref(),
            Some("HAZARDOUS WASTE STREAM")
        );
        // Runs of whitespace collapse, so these match each other ...
        assert_eq!(key("F 003"), key("f   003"));
        assert_eq!(key("F 003").as_deref(), Some("F 003"));
        // ... but whitespace is never removed, so "F 003" is NOT "F003".
        assert_ne!(
            key("F 003"),
            key("F003"),
            "removing spaces would merge distinct identifiers"
        );
        assert!(!same("F 003", "F003"));
        assert!(same("F 003", " f  003 "));
    }

    #[test]
    fn genuinely_distinct_identifiers_do_not_merge() {
        let keys: Vec<_> = ["F003", "F03", "F0030", "F004", "D001", "FO03"]
            .iter()
            .map(|v| key(v))
            .collect();
        for i in 0..keys.len() {
            for j in (i + 1)..keys.len() {
                assert_ne!(keys[i], keys[j], "{i} vs {j}");
            }
        }
    }

    #[test]
    fn blank_identifier_has_no_key_and_never_matches() {
        assert_eq!(key(""), None);
        assert_eq!(key("  \t "), None);
        assert!(!same("", ""));
        assert!(!same("F003", " "));
        assert!(same("F003", " f003"));
    }
}
