# Reconciliation
The reconciliation checks include RULE_01 quantity vs manifest, RULE_02 weight vs ticket (tolerance), RULE_03 rate mismatch, RULE_04 unauthorized fee, RULE_05 duplicate charge, RULE_06 stale rate, RULE_07 missing support, RULE_08 waste-stream mismatch, RULE_09 container count, and RULE_10 incompatible units (no configured unit conversion).

Inputs come from the invoice's linked manifest, all weight tickets of that manifest (aggregated in one unit; no unit conversion) and earlier invoices from the same vendor for the same service date (RULE_05 only).

- **RULE_05**: a line repeated identically on one invoice, or identical to a line on an earlier-entered invoice (same vendor, same service date). Amount = line total minus what RULE_03/04 already claim for that line; duplicate lines are excluded from the expected amount, so invoiced - expected equals the sum of findings. Credits and zero lines are ignored. The earlier invoice is never accused of duplicating a later one.
- **RULE_08**: invoice waste stream differs from the manifest's (case-insensitive). Reported at $0 because the correct price depends on the true stream; a reviewer decides.

Results are persisted as reviewable discrepancies; unchanged findings keep their review decision on re-run.


## Pricing integrity states (migration 0007)
Every new reconciliation includes `pricing_status`:

- `COMPLETE`: every non-duplicate primary invoice line could be priced using a rate effective on the service date. `expected_amount` and `discrepancy_amount` are available.
- `PARTIAL`: a rate exists, but at least one primary line has a unit mismatch without a configured conversion. `expected_amount` and bill-level `discrepancy_amount` are `null`; `partial_expected_amount` is only a diagnostic subtotal.
- `UNCOMPUTABLE`: no usable rate covers the service date. `expected_amount` and bill-level `discrepancy_amount` are `null`.
- `LEGACY_UNVERIFIED`: a historical reconciliation created before these completeness checks. API and evidence views conceal its previous expected/discrepancy totals until it is re-run.

`identified_finding_amount` is the sum of monetary amounts attached to findings. On partial or uncomputable pricing it is **not** total invoice exposure. The dashboard calls these identified finding amounts and separately counts reconciliations whose pricing is incomplete or unverified. Evidence PDFs explicitly distinguish the partial subtotal, if any, from a full expected amount.
