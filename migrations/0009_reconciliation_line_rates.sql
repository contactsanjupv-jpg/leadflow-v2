-- Record which contract rate was used to price each invoice line.
-- Historical reconciliation rows remain unchanged.

CREATE TABLE reconciliation_line_rates (
    organization_id UUID NOT NULL
        REFERENCES organizations(id) ON DELETE CASCADE,
    reconciliation_id UUID NOT NULL
        REFERENCES reconciliations(id) ON DELETE CASCADE,
    invoice_line_item_id UUID NOT NULL
        REFERENCES invoice_line_items(id) ON DELETE CASCADE,
    contract_rate_id UUID NOT NULL
        REFERENCES contract_rates(id),
    expected_line_amount NUMERIC(14,4) NOT NULL,
    PRIMARY KEY (reconciliation_id, invoice_line_item_id)
);

CREATE INDEX idx_reconciliation_line_rates_org
    ON reconciliation_line_rates (organization_id);

CREATE INDEX idx_reconciliation_line_rates_rate
    ON reconciliation_line_rates (contract_rate_id);