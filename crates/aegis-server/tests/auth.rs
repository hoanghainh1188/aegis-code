//! Sign-in, roles and the audit log (#29), against a real Postgres and a
//! fake SSO: no one sees or changes a repo they have no role in, and
//! every write leaves an audit entry.

mod common;

use aegis_ledger::{IssueId, LedgerEvent, LedgerStore, Outcome};
use aegis_server::api::router;
use aegis_server::auth::Signer;
use aegis_server::config::{AuthSection, OidcSection};
use aegis_server::web::opened_at_signature;
use aegis_store::testing::test_db;
use aegis_store::{
    ActorKind, JobEnd, JobId, JobSource, NewJob, NewRepo, Page, RepoId, RepoMode, RepoRole, Store,
};
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use common::Browser;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

struct Answer {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

async fn send(app: &Router, request: Request<Body>) -> Answer {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    Answer {
        status,
        headers,
        body: String::from_utf8(bytes.to_vec()).unwrap(),
    }
}

fn page(uri: &str, cookie: Option<&str>) -> Request<Body> {
    let mut request = Request::get(uri).header(header::HOST, "localhost:8080");
    if let Some(c) = cookie {
        request = request.header(header::COOKIE, c);
    }
    request.body(Body::empty()).unwrap()
}

fn form(uri: &str, cookie: &str, fields: &str) -> Request<Body> {
    Request::post(uri)
        .header(header::HOST, "localhost:8080")
        .header(header::ORIGIN, "http://localhost:8080")
        .header(header::COOKIE, cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(fields.to_owned()))
        .unwrap()
}

fn api(method: &str, uri: &str, token: Option<&str>, body: Option<Value>) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(t) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {t}"));
    }
    request
        .body(body.map_or_else(Body::empty, |b| Body::from(b.to_string())))
        .unwrap()
}

async fn repo(s: &Store, slug: &str) -> RepoId {
    s.create_repo(&NewRepo {
        slug: slug.into(),
        url: format!("https://github.com/x/{slug}"),
        default_branch: "main".into(),
        language: "rust".into(),
        mode: RepoMode::Poc,
        max_concurrent_jobs: 1,
        config: json!({}),
    })
    .await
    .unwrap()
    .id
}

/// A job of `repo` that opened a PR, so it can be reviewed.
async fn pr_job(s: &Store, repo: RepoId, key: &str) -> JobId {
    let job = s
        .create_job(&NewJob {
            repo_id: repo,
            issue_ref: format!("x/y#{key}"),
            ledger_key: IssueId::parse(key).unwrap(),
            source: JobSource::Cli,
        })
        .await
        .unwrap();
    let finished = LedgerEvent::Finished {
        outcome: Outcome::PrOpened,
        wall_time_s: 60.0,
    };
    s.ledger().append(&job.ledger_key, &finished).await.unwrap();
    s.end_job(job.id, JobEnd::Done(Outcome::PrOpened))
        .await
        .unwrap();
    job.id
}

/// The review fields of `who`'s form for `job`, signed `seconds_ago`.
fn review_fields(who: &Browser, job: JobId, seconds_ago: i64) -> String {
    let opened_at = chrono::Utc::now().timestamp_millis() - seconds_ago * 1000;
    let sig = opened_at_signature(
        &Signer::new(common::KEY.as_bytes()),
        job,
        who.user,
        opened_at,
    );
    format!(
        "verdict=accepted&lines_edited=0&opened_at={opened_at}&opened_sig={sig}&_csrf={}",
        who.csrf
    )
}

async fn reviewed(s: &Store, key: &str) -> bool {
    s.record(&IssueId::parse(key).unwrap())
        .await
        .unwrap()
        .is_some_and(|r| r.review.is_some())
}

/// The audit entries, oldest first, as `(kind, name, action, job)`.
async fn audit(s: &Store) -> Vec<(ActorKind, String, String, Option<i64>)> {
    let mut entries: Vec<_> = s
        .audit_log(None, Page::first(Page::MAX_LIMIT))
        .await
        .unwrap()
        .items
        .into_iter()
        .map(|e| (e.actor.kind, e.actor.name, e.action, e.job_id.map(|j| j.0)))
        .collect();
    entries.reverse();
    entries
}

