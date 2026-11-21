-- 0015: invoice confirmation support (Step 15b).
--   * one source document backs at most one invoice (partial unique index, enforced by the database)
--   * invoice_extraction_reviews: immutable, tenant-isolated provenance for a human-confirmed extraction
-- Additive only; 0001-0014 are never edited. Must be applied by a PRIVILEGED role (superuser or BYPASSRLS):
-- the guard below must see every tenant's rows, and FORCE RLS would otherwise hide them and make the
-- duplicate check meaninglessly return zero. migrate.sh wraps this file in BEGIN/COMMIT, so a failing guard
-- changes nothing and records nothing.
--
-- Deliberately NOT done here: invoices.source_document_id (0001) stays a plain FK to documents(id), so the
-- database does not itself stop an invoice from pointing at another organization's document; the API
-- refuses that (ownership check in create_invoice). Closing it would constrain existing rows and belongs in
-- its own migration with a preflight. documents_id_org_uniq below is the key such an FK would need.

DO $$
DECLARE
    dup_count integer;
    sample    text;
BEGIN
    IF NOT (SELECT rolsuper OR rolbypassrls FROM pg_roles WHERE rolname = current_user) THEN
        RAISE EXCEPTION '0015 must run as a superuser or BYPASSRLS role (current role: %), otherwise the duplicate check cannot see other tenants'' rows', current_user;
    END IF;
    SELECT count(*) INTO dup_count FROM (
        SELECT 1 FROM invoices WHERE source_document_id IS NOT NULL
         GROUP BY source_document_id HAVING count(*) > 1
    ) d;
    IF dup_count > 0 THEN
        SELECT string_agg(source_document_id::text, ', ') INTO sample FROM (
            SELECT source_document_id FROM invoices WHERE source_document_id IS NOT NULL
             GROUP BY source_document_id HAVING count(*) > 1 ORDER BY 1 LIMIT 10
        ) s;
        RAISE EXCEPTION '0015 stopped: % source document(s) back more than one invoice (first: %). Resolve them by hand; nothing was changed.', dup_count, sample;
    END IF;
END $$;

-- One source document, one invoice. NULL links are unconstrained.
CREATE UNIQUE INDEX invoices_source_document_uniq ON invoices (source_document_id) WHERE source_document_id IS NOT NULL;

-- Redundant unique keys that exist only so child tables can use tenant-aware composite foreign keys.
ALTER TABLE documents ADD CONSTRAINT documents_id_org_uniq UNIQUE (id, organization_id);
ALTER TABLE document_extractions ADD CONSTRAINT document_extractions_id_doc_org_uniq UNIQUE (id, document_id, organization_id);
ALTER TABLE invoices ADD CONSTRAINT invoices_id_source_doc_org_uniq UNIQUE (id, source_document_id, organization_id);

CREATE TABLE invoice_extraction_reviews (
    id                       UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    organization_id          UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    document_id              UUID NOT NULL,
    invoice_id               UUID NOT NULL,
    extraction_id            UUID NOT NULL,
    parser_version           TEXT NOT NULL,
    confirmed_by             UUID NOT NULL,
    confirmed_at             TIMESTAMPTZ NOT NULL DEFAULT now(),
    payload_hash             TEXT NOT NULL,
    text_truncated           BOOLEAN NOT NULL,
    parser_line_sum_complete BOOLEAN NOT NULL,
    original_document_checked BOOLEAN NOT NULL,
    candidates_snapshot      JSONB NOT NULL,
    submitted                JSONB NOT NULL,
    dispositions             JSONB NOT NULL,
    acknowledged             JSONB NOT NULL,
    CONSTRAINT invoice_extraction_reviews_document_uniq UNIQUE (document_id),
    CONSTRAINT invoice_extraction_reviews_invoice_uniq UNIQUE (invoice_id),
    CONSTRAINT invoice_extraction_reviews_hash_chk CHECK (payload_hash ~ '^[0-9a-f]{64}$'),
    CONSTRAINT invoice_extraction_reviews_ack_chk CHECK (jsonb_typeof(acknowledged) = 'array'),
    -- Tenant-aware: every referenced row must belong to THIS organization (RI checks bypass RLS).
    CONSTRAINT invoice_extraction_reviews_document_fk FOREIGN KEY (document_id, organization_id)
        REFERENCES documents (id, organization_id),
    CONSTRAINT invoice_extraction_reviews_extraction_fk FOREIGN KEY (extraction_id, document_id, organization_id)
        REFERENCES document_extractions (id, document_id, organization_id),
    -- The invoice must be in this organization AND be linked to this very document. As a side effect a
    -- confirmed invoice cannot be unlinked, re-pointed or deleted.
    CONSTRAINT invoice_extraction_reviews_invoice_fk FOREIGN KEY (invoice_id, document_id, organization_id)
        REFERENCES invoices (id, source_document_id, organization_id),
    CONSTRAINT invoice_extraction_reviews_member_fk FOREIGN KEY (organization_id, confirmed_by)
        REFERENCES memberships (organization_id, user_id)
);
CREATE INDEX idx_invoice_extraction_reviews_org ON invoice_extraction_reviews (organization_id);

ALTER TABLE invoice_extraction_reviews ENABLE ROW LEVEL SECURITY;
ALTER TABLE invoice_extraction_reviews FORCE ROW LEVEL SECURITY;
CREATE POLICY tenant_isolation ON invoice_extraction_reviews
    USING (organization_id = current_org_id())
    WITH CHECK (organization_id = current_org_id());

-- Immutable for the runtime role: no UPDATE, no DELETE (same pattern as audit_logs in 0002).
REVOKE ALL ON invoice_extraction_reviews FROM xflow_app;
GRANT SELECT, INSERT ON invoice_extraction_reviews TO xflow_app;
