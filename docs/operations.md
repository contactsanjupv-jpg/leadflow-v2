# Operations
Monitoring: scheduler enqueues one RECONCILE_PENDING job per org every `MONITOR_INTERVAL_SECS`; workers claim jobs via `claim_monitoring_job()` (safe with multiple replicas), failures retry with quadratic backoff up to `max_attempts`, stuck RUNNING jobs are reclaimed after 15 minutes. Inspect via `GET /monitoring/jobs` or the `monitoring_jobs` table. Back up Postgres and the uploads volume together.

## Unclassified authorized fees (migration 0011)
Fees entered before migration 0011 have `charge_type = NULL`. They are never assumed EXACT or CAP: invoices billing them reconcile as `PARTIAL` with a $0 `RULE_12_UNCLASSIFIED_FEE_TYPE` finding until each fee is classified. Classifying is a business decision per contract (is the amount fixed or a ceiling?); do not bulk-update the column from SQL, because the endpoint is what records who decided and when. Run the queries below as a privileged role (RLS applies to `xflow_app`).

Fees still to classify, per organization, vendor and rate:
```sql
SELECT o.name AS organization, v.name AS vendor, c.name AS contract, cr.id AS contract_rate_id,
       cr.waste_stream, cr.unit, cr.effective_from, cr.effective_to,
       f.fee_code, f.amount
  FROM contract_authorized_fees f
  JOIN contract_rates cr ON cr.id = f.contract_rate_id
  JOIN contracts c ON c.id = cr.contract_id
  JOIN vendors v ON v.id = cr.vendor_id
  JOIN organizations o ON o.id = cr.organization_id
 WHERE f.charge_type IS NULL
 ORDER BY o.name, v.name, cr.waste_stream, cr.unit, f.fee_code;
```
Invoices whose latest reconciliation is unresolved because of fee classification (unclassified RULE_12, or conflicting RULE_11):
```sql
SELECT o.name AS organization, v.name AS vendor, i.invoice_number, i.service_date,
       r.id AS reconciliation_id, r.pricing_status, r.created_at AS reconciled_at,
       d.rule_id, d.description
  FROM (SELECT DISTINCT ON (invoice_id) *
          FROM reconciliations
         ORDER BY invoice_id, created_at DESC, id DESC) r
  JOIN invoices i ON i.id = r.invoice_id
  JOIN vendors v ON v.id = i.vendor_id
  JOIN organizations o ON o.id = i.organization_id
  JOIN discrepancies d ON d.reconciliation_id = r.id
 WHERE d.rule_id IN ('RULE_12_UNCLASSIFIED_FEE_TYPE', 'RULE_11_AMBIGUOUS_FEE')
 ORDER BY o.name, v.name, i.service_date, i.invoice_number, d.rule_id;
```
Handling:
1. List the fees with the first query and decide EXACT or CAP for each with the contract owner.
2. Classify each one with `PUT /rates/:rate_id/fees/:fee_code/type` (see `api.md`; needs `ManageData`). A classified fee cannot be changed; a mistake needs a new rate version (end the rate with `PUT /rates/:id/end` and add a replacement rate with the correct types), and past reconciliations are not rewritten.
3. The next monitoring run re-reconciles each affected invoice once (an invoice already current is skipped). To re-run sooner, enqueue `POST /monitoring/run` or call `POST /reconciliations/run` for the invoice, then re-run the second query: invoices that now price completely disappear from it. Invoices with a RULE_11 conflict stay listed until the conflicting authorizations are resolved with a corrected rate version.
4. Verify and audit a classification:
```sql
SELECT fee_code, charge_type, charge_type_set_at, charge_type_set_by
  FROM contract_authorized_fees WHERE contract_rate_id = '<rate id>';
SELECT created_at, actor_user_id, before_state, after_state
  FROM audit_logs WHERE action = 'contract_authorized_fee.classify' AND resource_id = '<fee id>';
```
The demo seed (`scripts/seed_demo.sql`) inserts its fee without a type, so demo invoices reconcile as `PARTIAL` until that fee is classified.

Known limitation: if a reconciliation was running at the moment a fee was classified, the worker may not notice that classification (see "Classification timing race" in `reconciliation.md`). Re-run the invoice with `POST /reconciliations/run` if an invoice still shows a RULE_12 finding after its fee was classified.
