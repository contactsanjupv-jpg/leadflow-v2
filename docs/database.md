# Database
Migrations 0001–0004 in `migrations/` (never edit applied files; add 0005+). 0002 enables FORCE RLS on tenant tables and creates `xflow_app`; 0003 adds invoice→manifest link, rate-window exclusion constraint, document hashes, monitoring job functions; 0004 adds a case-insensitive unique email index. Apply with `scripts/migrate.sh` (tracked, idempotent) using a privileged role.

## Date plausibility (migration 0005)
All business dates must have a year in 1900-2100 (backend `common::dates`, CHECK constraints, frontend `DateInput`). The constraints are `NOT VALID`, so rows saved before the migration are untouched. Find and repair old bad rows (e.g. a year typed as `0206`), then validate:
```sql
SELECT 'manifests', id, shipment_date FROM manifests WHERE shipment_date NOT BETWEEN '1900-01-01' AND '2100-12-31'
UNION ALL SELECT 'weight_records', id, ticket_date FROM weight_records WHERE ticket_date NOT BETWEEN '1900-01-01' AND '2100-12-31'
UNION ALL SELECT 'invoices(invoice_date)', id, invoice_date FROM invoices WHERE invoice_date NOT BETWEEN '1900-01-01' AND '2100-12-31'
UNION ALL SELECT 'invoices(service_date)', id, service_date FROM invoices WHERE service_date NOT BETWEEN '1900-01-01' AND '2100-12-31'
UNION ALL SELECT 'contract_rates(from)', id, effective_from FROM contract_rates WHERE effective_from NOT BETWEEN '1900-01-01' AND '2100-12-31'
UNION ALL SELECT 'contract_rates(to)', id, effective_to FROM contract_rates WHERE effective_to NOT BETWEEN '1900-01-01' AND '2100-12-31';
-- after correcting the rows, e.g.:
-- UPDATE manifests SET shipment_date = '2026-12-09' WHERE id = '...';   -- (run as a privileged role; RLS applies to xflow_app)
-- ALTER TABLE manifests VALIDATE CONSTRAINT manifests_shipment_date_plausible;
```