-- Tenant isolation and same-org / same-invoice integrity for per-line pricing evidence.
-- 0009 created reconciliation_line_rates without RLS or composite keys.
-- This migration does not rewrite historical reconciliation totals.

-- (organization_id, id) is unique because id is already the primary key.
-- The extra key is only so foreign keys can require the same organization.
ALTER TABLE contract_rates
    ADD CONSTRAINT contract_rates_org_id_key UNIQUE (organization_id, id);

ALTER TABLE reconciliations
    ADD CONSTRAINT reconciliations_org_invoice_id_key
    UNIQUE (organization_id, id, invoice_id);

ALTER TABLE invoice_line_items
    ADD CONSTRAINT invoice_line_items_org_invoice_id_key
    UNIQUE (organization_id, invoice_id, id);

-- invoice_id lets a foreign key prove the line belongs to the reconciliation's invoice.
-- 0009 is unused by the application, so this column is added NOT NULL with no backfill.
ALTER TABLE reconciliation_line_rates
    ADD COLUMN invoice_id UUID NOT NULL;

ALTER TABLE reconciliation_line_rates
    ADD CONSTRAINT reconciliation_line_rates_reconciliation_same_scope
        FOREIGN KEY (organization_id, reconciliation_id, invoice_id)
        REFERENCES reconciliations (organization_id, id, invoice_id)
        ON DELETE CASCADE,
    ADD CONSTRAINT reconciliation_line_rates_line_same_invoice
        FOREIGN KEY (organization_id, invoice_id, invoice_line_item_id)
        REFERENCES invoice_line_items (organization_id, invoice_id, id)
        ON DELETE CASCADE,
    ADD CONSTRAINT reconciliation_line_rates_rate_same_org
        FOREIGN KEY (organization_id, contract_rate_id)
        REFERENCES contract_rates (organization_id, id);

CREATE INDEX idx_reconciliation_line_rates_invoice
    ON reconciliation_line_rates (organization_id, invoice_id);

ALTER TABLE reconciliation_line_rates ENABLE ROW LEVEL SECURITY;
ALTER TABLE reconciliation_line_rates FORCE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation ON reconciliation_line_rates
    USING (organization_id = current_org_id())
    WITH CHECK (organization_id = current_org_id());

GRANT SELECT, INSERT, UPDATE, DELETE ON reconciliation_line_rates TO xflow_app;