use github::GithubRestClient;

#[tokio::test]
async fn a_rotated_token_reaches_requests_made_through_a_clone() {
    let mut server = mockito::Server::new_async().await;
    let old = server
        .mock("GET", "/user")
        .match_header("authorization", "Bearer old")
        .with_body(r#"{"login":"app"}"#)
        .expect(1)
        .create_async()
        .await;
    let new = server
        .mock("GET", "/user")
        .match_header("authorization", "Bearer new")
        .with_body(r#"{"login":"app"}"#)
        .expect(1)
        .create_async()
        .await;

    let client = GithubRestClient::with_api_base("old", server.url());
    let clone = client.clone();
    clone.get_authenticated_user_login().await.unwrap();

    // The owner of the client rotates the token; a clone made earlier follows.
    client.token_handle().set("new");
    clone.get_authenticated_user_login().await.unwrap();

    old.assert_async().await;
    new.assert_async().await;
}
