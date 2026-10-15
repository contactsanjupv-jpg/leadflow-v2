//! Discrepancy review workflow rules (pure logic; persistence lives in the HTTP layer).
//!
//! Manual review actions: NEW/UNDER_REVIEW/NEEDS_INFORMATION can be confirmed, rejected, sent
//! back for information or put under review; CONFIRMED and REJECTED can be reopened.
//! DISPUTED and RESOLVED are owned by the dispute workflow and cannot be changed by hand.

use crate::reconciliation::model::ReviewStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewAction {
    StartReview,
    Confirm,
    Reject,
    NeedsInformation,
    Reopen,
}

impl ReviewAction {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "start_review" => ReviewAction::StartReview,
            "confirm" => ReviewAction::Confirm,
            "reject" => ReviewAction::Reject,
            "needs_information" => ReviewAction::NeedsInformation,
            "reopen" => ReviewAction::Reopen,
            _ => return None,
        })
    }

    pub fn target(&self) -> ReviewStatus {
        match self {
            ReviewAction::StartReview | ReviewAction::Reopen => ReviewStatus::UnderReview,
            ReviewAction::Confirm => ReviewStatus::Confirmed,
            ReviewAction::Reject => ReviewStatus::Rejected,
            ReviewAction::NeedsInformation => ReviewStatus::NeedsInformation,
        }
    }

    /// Rejecting and asking for information must be explained; a decision without a reason is
    /// useless to the next reviewer.
    pub fn requires_notes(&self) -> bool {
        matches!(self, ReviewAction::Reject | ReviewAction::NeedsInformation)
    }
}

pub fn transition_allowed(from: ReviewStatus, action: ReviewAction) -> bool {
    use ReviewStatus::*;
    match action {
        ReviewAction::StartReview => matches!(from, New | NeedsInformation),
        ReviewAction::Confirm | ReviewAction::Reject | ReviewAction::NeedsInformation => {
            matches!(from, New | UnderReview | NeedsInformation)
        }
        ReviewAction::Reopen => matches!(from, Confirmed | Rejected),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ReviewStatus::*;

    #[test]
    fn open_findings_can_be_decided() {
        for from in [New, UnderReview, NeedsInformation] {
            for a in [
                ReviewAction::Confirm,
                ReviewAction::Reject,
                ReviewAction::NeedsInformation,
            ] {
                assert!(transition_allowed(from, a), "{from:?} -> {a:?}");
            }
        }
    }

    #[test]
    fn dispute_owned_and_terminal_states_cannot_be_changed_by_hand() {
        for from in [Disputed, Resolved] {
            for a in [
                ReviewAction::StartReview,
                ReviewAction::Confirm,
                ReviewAction::Reject,
                ReviewAction::NeedsInformation,
                ReviewAction::Reopen,
            ] {
                assert!(
                    !transition_allowed(from, a),
                    "{from:?} -> {a:?} must be blocked"
                );
            }
        }
    }

    #[test]
    fn decided_findings_can_only_be_reopened() {
        for from in [Confirmed, Rejected] {
            assert!(transition_allowed(from, ReviewAction::Reopen));
            assert!(!transition_allowed(from, ReviewAction::Confirm));
            assert!(!transition_allowed(from, ReviewAction::Reject));
        }
        assert!(!transition_allowed(New, ReviewAction::Reopen));
    }

    #[test]
    fn notes_are_required_for_reject_and_needs_information_only() {
        assert!(ReviewAction::Reject.requires_notes());
        assert!(ReviewAction::NeedsInformation.requires_notes());
        assert!(!ReviewAction::Confirm.requires_notes());
        assert!(!ReviewAction::StartReview.requires_notes());
    }

    #[test]
    fn action_parsing() {
        assert_eq!(ReviewAction::parse("confirm"), Some(ReviewAction::Confirm));
        assert_eq!(ReviewAction::parse("nope"), None);
        assert_eq!(ReviewAction::Confirm.target(), Confirmed);
    }
}
