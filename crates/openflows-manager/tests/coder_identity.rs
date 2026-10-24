mod common;

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
async fn links_are_unique_and_browser_attempts_are_single_use() {
    let db = common::TestDb::new().await.unwrap();
    db.migrate().await.unwrap();
    let pool = db.pool();
    let one = uuid::Uuid::new_v4();
    let two = uuid::Uuid::new_v4();
    for user in [one, two] {
        sqlx::query("INSERT INTO users(id,display_name) VALUES($1,'owner')")
            .bind(user)
            .execute(pool)
            .await
            .unwrap();
    }
    sqlx::query("INSERT INTO coder_user_links VALUES($1,'https://coder.example','personal',now())")
        .bind(one)
        .execute(pool)
        .await
        .unwrap();
    assert!(sqlx::query(
        "INSERT INTO coder_user_links VALUES($1,'https://coder.example','personal',now())"
    )
    .bind(two)
    .execute(pool)
    .await
    .is_err());
    assert!(sqlx::query(
        "INSERT INTO coder_user_links VALUES($1,'https://coder.example','different',now())"
    )
    .bind(one)
    .execute(pool)
    .await
    .is_err());
    sqlx::query("INSERT INTO coder_link_attempts VALUES('state',$1,'browser','verifier',now()+interval '1 minute')")
        .bind(one).execute(pool).await.unwrap();
    for (user, browser, expected) in [
        (two, "browser", false),
        (one, "wrong", false),
        (one, "browser", true),
        (one, "browser", false),
    ] {
        let value: Option<String> = sqlx::query_scalar(
            "DELETE FROM coder_link_attempts WHERE state_hash='state' AND user_id=$1 AND browser_hash=$2 AND expires_at>now() RETURNING verifier")
            .bind(user).bind(browser).fetch_optional(pool).await.unwrap();
        assert_eq!(value.is_some(), expected);
    }
}