#[tokio::test]
async fn the_api_needs_a_token_and_a_role_in_the_repo() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "repo-a").await, repo(s, "repo-b").await);
    let job_b = pr_job(s, b, "b-1").await;
    common::grant(s, "connector-a", a, RepoRole::Maintainer).await;
    common::grant(s, "dashboard-a", a, RepoRole::Viewer).await;
    let maintainer = common::token(s, "connector-a").await;
    let viewer = common::token(s, "dashboard-a").await;
    let app = router(s.clone(), common::auth(s));

    // No token, a wrong one: 401, before anything is read.
    let none = send(&app, api("GET", "/api/v1/jobs", None, None)).await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert_eq!(none.headers[header::WWW_AUTHENTICATE], "Bearer");
    let body: Value = serde_json::from_str(&none.body).unwrap();
    assert_eq!(body["error"]["code"], "unauthorized");
    for bad in ["nope".to_owned(), "0".repeat(64)] {
        let wrong = send(&app, api("GET", "/api/v1/jobs", Some(&bad), None)).await;
        assert_eq!(wrong.status, StatusCode::UNAUTHORIZED, "{bad}");
    }
    let create = |repo: RepoId, n: u32| json!({ "repo_id": repo.0, "issue_ref": format!("x/y#{n}"), "source": "github" });
    let anonymous = send(&app, api("POST", "/api/v1/jobs", None, Some(create(a, 1)))).await;
    assert_eq!(anonymous.status, StatusCode::UNAUTHORIZED);
    let health = send(&app, api("GET", "/api/v1/health", None, None)).await;
    assert_eq!(health.status, StatusCode::OK);

    // The maintainer of A creates in A, not in B, and never sees B.
    let in_a = send(
        &app,
        api(
            "POST",
            "/api/v1/jobs",
            Some(&maintainer),
            Some(create(a, 1)),
        ),
    )
    .await;
    assert_eq!(in_a.status, StatusCode::CREATED, "{}", in_a.body);
    let job_a: i64 = serde_json::from_str::<Value>(&in_a.body).unwrap()["data"]["id"]
        .as_i64()
        .unwrap();
    let in_b = send(
        &app,
        api(
            "POST",
            "/api/v1/jobs",
            Some(&maintainer),
            Some(create(b, 2)),
        ),
    )
    .await;
    assert_eq!(in_b.status, StatusCode::FORBIDDEN);
    let listed = send(&app, api("GET", "/api/v1/jobs", Some(&maintainer), None)).await;
    let listed: Value = serde_json::from_str(&listed.body).unwrap();
    let ids: Vec<i64> = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|j| j["id"].as_i64().unwrap())
        .collect();
    assert_eq!(ids, [job_a]);
    let only_b = format!("/api/v1/jobs?repo_id={}", b.0);
    let listed = send(&app, api("GET", &only_b, Some(&maintainer), None)).await;
    assert!(listed.body.contains("\"data\":[]"), "{}", listed.body);
    for (method, uri) in [
        ("GET", format!("/api/v1/jobs/{}", job_b.0)),
        ("GET", format!("/api/v1/jobs/{}/runs", job_b.0)),
        ("POST", format!("/api/v1/jobs/{}/cancel", job_b.0)),
        ("POST", format!("/api/v1/jobs/{}/retry", job_b.0)),
    ] {
        let answer = send(&app, api(method, &uri, Some(&maintainer), None)).await;
        assert_eq!(answer.status, StatusCode::NOT_FOUND, "{method} {uri}");
    }

    // A viewer reads A but changes nothing.
    let read = send(
        &app,
        api("GET", &format!("/api/v1/jobs/{job_a}"), Some(&viewer), None),
    )
    .await;
    assert_eq!(read.status, StatusCode::OK);
    for (method, uri, body) in [
        ("POST", "/api/v1/jobs".to_owned(), Some(create(a, 3))),
        ("POST", format!("/api/v1/jobs/{job_a}/cancel"), None),
    ] {
        let answer = send(&app, api(method, &uri, Some(&viewer), body)).await;
        assert_eq!(
            answer.status,
            StatusCode::FORBIDDEN,
            "{uri}: {}",
            answer.body
        );
    }

    let cancel = format!("/api/v1/jobs/{job_a}/cancel");
    let cancelled = send(&app, api("POST", &cancel, Some(&maintainer), None)).await;
    assert_eq!(cancelled.status, StatusCode::OK);
    let retry = format!("/api/v1/jobs/{job_a}/retry");
    let retried = send(&app, api("POST", &retry, Some(&maintainer), None)).await;
    assert_eq!(retried.status, StatusCode::CREATED);

    // Each write by the token, in the order done.
    let by_token: Vec<_> = audit(s)
        .await
        .into_iter()
        .filter(|e| e.0 == ActorKind::Token)
        .map(|e| (e.1, e.2))
        .collect();
    let mine = |action: &str| ("connector-a".to_owned(), action.to_owned());
    assert_eq!(
        by_token,
        [mine("job.create"), mine("job.cancel"), mine("job.retry")]
    );
    db.drop().await;
}

