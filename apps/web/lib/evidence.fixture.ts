// SYNTHETIC package (hand-written, multi-document) used to exercise linked source documents, fee labels and
// partial pricing. The CT-2026-0915 fixture next to it is the backend-generated one.
export function sampleContent(): Record<string, unknown> {
  const rate = { contract_rate_id: "rate-1", contract_id: "c-1", contract_name: "Example Disposal Agreement 2026", contract_source_document_id: "doc-contract", waste_stream: "F003", unit: "ton", unit_rate: "450.0000", effective_from: "2026-01-01", effective_to: null, priced_line_ids: ["line-1"],
    authorized_fees: [{ fee_code: "TRANSPORT", description: "Transport", amount: "250.0000", charge_type: "CAP" }, { fee_code: "FUEL", description: null, amount: "40.0000", charge_type: null }] };
  return {
    format_version: 1,
    discrepancy: { id: "d-1", invoice_id: "inv-1", invoice_number: "CT-2026-0915", vendor_id: "ven-1", vendor_name: "Example Vendor", discrepancy_type: "RATE_MISMATCH", rule_id: "RULE_03_RATE_MISMATCH", description: "Billed rate differs from contract rate", amount: "700.0000", confidence: "HIGH", review_status: "NEW", reviewed_at: null, review_notes: null },
    reconciliation: { id: "rec-1", pricing_status: "COMPLETE", expected_amount: "4500.0000", invoiced_amount: "5200.0000", discrepancy_amount: "700.0000", partial_expected_amount: null, identified_finding_amount: "700.0000", engine_version: "2", created_at: "2026-09-16T10:00:00Z",
      calculation_trace: ["Invoice CT-2026-0915 service date 2026-09-10", "Line 1: 10 ton x 450.0000 = 4500.0000", "Billed 5200.0000 - expected 4500.0000 = 700.0000"] },
    invoice: { id: "inv-1", invoice_number: "CT-2026-0915", invoice_date: "2026-09-15", service_date: "2026-09-10", waste_stream: "F003", total: "5200.0000", source_document_id: "doc-invoice",
      lines: [{ description: "Disposal", fee_code: null, quantity: "10.0000", unit: "ton", unit_price: "520.0000", line_total: "5200.0000" }] },
    contract_rate: rate,
    priced_lines: [{ invoice_line_item_id: "line-1", description: "Disposal", quantity: "10.0000", invoice_unit: "ton", unit: "ton", unit_rate: "450.0000", contract_name: "Example Disposal Agreement 2026", expected_line_amount: "4500.0000" }],
    applicable_rates_basis: "recorded",
    applicable_rates: [rate],
    fee_lines: [],
    manifest: { id: "man-1", manifest_number: "MAN-77", waste_stream: "F003", container_count: 4, quantity: "10.0000", unit: "ton", shipment_date: "2026-09-09", source_document_id: "doc-manifest" },
    weight_tickets: [{ id: "w-1", ticket_number: "WT-1", net_weight: "10.0000", unit: "ton", container_count: 4, ticket_date: "2026-09-10", source_document_id: null }],
    source_documents: [
      { id: "doc-invoice", document_type: "INVOICE", original_filename: "CT-2026-0915.pdf", sha256: "a".repeat(64), created_at: "2026-09-16T09:00:00Z" },
      { id: "doc-contract", document_type: "CONTRACT", original_filename: "clean-harbors-contract.pdf", sha256: "b".repeat(64), created_at: "2026-09-01T09:00:00Z" },
    ],
  };
}
