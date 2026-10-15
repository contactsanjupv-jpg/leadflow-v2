//! Evidence package PDF rendering. Dependency-free, text-only PDF 1.4 writer using the built-in
//! base-14 fonts (Courier for body so wrapping/alignment is exact, Helvetica-Bold for headings).
//! Output is deterministic for a given input. Text is limited to what WinAnsi/ASCII can carry:
//! other characters are replaced with '?' (the full-fidelity record stays in the JSON package).

use serde_json::Value;

const PAGE_W: f32 = 595.0; // A4
const PAGE_H: f32 = 842.0;
const MARGIN: f32 = 50.0;
const BODY_SIZE: f32 = 9.0;
const LINE_H: f32 = 12.0;
const CHAR_W: f32 = 5.4; // Courier is 0.6 em wide
const MAX_COLS: usize = ((PAGE_W - 2.0 * MARGIN) / CHAR_W) as usize; // 91

#[derive(Clone, Copy, PartialEq)]
enum Font {
    Body,
    Bold,
}

struct Item {
    font: Font,
    size: f32,
    y: f32,
    x: f32,
    text: String,
}

/// Minimal page-flow layout + serialization.
pub struct PdfBuilder {
    pages: Vec<Vec<Item>>,
    y: f32,
    title: String,
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c == '\t' { ' ' } else if (' '..='~').contains(&c) { c } else if c == '\n' || c == '\r' { ' ' } else { '?' }).collect()
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '(' | ')' | '\\' => { o.push('\\'); o.push(c); }
            _ => o.push(c),
        }
    }
    o
}

/// Word-wrap to `cols`, hard-breaking tokens longer than a line. `indent` continuation lines.
fn wrap(text: &str, cols: usize, indent: usize) -> Vec<String> {
    let text = sanitize(text);
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let pad = " ".repeat(indent);
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        loop {
            let limit = if lines.is_empty() { cols } else { cols };
            let room = limit.saturating_sub(cur.chars().count());
            let need = word.chars().count() + if cur.trim().is_empty() { 0 } else { 1 };
            if need <= room {
                if !cur.trim().is_empty() { cur.push(' '); }
                cur.push_str(&word);
                break;
            }
            if cur.trim().is_empty() {
                // token longer than a whole line: hard break
                let head: String = word.chars().take(limit.saturating_sub(cur.chars().count()).max(1)).collect();
                let rest: String = word.chars().skip(head.chars().count()).collect();
                cur.push_str(&head);
                lines.push(std::mem::take(&mut cur));
                cur = pad.clone();
                word = rest;
                if word.is_empty() { break; }
            } else {
                lines.push(std::mem::take(&mut cur));
                cur = pad.clone();
            }
        }
    }
    if !cur.trim().is_empty() || lines.is_empty() { lines.push(cur); }
    lines
}

impl PdfBuilder {
    pub fn new(title: &str) -> Self {
        PdfBuilder { pages: vec![Vec::new()], y: PAGE_H - MARGIN, title: sanitize(title) }
    }

    fn ensure_room(&mut self, needed: f32) {
        if self.y - needed < MARGIN + 20.0 {
            self.pages.push(Vec::new());
            self.y = PAGE_H - MARGIN;
        }
    }

    fn push(&mut self, font: Font, size: f32, x: f32, text: String, advance: f32) {
        self.ensure_room(advance);
        let y = self.y;
        self.pages.last_mut().unwrap().push(Item { font, size, y, x, text });
        self.y -= advance;
    }

    pub fn title(&mut self, text: &str) {
        self.push(Font::Bold, 16.0, MARGIN, sanitize(text), 24.0);
    }

    pub fn heading(&mut self, text: &str) {
        self.y -= 6.0;
        self.push(Font::Bold, 11.0, MARGIN, sanitize(text), 16.0);
    }

    pub fn blank(&mut self) { self.y -= LINE_H; }

    /// Wrapped body paragraph. `indent` shifts the whole block (in characters).
    pub fn para(&mut self, text: &str, indent: usize) {
        for l in wrap(text, MAX_COLS.saturating_sub(indent), 2) {
            self.push(Font::Body, BODY_SIZE, MARGIN + indent as f32 * CHAR_W, l, LINE_H);
        }
    }

