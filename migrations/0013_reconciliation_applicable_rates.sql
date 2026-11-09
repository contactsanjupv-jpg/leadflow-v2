-- Record which contract rates were APPLICABLE to an invoice in a given reconciliation.
--
-- Why: reconciliation_line_rates records the rate that priced each priced line, and
-- reconciliations.contract_rate_id holds a parent rate only when exactly one rate applied. Neither
-- shows a rate that applied (it covered the service date, one rate per unit) but priced no line, yet
-- the engine evaluates authorized fees against EVERY applicable rate: such a rate can make a fee
-- conflicting or unclassified. Evidence for multi-rate invoices therefore could not show the
-- authorizations that actually produced the result.
--
-- Rows are written in the same transaction as the reconciliation they describe. Reconciliations that
-- existed before this migration have no rows and are NOT backfilled: history is not rewritten, and
-- evidence for them says it is built from the line-rate records only.

CREATE TABLE reconciliation_applicable_rates (
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    reconciliation_id UUID NOT NULL,
    invoice_id UUID NOT NULL,
    contract_rate_id UUID NOT NULL,
    PRIMARY KEY (reconciliation_id, contract_rate_id),
    CONSTRAINT reconciliation_applicable_rates_reconciliation_same_scope
        FOREIGN KEY (organization_id, reconciliation_id, invoice_id)
        REFERENCES reconciliations (organization_id, id, invoice_id) ON DELETE CASCADE,
    CONSTRAINT reconciliation_applicable_rates_rate_same_org
        FOREIGN KEY (organization_id, contract_rate_id)
        REFERENCES contract_rates (organization_id, id)
);

CREATE INDEX idx_reconciliation_applicable_rates_rate ON reconciliation_applicable_rates (contract_rate_id);
CREATE INDEX idx_reconciliation_applicable_rates_invoice ON reconciliation_applicable_rates (organization_id, invoice_id);

COMMENT ON TABLE reconciliation_applicable_rates IS
    'The contract rates the engine considered applicable (one per unit) for a reconciliation, including rates that priced no line. Absent for reconciliations created before migration 0013.';

ALTER TABLE reconciliation_applicable_rates ENABLE ROW LEVEL SECURITY;
ALTER TABLE reconciliation_applicable_rates FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON reconciliation_applicable_rates
    USING (organization_id = current_org_id())
    WITH CHECK (organization_id = current_org_id());
GRANT SELECT, INSERT, DELETE ON reconciliation_applicable_rates TO xflow_app;
