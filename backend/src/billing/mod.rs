//! Paddle Billing integration: webhook signature verification, idempotent event processing and
//! the entitlement rule. There are NO outbound calls to Paddle: checkout runs client-side with
//! Paddle.js, and Paddle tells us about subscription state through signed webhooks only.

use chrono::{DateTime, Utc};
use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use sqlx::PgPool;
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Subscription states that grant access. `past_due` is included deliberately: Paddle keeps
/// retrying the payment (dunning) and cutting access mid-retry would punish paying customers.
/// `canceled`, `paused` and no subscription do not entitle.
pub fn status_is_entitled(status: &str) -> bool {
    matches!(status, "active" | "trialing" | "past_due")
}

pub async fn is_entitled(pool: &PgPool, organization_id: Uuid) -> Result<bool, sqlx::Error> {
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM subscriptions WHERE organization_id = $1").bind(organization_id).fetch_optional(pool).await?;
    Ok(status.map_or(false, |s| status_is_entitled(&s)))
}

/// Backend-issued proof that a checkout belongs to `organization_id`. The checkout page passes it to
/// Paddle in `custom_data.organization_sig`; the webhook only trusts `custom_data.organization_id`
/// when this signature verifies. Without it, anyone could start a checkout carrying another
/// organization's id and later cancel it to revoke that organization's access.
pub fn checkout_signature(secret: &str, organization_id: Uuid) -> String {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(b"xflow-org-link:");
    mac.update(organization_id.to_string().as_bytes());
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn verify_checkout_signature(secret: &str, organization_id: Uuid, signature: &str) -> bool {
    let Some(expected) = hex_decode(signature) else { return false };
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key length");
    mac.update(b"xflow-org-link:");
    mac.update(organization_id.to_string().as_bytes());
    mac.verify_slice(&expected).is_ok()
}

#[derive(Debug, PartialEq, Eq)]
pub enum SignatureError {
    MissingOrMalformed,
    TimestampOutOfTolerance,
    Mismatch,
}

fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect()
}

/// Verify a `Paddle-Signature` header (`ts=<unix>;h1=<hex hmac>`; several `h1` values may be
/// present during secret rotation). The HMAC-SHA256 covers `"<ts>:<raw body>"` and is compared
/// in constant time. The timestamp must be within `tolerance_secs` of `now_unix` (replay defense).
pub fn verify_signature(header: &str, raw_body: &[u8], secret: &str, now_unix: i64, tolerance_secs: i64) -> Result<(), SignatureError> {
    let mut ts: Option<&str> = None;
    let mut sigs: Vec<&str> = Vec::new();
    for part in header.split(';') {
        match part.trim().split_once('=') {
            Some(("ts", v)) => ts = Some(v),
            Some(("h1", v)) => sigs.push(v),
            _ => {}
        }
    }
    let ts_str = ts.ok_or(SignatureError::MissingOrMalformed)?;
    let ts_val: i64 = ts_str.parse().map_err(|_| SignatureError::MissingOrMalformed)?;
    if sigs.is_empty() {
        return Err(SignatureError::MissingOrMalformed);
    }
    if (now_unix - ts_val).abs() > tolerance_secs {
        return Err(SignatureError::TimestampOutOfTolerance);
    }
    for sig in sigs {
        let Some(expected) = hex_decode(sig) else { continue };
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| SignatureError::MissingOrMalformed)?;
        mac.update(ts_str.as_bytes());
        mac.update(b":");
        mac.update(raw_body);
        if mac.verify_slice(&expected).is_ok() {
            return Ok(());
        }
    }
    Err(SignatureError::Mismatch)
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProcessOutcome {
    Applied,
    DuplicateIgnored,
    IgnoredStale,
    IgnoredUnhandledType,
    UnmappedOrganization,
    /// The organization already has a different, entitled subscription; the event is recorded but not applied.
    IgnoredConflictingSubscription,
}