#[tokio::test]
async fn each_service_has_its_rate() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    common::admin(s, "busy").await;
    common::admin(s, "calm").await;
    let (busy, calm) = (
        common::token(s, "busy").await,
        common::token(s, "calm").await,
    );
    let config = AuthSection {
        api_requests_per_minute: 2,
        ..AuthSection::default()
    };
    let app = router(s.clone(), common::auth_with(s, config, None));
    for _ in 0..2 {
        let ok = send(&app, api("GET", "/api/v1/jobs", Some(&busy), None)).await;
        assert_eq!(ok.status, StatusCode::OK);
    }
    let limited = send(&app, api("GET", "/api/v1/jobs", Some(&busy), None)).await;
    assert_eq!(limited.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(limited.headers.contains_key(header::RETRY_AFTER));
    let other = send(&app, api("GET", "/api/v1/jobs", Some(&calm), None)).await;
    assert_eq!(other.status, StatusCode::OK);
    db.drop().await;
}

#[tokio::test]
async fn pages_show_and_change_only_the_repos_of_a_role() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let (a, b) = (repo(s, "repo-a").await, repo(s, "repo-b").await);
    let (job_a, job_b) = (pr_job(s, a, "a-1").await, pr_job(s, b, "b-1").await);
    common::grant(s, "viewer@x.test", a, RepoRole::Viewer).await;
    common::grant(s, "maint@x.test", a, RepoRole::Maintainer).await;
    let viewer = common::sign_in(s, "viewer@x.test").await;
    let maint = common::sign_in(s, "maint@x.test").await;
    let app = router(s.clone(), common::auth(s));

    // Lists and the dashboard hold A's job only.
    for uri in ["/jobs", "/"] {
        let p = send(&app, page(uri, Some(&viewer.cookie))).await;
        assert_eq!(p.status, StatusCode::OK, "{uri}");
        assert!(
            !p.body.contains("repo-b") && !p.body.contains("x/y#b-1"),
            "{uri}: {}",
            p.body
        );
    }
    let jobs = send(&app, page("/jobs", Some(&viewer.cookie))).await;
    assert!(jobs.body.contains("x/y#a-1"), "{}", jobs.body);
    assert!(
        !jobs.body.contains("/review\""),
        "a viewer is not offered to review"
    );
    let other = send(
        &app,
        page(&format!("/jobs/{}", job_b.0), Some(&viewer.cookie)),
    )
    .await;
    assert_eq!(other.status, StatusCode::NOT_FOUND);

    // A viewer cannot review; a maintainer of A cannot touch B.
    let review_a = format!("/jobs/{}/review", job_a.0);
    let as_viewer = send(&app, page(&review_a, Some(&viewer.cookie))).await;
    assert_eq!(as_viewer.status, StatusCode::FORBIDDEN);
    let fields = review_fields(&viewer, job_a, 60);
    let sent = send(&app, form(&review_a, &viewer.cookie, &fields)).await;
    assert_eq!(sent.status, StatusCode::FORBIDDEN);
    let review_b = format!("/jobs/{}/review", job_b.0);
    let fields = review_fields(&maint, job_b, 60);
    let sent = send(&app, form(&review_b, &maint.cookie, &fields)).await;
    assert_eq!(sent.status, StatusCode::NOT_FOUND);
    assert!(!reviewed(s, "a-1").await && !reviewed(s, "b-1").await);

    // The maintainer of A reviews A, and the audit says so.
    let fields = review_fields(&maint, job_a, 60);
    let sent = send(&app, form(&review_a, &maint.cookie, &fields)).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);
    assert!(reviewed(s, "a-1").await);
    let last = audit(s).await.pop().unwrap();
    assert_eq!(
        last,
        (
            ActorKind::User,
            "maint@x.test".into(),
            "review.submit".into(),
            Some(job_a.0)
        )
    );
    db.drop().await;
}

