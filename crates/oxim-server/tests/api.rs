//! The REST API against a running engine with an in-memory store.
//!
//! All patient data in these tests is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use oxim_auth::{AuthStore, NewUser, Role};
use oxim_core::{
    DestinationConnector, Engine, EngineOptions, Registry, SendError, SourceConnector,
    SourceContext, SystemClock, async_trait,
};
use oxim_model::{
    ChannelId, ConnectorId, DataType, Envelope, MessageId, MessageIdGenerator, MessageStatus,
};
use oxim_server::{AppState, ServerConfig, router};
use oxim_store::{Delivery, SqliteStore};
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

const ORU: &[u8] = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|1|P|2.5.1\r\
PID|1||MRN4711^^^HOSP^MR||Testpatient^Synthetic||19700101|F|||1 Example Road^^Sampletown\r\
OBX|1|NM|GLU^Glucose||5.4|mmol/L\r";

const CHANNEL: &str = "id: lab
name: Laboratory
source:
  type: idle
  data_type: hl7v2
destinations:
  - id: lis
    type: accept
";

/// A source that only waits; tests store messages directly.
#[derive(Debug)]
struct IdleSource;

#[async_trait]
impl SourceConnector for IdleSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        context.cancelled().await;
        Ok(())
    }
}

/// A destination that accepts everything.
#[derive(Debug)]
struct Accept;

#[async_trait]
impl DestinationConnector for Accept {
    async fn send(&self, _: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        Ok(None)
    }
}

struct Api {
    app: Router,
    engine: Engine,
    auth: Arc<AuthStore>,
    dir: TempDir,
    ids: std::sync::Mutex<(MessageIdGenerator, u64)>,
}

#[derive(Clone)]
enum Auth {
    None,
    Bearer(String),
    Cookie {
        session: String,
        csrf: Option<String>,
    },
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|_| {
            panic!(
                "not JSON ({}): {}",
                self.status,
                String::from_utf8_lossy(&self.body)
            )
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn error_code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap().to_owned()
    }
}

async fn setup() -> Api {
    setup_with(|_| {}).await
}

