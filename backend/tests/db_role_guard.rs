//! The startup guard must accept the runtime role and refuse one that bypasses RLS.

use sqlx::postgres::PgPoolOptions;
use xflow_backend::common::db_role::assert_rls_enforced;

#[tokio::test]
async fn the_runtime_role_is_accepted() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL (xflow_app role) must be set");
    let pool = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    assert_rls_enforced(&pool).await.expect("xflow_app must pass the startup guard");
}

#[tokio::test]
#[ignore = "needs MIGRATION_DATABASE_URL (superuser)"]
async fn a_superuser_role_is_refused() {
    let url = std::env::var("MIGRATION_DATABASE_URL")
        .expect("set MIGRATION_DATABASE_URL to a superuser URL to run this test");
    let pool = PgPoolOptions::new().max_connections(1).connect(&url).await.unwrap();
    let err = assert_rls_enforced(&pool).await.expect_err("a superuser must be refused");
    assert!(err.to_string().contains("refusing to start"), "{err}");
}
