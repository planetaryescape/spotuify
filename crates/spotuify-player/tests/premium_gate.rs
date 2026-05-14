//! Phase 9.2a — premium gate contract tests.
//!
//! The gate decides whether the daemon should initialise librespot
//! streaming. Behaviour locked here:
//! - `product = "premium"` → Allowed.
//! - `product = "free"` or `"open"` → Denied; the init closure is
//!   NEVER called (tripwire test).
//! - HTTP 401 → bubbles up as an Auth error, not a Denied — so we
//!   don't tell Free users "premium required" when the real problem
//!   is a missing token.
//! - 30s response delay → bounded 5s timeout fires.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use spotuify_player::backends::premium_gate::{
    check_premium_then_init, GateError, HttpWebApiClient, PremiumDecision,
};
use spotuify_player::PlayerError;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn client(server: &MockServer) -> HttpWebApiClient {
    HttpWebApiClient::with_base_url(server.uri(), "test-token".to_string())
}

#[tokio::test]
async fn premium_account_yields_allowed() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "product": "premium",
        })))
        .mount(&server)
        .await;

    let decision = check_premium_then_init(&client(&server), || async { Ok::<_, PlayerError>(()) })
        .await
        .unwrap();
    assert!(matches!(decision, PremiumDecision::Allowed));
}

#[tokio::test]
async fn free_account_yields_denied_and_does_not_invoke_init() {
    // Adversarial tripwire: the init closure must NEVER fire when the
    // gate denies. A regression where the daemon eagerly initialised
    // librespot before checking premium would wedge Free users with
    // mysterious audio backend errors.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "product": "free",
        })))
        .mount(&server)
        .await;

    let init_was_called = Arc::new(AtomicBool::new(false));
    let flag = init_was_called.clone();
    let decision = check_premium_then_init(&client(&server), move || {
        let flag = flag.clone();
        async move {
            flag.store(true, Ordering::SeqCst);
            Ok::<_, PlayerError>(())
        }
    })
    .await
    .unwrap();

    assert!(
        matches!(decision, PremiumDecision::Denied { ref product } if product == "free"),
        "got {decision:?}"
    );
    assert!(
        !init_was_called.load(Ordering::SeqCst),
        "init closure must not run when gate denies"
    );
}

#[tokio::test]
async fn open_legacy_free_tier_is_also_denied() {
    // Adversarial: catches the `== "premium"` vs `!= "free"` bug.
    // Spotify still returns "open" for some legacy free accounts.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "product": "open",
        })))
        .mount(&server)
        .await;

    let decision = check_premium_then_init(&client(&server), || async { Ok::<_, PlayerError>(()) })
        .await
        .unwrap();
    assert!(
        matches!(decision, PremiumDecision::Denied { ref product } if product == "open"),
        "got {decision:?}"
    );
}

#[tokio::test]
async fn http_401_propagates_as_auth_error_not_denial() {
    // Adversarial: an expired or missing token must NOT surface as
    // "premium required". That message sends Free users into a
    // confusing upgrade prompt when the real fix is `spotuify login`.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let err = check_premium_then_init(&client(&server), || async { Ok::<_, PlayerError>(()) })
        .await
        .expect_err("401 must error");
    assert!(matches!(err, GateError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn slow_response_times_out_within_five_seconds() {
    // Bounded timeout: 5s ceiling locks in so a hung Spotify can't
    // wedge daemon startup.
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;

    let start = std::time::Instant::now();
    let err = check_premium_then_init(&client(&server), || async { Ok::<_, PlayerError>(()) })
        .await
        .expect_err("hang must time out");
    let elapsed = start.elapsed();
    assert!(matches!(err, GateError::Timeout(_)), "got {err:?}");
    assert!(
        elapsed < Duration::from_secs(8),
        "gate took {elapsed:?}; expected ~5s, not the full 30"
    );
}

#[tokio::test]
async fn allowed_path_invokes_init_closure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/v1/me"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "product": "premium",
        })))
        .mount(&server)
        .await;

    let init_was_called = Arc::new(AtomicBool::new(false));
    let flag = init_was_called.clone();
    let _ = check_premium_then_init(&client(&server), move || {
        let flag = flag.clone();
        async move {
            flag.store(true, Ordering::SeqCst);
            Ok::<_, PlayerError>(())
        }
    })
    .await
    .unwrap();

    assert!(
        init_was_called.load(Ordering::SeqCst),
        "init must run when gate allows"
    );
}