async fn setup_with(configure: impl FnOnce(&mut ServerConfig)) -> Api {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["channels", "tables", "data"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    let mut registry = Registry::new();
    registry.add_source("idle", |_| {
        Ok(Arc::new(IdleSource) as Arc<dyn SourceConnector>)
    });
    registry.add_destination("accept", |_| {
        Ok(Arc::new(Accept) as Arc<dyn DestinationConnector>)
    });
    let mut options = EngineOptions::default();
    options.idle_poll = Duration::from_millis(50);
    options.shutdown_grace = Duration::from_secs(2);
    let engine = Engine::start(
        Box::new(SqliteStore::open_in_memory().unwrap()),
        registry,
        Arc::new(SystemClock),
        options,
    )
    .await
    .unwrap();
    let auth = Arc::new(AuthStore::open_in_memory().unwrap());
    let now = engine.clock().now();
    for (name, role) in [
        ("admin", Role::Admin),
        ("operator", Role::Operator),
        ("viewer", Role::Viewer),
    ] {
        auth.create_user(
            &NewUser {
                username: name,
                display_name: name,
                password: PASSWORD,
                role,
            },
            now,
        )
        .unwrap();
    }
    let mut config = ServerConfig::new(
        dir.path().join("channels"),
        dir.path().join("tables"),
        dir.path().join("data"),
    );
    configure(&mut config);
    let state = AppState::new(engine.clone(), auth.clone(), config);
    Api {
        app: router(state),
        engine,
        auth,
        dir,
        ids: std::sync::Mutex::new((MessageIdGenerator::new(), 1_790_000_000_000)),
    }
}

impl Api {
    async fn call(&self, method: Method, uri: &str, auth: &Auth, body: Option<Body>) -> Reply {
        self.call_with(method, uri, auth, body, None).await
    }

    async fn call_with(
        &self,
        method: Method,
        uri: &str,
        auth: &Auth,
        body: Option<Body>,
        content_type: Option<&str>,
    ) -> Reply {
        let mut request = Request::builder().method(method).uri(uri);
        match auth {
            Auth::None => {}
            Auth::Bearer(token) => {
                request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
            }
            Auth::Cookie { session, csrf } => {
                request = request.header(
                    header::COOKIE,
                    format!(
                        "oxim_session={session}; oxim_csrf={}",
                        csrf.clone().unwrap_or_default()
                    ),
                );
                if let Some(csrf) = csrf {
                    request = request.header("x-csrf-token", csrf);
                }
            }
        }
        if let Some(kind) = content_type {
            request = request.header(header::CONTENT_TYPE, kind);
        }
        let request = request.body(body.unwrap_or_else(Body::empty)).unwrap();
        let response = self.app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec();
        Reply {
            status,
            headers,
            body,
        }
    }

    async fn get(&self, uri: &str, auth: &Auth) -> Reply {
        self.call(Method::GET, uri, auth, None).await
    }

    async fn send_json(&self, method: Method, uri: &str, auth: &Auth, body: Value) -> Reply {
        self.call_with(
            method,
            uri,
            auth,
            Some(Body::from(body.to_string())),
            Some("application/json"),
        )
        .await
    }

    async fn send_text(&self, method: Method, uri: &str, auth: &Auth, body: &str) -> Reply {
        self.call_with(
            method,
            uri,
            auth,
            Some(Body::from(body.to_owned())),
            Some("text/plain"),
        )
        .await
    }

    async fn login_reply(&self, username: &str, password: &str) -> Reply {
        self.send_json(
            Method::POST,
            "/api/v1/auth/login",
            &Auth::None,
            json!({ "username": username, "password": password }),
        )
        .await
    }

    /// Logs in and returns the bearer token.
    async fn login(&self, username: &str) -> Auth {
        let reply = self.login_reply(username, PASSWORD).await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        Auth::Bearer(reply.json()["token"].as_str().unwrap().to_owned())
    }

    async fn deploy_channel(&self, admin: &Auth) {
        let reply = self
            .send_text(Method::PUT, "/api/v1/channels/lab", admin, CHANNEL)
            .await;
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        let reply = self
            .call(Method::POST, "/api/v1/channels/lab/deploy", admin, None)
            .await;
        assert_eq!(reply.status, StatusCode::NO_CONTENT, "{}", reply.text());
    }

    /// Stores a message as a source would and waits until it is completed.
    async fn receive(&self, raw: &[u8]) -> MessageId {
        let id = {
            let mut ids = self.ids.lock().unwrap();
            ids.1 += 1;
            let millis = ids.1;
            ids.0.next(millis, u128::from(millis))
        };
        let channel = ChannelId::new("lab").unwrap();
        let mut envelope = Envelope::new(
            id,
            channel.clone(),
            ConnectorId::new("source").unwrap(),
            self.engine.clock().now(),
            DataType::Hl7V2,
            raw.to_vec(),
        );
        envelope
            .metadata
            .insert("file".into(), "Testpatient_Synthetic.hl7".into());
        self.engine
            .store()
            .run(move |store| store.receive(&[envelope]))
            .await
            .unwrap();
        self.engine.process_stored(&channel, id).await.unwrap();
        for _ in 0..400 {
            let record = self
                .engine
                .store()
                .run(move |store| store.message(id))
                .await
                .unwrap()
                .unwrap();
            if record.status == MessageStatus::Completed {
                return id;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("message {id} was not completed");
    }

    async fn audit_actions(&self, admin: &Auth) -> Vec<(String, String, String)> {
        let reply = self.get("/api/v1/audit?limit=500", admin).await;
        assert_eq!(reply.status, StatusCode::OK);
        reply.json()["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|event| {
                (
                    event["action"].as_str().unwrap_or_default().to_owned(),
                    event["actor"].as_str().unwrap_or_default().to_owned(),
                    event["detail"].as_str().unwrap_or_default().to_owned(),
                )
            })
            .collect()
    }
}

#[tokio::test]
async fn login_me_and_logout_with_cookies_and_csrf() {
    let api = setup().await;
    let reply = api.login_reply("admin", PASSWORD).await;
    assert_eq!(reply.status, StatusCode::OK);
    let body = reply.json();
    assert_eq!(body["user"]["role"], "admin");
    let cookies: Vec<&str> = reply
        .headers
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    let session_cookie = cookies
        .iter()
        .find(|cookie| cookie.starts_with("oxim_session="))
        .unwrap();
    assert!(session_cookie.contains("HttpOnly"));
    assert!(session_cookie.contains("Secure"));
    assert!(session_cookie.contains("SameSite=Strict"));
    let csrf_cookie = cookies
        .iter()
        .find(|cookie| cookie.starts_with("oxim_csrf="))
        .unwrap();
    assert!(!csrf_cookie.contains("HttpOnly"));

    let session = body["token"].as_str().unwrap().to_owned();
    let csrf = body["csrf_token"].as_str().unwrap().to_owned();
    let cookie = Auth::Cookie {
        session: session.clone(),
        csrf: None,
    };
    let me = api.get("/api/v1/auth/me", &cookie).await;
    assert_eq!(me.status, StatusCode::OK);
    assert_eq!(me.json()["username"], "admin");
    assert!(
        me.json()["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("manage_users"))
    );

    // Cookie-authenticated changes need the CSRF header.
    let refused = api
        .call(Method::POST, "/api/v1/auth/logout", &cookie, None)
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let wrong = Auth::Cookie {
        session: session.clone(),
        csrf: Some("wrong".into()),
    };
    let refused = api
        .call(Method::POST, "/api/v1/auth/logout", &wrong, None)
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);
    let right = Auth::Cookie {
        session: session.clone(),
        csrf: Some(csrf),
    };
    let done = api
        .call(Method::POST, "/api/v1/auth/logout", &right, None)
        .await;
    assert_eq!(done.status, StatusCode::NO_CONTENT);
    assert!(
        done.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|value| value.to_str().unwrap().starts_with("oxim_session=;"))
    );
    let gone = api.get("/api/v1/auth/me", &Auth::Bearer(session)).await;
    assert_eq!(gone.status, StatusCode::UNAUTHORIZED);
    assert_eq!(gone.error_code(), "unauthenticated");
}

#[tokio::test]
async fn failed_logins_are_rejected_audited_and_throttled() {
    let api = setup().await;
    let reply = api.login_reply("admin", "not the password").await;
    assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
    assert_eq!(reply.error_code(), "invalid_credentials");
    let unknown = api.login_reply("nobody", "not the password").await;
    assert_eq!(unknown.status, StatusCode::UNAUTHORIZED);
    assert_eq!(unknown.json(), reply.json(), "unknown users look the same");
    for _ in 0..9 {
        api.login_reply("admin", "not the password").await;
    }
    let blocked = api.login_reply("admin", PASSWORD).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(blocked.error_code(), "too_many_attempts");
    assert!(blocked.headers.contains_key(header::RETRY_AFTER));
    // Other users are not affected.
    let admin = api.login("operator").await;
    let events = api.get("/api/v1/audit", &admin).await;
    assert_eq!(
        events.status,
        StatusCode::FORBIDDEN,
        "operators cannot read the audit trail"
    );
    let admin_token = api
        .auth
        .create_api_token("audit", Role::Admin, "test", None, api.engine.clock().now())
        .unwrap()
        .0;
    let actions = api.audit_actions(&Auth::Bearer(admin_token)).await;
    assert!(
        actions
            .iter()
            .any(|(action, actor, _)| action == "auth.login_failed" && actor == "admin")
    );
}

#[tokio::test]
async fn every_documented_endpoint_exists_and_requires_authentication() {
    let api = setup().await;
    let served = api.get("/api/v1/openapi.json", &Auth::None).await;
    assert_eq!(served.status, StatusCode::OK);
    let document = served.json();
    assert_eq!(document, oxim_server::openapi());
    assert_eq!(document["openapi"], "3.1.0");
    let paths = document["paths"].as_object().unwrap();
    let mut checked = 0;
    for (path, operations) in paths {
        let uri = path
            .replace("{id}", "x1")
            .replace("{name}", "x.csv")
            .replace("{username}", "someone");
        for (method, operation) in operations.as_object().unwrap() {
            let method: Method = method.to_uppercase().parse().unwrap();
            let public = operation.get("security") == Some(&json!([]));
            let reply = api.call(method.clone(), &uri, &Auth::None, None).await;
            if public {
                assert_ne!(reply.status, StatusCode::NOT_FOUND, "{method} {uri}");
                assert_ne!(
                    reply.status,
                    StatusCode::METHOD_NOT_ALLOWED,
                    "{method} {uri}"
                );
            } else {
                assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{method} {uri}");
                assert_eq!(reply.error_code(), "unauthenticated");
            }
            checked += 1;
        }
    }
    assert!(checked >= 30, "{checked} operations");
    let unknown = api.get("/api/v1/nothing", &Auth::None).await;
    assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    assert_eq!(unknown.error_code(), "not_found");
    let invalid = api
        .get("/api/v1/auth/me", &Auth::Bearer("oxs_forged".into()))
        .await;
    assert_eq!(invalid.status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn roles_limit_what_callers_may_do() {
    let api = setup().await;
    let admin = api.login("admin").await;
    let operator = api.login("operator").await;
    let viewer = api.login("viewer").await;
    api.deploy_channel(&admin).await;

    assert_eq!(
        api.get("/api/v1/channels", &viewer).await.status,
        StatusCode::OK
    );
    // Channel YAML may hold credentials.
    assert_eq!(
        api.get("/api/v1/channels/lab", &viewer).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        api.get("/api/v1/channels/lab", &operator).await.status,
        StatusCode::OK
    );
    let denied = api
        .send_text(Method::PUT, "/api/v1/channels/lab", &viewer, CHANNEL)
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
    assert_eq!(denied.error_code(), "forbidden");
    for uri in ["/api/v1/users", "/api/v1/tokens", "/api/v1/audit"] {
        assert_eq!(
            api.get(uri, &viewer).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        assert_eq!(
            api.get(uri, &operator).await.status,
            StatusCode::FORBIDDEN,
            "{uri}"
        );
        assert_eq!(api.get(uri, &admin).await.status, StatusCode::OK, "{uri}");
    }
    let undeploy = api
        .call(Method::POST, "/api/v1/channels/lab/undeploy", &viewer, None)
        .await;
    assert_eq!(undeploy.status, StatusCode::FORBIDDEN);
    let undeploy = api
        .call(
            Method::POST,
            "/api/v1/channels/lab/undeploy",
            &operator,
            None,
        )
        .await;
    assert_eq!(undeploy.status, StatusCode::NO_CONTENT);
    let id = {
        api.deploy_channel(&admin).await;
        api.receive(ORU).await
    };
    let erase = api
        .send_json(
            Method::POST,
            &format!("/api/v1/messages/{id}/erase"),
            &operator,
            json!({ "reason": "patient request 2026-17" }),
        )
        .await;
    assert_eq!(erase.status, StatusCode::FORBIDDEN);
    let system = api.get("/api/v1/system", &viewer).await;
    assert_eq!(system.status, StatusCode::OK);
    assert_eq!(system.json()["deployed_channels"], json!(["lab"]));
}

#[tokio::test]
async fn viewers_see_masked_content_and_break_glass_is_audited() {
    let api = setup().await;
    let admin = api.login("admin").await;
    let operator = api.login("operator").await;
    let viewer = api.login("viewer").await;
    api.deploy_channel(&admin).await;
    let id = api.receive(ORU).await;

    let record = api.get(&format!("/api/v1/messages/{id}"), &viewer).await;
    assert_eq!(record.status, StatusCode::OK);
    let record = record.json();
    assert_eq!(record["status"], "completed");
    assert_eq!(record["metadata"]["file"], "***");
    let stages: Vec<&str> = record["contents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|content| content["stage"].as_str().unwrap())
        .collect();
    assert!(stages.contains(&"raw"), "{stages:?}");
    assert!(stages.contains(&"encoded"), "{stages:?}");

    let masked = api
        .get(&format!("/api/v1/messages/{id}/content?stage=raw"), &viewer)
        .await;
    assert_eq!(masked.status, StatusCode::OK);
    let masked = masked.json();
    assert_eq!(masked["masked"], true);
    let text = masked["data"].as_str().unwrap();
    assert!(text.contains("PID|1||***||***||***|F|||***"), "{text}");
    assert!(!text.contains("Testpatient"));
    assert!(!text.contains("MRN4711"));
    assert!(text.contains("OBX|1|NM|GLU^Glucose||5.4|mmol/L"));

    let encoded = api
        .get(
            &format!("/api/v1/messages/{id}/content?stage=encoded"),
            &viewer,
        )
        .await;
    assert_eq!(
        encoded.status,
        StatusCode::BAD_REQUEST,
        "encoded needs a destination"
    );
    let encoded = api
        .get(
            &format!("/api/v1/messages/{id}/content?stage=encoded&destination=lis"),
            &viewer,
        )
        .await;
    assert_eq!(encoded.status, StatusCode::OK);
    assert!(!encoded.text().contains("Testpatient"));

    let unmasked = api
        .get(
            &format!("/api/v1/messages/{id}/content?stage=raw"),
            &operator,
        )
        .await
        .json();
    assert_eq!(unmasked["masked"], false);
    assert!(
        unmasked["data"]
            .as_str()
            .unwrap()
            .contains("Testpatient^Synthetic")
    );
    let record = api
        .get(&format!("/api/v1/messages/{id}"), &operator)
        .await
        .json();
    assert_eq!(record["metadata"]["file"], "Testpatient_Synthetic.hl7");

    // Break-glass: a reason is mandatory.
    let uri = format!("/api/v1/messages/{id}/content/unmasked");
    let refused = api
        .send_json(Method::POST, &uri, &viewer, json!({ "stage": "raw" }))
        .await;
    assert_eq!(refused.status, StatusCode::BAD_REQUEST);
    let glass = api
        .send_json(
            Method::POST,
            &uri,
            &viewer,
            json!({ "stage": "raw", "reason": "clinical emergency, ward 3" }),
        )
        .await;
    assert_eq!(glass.status, StatusCode::OK, "{}", glass.text());
    assert!(
        glass.json()["data"]
            .as_str()
            .unwrap()
            .contains("Testpatient")
    );

    // API tokens cannot break the glass.
    let now = api.engine.clock().now();
    let (token, _) = api
        .auth
        .create_api_token("dashboard", Role::Viewer, "admin", None, now)
        .unwrap();
    let refused = api
        .send_json(
            Method::POST,
            &uri,
            &Auth::Bearer(token),
            json!({ "stage": "raw", "reason": "clinical emergency, ward 3" }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    let actions = api.audit_actions(&admin).await;
    let viewed: Vec<_> = actions
        .iter()
        .filter(|(action, _, _)| action == "message.content_viewed")
        .collect();
    assert_eq!(viewed.len(), 3, "{viewed:?}");
    assert!(
        viewed
            .iter()
            .any(|(_, actor, detail)| actor == "viewer" && detail == "stage=raw masked")
    );
    assert!(
        viewed
            .iter()
            .any(|(_, actor, detail)| actor == "operator" && detail == "stage=raw unmasked")
    );
    assert!(
        actions
            .iter()
            .any(|(action, actor, detail)| action == "message.break_glass"
                && actor == "viewer"
                && detail.contains("reason: clinical emergency, ward 3"))
    );
    let per_message = api
        .get(
            &format!("/api/v1/audit?message={id}&action=message.break_glass"),
            &admin,
        )
        .await
        .json();
    assert_eq!(per_message["events"].as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn channels_are_validated_saved_deployed_and_deleted() {
    let api = setup().await;
    let admin = api.login("admin").await;
    let uri = "/api/v1/channels/lab";

    let broken = api
        .send_text(Method::PUT, uri, &admin, "id: [unclosed")
        .await;
    assert_eq!(broken.status, StatusCode::BAD_REQUEST);
    assert_eq!(broken.error_code(), "invalid_channel");
    let unknown = api
        .send_text(
            Method::PUT,
            uri,
            &admin,
            &CHANNEL.replace("type: idle", "type: nosuch"),
        )
        .await;
    assert_eq!(unknown.status, StatusCode::BAD_REQUEST);
    assert!(
        unknown.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("nosuch")
    );
    let other = api
        .send_text(Method::PUT, "/api/v1/channels/other", &admin, CHANNEL)
        .await;
    assert_eq!(other.status, StatusCode::BAD_REQUEST);
    assert!(!api.dir.path().join("channels/lab.yaml").exists());

    let saved = api.send_text(Method::PUT, uri, &admin, CHANNEL).await;
    assert_eq!(saved.status, StatusCode::OK);
    assert_eq!(saved.json()["file"], "lab.yaml");
    let on_disk = std::fs::read_to_string(api.dir.path().join("channels/lab.yaml")).unwrap();
    assert_eq!(on_disk, CHANNEL);
    let fetched = api.get(uri, &admin).await.json();
    assert_eq!(fetched["yaml"], CHANNEL);
    assert_eq!(fetched["deployed"], false);

    std::fs::write(api.dir.path().join("channels/broken.yaml"), "id: [").unwrap();
    let listed = api.get("/api/v1/channels", &admin).await.json();
    let channels = listed["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 2);
    assert!(
        channels
            .iter()
            .any(|c| c["file"] == "broken.yaml" && c["error"].is_string())
    );

    let deploy = api
        .call(Method::POST, "/api/v1/channels/lab/deploy", &admin, None)
        .await;
    assert_eq!(deploy.status, StatusCode::NO_CONTENT);
    let again = api
        .call(Method::POST, "/api/v1/channels/lab/deploy", &admin, None)
        .await;
    assert_eq!(again.status, StatusCode::CONFLICT);
    let redeploy = api
        .call(Method::POST, "/api/v1/channels/lab/redeploy", &admin, None)
        .await;
    assert_eq!(redeploy.status, StatusCode::NO_CONTENT);
    let listed = api.get("/api/v1/channels", &admin).await.json();
    let lab = listed["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "lab")
        .unwrap()
        .clone();
    assert_eq!(lab["deployed"], true);
    assert_eq!(lab["destinations"][0]["queue"]["queued"], 0);

    let deleted = api.call(Method::DELETE, uri, &admin, None).await;
    assert_eq!(deleted.status, StatusCode::NO_CONTENT);
    assert!(!api.dir.path().join("channels/lab.yaml").exists());
    let archived: Vec<_> = std::fs::read_dir(api.dir.path().join("channels/.deleted"))
        .unwrap()
        .collect();
    assert_eq!(archived.len(), 1);
    assert!(api.engine.deployed().await.is_empty());
    assert_eq!(api.get(uri, &admin).await.status, StatusCode::NOT_FOUND);

    let actions = api.audit_actions(&admin).await;
    for expected in [
        "channel.saved",
        "channel.deployed",
        "channel.redeployed",
        "channel.deleted",
    ] {
        assert!(
            actions.iter().any(|(action, _, _)| action == expected),
            "{expected}"
        );
    }
}

#[tokio::test]
async fn messages_are_listed_paginated_repaired_and_erased() {
    let api = setup().await;
    let admin = api.login("admin").await;
    let operator = api.login("operator").await;
    api.deploy_channel(&admin).await;
    let first = api.receive(ORU).await;
    let second = api.receive(ORU).await;

    let page = api
        .get("/api/v1/messages?channel=lab&limit=1", &operator)
        .await;
    assert_eq!(page.status, StatusCode::OK);
    let page = page.json();
    assert_eq!(page["messages"][0]["id"], second.to_string());
    assert_eq!(page["next_before"], second.to_string());
    let next = api
        .get(
            &format!("/api/v1/messages?limit=1&before={second}"),
            &operator,
        )
        .await
        .json();
    assert_eq!(next["messages"][0]["id"], first.to_string());
    let filtered = api
        .get("/api/v1/messages?status=error", &operator)
        .await
        .json();
    assert!(filtered["messages"].as_array().unwrap().is_empty());
    let invalid = api.get("/api/v1/messages?status=bogus", &operator).await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);

    let reprocess = api
        .call(
            Method::POST,
            &format!("/api/v1/messages/{first}/reprocess"),
            &operator,
            None,
        )
        .await;
    assert_eq!(reprocess.status, StatusCode::OK, "{}", reprocess.text());
    assert_eq!(reprocess.json()["scheduled"], true);

    // A delivery that was sent cannot be requeued.
    let requeue = api
        .send_json(
            Method::POST,
            &format!("/api/v1/messages/{second}/requeue"),
            &operator,
            json!({ "destination": "lis" }),
        )
        .await;
    assert_eq!(requeue.status, StatusCode::CONFLICT, "{}", requeue.text());

    let uri = format!("/api/v1/messages/{second}/erase");
    let no_reason = api.send_json(Method::POST, &uri, &admin, json!({})).await;
    assert_eq!(no_reason.status, StatusCode::BAD_REQUEST);
    let erased = api
        .send_json(
            Method::POST,
            &uri,
            &admin,
            json!({ "reason": "erasure request 2026-17" }),
        )
        .await;
    assert_eq!(erased.status, StatusCode::NO_CONTENT);
    let gone = api.get(&format!("/api/v1/messages/{second}"), &admin).await;
    assert_eq!(gone.status, StatusCode::NOT_FOUND);
    let trail = api
        .get(&format!("/api/v1/audit?message={second}"), &admin)
        .await
        .json();
    assert_eq!(trail["events"][0]["action"], "message.erased");
    assert_eq!(
        trail["events"][0]["detail"],
        "reason: erasure request 2026-17"
    );
}

#[tokio::test]
async fn code_tables_are_validated() {
    let api = setup().await;
    let operator = api.login("operator").await;
    let viewer = api.login("viewer").await;
    let bad_name = api
        .send_text(
            Method::PUT,
            "/api/v1/tables/..%2Fescape.csv",
            &operator,
            "from,to\n",
        )
        .await;
    assert_eq!(bad_name.status, StatusCode::BAD_REQUEST);
    let invalid = api
        .send_text(
            Method::PUT,
            "/api/v1/tables/tests.csv",
            &operator,
            "code,target\nA,B\n",
        )
        .await;
    assert_eq!(invalid.status, StatusCode::BAD_REQUEST);
    assert_eq!(invalid.error_code(), "invalid_table");
    let saved = api
        .send_text(
            Method::PUT,
            "/api/v1/tables/tests.csv",
            &operator,
            "from,to,display\nGLU,2345-7,Glucose\n",
        )
        .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
    assert_eq!(saved.json()["entries"], 1);
    let listed = api.get("/api/v1/tables", &viewer).await.json();
    assert_eq!(listed["tables"][0]["name"], "tests.csv");
    assert_eq!(listed["tables"][0]["entries"], 1);
    let fetched = api.get("/api/v1/tables/tests.csv", &viewer).await.json();
    assert!(fetched["csv"].as_str().unwrap().contains("GLU"));
    let denied = api
        .send_text(
            Method::PUT,
            "/api/v1/tables/tests.csv",
            &viewer,
            "from,to\n",
        )
        .await;
    assert_eq!(denied.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn admins_manage_users_and_tokens() {
    let api = setup().await;
    let admin = api.login("admin").await;
    let weak = api
        .send_json(
            Method::POST,
            "/api/v1/users",
            &admin,
            json!({ "username": "nurse", "password": "short", "role": "viewer" }),
        )
        .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    let created = api
        .send_json(
            Method::POST,
            "/api/v1/users",
            &admin,
            json!({ "username": "nurse", "display_name": "Night Nurse", "password": PASSWORD, "role": "viewer" }),
        )
        .await;
    assert_eq!(created.status, StatusCode::CREATED, "{}", created.text());
    assert!(!created.text().contains("argon2"));
    let duplicate = api
        .send_json(
            Method::POST,
            "/api/v1/users",
            &admin,
            json!({ "username": "NURSE", "password": PASSWORD, "role": "viewer" }),
        )
        .await;
    assert_eq!(duplicate.status, StatusCode::CONFLICT);

    let nurse = api.login("nurse").await;
    let disabled = api
        .send_json(
            Method::PATCH,
            "/api/v1/users/nurse",
            &admin,
            json!({ "disabled": true }),
        )
        .await;
    assert_eq!(disabled.status, StatusCode::OK);
    assert_eq!(
        api.get("/api/v1/auth/me", &nurse).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api.login_reply("nurse", PASSWORD).await.status,
        StatusCode::UNAUTHORIZED
    );
    let last_admin = api
        .send_json(
            Method::PATCH,
            "/api/v1/users/admin",
            &admin,
            json!({ "role": "viewer" }),
        )
        .await;
    assert_eq!(last_admin.status, StatusCode::CONFLICT);

    let token = api
        .send_json(
            Method::POST,
            "/api/v1/tokens",
            &admin,
            json!({ "name": "prometheus", "role": "viewer" }),
        )
        .await;
    assert_eq!(token.status, StatusCode::CREATED);
    let token = token.json();
    let secret = token["token"].as_str().unwrap().to_owned();
    assert!(secret.starts_with("oxt_"));
    let bearer = Auth::Bearer(secret);
    assert_eq!(
        api.get("/api/v1/channels", &bearer).await.status,
        StatusCode::OK
    );
    let me = api.get("/api/v1/auth/me", &bearer).await.json();
    assert_eq!(me["kind"], "api_token");
    let listed = api.get("/api/v1/tokens", &admin).await;
    assert!(!listed.text().contains(token["token"].as_str().unwrap()));
    let revoke = api
        .call(
            Method::DELETE,
            &format!("/api/v1/tokens/{}", token["id"]),
            &admin,
            None,
        )
        .await;
    assert_eq!(revoke.status, StatusCode::NO_CONTENT);
    assert_eq!(
        api.get("/api/v1/channels", &bearer).await.status,
        StatusCode::UNAUTHORIZED
    );

    let operator = api.login("operator").await;
    let refused = api
        .send_json(
            Method::POST,
            "/api/v1/tokens",
            &operator,
            json!({ "name": "x", "role": "viewer" }),
        )
        .await;
    assert_eq!(refused.status, StatusCode::FORBIDDEN);

    let revoked = api
        .call(
            Method::DELETE,
            "/api/v1/users/operator/sessions",
            &admin,
            None,
        )
        .await;
    assert_eq!(revoked.status, StatusCode::OK);
    assert_eq!(revoked.json()["ended"], 1);
    assert_eq!(
        api.get("/api/v1/auth/me", &operator).await.status,
        StatusCode::UNAUTHORIZED
    );

    let actions = api.audit_actions(&admin).await;
    for expected in [
        "user.created",
        "user.updated",
        "token.created",
        "token.revoked",
        "user.sessions_revoked",
    ] {
        assert!(
            actions.iter().any(|(action, _, _)| action == expected),
            "{expected}"
        );
    }
}

#[tokio::test]
async fn users_change_their_own_password() {
    let api = setup().await;
    let viewer = api.login("viewer").await;
    let wrong = api
        .send_json(
            Method::POST,
            "/api/v1/auth/password",
            &viewer,
            json!({ "current_password": "not the password", "new_password": "another long secret" }),
        )
        .await;
    assert_eq!(wrong.status, StatusCode::UNAUTHORIZED);
    let changed = api
        .send_json(
            Method::POST,
            "/api/v1/auth/password",
            &viewer,
            json!({ "current_password": PASSWORD, "new_password": "another long secret" }),
        )
        .await;
    assert_eq!(changed.status, StatusCode::NO_CONTENT);
    assert_eq!(
        api.get("/api/v1/auth/me", &viewer).await.status,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        api.login_reply("viewer", "another long secret")
            .await
            .status,
        StatusCode::OK
    );
}

#[tokio::test]
async fn metrics_use_the_prometheus_format() {
    let api = setup().await;
    let admin = api.login("admin").await;
    api.deploy_channel(&admin).await;
    api.receive(ORU).await;
    let reply = api.get("/metrics", &admin).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert!(
        reply.headers[header::CONTENT_TYPE]
            .to_str()
            .unwrap()
            .starts_with("text/plain; version=0.0.4")
    );
    let text = reply.text();
    for expected in [
        "# TYPE oxim_messages gauge",
        "oxim_messages{channel=\"lab\",status=\"completed\"} 1",
        "oxim_deliveries{channel=\"lab\",destination=\"lis\",status=\"sent\"} 1",
        "oxim_channel_deployed{channel=\"lab\"} 1",
        "oxim_queue_oldest_pending_seconds{channel=\"lab\",destination=\"lis\"} 0.000",
        "# TYPE oxim_http_responses_total counter",
        "oxim_login_failures_total 0",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in\n{text}");
    }
    for line in text.lines().filter(|line| !line.starts_with('#')) {
        let (_, value) = line.rsplit_once(' ').unwrap();
        value.parse::<f64>().unwrap();
    }
    assert_eq!(
        api.get("/metrics", &Auth::None).await.status,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn responses_carry_security_headers_and_json_errors() {
    let api = setup().await;
    let reply = api.get("/api/v1/auth/me", &Auth::None).await;
    for (name, value) in [
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
    ] {
        assert_eq!(reply.headers[name], value, "{name}");
    }
    assert!(reply.headers.contains_key("content-security-policy"));
    assert!(!reply.headers.contains_key("strict-transport-security"));

    let bad_json = api
        .call_with(
            Method::POST,
            "/api/v1/auth/login",
            &Auth::None,
            Some(Body::from("{not json")),
            Some("application/json"),
        )
        .await;
    assert_eq!(bad_json.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_json.error_code(), "invalid_request");
    let wrong_method = api
        .call(Method::DELETE, "/api/v1/auth/login", &Auth::None, None)
        .await;
    assert_eq!(wrong_method.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(wrong_method.error_code(), "method_not_allowed");

    let root = api.get("/", &Auth::None).await.json();
    assert_eq!(root["api"], "/api/v1");
}

#[tokio::test]
async fn request_bodies_are_limited() {
    let api = setup_with(|config| config.max_body_bytes = 1024).await;
    let admin = api.login("admin").await;
    let large = format!("from,to\n{}", "A,B\n".repeat(1000));
    let reply = api
        .send_text(Method::PUT, "/api/v1/tables/big.csv", &admin, &large)
        .await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(reply.error_code(), "payload_too_large");
}

#[tokio::test]
async fn the_ui_is_served_when_installed() {
    let ui = tempfile::tempdir().unwrap();
    std::fs::write(ui.path().join("index.html"), "<html>oxim ui</html>").unwrap();
    std::fs::write(ui.path().join("app.js"), "console.log(1)").unwrap();
    let path = ui.path().to_path_buf();
    let api = setup_with(move |config| config.ui_dir = Some(path)).await;
    assert!(api.get("/", &Auth::None).await.text().contains("oxim ui"));
    assert!(
        api.get("/app.js", &Auth::None)
            .await
            .text()
            .contains("console")
    );
    assert!(
        api.get("/channels/lab", &Auth::None)
            .await
            .text()
            .contains("oxim ui")
    );
    let api_missing = api.get("/api/v1/nothing", &Auth::None).await;
    assert_eq!(api_missing.status, StatusCode::NOT_FOUND);
    assert_eq!(api_missing.error_code(), "not_found");
}

#[tokio::test]
async fn event_streams_send_stats_and_are_bounded() {
    let api = setup_with(|config| config.max_event_clients = 1).await;
    let admin = api.login("admin").await;
    let Auth::Bearer(token) = &admin else {
        unreachable!()
    };
    let request = || {
        Request::builder()
            .uri("/api/v1/events")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap()
    };
    let response = api.app.clone().oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "text/event-stream"
    );
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    assert!(text.starts_with("event: stats\ndata: {"), "{text}");

    let second = api.app.clone().oneshot(request()).await.unwrap();
    assert_eq!(second.status(), StatusCode::SERVICE_UNAVAILABLE);
    drop(body);
    let third = api.app.clone().oneshot(request()).await.unwrap();
    assert_eq!(third.status(), StatusCode::OK);
}
