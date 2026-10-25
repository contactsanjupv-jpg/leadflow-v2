# Reconciliation
The reconciliation checks include RULE_01 quantity vs manifest, RULE_02 weight vs ticket (tolerance), RULE_03 rate mismatch, RULE_04 unauthorized fee, RULE_05 duplicate charge, RULE_06 stale rate, RULE_07 missing support, RULE_08 waste-stream mismatch, RULE_09 container count, and RULE_10 incompatible units (no configured unit conversion), RULE_11 ambiguous fee (conflicting authorizations), RULE_12 unclassified fee type, and RULE_13 unreconciled bill total.

Inputs come from the invoice's linked manifest, all weight tickets of that manifest (aggregated in one unit; no unit conversion) and earlier invoices from the same vendor for the same service date (RULE_05 only).

- **RULE_05**: a line repeated identically on one invoice, or identical to a line on an earlier-entered invoice (same vendor, same service date). Amount = line total minus what RULE_03/04 already claim for that line; duplicate lines are excluded from the expected amount, so invoiced - expected equals the sum of findings. Credits and zero lines are ignored. The earlier invoice is never accused of duplicating a later one.
- **RULE_08**: invoice waste stream differs from the manifest's (case-insensitive). Reported at $0 because the correct price depends on the true stream; a reviewer decides.

Results are persisted as reviewable discrepancies; unchanged findings keep their review decision on re-run.


## Pricing integrity states (migration 0007)
Every new reconciliation includes `pricing_status`:

- `COMPLETE`: every non-duplicate primary invoice line could be priced using a rate effective on the service date. `expected_amount` and `discrepancy_amount` are available.
- `PARTIAL`: a rate exists, but the bill could not be fully priced: at least one primary line has a unit mismatch without a configured conversion, at least one billed fee is unclassified or conflicting (see Authorized fees below), or the bill-level guard (RULE_13) found the totals inconsistent. `expected_amount` and bill-level `discrepancy_amount` are `null`; `partial_expected_amount` is only a diagnostic subtotal.
- `UNCOMPUTABLE`: no usable rate covers the service date. `expected_amount` and bill-level `discrepancy_amount` are `null`.
- `LEGACY_UNVERIFIED`: a historical reconciliation created before these completeness checks. API and evidence views conceal its previous expected/discrepancy totals until it is re-run.

`identified_finding_amount` is the sum of monetary amounts attached to findings. On partial or uncomputable pricing it is **not** total invoice exposure. The dashboard calls these identified finding amounts and separately counts reconciliations whose pricing is incomplete or unverified. Evidence PDFs explicitly distinguish the partial subtotal, if any, from a full expected amount.

## Authorized fees: EXACT, CAP and unclassified (migration 0011)
Each authorized fee on a contract rate has a `charge_type`: `EXACT` (the contract authorizes exactly this amount) or `CAP` (the contract authorizes up to this amount). New fees must state one (`"EXACT"` or `"CAP"`, upper case). Fees that predate migration 0011 have `charge_type = NULL` (**unclassified**): nothing is defaulted, backfilled or inferred, so a fee is never treated as EXACT or CAP until a person classifies it (`PUT /rates/:rate_id/fees/:fee_code/type`, see `api.md`). Fee codes on invoice lines match authorized fees ignoring case and surrounding whitespace.

A finding amount is always `billed - expected`: positive is overbilled, negative is underbilled, zero is informational. Dashboard and recovery totals count only findings with `amount > 0`, and a discrepancy with `amount <= 0` cannot be disputed.

| Authorization | Billed fee | Expected contribution | Finding (RULE_04) |
|---|---|---|---|
| EXACT $500 | any amount different from $500 | $500 | `billed - 500`, e.g. $600 gives +$100 and $400 gives -$100; text "Fee 'X' billed at $A but authorized amount is $B" (unchanged from before charge types, so existing review decisions still carry forward) |
| CAP $500 | at or below $500 | the billed amount (`min(billed, cap)`) | none |
| CAP $500 | above $500 | $500 | `billed - 500`; text "Fee 'X' billed at $A exceeds the authorized cap of $B" |
| none | any | $0 | the full billed amount (fee not authorized) |