#[tokio::test]
async fn forms_need_the_session_and_its_csrf_token() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    let job = pr_job(s, a, "a-1").await;
    common::grant(s, "rev@x.test", a, RepoRole::Reviewer).await;
    let rev = common::sign_in(s, "rev@x.test").await;
    let other = common::sign_in(s, "rev@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let uri = format!("/jobs/{}/review", job.0);

    // No session: a page sends to sign-in, a form is refused.
    let away = send(&app, page(&uri, None)).await;
    assert_eq!(away.status, StatusCode::SEE_OTHER);
    assert_eq!(
        away.headers[header::LOCATION],
        format!("/auth/login?next=%2Fjobs%2F{}%2Freview", job.0)
    );
    let fields = review_fields(&rev, job, 60);
    let unsigned = send(&app, form(&uri, "x=1", &fields)).await;
    assert_eq!(unsigned.status, StatusCode::UNAUTHORIZED);

    // A session without its CSRF token, or with another session's.
    let no_token = fields.replace(&format!("&_csrf={}", rev.csrf), "");
    let missing = send(&app, form(&uri, &rev.cookie, &no_token)).await;
    assert_eq!(missing.status, StatusCode::FORBIDDEN);
    let borrowed = fields.replace(&rev.csrf, &other.csrf);
    let wrong = send(&app, form(&uri, &rev.cookie, &borrowed)).await;
    assert_eq!(wrong.status, StatusCode::FORBIDDEN);
    assert!(!reviewed(s, "a-1").await);

    // The form carries its token, and with it the review goes through.
    let served = send(&app, page(&uri, Some(&rev.cookie))).await;
    assert!(
        served
            .body
            .contains(&format!("name=\"_csrf\" value=\"{}\"", rev.csrf))
    );
    let sent = send(&app, form(&uri, &rev.cookie, &fields)).await;
    assert_eq!(sent.status, StatusCode::SEE_OTHER, "{}", sent.body);

    // Signing out ends the session.
    let out = send(
        &app,
        form("/auth/logout", &rev.cookie, &format!("_csrf={}", rev.csrf)),
    )
    .await;
    assert_eq!(out.status, StatusCode::SEE_OTHER);
    let after = send(&app, page("/", Some(&rev.cookie))).await;
    assert_eq!(after.status, StatusCode::SEE_OTHER);
    let actions: Vec<String> = audit(s).await.into_iter().map(|e| e.2).collect();
    assert!(
        actions.ends_with(&["review.submit".into(), "session.logout".into()]),
        "{actions:?}"
    );
    db.drop().await;
}

