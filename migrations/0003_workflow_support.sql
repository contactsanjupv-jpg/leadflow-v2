-- 0003: schema support for the workflow subsystems (additive only; 0001/0002 are never edited).
-- Must be applied by a PRIVILEGED role (superuser or BYPASSRLS owner): the SECURITY DEFINER
-- worker functions below rely on their owner bypassing row level security.

CREATE EXTENSION IF NOT EXISTS btree_gist;

-- Explicit invoice -> manifest linkage (there was no way to say which manifest supports an invoice).
ALTER TABLE invoices ADD COLUMN manifest_id UUID REFERENCES manifests(id);
CREATE INDEX idx_invoices_manifest ON invoices(manifest_id);

-- Contract rates for the same vendor/waste stream/facility must not have overlapping effective
-- windows: overlapping windows make "the applicable rate" ambiguous. Enforced by the database.
ALTER TABLE contract_rates ADD CONSTRAINT contract_rates_no_overlap EXCLUDE USING gist (
    vendor_id WITH =,
    waste_stream WITH =,
    (COALESCE(facility_id, '00000000-0000-0000-0000-000000000000'::uuid)) WITH =,
    daterange(effective_from, effective_to, '[]') WITH &&
);
ALTER TABLE contract_rates ADD CONSTRAINT contract_rates_unit_rate_nonneg CHECK (unit_rate >= 0);

-- Latest-reconciliation-per-invoice lookups.
CREATE INDEX idx_reconciliations_invoice_created ON reconciliations(invoice_id, created_at DESC);

-- Documents: content hash for duplicate-upload detection and an explanatory status detail.
ALTER TABLE documents ADD COLUMN sha256 TEXT;
ALTER TABLE documents ADD COLUMN status_detail TEXT;
CREATE UNIQUE INDEX idx_documents_org_sha256 ON documents(organization_id, sha256) WHERE sha256 IS NOT NULL;

-- Billing: ignore out-of-order webhook deliveries.
ALTER TABLE subscriptions ADD COLUMN last_event_occurred_at TIMESTAMPTZ;

-- Monitoring: at most one active job of a given type per organization (no pile-ups).
CREATE UNIQUE INDEX idx_monitoring_jobs_active_unique ON monitoring_jobs(organization_id, job_type) WHERE status IN ('PENDING', 'RUNNING');

-- Cross-tenant job claiming for the background worker. monitoring_jobs has FORCE RLS, so a
-- worker without tenant context can see nothing; these narrow SECURITY DEFINER functions are
-- the only cross-tenant capability granted, and they only touch monitoring_jobs.
CREATE OR REPLACE FUNCTION claim_monitoring_job()
RETURNS TABLE (job_id UUID, org_id UUID, job_kind TEXT, job_payload JSONB, job_attempts INTEGER)
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
DECLARE
    j monitoring_jobs%ROWTYPE;
BEGIN
    SELECT * INTO j FROM monitoring_jobs
     WHERE (status = 'PENDING' AND scheduled_for <= now())
        OR (status = 'RUNNING' AND started_at < now() - interval '15 minutes')
     ORDER BY scheduled_for
     FOR UPDATE SKIP LOCKED
     LIMIT 1;
    IF NOT FOUND THEN
        RETURN;
    END IF;
    UPDATE monitoring_jobs SET status = 'RUNNING', started_at = now(), attempts = monitoring_jobs.attempts + 1
     WHERE monitoring_jobs.id = j.id;
    RETURN QUERY SELECT j.id, j.organization_id, j.job_type, j.payload, j.attempts + 1;
END $$;

CREATE OR REPLACE FUNCTION finish_monitoring_job(p_job_id UUID, p_error TEXT)
RETURNS VOID
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public AS $$
DECLARE
    j monitoring_jobs%ROWTYPE;
BEGIN
    SELECT * INTO j FROM monitoring_jobs WHERE id = p_job_id FOR UPDATE;
    IF NOT FOUND THEN
        RETURN;
    END IF;
    IF p_error IS NULL THEN
        UPDATE monitoring_jobs SET status = 'COMPLETED', completed_at = now(), last_error = NULL WHERE id = p_job_id;
    ELSIF j.attempts >= j.max_attempts THEN
        UPDATE monitoring_jobs SET status = 'FAILED', completed_at = now(), last_error = p_error WHERE id = p_job_id;
    ELSE
        UPDATE monitoring_jobs SET status = 'PENDING', last_error = p_error,
               scheduled_for = now() + make_interval(secs => 30 * j.attempts * j.attempts)
         WHERE id = p_job_id;
    END IF;
END $$;

REVOKE ALL ON FUNCTION claim_monitoring_job() FROM PUBLIC;
REVOKE ALL ON FUNCTION finish_monitoring_job(UUID, TEXT) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION claim_monitoring_job() TO xflow_app;
GRANT EXECUTE ON FUNCTION finish_monitoring_job(UUID, TEXT) TO xflow_app;
