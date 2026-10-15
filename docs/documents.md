# Documents
Upload validates type, magic bytes and size, rejects duplicate content (SHA-256), stores bytes via `ObjectStorage` (local FS) and runs the extractor chain:
1. `PdfTextExtractor` - born-digital PDFs only, via poppler `pdftotext` (fixed argv, no shell, timeout, output cap).
2. `PlainTextExtractor` - text/plain and text/csv.

**There is no OCR.** Scans, photos, corrupt/encrypted PDFs, and a missing `pdftotext` all end as REVIEW_REQUIRED with a reason and no extraction row. Even successful extraction only stores text for a person to read; nothing creates invoices/rates/manifests.

Manual fallback: enter the data in the normal forms and pick the **Source document** (marks it PROCESSED automatically), or use "Mark as manually processed" (`POST /documents/:id/resolve`, only from REVIEW_REQUIRED/FAILED).