#[tokio::test]
async fn a_changed_opened_at_is_refused() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    let (job, twin) = (pr_job(s, a, "a-1").await, pr_job(s, a, "a-2").await);
    common::grant(s, "rev@x.test", a, RepoRole::Reviewer).await;
    let rev = common::sign_in(s, "rev@x.test").await;
    let app = router(s.clone(), common::auth(s));
    let uri = format!("/jobs/{}/review", job.0);

    let fields = review_fields(&rev, job, 60);
    let opened_at = fields
        .split('&')
        .find_map(|f| f.strip_prefix("opened_at="))
        .unwrap()
        .to_owned();
    // An hour earlier, to look slow; or the signature of another job.
    let earlier = (opened_at.parse::<i64>().unwrap() - 3_600_000).to_string();
    for (why, changed) in [
        ("time", fields.replace(&opened_at, &earlier)),
        ("job", review_fields(&rev, twin, 60)),
        ("no signature", fields.replace("opened_sig=", "x=")),
    ] {
        let sent = send(&app, form(&uri, &rev.cookie, &changed)).await;
        assert_eq!(sent.status, StatusCode::UNPROCESSABLE_ENTITY, "{why}");
        assert!(sent.body.contains("không hợp lệ"), "{why}: {}", sent.body);
    }
    assert!(!reviewed(s, "a-1").await);
    db.drop().await;
}

/// The fake SSO: discovery, keys and a token endpoint, with an RSA key
/// made for the test.
mod sso {
    use openidconnect::core::{
        CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreProviderMetadata, CoreResponseType, CoreRsaPrivateSigningKey,
        CoreSubjectIdentifierType,
    };
    use openidconnect::{
        Audience, AuthUrl, EmptyAdditionalClaims, EmptyAdditionalProviderMetadata, EndUserEmail,
        IssuerUrl, JsonWebKeyId, JsonWebKeySetUrl, Nonce, PrivateSigningKey, ResponseTypes,
        StandardClaims, SubjectIdentifier, TokenUrl,
    };
    use rsa::pkcs1::{EncodeRsaPrivateKey, LineEnding};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    pub const CLIENT_ID: &str = "aegis-test";

    pub struct Sso {
        pub server: MockServer,
        key: CoreRsaPrivateSigningKey,
    }

    fn new_key(id: &str) -> CoreRsaPrivateSigningKey {
        let rsa = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 1024).unwrap();
        let pem = rsa.to_pkcs1_pem(LineEnding::LF).unwrap();
        CoreRsaPrivateSigningKey::from_pem(&pem, Some(JsonWebKeyId::new(id.into()))).unwrap()
    }

    impl Sso {
        pub async fn start() -> Self {
            let sso = Self {
                server: MockServer::start().await,
                key: new_key("k1"),
            };
            sso.publish().await;
            sso
        }

        /// The SSO signs with a new key from now on; the old one is gone.
        pub async fn rotate_keys(&mut self) {
            self.key = new_key("k2");
            self.server.reset().await;
            self.publish().await;
        }

        /// Discovery and the keys.
        async fn publish(&self) {
            let base = self.server.uri();
            let metadata = CoreProviderMetadata::new(
                self.issuer(),
                AuthUrl::new(format!("{base}/authorize")).unwrap(),
                JsonWebKeySetUrl::new(format!("{base}/jwks")).unwrap(),
                vec![ResponseTypes::new(vec![CoreResponseType::Code])],
                vec![CoreSubjectIdentifierType::Public],
                vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256],
                EmptyAdditionalProviderMetadata {},
            )
            .set_token_endpoint(Some(TokenUrl::new(format!("{base}/token")).unwrap()));
            Mock::given(method("GET"))
                .and(path("/.well-known/openid-configuration"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&metadata))
                .mount(&self.server)
                .await;
            let keys = CoreJsonWebKeySet::new(vec![self.key.as_verification_key()]);
            Mock::given(method("GET"))
                .and(path("/jwks"))
                .respond_with(ResponseTemplate::new(200).set_body_json(&keys))
                .mount(&self.server)
                .await;
        }

        pub fn issuer(&self) -> IssuerUrl {
            IssuerUrl::new(self.server.uri()).unwrap()
        }

        /// The token endpoint answers with an ID token for `email`.
        pub async fn will_sign_in(&self, subject: &str, email: &str, nonce: &str) {
            let now = chrono::Utc::now();
            let claims = CoreIdTokenClaims::new(
                self.issuer(),
                vec![Audience::new(CLIENT_ID.into())],
                now + chrono::Duration::minutes(5),
                now,
                StandardClaims::new(SubjectIdentifier::new(subject.into()))
                    .set_email(Some(EndUserEmail::new(email.into())))
                    .set_email_verified(Some(true)),
                EmptyAdditionalClaims {},
            )
            .set_nonce(Some(Nonce::new(nonce.into())));
            let id_token = CoreIdToken::new(
                claims,
                &self.key,
                CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                None,
                None,
            )
            .unwrap();
            let body = serde_json::json!({
                "access_token": "at",
                "token_type": "Bearer",
                "expires_in": 300,
                "id_token": id_token.to_string(),
            });
            Mock::given(method("POST"))
                .and(path("/token"))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&self.server)
                .await;
        }
    }
}

