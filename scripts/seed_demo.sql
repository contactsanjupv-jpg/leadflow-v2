-- Usage: psql -d xflow_dev -v org_id=<ORG UUID> -f scripts/seed_demo.sql   (run as a privileged role)
-- Demo case from spec section 55: contract $400/drum + $500 transport authorized; invoice bills $450/drum x10 + $500 + $200 unauthorized fee.
INSERT INTO vendors (id, organization_id, name) VALUES ('aaaaaaaa-0000-0000-0000-000000000001', :'org_id', 'Demo Hazardous Waste Vendor');
INSERT INTO contracts (id, organization_id, vendor_id, name) VALUES ('aaaaaaaa-0000-0000-0000-000000000002', :'org_id', 'aaaaaaaa-0000-0000-0000-000000000001', 'Demo Disposal Contract 2026');
INSERT INTO contract_rates (id, organization_id, contract_id, vendor_id, waste_stream, unit, unit_rate, effective_from)
VALUES ('aaaaaaaa-0000-0000-0000-000000000003', :'org_id', 'aaaaaaaa-0000-0000-0000-000000000002', 'aaaaaaaa-0000-0000-0000-000000000001', 'F003', 'drum', 400.00, '2026-01-01');
INSERT INTO contract_authorized_fees (contract_rate_id, fee_code, description, amount) VALUES ('aaaaaaaa-0000-0000-0000-000000000003', 'TRANSPORT', 'Transportation', 500.00);
INSERT INTO invoices (id, organization_id, vendor_id, invoice_number, invoice_date, service_date, waste_stream, total)
VALUES ('aaaaaaaa-0000-0000-0000-000000000004', :'org_id', 'aaaaaaaa-0000-0000-0000-000000000001', 'INV-1001', '2026-03-15', '2026-03-10', 'F003', 5200.00);
INSERT INTO invoice_line_items (invoice_id, organization_id, description, fee_code, quantity, unit, unit_price, line_total) VALUES
('aaaaaaaa-0000-0000-0000-000000000004', :'org_id', 'Drum disposal', NULL, 10, 'drum', 450.00, 4500.00),
('aaaaaaaa-0000-0000-0000-000000000004', :'org_id', 'Transportation', 'TRANSPORT', 1, 'flat', 500.00, 500.00),
('aaaaaaaa-0000-0000-0000-000000000004', :'org_id', 'Environmental fee', 'ENV_FEE', 1, 'flat', 200.00, 200.00);