Credits (negative fee lines) keep their existing behavior: an EXACT credit differs from the authorized amount (billed -$500 against $500 gives -$1,000), and a CAP credit is below any cap, is priced at the credit itself and is never flagged.

**Unresolved fees.** A billed fee whose authorization cannot be priced is *unresolved*: it adds no expected amount, makes `pricing_status` `PARTIAL` (never `COMPLETE`), and produces a $0 informational finding for review:
- **RULE_12** unclassified fee type: the authorizing rate has the fee with `charge_type = NULL` ("...its charge type (exact or cap) has not been set...").
- **RULE_11** ambiguous fee: when several rates priced the invoice and authorize the same fee code with different amounts, or with different charge types ("...conflicting amounts..." / "...conflicting charge types..."). A classified and an unclassified authorization of the same amount also conflict. Identical (amount, type) authorizations from several rates count once.

A stored `charge_type` other than NULL, `EXACT` or `CAP` is a data error, not an unclassified fee: reconciliation fails with an internal error naming the fee. The database CHECK constraint prevents this.

**Bill-level guard (RULE_13).** After findings are known, a result that would be `COMPLETE` must satisfy `(invoiced - expected) - sum(findings)` within a rounding allowance of **$0.01 x (number of invoice lines + 1)**. Invoice entry permits $0.01 per line and $0.01 between header total and lines, so a flat cent would wrongly downgrade valid invoices. If the difference is larger, the result is downgraded to `PARTIAL`, a $0 `RULE_13_UNRECONCILED_BILL_TOTAL` finding is added, `expected_amount` and `discrepancy_amount` become `null`, and the computed subtotal is kept in `partial_expected_amount`. Within the allowance the result stays `COMPLETE`.

**Classification and re-reconciliation.** Classifying a fee does not rewrite anything: earlier reconciliations, their findings and stored evidence snapshots stay exactly as they were. The monitoring worker re-reconciles an invoice when a fee for its vendor and waste stream (matched like the existing rate check, ignoring effective dates) has `charge_type_set_at` newer than the invoice's latest reconciliation, once; an invoice that is current is not re-run. `POST /reconciliations/run` also re-runs on demand. Unchanged findings keep their review decision (carry-forward matches on type, rule id, description and amount).

**Evidence and PDF.** The evidence JSON lists each authorized fee of the parent rate under `contract_rate.authorized_fees[]` with `charge_type` (`"EXACT"`, `"CAP"`, or `null` when unclassified). Evidence packages stored before charge types have no `charge_type` key. The evidence PDF prints the fee as `CODE - $amount [label]` with the labels `exact charge`, `cap: maximum allowed`, `unclassified: type not set` (explicit `null`), `type not recorded in this evidence package` (key missing) and `unrecognised charge type`.

**Known limitations (accepted, not defects fixed here)**
- *Classification timing race.* `charge_type_set_at` is the start time of the classification transaction. A reconciliation that starts after that moment but commits before the classification does reads the fee as unclassified yet looks newer than `charge_type_set_at`, so the worker will not re-run it until something else changes the invoice. If this is suspected, run `POST /reconciliations/run` for the invoice.
- *Evidence is built from the live fee.* A new evidence package for a finding of an older reconciliation shows the fee's current type next to that reconciliation's own calculation trace. For example, evidence generated after classification for a finding computed while the fee was unclassified shows `EXACT` beside a trace saying the type was not set. `charge_type_set_at` is deliberately not part of evidence, and earlier packages are kept unchanged.
- *Multi-rate invoices.* When several rates priced an invoice the reconciliation has no single parent rate, so evidence has `contract_rate: null` and the PDF prints no authorized fees; `priced_lines` carries the rates instead. Fee types appear in evidence and the PDF only for single-rate invoices.
- *Worker matching ignores effective dates*, like the rate check, so classifying a fee on an old or unrelated rate of the same vendor and stream causes at most one extra reconciliation per affected invoice.