/// The value of `name` in the query of `url`.
fn query_value(url: &str, name: &str) -> String {
    let url = openidconnect::url::Url::parse(url).unwrap();
    url.query_pairs()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v.into_owned())
        .unwrap_or_else(|| panic!("no {name} in {url}"))
}

/// `name=value` of the `Set-Cookie` for `name`.
fn set_cookie(headers: &HeaderMap, name: &str) -> String {
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|c| c.starts_with(&format!("{name}=")))
        .and_then(|c| c.split(';').next())
        .unwrap_or_else(|| panic!("no cookie {name}: {headers:?}"))
        .to_owned()
}

#[tokio::test]
async fn people_sign_in_through_the_sso() {
    let Some(db) = test_db().await else { return };
    let s = &db.store;
    let a = repo(s, "repo-a").await;
    common::grant(s, "dev@x.test", a, RepoRole::Viewer).await;
    let mut sso = sso::Sso::start().await;
    let config = AuthSection {
        public_url: Some("https://aegis.test".parse().unwrap()),
        oidc: Some(OidcSection {
            issuer: sso.server.uri(),
            client_id: sso::CLIENT_ID.into(),
        }),
        ..AuthSection::default()
    };
    let app = router(
        s.clone(),
        common::auth_with(s, config, Some("client secret")),
    );

    let login = send(&app, page("/auth/login?next=%2Fjobs", None)).await;
    assert_eq!(login.status, StatusCode::SEE_OTHER, "{}", login.body);
    let to_sso = login.headers[header::LOCATION].to_str().unwrap().to_owned();
    assert!(
        to_sso.starts_with(&format!("{}/authorize?", sso.server.uri())),
        "{to_sso}"
    );
    assert_eq!(
        query_value(&to_sso, "redirect_uri"),
        "https://aegis.test/auth/callback"
    );
    assert_eq!(query_value(&to_sso, "code_challenge_method"), "S256");
    let (state, nonce) = (query_value(&to_sso, "state"), query_value(&to_sso, "nonce"));
    let pending = set_cookie(&login.headers, "__Host-aegis_login");
    let set = login.headers[header::SET_COOKIE].to_str().unwrap();
    assert!(set.contains("HttpOnly") && set.contains("Secure"), "{set}");

    // Another state than the one sent, or no pending cookie: refused.
    let forged = format!("/auth/callback?code=c&state=x{state}");
    let refused = send(&app, page(&forged, Some(&pending))).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let callback = format!("/auth/callback?code=c&state={state}");
    let no_cookie = send(&app, page(&callback, None)).await;
    assert_eq!(no_cookie.status, StatusCode::FORBIDDEN);

    sso.will_sign_in("sub-42", "Dev@X.test", &nonce).await;
    let back = send(&app, page(&callback, Some(&pending))).await;
    assert_eq!(back.status, StatusCode::SEE_OTHER, "{}", back.body);
    assert_eq!(back.headers[header::LOCATION], "/jobs");
    let session = set_cookie(&back.headers, "__Host-aegis_session");
    let jobs = send(&app, page("/jobs", Some(&session))).await;
    assert_eq!(jobs.status, StatusCode::OK);
    assert!(
        jobs.body.contains("dev@x.test") && jobs.body.contains("repo-a"),
        "{}",
        jobs.body
    );
    // The role granted by e-mail before the first sign-in holds.
    assert!(
        audit(s)
            .await
            .iter()
            .any(|e| e.1 == "dev@x.test" && e.2 == "session.login")
    );
    // The cookie holds a token the database keeps only hashed.
    let token = session.split_once('=').unwrap().1;
    assert_eq!(token.len(), 64);

    // The SSO rotates its keys: the next sign-in reads them again.
    sso.rotate_keys().await;
    let login = send(&app, page("/auth/login", None)).await;
    let to_sso = login.headers[header::LOCATION].to_str().unwrap().to_owned();
    let (state, nonce) = (query_value(&to_sso, "state"), query_value(&to_sso, "nonce"));
    let pending = set_cookie(&login.headers, "__Host-aegis_login");
    sso.will_sign_in("sub-42", "dev@x.test", &nonce).await;
    // A failed callback spends the pending sign-in.
    let forged = format!("/auth/callback?code=c&state=x{state}");
    let refused = send(&app, page(&forged, Some(&pending))).await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    assert_eq!(
        set_cookie(&refused.headers, "__Host-aegis_login"),
        "__Host-aegis_login="
    );
    let callback = format!("/auth/callback?code=c2&state={state}");
    let back = send(&app, page(&callback, Some(&pending))).await;
    assert_eq!(back.status, StatusCode::SEE_OTHER, "{}", back.body);
    assert_eq!(back.headers[header::LOCATION], "/");
    db.drop().await;
}

