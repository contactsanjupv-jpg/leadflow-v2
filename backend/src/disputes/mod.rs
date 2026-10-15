//! Dispute workflow state machine (spec: OPEN -> ... -> CLOSED). Pure logic with tests;
//! persistence and side effects live in the HTTP layer.
//!
//! Forward path: OPEN -> PREPARE -> REVIEW -> SUBMITTED -> VENDOR_RESPONDED -> CREDIT_PROMISED
//! -> CREDIT_RECEIVED -> VERIFIED -> CLOSED. REVIEW may be sent back to PREPARE. A dispute may
//! be closed (withdrawn / vendor refused) from any state before a credit is received; once a
//! credit has been received it must be VERIFIED before it can be closed.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisputeStatus {
    Open,
    Prepare,
    Review,
    Submitted,
    VendorResponded,
    CreditPromised,
    CreditReceived,
    Verified,
    Closed,
}

impl DisputeStatus {
    pub fn as_db(&self) -> &'static str {
        match self {
            DisputeStatus::Open => "OPEN",
            DisputeStatus::Prepare => "PREPARE",
            DisputeStatus::Review => "REVIEW",
            DisputeStatus::Submitted => "SUBMITTED",
            DisputeStatus::VendorResponded => "VENDOR_RESPONDED",
            DisputeStatus::CreditPromised => "CREDIT_PROMISED",
            DisputeStatus::CreditReceived => "CREDIT_RECEIVED",
            DisputeStatus::Verified => "VERIFIED",
            DisputeStatus::Closed => "CLOSED",
        }
    }

    pub fn from_db(s: &str) -> Option<Self> {
        Some(match s {
            "OPEN" => DisputeStatus::Open,
            "PREPARE" => DisputeStatus::Prepare,
            "REVIEW" => DisputeStatus::Review,
            "SUBMITTED" => DisputeStatus::Submitted,
            "VENDOR_RESPONDED" => DisputeStatus::VendorResponded,
            "CREDIT_PROMISED" => DisputeStatus::CreditPromised,
            "CREDIT_RECEIVED" => DisputeStatus::CreditReceived,
            "VERIFIED" => DisputeStatus::Verified,
            "CLOSED" => DisputeStatus::Closed,
            _ => return None,
        })
    }
}

pub fn transition_allowed(from: DisputeStatus, to: DisputeStatus) -> bool {
    use DisputeStatus::*;
    match (from, to) {
        (Open, Prepare)
        | (Prepare, Review)
        | (Review, Prepare)
        | (Review, Submitted)
        | (Submitted, VendorResponded)
        | (VendorResponded, CreditPromised)
        | (CreditPromised, CreditReceived)
        | (CreditReceived, Verified)
        | (Verified, Closed) => true,
        (Open | Prepare | Review | Submitted | VendorResponded | CreditPromised, Closed) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use DisputeStatus::*;

    const ALL: [DisputeStatus; 9] = [
        Open,
        Prepare,
        Review,
        Submitted,
        VendorResponded,
        CreditPromised,
        CreditReceived,
        Verified,
        Closed,
    ];

    #[test]
    fn happy_path_is_allowed_step_by_step() {
        let path = [
            Open,
            Prepare,
            Review,
            Submitted,
            VendorResponded,
            CreditPromised,
            CreditReceived,
            Verified,
            Closed,
        ];
        for w in path.windows(2) {
            assert!(transition_allowed(w[0], w[1]), "{:?} -> {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn no_skipping_forward() {
        assert!(!transition_allowed(Open, Submitted));
        assert!(!transition_allowed(Submitted, CreditPromised));
        assert!(!transition_allowed(CreditPromised, Verified));
        assert!(
            !transition_allowed(CreditReceived, Closed),
            "a received credit must be verified before closing"
        );
    }

    #[test]
    fn closed_is_terminal_and_nothing_transitions_to_itself() {
        for to in ALL {
            assert!(!transition_allowed(Closed, to), "Closed -> {to:?}");
        }
        for s in ALL {
            assert!(!transition_allowed(s, s), "{s:?} -> itself");
        }
    }

    #[test]
    fn only_review_can_step_back() {
        assert!(transition_allowed(Review, Prepare));
        for from in [
            Submitted,
            VendorResponded,
            CreditPromised,
            CreditReceived,
            Verified,
        ] {
            assert!(!transition_allowed(from, Prepare));
            assert!(!transition_allowed(from, Open));
        }
    }

    #[test]
    fn can_close_early_before_a_credit_is_received() {
        for from in [
            Open,
            Prepare,
            Review,
            Submitted,
            VendorResponded,
            CreditPromised,
        ] {
            assert!(transition_allowed(from, Closed), "{from:?} -> Closed");
        }
    }

    #[test]
    fn db_labels_round_trip() {
        for s in ALL {
            assert_eq!(DisputeStatus::from_db(s.as_db()), Some(s));
        }
        assert_eq!(DisputeStatus::from_db("x"), None);
    }
}