    /// "Label: value" with the value wrapped under a hanging indent.
    pub fn field(&mut self, label: &str, value: &str) {
        let label = sanitize(label);
        let first = format!("{label}: ");
        let cols = MAX_COLS;
        let avail = cols.saturating_sub(first.chars().count()).max(10);
        let mut wrapped = wrap(value, avail, 0).into_iter();
        let head = wrapped.next().unwrap_or_default();
        self.push(Font::Body, BODY_SIZE, MARGIN, format!("{first}{head}"), LINE_H);
        for l in wrapped {
            self.push(Font::Body, BODY_SIZE, MARGIN + first.chars().count() as f32 * CHAR_W, l, LINE_H);
        }
    }

    /// One pre-formatted table row (no wrapping beyond the line width).
    pub fn row(&mut self, text: &str) {
        let t: String = sanitize(text).chars().take(MAX_COLS).collect();
        self.push(Font::Body, BODY_SIZE, MARGIN, t, LINE_H);
    }

    pub fn finish(mut self) -> Vec<u8> {
        let total = self.pages.len();
        for (n, page) in self.pages.iter_mut().enumerate() {
            page.push(Item { font: Font::Body, size: 8.0, y: 30.0, x: MARGIN, text: format!("XFlow evidence package - page {} of {}", n + 1, total) });
        }

        // Object layout: 1 catalog, 2 pages, 3 Courier, 4 Helvetica-Bold, 5 info, then (page, content) pairs.
        let mut objects: Vec<String> = Vec::new();
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_string());
        let kids: Vec<String> = (0..total).map(|i| format!("{} 0 R", 6 + i * 2)).collect();
        objects.push(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids.join(" "), total));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>".to_string());
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>".to_string());
        objects.push(format!("<< /Title ({}) /Producer (XFlow) >>", escape(&self.title)));
        for (i, page) in self.pages.iter().enumerate() {
            let content_id = 7 + i * 2;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {PAGE_W} {PAGE_H}] /Resources << /Font << /F1 3 0 R /F2 4 0 R >> >> /Contents {content_id} 0 R >>"
            ));
            let mut stream = String::new();
            for it in page {
                let f = if it.font == Font::Bold { "F2" } else { "F1" };
                stream.push_str(&format!("BT /{f} {} Tf {} {} Td ({}) Tj ET\n", it.size, it.x, it.y, escape(&it.text)));
            }
            objects.push(format!("<< /Length {} >>\nstream\n{}endstream", stream.len(), stream));
        }

        let mut out: Vec<u8> = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (i, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", i + 1, body).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for off in offsets {
            out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R >>\nstartxref\n{}\n%%EOF\n", objects.len() + 1, xref).as_bytes());
        out
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

/// "5200.0000" -> "$5,200.00" working on the decimal STRING only (no floats).
pub fn money(v: &str) -> String {
    let v = v.trim();
    let (neg, digits) = match v.strip_prefix('-') { Some(r) => (true, r), None => (false, v) };
    let (int, frac) = digits.split_once('.').unwrap_or((digits, ""));
    if int.is_empty() || !int.chars().all(|c| c.is_ascii_digit()) || !frac.chars().all(|c| c.is_ascii_digit()) {
        return v.to_string();
    }
    let mut grouped = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i) % 3 == 0 { grouped.push(','); }
        grouped.push(c);
    }
    let cents = format!("{frac}00");
    let extra = frac.get(2..).unwrap_or("").trim_end_matches('0');
    format!("{}${}.{}{}", if neg { "-" } else { "" }, grouped, &cents[..2], extra)
}

fn cell(v: &str, w: usize) -> String {
    let c: String = sanitize(v).chars().take(w).collect();
    format!("{:<w$}", c, w = w)
}
fn rcell(v: &str, w: usize) -> String {
    let c: String = sanitize(v).chars().take(w).collect();
    format!("{:>w$}", c, w = w)
}

