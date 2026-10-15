-- XFlow V2 initial schema
CREATE EXTENSION IF NOT EXISTS "uuid-ossp";

CREATE TABLE organizations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    email TEXT NOT NULL UNIQUE,
    password_hash TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TYPE membership_role AS ENUM ('OWNER', 'ADMIN', 'MEMBER', 'REVIEWER');

CREATE TABLE memberships (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    role membership_role NOT NULL DEFAULT 'MEMBER',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, user_id)
);
CREATE INDEX idx_memberships_org ON memberships(organization_id);
CREATE INDEX idx_memberships_user ON memberships(user_id);

CREATE TABLE sessions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    token_hash TEXT NOT NULL UNIQUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    revoked_at TIMESTAMPTZ
);
CREATE INDEX idx_sessions_user ON sessions(user_id);
CREATE INDEX idx_sessions_expires ON sessions(expires_at);

CREATE TYPE document_status AS ENUM ('UPLOADED', 'PROCESSING', 'PROCESSED', 'FAILED', 'REVIEW_REQUIRED', 'ARCHIVED');
CREATE TYPE document_type AS ENUM ('INVOICE', 'CONTRACT', 'RATE_SHEET', 'MANIFEST', 'WEIGHT_TICKET', 'OTHER');

CREATE TABLE documents (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    uploaded_by UUID NOT NULL REFERENCES users(id),
    document_type document_type NOT NULL,
    source TEXT,
    object_storage_key TEXT NOT NULL,
    original_filename TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    file_size_bytes BIGINT NOT NULL,
    status document_status NOT NULL DEFAULT 'UPLOADED',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_documents_org ON documents(organization_id);
CREATE INDEX idx_documents_status ON documents(organization_id, status);

CREATE TABLE document_extractions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    document_id UUID NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    extractor_provider TEXT NOT NULL,
    raw_extraction JSONB NOT NULL,
    confidence NUMERIC(5,4),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_extractions_document ON document_extractions(document_id);
CREATE INDEX idx_extractions_org ON document_extractions(organization_id);

CREATE TABLE vendors (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_vendors_org ON vendors(organization_id);

CREATE TABLE contracts (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    vendor_id UUID NOT NULL REFERENCES vendors(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    source_document_id UUID REFERENCES documents(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_contracts_org ON contracts(organization_id);
CREATE INDEX idx_contracts_vendor ON contracts(vendor_id);

CREATE TABLE contract_rates (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    contract_id UUID NOT NULL REFERENCES contracts(id) ON DELETE CASCADE,
    vendor_id UUID NOT NULL REFERENCES vendors(id),
    facility_id UUID,
    waste_stream TEXT NOT NULL,
    unit TEXT NOT NULL,
    unit_rate NUMERIC(14,4) NOT NULL,
    effective_from DATE NOT NULL,
    effective_to DATE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT effective_window_valid CHECK (effective_to IS NULL OR effective_to >= effective_from)
);
CREATE INDEX idx_rates_org ON contract_rates(organization_id);
CREATE INDEX idx_rates_lookup ON contract_rates(organization_id, vendor_id, waste_stream, effective_from, effective_to);

CREATE TABLE contract_authorized_fees (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    contract_rate_id UUID NOT NULL REFERENCES contract_rates(id) ON DELETE CASCADE,
    fee_code TEXT NOT NULL,
    description TEXT,
    amount NUMERIC(14,4) NOT NULL,
    UNIQUE (contract_rate_id, fee_code)
);

CREATE TABLE invoices (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    vendor_id UUID NOT NULL REFERENCES vendors(id),
    invoice_number TEXT NOT NULL,
    invoice_date DATE NOT NULL,
    service_date DATE NOT NULL,
    facility_id UUID,
    waste_stream TEXT,
    total NUMERIC(14,4) NOT NULL,
    source_document_id UUID REFERENCES documents(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, vendor_id, invoice_number)
);
CREATE INDEX idx_invoices_org ON invoices(organization_id);
CREATE INDEX idx_invoices_service_date ON invoices(organization_id, service_date);

CREATE TABLE invoice_line_items (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    invoice_id UUID NOT NULL REFERENCES invoices(id) ON DELETE CASCADE,
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    description TEXT NOT NULL,
    fee_code TEXT,
    quantity NUMERIC(14,4) NOT NULL,
    unit TEXT NOT NULL,
    unit_price NUMERIC(14,4) NOT NULL,
    line_total NUMERIC(14,4) NOT NULL
);
CREATE INDEX idx_line_items_invoice ON invoice_line_items(invoice_id);

CREATE TABLE manifests (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    manifest_number TEXT NOT NULL,
    waste_stream TEXT NOT NULL,
    container_count INTEGER,
    quantity NUMERIC(14,4) NOT NULL,
    unit TEXT NOT NULL,
    shipment_date DATE NOT NULL,
    source_document_id UUID REFERENCES documents(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, manifest_number)
);
CREATE INDEX idx_manifests_org ON manifests(organization_id);
CREATE INDEX idx_manifests_shipment_date ON manifests(organization_id, shipment_date);

CREATE TABLE weight_records (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    manifest_id UUID REFERENCES manifests(id) ON DELETE SET NULL,
    ticket_number TEXT NOT NULL,
    net_weight NUMERIC(14,4) NOT NULL,
    unit TEXT NOT NULL,
    container_count INTEGER,
    ticket_date DATE NOT NULL,
    source_document_id UUID REFERENCES documents(id),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (organization_id, ticket_number)
);
CREATE INDEX idx_weight_records_org ON weight_records(organization_id);
CREATE INDEX idx_weight_records_manifest ON weight_records(manifest_id);

CREATE TYPE review_status AS ENUM ('NEW', 'UNDER_REVIEW', 'CONFIRMED', 'REJECTED', 'NEEDS_INFORMATION', 'DISPUTED', 'RESOLVED');

CREATE TABLE reconciliations (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    invoice_id UUID NOT NULL REFERENCES invoices(id) ON DELETE CASCADE,
    contract_rate_id UUID REFERENCES contract_rates(id),
    manifest_id UUID REFERENCES manifests(id),
    weight_record_id UUID REFERENCES weight_records(id),
    expected_amount NUMERIC(14,4) NOT NULL,
    invoiced_amount NUMERIC(14,4) NOT NULL,
    discrepancy_amount NUMERIC(14,4) NOT NULL,
    review_status review_status NOT NULL DEFAULT 'NEW',
    tolerances_applied JSONB NOT NULL,
    calculation_trace JSONB NOT NULL,
    engine_version TEXT NOT NULL DEFAULT 'v1',
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_reconciliations_org ON reconciliations(organization_id);
CREATE INDEX idx_reconciliations_invoice ON reconciliations(invoice_id);
CREATE INDEX idx_reconciliations_status ON reconciliations(organization_id, review_status);

CREATE TYPE discrepancy_type AS ENUM (
    'QUANTITY_MISMATCH', 'RATE_MISMATCH', 'UNAUTHORIZED_FEE', 'DUPLICATE_CHARGE',
    'WRONG_WASTE_STREAM', 'WRONG_SERVICE_DATE', 'WRONG_FACILITY', 'CONTRACT_TERM_MISMATCH',
    'WEIGHT_MISMATCH', 'CONTAINER_COUNT_MISMATCH', 'MISSING_SUPPORT', 'OTHER'
);

CREATE TABLE discrepancies (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    reconciliation_id UUID NOT NULL REFERENCES reconciliations(id) ON DELETE CASCADE,
    discrepancy_type discrepancy_type NOT NULL,
    rule_id TEXT NOT NULL,
    description TEXT NOT NULL,
    amount NUMERIC(14,4) NOT NULL,
    confidence TEXT NOT NULL,
    review_status review_status NOT NULL DEFAULT 'NEW',
    reviewed_by UUID REFERENCES users(id),
    reviewed_at TIMESTAMPTZ,
    review_notes TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_discrepancies_org ON discrepancies(organization_id);
CREATE INDEX idx_discrepancies_reconciliation ON discrepancies(reconciliation_id);
CREATE INDEX idx_discrepancies_status ON discrepancies(organization_id, review_status);
CREATE INDEX idx_discrepancies_vendor_pattern ON discrepancies(organization_id, discrepancy_type);

CREATE TABLE evidence_packages (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    discrepancy_id UUID NOT NULL REFERENCES discrepancies(id) ON DELETE CASCADE,
    generated_by UUID REFERENCES users(id),
    content JSONB NOT NULL,
    pdf_object_storage_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_evidence_org ON evidence_packages(organization_id);
CREATE INDEX idx_evidence_discrepancy ON evidence_packages(discrepancy_id);

CREATE TYPE dispute_status AS ENUM ('OPEN', 'PREPARE', 'REVIEW', 'SUBMITTED', 'VENDOR_RESPONDED', 'CREDIT_PROMISED', 'CREDIT_RECEIVED', 'VERIFIED', 'CLOSED');

CREATE TABLE disputes (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    discrepancy_id UUID NOT NULL REFERENCES discrepancies(id) ON DELETE CASCADE,
    vendor_id UUID NOT NULL REFERENCES vendors(id),
    status dispute_status NOT NULL DEFAULT 'OPEN',
    disputed_amount NUMERIC(14,4) NOT NULL,
    confirmed_amount NUMERIC(14,4),
    submitted_at TIMESTAMPTZ,
    resolved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_disputes_org ON disputes(organization_id);
CREATE INDEX idx_disputes_status ON disputes(organization_id, status);

CREATE TABLE credits (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    dispute_id UUID NOT NULL REFERENCES disputes(id) ON DELETE CASCADE,
    identified_amount NUMERIC(14,4) NOT NULL,
    confirmed_amount NUMERIC(14,4),
    recovered_amount NUMERIC(14,4),
    credit_memo_reference TEXT,
    verified_by UUID REFERENCES users(id),
    verified_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_credits_org ON credits(organization_id);
CREATE INDEX idx_credits_dispute ON credits(dispute_id);

CREATE TABLE monitoring_jobs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'PENDING',
    payload JSONB,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 5,
    scheduled_for TIMESTAMPTZ NOT NULL DEFAULT now(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_monitoring_jobs_org ON monitoring_jobs(organization_id);
CREATE INDEX idx_monitoring_jobs_status ON monitoring_jobs(status, scheduled_for);

CREATE TABLE notifications (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL REFERENCES organizations(id) ON DELETE CASCADE,
    user_id UUID REFERENCES users(id),
    event_type TEXT NOT NULL,
    payload JSONB NOT NULL,
    read_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_notifications_org ON notifications(organization_id);
CREATE INDEX idx_notifications_user ON notifications(user_id, read_at);

CREATE TABLE audit_logs (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID REFERENCES organizations(id) ON DELETE CASCADE,
    actor_user_id UUID REFERENCES users(id),
    action TEXT NOT NULL,
    resource_type TEXT NOT NULL,
    resource_id UUID,
    before_state JSONB,
    after_state JSONB,
    request_id UUID,
    ip_address INET,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_audit_logs_org ON audit_logs(organization_id, created_at);
CREATE INDEX idx_audit_logs_resource ON audit_logs(resource_type, resource_id);

CREATE TABLE subscriptions (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    organization_id UUID NOT NULL UNIQUE REFERENCES organizations(id) ON DELETE CASCADE,
    paddle_subscription_id TEXT UNIQUE,
    paddle_customer_id TEXT,
    plan TEXT,
    status TEXT NOT NULL DEFAULT 'NONE',
    current_period_end TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE billing_events (
    id UUID PRIMARY KEY DEFAULT uuid_generate_v4(),
    paddle_event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    occurred_at TIMESTAMPTZ NOT NULL,
    payload JSONB NOT NULL,
    processed_at TIMESTAMPTZ,
    processing_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX idx_billing_events_occurred ON billing_events(occurred_at);