/// Store the event (idempotent on `paddle_event_id`) and apply subscription state changes.
/// Duplicate deliveries are acknowledged without reprocessing; deliveries older than the last
/// applied event for the organization are stored but do not overwrite newer state.
pub async fn process_event(pool: &PgPool, event: &Value, link_secret: &str) -> Result<ProcessOutcome, anyhow::Error> {
    let event_id = event["event_id"].as_str().ok_or_else(|| anyhow::anyhow!("event_id missing"))?;
    let event_type = event["event_type"].as_str().ok_or_else(|| anyhow::anyhow!("event_type missing"))?;
    let occurred_at: DateTime<Utc> = event["occurred_at"].as_str().ok_or_else(|| anyhow::anyhow!("occurred_at missing"))?.parse()?;

    let mut tx = pool.begin().await?;
    let inserted = sqlx::query("INSERT INTO billing_events (paddle_event_id, event_type, occurred_at, payload) VALUES ($1, $2, $3, $4) ON CONFLICT (paddle_event_id) DO NOTHING")
        .bind(event_id)
        .bind(event_type)
        .bind(occurred_at)
        .bind(event)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if inserted == 0 {
        tx.commit().await?;
        return Ok(ProcessOutcome::DuplicateIgnored);
    }

    if !event_type.starts_with("subscription.") {
        sqlx::query("UPDATE billing_events SET processed_at = now() WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(ProcessOutcome::IgnoredUnhandledType);
    }

    let data = &event["data"];
    let paddle_subscription_id = data["id"].as_str().ok_or_else(|| anyhow::anyhow!("data.id missing"))?;
    let status = data["status"].as_str().unwrap_or("").to_string();
    let customer_id = data["customer_id"].as_str().map(|s| s.to_string());
    let plan = data["items"][0]["price"]["id"].as_str().map(|s| s.to_string());
    let period_end: Option<DateTime<Utc>> = data["current_billing_period"]["ends_at"].as_str().and_then(|s| s.parse().ok());

    // Organization mapping. Preferred: a subscription we already know (Paddle-signed ids cannot be forged).
    // Otherwise `custom_data.organization_id`, trusted only with a valid backend-issued signature.
    let mut org_id: Option<Uuid> = sqlx::query_scalar("SELECT organization_id FROM subscriptions WHERE paddle_subscription_id = $1").bind(paddle_subscription_id).fetch_optional(&mut *tx).await?;
    if org_id.is_none() {
        let claimed: Option<Uuid> = data["custom_data"]["organization_id"].as_str().and_then(|s| s.parse().ok());
        let sig = data["custom_data"]["organization_sig"].as_str().unwrap_or("");
        match claimed {
            Some(c) if verify_checkout_signature(link_secret, c, sig) => org_id = Some(c),
            Some(_) => {
                sqlx::query("UPDATE billing_events SET processing_error = 'custom_data.organization_sig missing or invalid' WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
                tx.commit().await?;
                return Ok(ProcessOutcome::UnmappedOrganization);
            }
            None => {}
        }
    }
    let Some(org_id) = org_id else {
        sqlx::query("UPDATE billing_events SET processing_error = 'no organization mapping (custom_data.organization_id missing)' WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(ProcessOutcome::UnmappedOrganization);
    };
    let org_exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM organizations WHERE id = $1)").bind(org_id).fetch_one(&mut *tx).await?;
    if !org_exists {
        sqlx::query("UPDATE billing_events SET processing_error = 'organization does not exist' WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(ProcessOutcome::UnmappedOrganization);
    }

    // Serialize concurrent events for the same organization, then apply only if not stale.
    sqlx::query("INSERT INTO subscriptions (organization_id) VALUES ($1) ON CONFLICT (organization_id) DO NOTHING").bind(org_id).execute(&mut *tx).await?;
    let (last, existing_sub, existing_status): (Option<DateTime<Utc>>, Option<String>, String) =
        sqlx::query_as("SELECT last_event_occurred_at, paddle_subscription_id, status FROM subscriptions WHERE organization_id = $1 FOR UPDATE").bind(org_id).fetch_one(&mut *tx).await?;
    if existing_sub.as_deref().map_or(false, |e| e != paddle_subscription_id) && status_is_entitled(&existing_status) {
        sqlx::query("UPDATE billing_events SET processed_at = now(), processing_error = 'ignored: organization already has a different active subscription' WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(ProcessOutcome::IgnoredConflictingSubscription);
    }
    if last.map_or(false, |l| occurred_at < l) {
        sqlx::query("UPDATE billing_events SET processed_at = now(), processing_error = 'stale event ignored (older than last applied)' WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(ProcessOutcome::IgnoredStale);
    }
    sqlx::query(
        "UPDATE subscriptions SET paddle_subscription_id = $2, paddle_customer_id = COALESCE($3, paddle_customer_id), plan = COALESCE($4, plan),
                status = $5, current_period_end = COALESCE($6, current_period_end), last_event_occurred_at = $7, updated_at = now()
         WHERE organization_id = $1",
    )
    .bind(org_id)
    .bind(paddle_subscription_id)
    .bind(customer_id)
    .bind(plan)
    .bind(&status)
    .bind(period_end)
    .bind(occurred_at)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE billing_events SET processed_at = now() WHERE paddle_event_id = $1").bind(event_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(ProcessOutcome::Applied)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sign(secret: &str, ts: i64, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("{ts}:").as_bytes());
        mac.update(body);
        let hex: String = mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect();
        format!("ts={ts};h1={hex}")
    }

    #[test]
    fn valid_signature_verifies() {
        let h = sign("secret", 1_000, b"{\"a\":1}");
        assert_eq!(verify_signature(&h, b"{\"a\":1}", "secret", 1_010, 300), Ok(()));
    }

    #[test]
    fn tampered_body_wrong_secret_and_garbage_are_rejected() {
        let h = sign("secret", 1_000, b"{\"a\":1}");
        assert_eq!(verify_signature(&h, b"{\"a\":2}", "secret", 1_000, 300), Err(SignatureError::Mismatch));
        assert_eq!(verify_signature(&h, b"{\"a\":1}", "other", 1_000, 300), Err(SignatureError::Mismatch));
        assert_eq!(verify_signature("garbage", b"x", "secret", 1_000, 300), Err(SignatureError::MissingOrMalformed));
        assert_eq!(verify_signature("ts=1000", b"x", "secret", 1_000, 300), Err(SignatureError::MissingOrMalformed));
        assert_eq!(verify_signature("ts=1000;h1=zz", b"x", "secret", 1_000, 300), Err(SignatureError::Mismatch));
    }

    #[test]
    fn old_or_future_timestamps_are_rejected() {
        let h = sign("secret", 1_000, b"x");
        assert_eq!(verify_signature(&h, b"x", "secret", 1_301, 300), Err(SignatureError::TimestampOutOfTolerance));
        assert_eq!(verify_signature(&h, b"x", "secret", 699, 300), Err(SignatureError::TimestampOutOfTolerance));
        assert_eq!(verify_signature(&h, b"x", "secret", 1_300, 300), Ok(()));
    }

    #[test]
    fn any_of_multiple_h1_values_may_match_for_secret_rotation() {
        let good = sign("new", 1_000, b"x");
        let good_h1 = good.split("h1=").nth(1).unwrap();
        let header = format!("ts=1000;h1={};h1={}", "00".repeat(32), good_h1);
        assert_eq!(verify_signature(&header, b"x", "new", 1_000, 300), Ok(()));
    }

    #[test]
    fn entitlement_rule() {
        for s in ["active", "trialing", "past_due"] { assert!(status_is_entitled(s), "{s}"); }
        for s in ["canceled", "paused", "NONE", "", "unknown"] { assert!(!status_is_entitled(s), "{s}"); }
    }

    #[test]
    fn checkout_signature_binds_to_the_organization_and_the_secret() {
        let org = Uuid::new_v4();
        let sig = checkout_signature("s", org);
        assert!(verify_checkout_signature("s", org, &sig));
        assert!(!verify_checkout_signature("other", org, &sig));
        assert!(!verify_checkout_signature("s", Uuid::new_v4(), &sig), "a signature for one org must not validate another");
        assert!(!verify_checkout_signature("s", org, "zz"));
        assert!(!verify_checkout_signature("s", org, ""));
    }
}
