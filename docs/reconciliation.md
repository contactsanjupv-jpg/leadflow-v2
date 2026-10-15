# Reconciliation
All nine rules are implemented: RULE_01 quantity vs manifest, 02 weight vs ticket (tolerance), 03 rate mismatch, 04 unauthorized fee, 05 duplicate charge, 06 stale rate, 07 missing support, 08 waste-stream mismatch, 09 container count.

Inputs come from the invoice's linked manifest, all weight tickets of that manifest (aggregated in one unit; no unit conversion) and earlier invoices from the same vendor for the same service date (RULE_05 only).

- **RULE_05**: a line repeated identically on one invoice, or identical to a line on an earlier-entered invoice (same vendor, same service date). Amount = line total minus what RULE_03/04 already claim for that line; duplicate lines are excluded from the expected amount, so invoiced - expected equals the sum of findings. Credits and zero lines are ignored. The earlier invoice is never accused of duplicating a later one.
- **RULE_08**: invoice waste stream differs from the manifest's (case-insensitive). Reported at $0 because the correct price depends on the true stream; a reviewer decides.

Results are persisted as reviewable discrepancies; unchanged findings keep their review decision on re-run.
