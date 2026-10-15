//! Document extraction abstraction (spec: provider-abstracted OCR).
//!
//! Implemented providers: `PlainTextExtractor` (text/plain, text/csv) and `PdfTextExtractor`, which
//! reads the embedded TEXT LAYER of born-digital PDFs by running poppler's `pdftotext`. There is NO OCR:
//! scanned PDFs (no text layer) and images are stored and reported as `Unsupported` with an explicit
//! reason, so the document becomes REVIEW_REQUIRED and a person enters the data through the normal
//! ingestion endpoints, linking the document as its source (which marks it PROCESSED).
//! Extraction output is stored for review and is NEVER turned into invoices/manifests/rates
//! automatically, and never feeds the financial engine. `ChainExtractor` tries providers in order.

use serde_json::{json, Value};

#[derive(Debug)]
pub enum ExtractionOutcome {
    Extracted {
        provider: &'static str,
        raw: Value,
        confidence: Option<rust_decimal::Decimal>,
    },
    /// A provider handles this content type but could not read this file (or none handles it).
    Unsupported { reason: String },
    /// This provider does not handle this content type at all (lets a chain move on silently).
    NotApplicable,
}

#[axum::async_trait]
pub trait DocumentExtractor: Send + Sync {
    async fn extract(&self, mime_type: &str, bytes: &[u8]) -> anyhow::Result<ExtractionOutcome>;
}

pub struct PlainTextExtractor;

#[axum::async_trait]
impl DocumentExtractor for PlainTextExtractor {
    async fn extract(&self, mime_type: &str, bytes: &[u8]) -> anyhow::Result<ExtractionOutcome> {
        let base = mime_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if base != "text/plain" && base != "text/csv" {
            return Ok(ExtractionOutcome::Unsupported {
                reason: format!("no extraction provider is configured for content type '{base}'; enter the data manually and link this document as its source"),
            });
        }
        match std::str::from_utf8(bytes) {
            Ok(text) => Ok(ExtractionOutcome::Extracted {
                provider: "plain_text",
                raw: json!({ "text": text, "line_count": text.lines().count() }),
                // Verbatim text is exact; there is no recognition confidence to report.
                confidence: None,
            }),
            Err(_) => Ok(ExtractionOutcome::Unsupported {
                reason: "file is not valid UTF-8 text".to_string(),
            }),
        }
    }
}

/// Reads the embedded text layer of a PDF with `pdftotext` (no shell: fixed argv, bytes on stdin).
/// Bounded in time and output size; any failure degrades to `Unsupported` (manual entry).
pub struct PdfTextExtractor {
    pub binary: String,
    pub timeout: std::time::Duration,
    pub max_output_bytes: usize,
}

impl PdfTextExtractor {
    pub fn new(binary: impl Into<String>) -> Self {
        PdfTextExtractor {
            binary: binary.into(),
            timeout: std::time::Duration::from_secs(20),
            max_output_bytes: 1024 * 1024,
        }
    }