#[tokio::test]
async fn the_api_does_not_start_without_the_secrets_of_sign_in() {
    use aegis_secrets::{MemorySource, Secrets};
    use aegis_server::auth::{OIDC_CLIENT_SECRET, SESSION_KEY, load_at_start};

    let with_sso = AuthSection {
        public_url: Some("https://aegis.test".parse().unwrap()),
        oidc: Some(OidcSection {
            issuer: "https://sso.test".into(),
            client_id: "aegis".into(),
        }),
        ..AuthSection::default()
    };
    // Neither: both named.
    let none = Secrets::new(MemorySource::new([("OTHER", "s3cret-other")]));
    let err = load_at_start(&none, &with_sso).await.unwrap_err();
    assert!(
        err.contains(SESSION_KEY) && err.contains(OIDC_CLIENT_SECRET),
        "{err}"
    );
    // Without SSO the client secret is not needed.
    let err = load_at_start(&none, &AuthSection::default())
        .await
        .unwrap_err();
    assert!(
        err.contains(SESSION_KEY) && !err.contains(OIDC_CLIENT_SECRET),
        "{err}"
    );
    // Too short a key is named, its value is not.
    let short = Secrets::new(MemorySource::new([
        (SESSION_KEY, "sh0rt-k3y"),
        (OIDC_CLIENT_SECRET, "client-s3cret"),
    ]));
    let err = load_at_start(&short, &with_sso).await.unwrap_err();
    assert!(
        err.contains(SESSION_KEY) && !err.contains("sh0rt-k3y"),
        "{err}"
    );
    // Both there: read from the store now, and the API can be built.
    let key = common::KEY;
    let both = Secrets::new(MemorySource::new([
        (SESSION_KEY, key),
        (OIDC_CLIENT_SECRET, "client-s3cret"),
    ]));
    load_at_start(&both, &with_sso).await.unwrap();
    assert_eq!(
        both.get(OIDC_CLIENT_SECRET).unwrap().expose(),
        "client-s3cret"
    );
    let Some(db) = test_db().await else { return };
    aegis_server::auth::Auth::new(db.store.clone(), with_sso, both).unwrap();
    db.drop().await;
}