/// Render the stored evidence package (`content` from `evidence_packages`) as a PDF.
pub fn render(organization_name: &str, package_id: &str, generated_at: &str, content: &Value) -> Vec<u8> {
    let d = &content["discrepancy"];
    let r = &content["reconciliation"];
    let inv = &content["invoice"];
    let mut p = PdfBuilder::new(&format!("Evidence package - invoice {}", s(inv, "invoice_number")));

    p.title("Evidence Package");
    p.field("Organization", organization_name);
    p.field("Package ID", package_id);
    p.field("Generated", generated_at);
    p.blank();

    p.heading("1. Discrepancy");
    p.field("Invoice", s(d, "invoice_number"));
    p.field("Vendor", s(d, "vendor_name"));
    p.field("Finding", s(d, "description"));
    p.field("Type / rule", &format!("{} / {}", s(d, "discrepancy_type"), s(d, "rule_id")));
    p.field("Amount in question", &money(s(d, "amount")));
    p.field("Confidence", s(d, "confidence"));
    p.field("Review status", s(d, "review_status"));
    if !s(d, "review_notes").is_empty() { p.field("Reviewer notes", s(d, "review_notes")); }

    p.heading("2. Reconciliation result");
    p.field("Expected amount", &money(s(r, "expected_amount")));
    p.field("Invoiced amount", &money(s(r, "invoiced_amount")));
    p.field("Total discrepancy", &money(s(r, "discrepancy_amount")));
    p.field("Engine version", s(r, "engine_version"));
    p.para("Calculation trace (deterministic engine, exact decimal arithmetic):", 0);
    if let Some(trace) = r["calculation_trace"].as_array() {
        for t in trace { p.para(t.as_str().unwrap_or(""), 2); }
    }

    p.heading("3. Invoice");
    p.field("Invoice number", s(inv, "invoice_number"));
    p.field("Invoice date / service date", &format!("{} / {}", s(inv, "invoice_date"), s(inv, "service_date")));
    p.field("Waste stream", s(inv, "waste_stream"));
    p.field("Invoice total", &money(s(inv, "total")));
    p.row(&format!("{} {} {} {}", cell("Description", 40), rcell("Qty", 9), rcell("Unit price", 15), rcell("Line total", 17)));
    if let Some(lines) = inv["lines"].as_array() {
        for l in lines {
            let mut desc = s(l, "description").to_string();
            if !s(l, "fee_code").is_empty() { desc = format!("{} [{}]", desc, s(l, "fee_code")); }
            p.row(&format!("{} {} {} {}", cell(&desc, 40), rcell(s(l, "quantity"), 9), rcell(&money(s(l, "unit_price")), 15), rcell(&money(s(l, "line_total")), 17)));
        }
    }

    p.heading("4. Contract terms applied");
    let rate = &content["contract_rate"];
    if rate.is_null() {
        p.para("No contract rate covered this invoice's service date.", 0);
    } else {
        p.field("Contract", s(rate, "contract_name"));
        p.field("Waste stream / unit", &format!("{} / {}", s(rate, "waste_stream"), s(rate, "unit")));
        p.field("Contracted rate", &money(s(rate, "unit_rate")));
        p.field("Effective", &format!("{} to {}", s(rate, "effective_from"), if rate["effective_to"].is_null() { "open" } else { s(rate, "effective_to") }));
        if let Some(fees) = rate["authorized_fees"].as_array() {
            for f in fees { p.field("Authorized fee", &format!("{} - {}", s(f, "fee_code"), money(s(f, "amount")))); }
            if fees.is_empty() { p.para("No authorized fees are listed in the contract.", 0); }
        }
    }

    p.heading("5. Supporting documents");
    let m = &content["manifest"];
    if m.is_null() {
        p.para("No manifest was linked to this invoice.", 0);
    } else {
        p.field("Manifest", &format!("{} ({} {}, {} containers, shipped {})", s(m, "manifest_number"), s(m, "quantity"), s(m, "unit"),
            m["container_count"].as_i64().map(|c| c.to_string()).unwrap_or_else(|| "n/a".into()), s(m, "shipment_date")));
        p.field("Manifest waste stream", s(m, "waste_stream"));
    }
    match content["weight_tickets"].as_array() {
        Some(t) if !t.is_empty() => for w in t { p.field("Weight ticket", &format!("{}: {} {} on {}", s(w, "ticket_number"), s(w, "net_weight"), s(w, "unit"), s(w, "ticket_date"))); },
        _ => p.para("No weight tickets were recorded.", 0),
    }
    if let Some(docs) = content["source_documents"].as_array() {
        for doc in docs { p.field("Source document", &format!("{} ({}) sha256 {}", s(doc, "original_filename"), s(doc, "document_type"), s(doc, "sha256"))); }
    }

    p.blank();
    p.para("Amounts are computed deterministically from the recorded contract, invoice, manifest and weight-ticket data. This document is a snapshot taken when the package was generated.", 0);
    p.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn money_formats_decimal_strings_exactly() {
        assert_eq!(money("5200.0000"), "$5,200.00");
        assert_eq!(money("0.1250"), "$0.125");
        assert_eq!(money("-12.5000"), "-$12.50");
        assert_eq!(money("1234567.8900"), "$1,234,567.89");
        assert_eq!(money("n/a"), "n/a");
    }

    #[test]
    fn wrapping_never_exceeds_the_line_width_and_keeps_all_text() {
        let long = "word ".repeat(80) + &"X".repeat(300);
        let lines = wrap(&long, 40, 2);
        assert!(lines.iter().all(|l| l.chars().count() <= 40), "{:?}", lines.iter().map(|l| l.len()).collect::<Vec<_>>());
        let joined: String = lines.join(" ").split_whitespace().collect();
        assert_eq!(joined.matches('X').count(), 300);
        assert_eq!(joined.matches("word").count(), 80);
    }

    #[test]
    fn pdf_structure_is_valid_xref_offsets_are_exact_and_text_is_escaped() {
        let mut b = PdfBuilder::new("T (x)");
        b.title("Hello (world) \\ back");
        for i in 0..200 { b.row(&format!("row {i} \u{00e9}\u{4e2d}")); } // forces multiple pages + non-ASCII
        let pdf = b.finish();
        let text = String::from_utf8_lossy(&pdf).to_string();
        assert!(text.starts_with("%PDF-1.4"));
        assert!(text.trim_end().ends_with("%%EOF"));
        assert!(text.contains("Hello \\(world\\) \\\\ back"));
        assert!(text.contains("row 5 ??"), "non-ASCII is replaced, not emitted raw");
        let pages = text.matches("/Type /Page ").count();
        assert!(pages >= 3, "200 rows must flow onto several pages, got {pages}");
        // Every xref entry must point at "<n> 0 obj" -- checked on RAW BYTES like a real reader (the
        // binary comment line in the header would shift offsets in a lossy UTF-8 string).
        let tail = &pdf[pdf.len() - 64..];
        let tail_s = String::from_utf8_lossy(tail).to_string();
        let xref_pos: usize = tail_s.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
        assert!(pdf[xref_pos..].starts_with(b"xref"));
        let xref_s = String::from_utf8_lossy(&pdf[xref_pos..]).to_string();
        let table: Vec<&str> = xref_s.lines().skip(3).take_while(|l| l.ends_with(" n ")).collect();
        for (i, entry) in table.iter().enumerate() {
            let off: usize = entry[..10].parse().unwrap();
            let want = format!("{} 0 obj", i + 1);
            assert!(pdf[off..].starts_with(want.as_bytes()), "xref entry {} points to the wrong place", i + 1);
        }
        assert_eq!(table.len(), text.matches(" 0 obj\n").count());
    }

    #[test]
    fn render_includes_every_section_and_exact_amounts() {
        let content = json!({
            "discrepancy": { "invoice_number": "INV-1", "vendor_name": "Acme", "description": "Invoiced at 450.0000/drum", "discrepancy_type": "RATE_MISMATCH", "rule_id": "RULE_03_RATE_MISMATCH", "amount": "500.0000", "confidence": "High", "review_status": "CONFIRMED" },
            "reconciliation": { "expected_amount": "4500.0000", "invoiced_amount": "5200.0000", "discrepancy_amount": "700.0000", "engine_version": "1", "calculation_trace": ["Reconciling invoice INV-1"] },
            "invoice": { "invoice_number": "INV-1", "invoice_date": "2026-03-15", "service_date": "2026-03-10", "waste_stream": "F003", "total": "5200.0000",
                         "lines": [{ "description": "Drum disposal", "fee_code": null, "quantity": "10.0000", "unit_price": "450.0000", "line_total": "4500.0000" }] },
            "contract_rate": null, "manifest": null, "weight_tickets": [], "source_documents": []
        });
        let pdf = render("Org", "pkg", "2026-10-01", &content);
        let t = String::from_utf8_lossy(&pdf).to_string();
        for needle in ["Evidence Package", "1. Discrepancy", "$500.00", "$4,500.00", "$5,200.00", "$700.00", "RULE_03_RATE_MISMATCH", "No contract rate covered", "No manifest was linked", "Drum disposal"] {
            assert!(t.contains(needle), "PDF text is missing {needle:?}");
        }
    }
}
