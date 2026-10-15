# Security — see /SECURITY.md. RBAC matrix (implementation decision; original spec text not in repo)
OWNER, ADMIN: everything. MEMBER: view + manage source data + run reconciliation/monitoring. REVIEWER: view + review discrepancies + manage disputes (cannot enter source data, cannot verify credits). Only OWNER may create OWNER/ADMIN; ADMIN may create MEMBER/REVIEWER.
