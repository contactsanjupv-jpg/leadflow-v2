//! Role-based access control (spec sections 8-9). One authoritative permission matrix.
//!
//! NOTE: the original spec text is not in the repo, so this matrix is an implementation
//! decision documented in docs/security.md. Every handler that touches tenant data declares the
//! permission it needs; there is no route that reads or writes tenant data without one.

use crate::http::authed_user::Role;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// Read organization data (invoices, contracts, discrepancies, ...).
    ViewData,
    /// Create/modify source data, upload documents, run reconciliations, trigger monitoring.
    ManageData,
    /// Start review, confirm/reject discrepancies, generate evidence packages.
    ReviewDiscrepancies,
    /// Create disputes, move them through the workflow, record credits.
    ManageDisputes,
    /// Verify that a credit was actually recovered (financial sign-off).
    VerifyCredits,
    /// Add organization members.
    ManageMembers,
    /// Read the audit log.
    ViewAudit,
    /// Read subscription/billing status.
    ViewBilling,
}

pub fn role_can(role: &Role, permission: Permission) -> bool {
    use Permission::*;
    match role {
        Role::Owner => true,
        Role::Admin => true,
        Role::Member => matches!(permission, ViewData | ManageData),
        Role::Reviewer => matches!(permission, ViewData | ReviewDiscrepancies | ManageDisputes),
    }
}

/// Which roles `actor` may assign when adding a member. Only an owner may create owners/admins.
pub fn can_assign_role(actor: &Role, target: &Role) -> bool {
    match actor {
        Role::Owner => true,
        Role::Admin => matches!(target, Role::Member | Role::Reviewer),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use Permission::*;

    const ALL: [Permission; 8] = [
        ViewData,
        ManageData,
        ReviewDiscrepancies,
        ManageDisputes,
        VerifyCredits,
        ManageMembers,
        ViewAudit,
        ViewBilling,
    ];

    #[test]
    fn owner_and_admin_have_every_permission() {
        for p in ALL {
            assert!(role_can(&Role::Owner, p));
            assert!(role_can(&Role::Admin, p));
        }
    }

    #[test]
    fn member_can_read_and_manage_data_but_not_review_or_administer() {
        for p in ALL {
            let expected = matches!(p, ViewData | ManageData);
            assert_eq!(role_can(&Role::Member, p), expected, "member / {p:?}");
        }
    }

    #[test]
    fn reviewer_can_read_review_and_manage_disputes_only() {
        for p in ALL {
            let expected = matches!(p, ViewData | ReviewDiscrepancies | ManageDisputes);
            assert_eq!(role_can(&Role::Reviewer, p), expected, "reviewer / {p:?}");
        }
        assert!(!role_can(&Role::Reviewer, ManageData));
        assert!(!role_can(&Role::Reviewer, VerifyCredits));
    }

    #[test]
    fn role_assignment_rules() {
        assert!(can_assign_role(&Role::Owner, &Role::Owner));
        assert!(can_assign_role(&Role::Owner, &Role::Admin));
        assert!(can_assign_role(&Role::Admin, &Role::Member));
        assert!(can_assign_role(&Role::Admin, &Role::Reviewer));
        assert!(!can_assign_role(&Role::Admin, &Role::Admin));
        assert!(!can_assign_role(&Role::Admin, &Role::Owner));
        assert!(!can_assign_role(&Role::Member, &Role::Member));
        assert!(!can_assign_role(&Role::Reviewer, &Role::Reviewer));
    }
}
