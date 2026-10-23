-- Authorized fees must state whether the contracted amount is an exact fixed charge or a maximum cap.
-- Existing rows predate this distinction and nothing in the schema, API, UI or documentation verified
-- which meaning applied. They are therefore left unclassified (NULL): no default and no backfill.
-- The reconciliation engine must never treat an unclassified fee as EXACT or CAP; it must report it for
-- review until a person explicitly classifies it. Historical reconciliations are not modified.
--
-- Table-level grants and the tenant_isolation_via_rate policy from 0002 already cover these new columns.

ALTER TABLE contract_authorized_fees
    ADD COLUMN charge_type TEXT,
    ADD COLUMN charge_type_set_at TIMESTAMPTZ,
    ADD COLUMN charge_type_set_by UUID REFERENCES users(id) ON DELETE SET NULL;

-- NULL (unclassified) satisfies this CHECK on purpose.
ALTER TABLE contract_authorized_fees
    ADD CONSTRAINT contract_authorized_fees_charge_type_valid
        CHECK (charge_type IN ('EXACT', 'CAP')),
    -- A classified fee always records when it was classified; an unclassified fee never does.
    -- Monitoring uses charge_type_set_at to re-reconcile invoices after a fee is classified.
    ADD CONSTRAINT contract_authorized_fees_charge_type_audited
        CHECK ((charge_type IS NULL) = (charge_type_set_at IS NULL)),
    -- Only a classified fee can name who classified it (NULL is allowed for system or fixture rows).
    ADD CONSTRAINT contract_authorized_fees_charge_type_actor
        CHECK (charge_type_set_by IS NULL OR charge_type IS NOT NULL);

COMMENT ON COLUMN contract_authorized_fees.charge_type IS
    'EXACT: amount is an exact fixed charge. CAP: amount is a maximum; a bill at or below it is not a discrepancy. NULL: unclassified legacy fee, flagged for review and never inferred.';
COMMENT ON COLUMN contract_authorized_fees.charge_type_set_at IS
    'When charge_type was set. NULL while the fee is unclassified.';
COMMENT ON COLUMN contract_authorized_fees.charge_type_set_by IS
    'User who set charge_type; NULL for unclassified, system or fixture rows.';

-- A classified fee is immutable. Changing the meaning of a contracted fee requires a new rate version
-- so earlier reconciliations and evidence remain explainable. charge_type_set_by is deliberately not
-- guarded so that deleting a user (ON DELETE SET NULL) is not blocked.
CREATE FUNCTION contract_authorized_fees_guard_classification() RETURNS trigger AS $$
BEGIN
    IF OLD.charge_type IS NOT NULL
       AND (NEW.charge_type IS DISTINCT FROM OLD.charge_type
            OR NEW.charge_type_set_at IS DISTINCT FROM OLD.charge_type_set_at) THEN
        RAISE EXCEPTION 'authorized fee charge_type is immutable once set (fee_code %, rate %)',
            OLD.fee_code, OLD.contract_rate_id
            USING ERRCODE = 'check_violation';
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER trg_contract_authorized_fees_guard_classification
    BEFORE UPDATE ON contract_authorized_fees
    FOR EACH ROW
    EXECUTE FUNCTION contract_authorized_fees_guard_classification();