-- Defense-in-depth tenant isolation (spec section 7 & 29).
CREATE ROLE xflow_app LOGIN PASSWORD 'CHANGE_ME_IN_PRODUCTION';

CREATE OR REPLACE FUNCTION current_org_id() RETURNS UUID AS $$
    SELECT NULLIF(current_setting('app.current_org_id', true), '')::UUID;
$$ LANGUAGE sql STABLE;

DO $$
DECLARE
    t TEXT;
BEGIN
    FOREACH t IN ARRAY ARRAY[
        'documents', 'document_extractions', 'vendors', 'contracts', 'contract_rates',
        'invoices', 'invoice_line_items', 'manifests', 'weight_records',
        'reconciliations', 'discrepancies', 'evidence_packages', 'disputes',
        'credits', 'monitoring_jobs', 'notifications', 'audit_logs'
    ]
    LOOP
        EXECUTE format('ALTER TABLE %I ENABLE ROW LEVEL SECURITY', t);
        EXECUTE format('ALTER TABLE %I FORCE ROW LEVEL SECURITY', t);
        EXECUTE format(
            'CREATE POLICY tenant_isolation ON %I USING (organization_id = current_org_id()) WITH CHECK (organization_id = current_org_id())',
            t
        );
        EXECUTE format('GRANT SELECT, INSERT, UPDATE, DELETE ON %I TO xflow_app', t);
    END LOOP;
END $$;

REVOKE UPDATE, DELETE ON audit_logs FROM xflow_app;

GRANT SELECT, INSERT, UPDATE ON organizations, users, memberships, sessions, subscriptions TO xflow_app;
GRANT SELECT, INSERT, UPDATE ON billing_events TO xflow_app;

GRANT SELECT, INSERT, UPDATE, DELETE ON contract_authorized_fees TO xflow_app;
ALTER TABLE contract_authorized_fees ENABLE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation_via_rate ON contract_authorized_fees
    USING (contract_rate_id IN (SELECT id FROM contract_rates WHERE organization_id = current_org_id()))
    WITH CHECK (contract_rate_id IN (SELECT id FROM contract_rates WHERE organization_id = current_org_id()));
