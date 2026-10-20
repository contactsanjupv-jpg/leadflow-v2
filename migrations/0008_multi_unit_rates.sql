-- Allow overlapping effective windows for different units.
-- Continue rejecting overlapping rates for the same vendor,
-- canonical waste stream, facility scope, and unit.

ALTER TABLE contract_rates
    DROP CONSTRAINT contract_rates_no_overlap;

ALTER TABLE contract_rates
    ADD CONSTRAINT contract_rates_no_overlap
    EXCLUDE USING gist (
        vendor_id WITH =,
        waste_stream_key WITH =,
        (COALESCE(
            facility_id,
            '00000000-0000-0000-0000-000000000000'::uuid
        )) WITH =,
        unit WITH =,
        daterange(effective_from, effective_to, '[]') WITH &&
    );