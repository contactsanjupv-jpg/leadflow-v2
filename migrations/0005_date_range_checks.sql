-- 0005: database-level backstop for date plausibility (same range as backend `common::dates`: 1900-2100).
-- A browser date control can emit a 3-digit year such as 0206 after a lost keystroke; PostgreSQL DATE would
-- store it happily. Constraints are added NOT VALID so that applying this migration never fails on rows that
-- already exist: they are enforced for every new INSERT/UPDATE immediately. To audit and repair old rows, run
-- the SELECTs in docs/database.md, fix the dates, then `ALTER TABLE ... VALIDATE CONSTRAINT <name>`.

ALTER TABLE invoices ADD CONSTRAINT invoices_invoice_date_plausible CHECK (invoice_date BETWEEN DATE '1900-01-01' AND DATE '2100-12-31') NOT VALID;
ALTER TABLE invoices ADD CONSTRAINT invoices_service_date_plausible CHECK (service_date BETWEEN DATE '1900-01-01' AND DATE '2100-12-31') NOT VALID;
ALTER TABLE manifests ADD CONSTRAINT manifests_shipment_date_plausible CHECK (shipment_date BETWEEN DATE '1900-01-01' AND DATE '2100-12-31') NOT VALID;
ALTER TABLE weight_records ADD CONSTRAINT weight_records_ticket_date_plausible CHECK (ticket_date BETWEEN DATE '1900-01-01' AND DATE '2100-12-31') NOT VALID;
ALTER TABLE contract_rates ADD CONSTRAINT contract_rates_effective_from_plausible CHECK (effective_from BETWEEN DATE '1900-01-01' AND DATE '2100-12-31') NOT VALID;
ALTER TABLE contract_rates ADD CONSTRAINT contract_rates_effective_to_plausible CHECK (effective_to IS NULL OR effective_to BETWEEN DATE '1900-01-01' AND DATE '2100-12-31') NOT VALID;