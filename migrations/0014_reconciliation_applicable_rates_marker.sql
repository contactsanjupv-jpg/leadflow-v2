-- Say explicitly whether a reconciliation's applicable-rate set was RECORDED.
--
-- Migration 0013 started recording the rates a reconciliation considered, but "recorded" was only
-- inferable from the presence of rows in reconciliation_applicable_rates. A reconciliation that
-- legitimately had NO applicable rate (nothing covered the service date) also has no rows, so it was
-- indistinguishable from a reconciliation made before 0013, and its evidence was mislabeled as built
-- from line-rate records only.
--
-- The flag is written by the INSERT that creates the reconciliation, so it is committed in the same
-- transaction as the reconciliation and its applicable-rate rows (an empty set is still "recorded").
-- Existing rows get false, which is exactly the legacy state: they are NOT backfilled and no historical
-- row is modified. (A reconciliation made after 0013 and before this migration that has rate rows is
-- still recognised as recorded by those rows; only an empty set from that short window stays legacy.)

ALTER TABLE reconciliations ADD COLUMN applicable_rates_recorded BOOLEAN NOT NULL DEFAULT false;

COMMENT ON COLUMN reconciliations.applicable_rates_recorded IS
    'true when the set of applicable contract rates was recorded for this reconciliation (migration 0013 onwards), even if that set is empty; false for reconciliations created before.';