    async fn run(&self, bytes: &[u8]) -> Result<Vec<u8>, String> {
        use std::process::Stdio;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut child = tokio::process::Command::new(&self.binary)
            .args(["-layout", "-q", "-enc", "UTF-8", "-", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|_| {
                "the PDF text extraction tool (pdftotext) is not installed on this server"
                    .to_string()
            })?;
        let mut stdin = child.stdin.take().ok_or("could not open extractor input")?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or("could not open extractor output")?;
        let input = bytes.to_vec();
        let max = self.max_output_bytes;
        let work = async move {
            let writer = tokio::spawn(async move {
                let _ = stdin.write_all(&input).await; // a failed write surfaces as a non-zero exit below
                drop(stdin);
            });
            let mut out = Vec::new();
            (&mut stdout)
                .take(max as u64 + 1)
                .read_to_end(&mut out)
                .await
                .map_err(|e| e.to_string())?;
            let _ = writer.await;
            let status = child.wait().await.map_err(|e| e.to_string())?;
            Ok::<_, String>((out, status.success()))
        };
        match tokio::time::timeout(self.timeout, work).await {
            Err(_) => Err("reading the PDF took too long".to_string()),
            Ok(Err(e)) => Err(e),
            Ok(Ok((out, ok))) => {
                if !ok {
                    return Err(
                        "the PDF could not be read (it may be corrupt or password-protected)"
                            .to_string(),
                    );
                }
                Ok(out)
            }
        }
    }
}

#[axum::async_trait]
impl DocumentExtractor for PdfTextExtractor {
    async fn extract(&self, mime_type: &str, bytes: &[u8]) -> anyhow::Result<ExtractionOutcome> {
        if mime_type
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase()
            != "application/pdf"
        {
            return Ok(ExtractionOutcome::NotApplicable);
        }
        let manual = "enter the data manually and link this document as its source";
        let out = match self.run(bytes).await {
            Ok(o) => o,
            Err(why) => {
                return Ok(ExtractionOutcome::Unsupported {
                    reason: format!("{why}; {manual}"),
                })
            }
        };
        let truncated = out.len() > self.max_output_bytes;
        let text =
            String::from_utf8_lossy(&out[..out.len().min(self.max_output_bytes)]).to_string();
        let meaningful = text.chars().filter(|c| c.is_alphanumeric()).count();
        if meaningful < 20 {
            return Ok(ExtractionOutcome::Unsupported {
                reason: format!("this PDF has no readable text layer (it is probably a scan, and OCR is not enabled); {manual}"),
            });
        }
        Ok(ExtractionOutcome::Extracted {
            provider: "pdftotext",
            raw: json!({ "text": text, "line_count": text.lines().count(), "truncated": truncated }),
            confidence: None,
        })
    }
}

/// Tries each extractor in order: the first `Extracted` wins; otherwise the first provider-specific
/// `Unsupported` explanation is reported (falling back to a generic one).
pub struct ChainExtractor {
    pub extractors: Vec<std::sync::Arc<dyn DocumentExtractor>>,
}

#[axum::async_trait]
impl DocumentExtractor for ChainExtractor {
    async fn extract(&self, mime_type: &str, bytes: &[u8]) -> anyhow::Result<ExtractionOutcome> {
        let mut first_unsupported: Option<String> = None;
        for e in &self.extractors {
            match e.extract(mime_type, bytes).await? {
                done @ ExtractionOutcome::Extracted { .. } => return Ok(done),
                ExtractionOutcome::Unsupported { reason } => {
                    first_unsupported.get_or_insert(reason);
                }
                ExtractionOutcome::NotApplicable => {}
            }
        }
        Ok(ExtractionOutcome::Unsupported {
            reason: first_unsupported.unwrap_or_else(|| format!("no extraction provider is configured for content type '{mime_type}'; enter the data manually and link this document as its source")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn extracts_utf8_text_and_reports_line_count() {
        let out = PlainTextExtractor
            .extract("text/plain; charset=utf-8", b"a\nb\nc")
            .await
            .unwrap();
        match out {
            ExtractionOutcome::Extracted { raw, provider, .. } => {
                assert_eq!(provider, "plain_text");
                assert_eq!(raw["line_count"], 3);
            }
            other => panic!("expected extraction, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn pdf_and_images_are_explicitly_unsupported_not_faked() {
        for mime in ["application/pdf", "image/png"] {
            match PlainTextExtractor.extract(mime, b"%PDF-1.7").await.unwrap() {
                ExtractionOutcome::Unsupported { reason } => {
                    assert!(reason.contains("no extraction provider"))
                }
                other => panic!("{mime}: expected Unsupported, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn invalid_utf8_is_unsupported() {
        assert!(matches!(
            PlainTextExtractor
                .extract("text/csv", &[0xff, 0xfe, 0x00])
                .await
                .unwrap(),
            ExtractionOutcome::Unsupported { .. }
        ));
    }

    // ---- PdfTextExtractor / ChainExtractor (these use the real `pdftotext` binary when present) ----
    fn have_pdftotext() -> bool {
        std::process::Command::new("pdftotext")
            .arg("-v")
            .output()
            .is_ok()
    }

    /// A tiny born-digital PDF with a real text layer, built by our own writer.
    fn text_pdf() -> Vec<u8> {
        let mut b = crate::evidence_pdf::PdfBuilder::new("t");
        b.title("Hazardous waste manifest M-1001 drum disposal");
        b.row("Quantity 10 drums waste stream F003 shipped 2026-03-09");
        b.finish()
    }

    #[tokio::test]
    async fn pdf_with_a_text_layer_is_extracted() {
        if !have_pdftotext() {
            eprintln!("SKIP: pdftotext not installed");
            return;
        }
        match PdfTextExtractor::new("pdftotext")
            .extract("application/pdf", &text_pdf())
            .await
            .unwrap()
        {
            ExtractionOutcome::Extracted { provider, raw, .. } => {
                assert_eq!(provider, "pdftotext");
                assert!(raw["text"].as_str().unwrap().contains("M-1001"));
            }
            other => panic!("expected extraction, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn corrupt_pdf_and_missing_tool_degrade_to_manual_entry_never_a_fake_result() {
        if have_pdftotext() {
            match PdfTextExtractor::new("pdftotext")
                .extract("application/pdf", b"%PDF-1.7 not really a pdf")
                .await
                .unwrap()
            {
                ExtractionOutcome::Unsupported { reason } => {
                    assert!(reason.contains("enter the data manually"), "{reason}")
                }
                other => panic!("expected Unsupported, got {other:?}"),
            }
        }
        match PdfTextExtractor::new("/nonexistent/pdftotext")
            .extract("application/pdf", &text_pdf())
            .await
            .unwrap()
        {
            ExtractionOutcome::Unsupported { reason } => assert!(
                reason.contains("not installed") && reason.contains("manually"),
                "{reason}"
            ),
            other => panic!("expected Unsupported, got {other:?}"),
        }
        assert!(matches!(
            PdfTextExtractor::new("pdftotext")
                .extract("image/png", b"x")
                .await
                .unwrap(),
            ExtractionOutcome::NotApplicable
        ));
    }

    #[tokio::test]
    async fn chain_routes_by_type_and_reports_the_specific_reason() {
        use std::sync::Arc;
        let chain = ChainExtractor {
            extractors: vec![
                Arc::new(PdfTextExtractor::new("/nonexistent/pdftotext")),
                Arc::new(PlainTextExtractor),
            ],
        };
        assert!(matches!(
            chain.extract("text/plain", b"hello").await.unwrap(),
            ExtractionOutcome::Extracted {
                provider: "plain_text",
                ..
            }
        ));
        match chain.extract("application/pdf", &text_pdf()).await.unwrap() {
            ExtractionOutcome::Unsupported { reason } => assert!(
                reason.contains("pdftotext"),
                "PDF-specific reason must win over the generic one: {reason}"
            ),
            other => panic!("{other:?}"),
        }
        match chain.extract("image/jpeg", b"\xff\xd8\xff").await.unwrap() {
            ExtractionOutcome::Unsupported { reason } => {
                assert!(reason.contains("no extraction provider") && reason.contains("manually"))
            }
            other => panic!("{other:?}"),
        }
    }
}
