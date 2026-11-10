//! Startup guard: tenant isolation rests on row-level security, and PostgreSQL skips RLS entirely for
//! superusers and for roles with BYPASSRLS. A runtime pool connected as such a role would silently
//! see every tenant's rows, so the server refuses to start with one.

use sqlx::PgPool;

/// Pure decision, kept separate so it can be tested without a database.
pub fn check_role_attributes(role: &str, rolsuper: bool, rolbypassrls: bool) -> Result<(), String> {
    if rolsuper || rolbypassrls {
        return Err(format!(
            "refusing to start: database role \"{role}\" is {}, which bypasses row-level security and \
             would expose every tenant's data. Point DATABASE_URL at the least-privilege xflow_app role \
             (superuser: {rolsuper}, bypassrls: {rolbypassrls}).",
            if rolsuper { "a superuser" } else { "BYPASSRLS" }
        ));
    }
    Ok(())
}

/// Reads the connected role's attributes and applies [`check_role_attributes`].
pub async fn assert_rls_enforced(pool: &PgPool) -> anyhow::Result<()> {
    let (role, rolsuper, rolbypassrls): (String, bool, bool) = sqlx::query_as(
        "SELECT rolname::text, rolsuper, rolbypassrls FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(pool)
    .await?;
    check_role_attributes(&role, rolsuper, rolbypassrls).map_err(anyhow::Error::msg)
}

#[cfg(test)]
mod tests {
    use super::check_role_attributes;

    #[test]
    fn plain_role_is_accepted() {
        assert!(check_role_attributes("xflow_app", false, false).is_ok());
    }

    #[test]
    fn superuser_and_bypassrls_roles_are_refused() {
        assert!(check_role_attributes("postgres", true, true).unwrap_err().contains("superuser"));
        assert!(check_role_attributes("x", false, true).unwrap_err().contains("BYPASSRLS"));
        assert!(check_role_attributes("x", true, false).is_err());
    }
}
