-- 0006: canonical waste-stream key (additive; 0001-0005 are never edited).
--
-- Problem: waste streams were compared with exact string equality, so 'F003' and 'f003' (or
-- ' F003 ') did not match the same contract rate and a valid rate "disappeared".
--
-- Strategy: the text a user entered is preserved untouched (audit trail, evidence, PDFs). A
-- GENERATED column holds the canonical comparison key = trim, collapse internal whitespace,
-- upper-case. Rate lookups, the monitoring query and the overlap constraint use the key.
-- Behavior: 'F003' = 'f003' = ' F003 '. Internal whitespace is COLLAPSED to one space, never
-- removed: 'F 003' = 'F   003' but 'F 003' is NOT the same identifier as 'F003' (no domain rule
-- allows dropping it). 'F003', 'F03', 'F0030' and 'D001' are all distinct.
-- Only contract_rates and invoices need the column (the only places a key is compared in SQL);
-- manifests are compared in Rust (RULE_08) using the same expression.
-- The Rust twin of this expression is common::waste_stream::key (a test asserts they agree).

ALTER TABLE contract_rates ADD COLUMN waste_stream_key TEXT GENERATED ALWAYS AS
    (NULLIF(upper(btrim(regexp_replace(waste_stream, '[[:space:]]+', ' ', 'g'))), '')) STORED;
ALTER TABLE invoices ADD COLUMN waste_stream_key TEXT GENERATED ALWAYS AS
    (NULLIF(upper(btrim(regexp_replace(waste_stream, '[[:space:]]+', ' ', 'g'))), '')) STORED;

CREATE INDEX idx_rates_lookup_key ON contract_rates(organization_id, vendor_id, waste_stream_key, effective_from, effective_to);

-- Preflight: existing rates that differ only by case/whitespace and overlap in time would make
-- "the applicable rate" ambiguous once they are treated as the same stream. Never merge or delete
-- legacy rows silently: fail loudly so a human decides which rate is right.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM contract_rates a JOIN contract_rates b
          ON a.id < b.id AND a.vendor_id = b.vendor_id AND a.waste_stream_key = b.waste_stream_key
         AND COALESCE(a.facility_id, '00000000-0000-0000-0000-000000000000'::uuid) = COALESCE(b.facility_id, '00000000-0000-0000-0000-000000000000'::uuid)
         AND daterange(a.effective_from, a.effective_to, '[]') && daterange(b.effective_from, b.effective_to, '[]')
         AND a.waste_stream <> b.waste_stream
    ) THEN
        RAISE EXCEPTION '0006: overlapping contract_rates differ only by waste-stream case/whitespace; resolve them (end or correct one rate) before migrating';
    END IF;
END $$;

-- Rebuild the overlap guard on the canonical key (same constraint name: the API maps it to 409).
ALTER TABLE contract_rates DROP CONSTRAINT contract_rates_no_overlap;
ALTER TABLE contract_rates ADD CONSTRAINT contract_rates_no_overlap EXCLUDE USING gist (
    vendor_id WITH =,
    waste_stream_key WITH =,
    (COALESCE(facility_id, '00000000-0000-0000-0000-000000000000'::uuid)) WITH =,
    daterange(effective_from, effective_to, '[]') WITH &&
);
