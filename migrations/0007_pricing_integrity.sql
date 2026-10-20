-- Reconciliation totals must distinguish complete pricing from partial or unavailable pricing.
-- Existing reconciliations predate this signal; retain their stored values for audit but do not
-- present them as verified bill-level amounts until a fresh reconciliation is run.
ALTER TABLE reconciliations
    ALTER COLUMN expected_amount DROP NOT NULL,
    ALTER COLUMN discrepancy_amount DROP NOT NULL;

ALTER TABLE reconciliations
    ADD COLUMN pricing_status TEXT NOT NULL DEFAULT 'LEGACY_UNVERIFIED'
        CHECK (pricing_status IN ('COMPLETE', 'PARTIAL', 'UNCOMPUTABLE', 'LEGACY_UNVERIFIED')),
    ADD COLUMN partial_expected_amount NUMERIC(14,4),
    ADD COLUMN identified_finding_amount NUMERIC(14,4) NOT NULL DEFAULT 0;

CREATE INDEX idx_reconciliations_pricing_status
    ON reconciliations (organization_id, pricing_status);
