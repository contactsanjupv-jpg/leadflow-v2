-- Durable "this invoice must be reconciled again" requests.
--
-- Why: the monitoring worker used to detect a stale invoice by comparing timestamps. A fee
-- classification stamps charge_type_set_at with now(), which PostgreSQL fixes at TRANSACTION START. A
-- classification that starts before a reconciliation but commits after that reconciliation has read
-- the fee therefore has charge_type_set_at < reconciliation.created_at, and the timestamp comparison
-- silently concluded the invoice was current while it still reflected the unclassified fee.
--
-- A request row is inserted in the SAME transaction as the classification (so it commits or rolls
-- back with it; there is no window where one exists without the other). A reconciliation deletes the
-- requests for its invoice as its FIRST statement, before it reads any fee, in the same transaction
-- that inserts the reconciliation. Consequently:
--   * a request committed before a reconciliation's first statement is consumed by it, and the
--     reconciliation reads the classification that caused it;
--   * a request committed after that statement is not visible to the DELETE, survives the
--     reconciliation's commit, and makes the worker reconcile the invoice again;
--   * if the reconciliation fails or crashes, its DELETE rolls back and the request remains.
-- Rows are append-only apart from that consumption, so concurrent classifications never overwrite
-- each other's requests.

CREATE TABLE invoice_reconciliation_requests (
    id BIGINT GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    invoice_id UUID NOT NULL REFERENCES invoices(id) ON DELETE CASCADE,
    reason TEXT NOT NULL CHECK (reason IN ('FEE_CLASSIFIED')),
    source_fee_id UUID REFERENCES contract_authorized_fees(id) ON DELETE SET NULL,
    requested_by UUID REFERENCES users(id) ON DELETE SET NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_invoice_reconciliation_requests_invoice ON invoice_reconciliation_requests (invoice_id);

COMMENT ON TABLE invoice_reconciliation_requests IS
    'Pending requests to reconcile an invoice again. Inserted atomically with the change that invalidated the last reconciliation; deleted by the reconciliation that consumes them.';

-- Same tenant isolation as the other tenant tables (migration 0002).
ALTER TABLE invoice_reconciliation_requests ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_reconciliation_requests FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON invoice_reconciliation_requests
    USING (organization_id = current_org_id())
    WITH CHECK (organization_id = current_org_id());
GRANT SELECT, INSERT, DELETE ON invoice_reconciliation_requests TO xflow_app;
