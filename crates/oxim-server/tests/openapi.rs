//! The OpenAPI document: it matches the copy the web UI generates its types
//! from, and real responses have the shapes it describes.
//!
//! All patient data in these tests is synthetic.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use http_body_util::BodyExt;
use oxim_alert::{AlertEngine, AlertSettings, Sources};
use oxim_auth::{AuthStore, NewUser, Role};
use oxim_core::{
    DestinationConnector, Engine, EngineOptions, Registry, SendError, SourceConnector,
    SourceContext, SystemClock, async_trait,
};
use oxim_devices::registry::Sighting;
use oxim_devices::{DeviceEnvironment, DeviceIdentity, DeviceRegistry};
use oxim_model::{
    ChannelId, ConnectorId, DataType, DeviceId, Envelope, MessageIdGenerator, MessageStatus,
};
use oxim_server::history::ChannelHistory;
use oxim_server::{AppState, ServerConfig, Services, router};
use oxim_store::{Delivery, SqliteStore};
use serde_json::{Value, json};
use tower::ServiceExt;

const PASSWORD: &str = "correct horse battery";

const ORU: &[u8] = b"MSH|^~\\&|LAB|HOSP|LIS|HOSP|20260929120000||ORU^R01|1|P|2.5.1\r\
PID|1||MRN4711^^^HOSP^MR||Testpatient^Synthetic||19700101|F\r\
OBX|1|NM|GLU^Glucose||5.4|mmol/L\r";

/// The copy of the document in the web UI's source tree.
fn ui_copy() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ui/openapi.json")
}

#[test]
fn the_ui_copy_of_the_document_is_current() {
    let current = serde_json::to_string_pretty(&oxim_server::openapi()).unwrap() + "\n";
    let path = ui_copy();
    if std::env::var_os("OXIM_BLESS").is_some() {
        std::fs::write(&path, &current).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path)
        .unwrap_or_default()
        .replace("\r\n", "\n");
    assert!(
        committed == current,
        "ui/openapi.json is out of date: run `OXIM_BLESS=1 cargo test -p oxim-server --test openapi` \
         and then `npm run gen:api` in ui/"
    );
}

/// Checks that `value` has the shape of `schema`, resolving references
/// against `schemas`. Supports the subset of JSON Schema the document uses.
fn check(schemas: &Value, schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    if let Some(target) = schema.get("$ref").and_then(Value::as_str) {
        let name = target.trim_start_matches("#/components/schemas/");
        let resolved = schemas
            .get(name)
            .ok_or_else(|| format!("{path}: unknown schema {name}"))?;
        return check(schemas, resolved, value, path);
    }
    if let Some(options) = schema.get("oneOf").and_then(Value::as_array) {
        let matches = options
            .iter()
            .filter(|option| check(schemas, option, value, path).is_ok())
            .count();
        return if matches == 1 {
            Ok(())
        } else {
            Err(format!(
                "{path}: {matches} oneOf alternatives match {value}"
            ))
        };
    }
    if let Some(kinds) = schema.get("type") {
        let kinds: Vec<&str> = match kinds {
            Value::String(kind) => vec![kind.as_str()],
            Value::Array(kinds) => kinds.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        let actual = match value {
            Value::Null => "null",
            Value::Bool(_) => "boolean",
            Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
            Value::Number(_) => "number",
            Value::String(_) => "string",
            Value::Array(_) => "array",
            Value::Object(_) => "object",
        };
        let fits = kinds.contains(&actual) || (actual == "integer" && kinds.contains(&"number"));
        if !fits {
            return Err(format!("{path}: expected {kinds:?}, found {value}"));
        }
    }
    if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
        && !allowed.contains(value)
    {
        return Err(format!("{path}: {value} is not one of {allowed:?}"));
    }
    match value {
        Value::Object(object) => {
            let properties = schema.get("properties").and_then(Value::as_object);
            if let Some(required) = schema.get("required").and_then(Value::as_array) {
                for name in required.iter().filter_map(Value::as_str) {
                    if !object.contains_key(name) {
                        return Err(format!("{path}: missing property {name}"));
                    }
                }
            }
            for (name, item) in object {
                let item_path = format!("{path}/{name}");
                match properties.and_then(|properties| properties.get(name)) {
                    Some(property) => check(schemas, property, item, &item_path)?,
                    None => match schema.get("additionalProperties") {
                        Some(Value::Bool(false)) => {
                            return Err(format!("{item_path}: property not in the schema"));
                        }
                        Some(extra @ Value::Object(_)) => check(schemas, extra, item, &item_path)?,
                        _ => {}
                    },
                }
            }
        }
        Value::Array(items) => {
            if let Some(item_schema) = schema.get("items") {
                for (index, item) in items.iter().enumerate() {
                    check(schemas, item_schema, item, &format!("{path}/{index}"))?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

#[derive(Debug)]
struct IdleSource;

#[async_trait]
impl SourceConnector for IdleSource {
    async fn run(&self, context: SourceContext) -> Result<(), oxim_core::ConnectorError> {
        context.cancelled().await;
        Ok(())
    }
}

#[derive(Debug)]
struct Accept;

#[async_trait]
impl DestinationConnector for Accept {
    async fn send(&self, _: &Delivery) -> Result<Option<Vec<u8>>, SendError> {
        Ok(Some(b"MSA|AA|1".to_vec()))
    }
}

struct Harness {
    app: Router,
    document: Value,
}

impl Harness {
    async fn call(
        &self,
        method: Method,
        uri: &str,
        token: Option<&str>,
        body: Option<(&str, String)>,
    ) -> (StatusCode, Value) {
        let mut request = Request::builder().method(method).uri(uri);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let body = match body {
            Some((kind, text)) => {
                request = request.header(header::CONTENT_TYPE, kind);
                Body::from(text)
            }
            None => Body::empty(),
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        (status, value)
    }

    /// Calls a documented operation and checks the response against its
    /// success schema. Returns the body.
    async fn expect(
        &self,
        method: Method,
        uri: &str,
        template: &str,
        token: Option<&str>,
        body: Option<(&str, String)>,
    ) -> Value {
        let (status, value) = self.call(method.clone(), uri, token, body).await;
        let operation = &self.document["paths"][template][method.as_str().to_lowercase()];
        assert!(
            operation.is_object(),
            "{method} {template} is not documented"
        );
        let responses = operation["responses"].as_object().unwrap();
        let (code, response) = responses
            .iter()
            .find(|(code, _)| code.starts_with('2'))
            .unwrap();
        assert_eq!(status.as_str(), code, "{method} {uri}: {value}");
        if let Some(schema) = response["content"]["application/json"].get("schema") {
            let schemas = &self.document["components"]["schemas"];
            if let Err(problem) = check(schemas, schema, &value, "") {
                panic!("{method} {uri} does not match its schema: {problem}\n{value:#}");
            }
        }
        value
    }
}

#[tokio::test]
async fn responses_match_the_documented_schemas() {
    let dir = tempfile::tempdir().unwrap();
    for sub in ["channels", "tables", "data"] {
        std::fs::create_dir_all(dir.path().join(sub)).unwrap();
    }
    std::fs::write(dir.path().join("channels/broken.yaml"), "id: [").unwrap();
    std::fs::write(
        dir.path().join("tables/broken.csv"),
        "nothing,useful\n1,2\n",
    )
    .unwrap();
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
    for (name, role) in [("admin", Role::Admin), ("viewer", Role::Viewer)] {
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
    let config = ServerConfig::new(
        dir.path().join("channels"),
        dir.path().join("tables"),
        dir.path().join("data"),
    );
    // Operations services: an alert that always fires, a device registry
    // with one seen and one declared device, and channel history.
    let alert_settings: AlertSettings = serde_json::from_value(json!({
        "targets": [{"id": "log", "type": "log"}],
        "rules": [{"id": "disk", "kind": "disk_space", "below": "100%", "severity": "warning"}]
    }))
    .unwrap();
    let alerts = Arc::new(AlertEngine::new(alert_settings, engine.registry()).unwrap());
    let snapshot = alerts
        .collect(&Sources {
            engine: engine.clone(),
            devices: None,
            data_dir: dir.path().join("data"),
            certificates: Vec::new(),
        })
        .await;
    alerts.process(&snapshot);
    let registry = Arc::new(DeviceRegistry::open_in_memory().unwrap());
    let device = DeviceId::new("chem-1").unwrap();
    let lab = ChannelId::new("lab").unwrap();
    registry
        .record(&Sighting {
            device: &device,
            channel: &lab,
            message: MessageIdGenerator::new().next(1_790_000_000_000, 7),
            at: now,
            silence_after: Some(Duration::from_secs(1800)),
            identity: &DeviceIdentity {
                model: Some("Chemistry analyzer".into()),
                serial_number: Some("SN-0001".into()),
                ..DeviceIdentity::default()
            },
        })
        .unwrap();
    let devices = DeviceEnvironment::with_registry(registry);
    devices.declare(
        DeviceId::new("chem-2").unwrap(),
        Some(Duration::from_secs(600)),
    );
    let services = Services::new()
        .with_alerts(alerts)
        .with_devices(devices)
        .with_history(Arc::new(ChannelHistory::open_in_memory().unwrap()));
    let harness = Harness {
        app: router(AppState::with_services(
            engine.clone(),
            auth,
            config,
            services,
        )),
        document: oxim_server::openapi(),
    };
    let json_body = |value: Value| Some(("application/json", value.to_string()));

    let login = |user: &str| json_body(json!({ "username": user, "password": PASSWORD }));
    let session = harness
        .expect(
            Method::POST,
            "/api/v1/auth/login",
            "/api/v1/auth/login",
            None,
            login("admin"),
        )
        .await;
    let admin_token = session["token"].as_str().unwrap().to_owned();
    let admin = Some(admin_token.as_str());
    harness
        .expect(
            Method::GET,
            "/api/v1/auth/me",
            "/api/v1/auth/me",
            admin,
            None,
        )
        .await;

    let channel = "id: lab\nname: Laboratory\nsource:\n  type: idle\n  data_type: hl7v2\ndestinations:\n  - id: lis\n    type: accept\n";
    harness
        .expect(
            Method::PUT,
            "/api/v1/channels/lab",
            "/api/v1/channels/{id}",
            admin,
            Some(("application/yaml", channel.to_owned())),
        )
        .await;
    let (status, _) = harness
        .call(Method::POST, "/api/v1/channels/lab/deploy", admin, None)
        .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let channels = harness
        .expect(
            Method::GET,
            "/api/v1/channels",
            "/api/v1/channels",
            admin,
            None,
        )
        .await;
    assert_eq!(channels["channels"].as_array().unwrap().len(), 2);
    harness
        .expect(
            Method::GET,
            "/api/v1/channels/lab",
            "/api/v1/channels/{id}",
            admin,
            None,
        )
        .await;

    // A message processed by the channel, with a destination response.
    let mut ids = MessageIdGenerator::new();
    let id = ids.next(1_790_000_000_001, 1);
    let channel_id = ChannelId::new("lab").unwrap();
    let mut envelope = Envelope::new(
        id,
        channel_id.clone(),
        ConnectorId::new("source").unwrap(),
        engine.clock().now(),
        DataType::Hl7V2,
        ORU.to_vec(),
    );
    envelope
        .metadata
        .insert("file".into(), "synthetic.hl7".into());
    engine
        .store()
        .run(move |store| store.receive(&[envelope]))
        .await
        .unwrap();
    engine.process_stored(&channel_id, id).await.unwrap();
    for _ in 0..400 {
        let record = engine
            .store()
            .run(move |store| store.message(id))
            .await
            .unwrap()
            .unwrap();
        if record.status == MessageStatus::Completed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let list = harness
        .expect(
            Method::GET,
            "/api/v1/messages?limit=1",
            "/api/v1/messages",
            admin,
            None,
        )
        .await;
    assert_eq!(list["messages"].as_array().unwrap().len(), 1);
    let detail = harness
        .expect(
            Method::GET,
            &format!("/api/v1/messages/{id}"),
            "/api/v1/messages/{id}",
            admin,
            None,
        )
        .await;
    assert!(
        detail["contents"].as_array().unwrap().len() >= 2,
        "{detail}"
    );
    for (stage, destination) in [("raw", None), ("response", Some("lis"))] {
        let query = match destination {
            Some(destination) => format!("stage={stage}&destination={destination}"),
            None => format!("stage={stage}"),
        };
        harness
            .expect(
                Method::GET,
                &format!("/api/v1/messages/{id}/content?{query}"),
                "/api/v1/messages/{id}/content",
                admin,
                None,
            )
            .await;
    }
    let viewer_session = harness
        .expect(
            Method::POST,
            "/api/v1/auth/login",
            "/api/v1/auth/login",
            None,
            login("viewer"),
        )
        .await;
    let viewer = viewer_session["token"].as_str().unwrap().to_owned();
    let masked = harness
        .expect(
            Method::GET,
            &format!("/api/v1/messages/{id}/content?stage=raw"),
            "/api/v1/messages/{id}/content",
            Some(&viewer),
            None,
        )
        .await;
    assert_eq!(masked["masked"], true);
    harness
        .expect(
            Method::POST,
            &format!("/api/v1/messages/{id}/content/unmasked"),
            "/api/v1/messages/{id}/content/unmasked",
            Some(&viewer),
            json_body(json!({ "stage": "raw", "reason": "synthetic schema test" })),
        )
        .await;
    harness
        .expect(
            Method::POST,
            &format!("/api/v1/messages/{id}/reprocess"),
            "/api/v1/messages/{id}/reprocess",
            admin,
            None,
        )
        .await;

    std::fs::write(dir.path().join("tables/codes.csv"), "from,to\nGLU,2345-7\n").unwrap();
    let tables = harness
        .expect(Method::GET, "/api/v1/tables", "/api/v1/tables", admin, None)
        .await;
    assert_eq!(tables["tables"].as_array().unwrap().len(), 2);
    harness
        .expect(
            Method::GET,
            "/api/v1/tables/codes.csv",
            "/api/v1/tables/{name}",
            admin,
            None,
        )
        .await;
    harness
        .expect(
            Method::PUT,
            "/api/v1/tables/codes.csv",
            "/api/v1/tables/{name}",
            admin,
            Some(("text/csv", "from,to\nGLU,2345-7\nNA,2951-2\n".to_owned())),
        )
        .await;

    harness
        .expect(Method::GET, "/api/v1/users", "/api/v1/users", admin, None)
        .await;
    harness
        .expect(
            Method::POST,
            "/api/v1/users",
            "/api/v1/users",
            admin,
            json_body(json!({
                "username": "operator1",
                "password": "another long password",
                "role": "operator",
            })),
        )
        .await;
    harness
        .expect(
            Method::PATCH,
            "/api/v1/users/operator1",
            "/api/v1/users/{username}",
            admin,
            json_body(json!({ "display_name": "Night shift", "disabled": true })),
        )
        .await;
    harness
        .expect(
            Method::DELETE,
            "/api/v1/users/viewer/sessions",
            "/api/v1/users/{username}/sessions",
            admin,
            None,
        )
        .await;
    harness
        .expect(
            Method::POST,
            "/api/v1/tokens",
            "/api/v1/tokens",
            admin,
            json_body(json!({ "name": "monitoring", "role": "viewer" })),
        )
        .await;
    harness
        .expect(Method::GET, "/api/v1/tokens", "/api/v1/tokens", admin, None)
        .await;
    let audit = harness
        .expect(Method::GET, "/api/v1/audit", "/api/v1/audit", admin, None)
        .await;
    assert!(!audit["events"].as_array().unwrap().is_empty());
    harness
        .expect(Method::GET, "/api/v1/system", "/api/v1/system", admin, None)
        .await;

    // Operations.
    let overview = harness
        .expect(Method::GET, "/api/v1/alerts", "/api/v1/alerts", admin, None)
        .await;
    assert_eq!(overview["active"][0]["rule"], "disk");
    assert_eq!(overview["targets"][0]["type"], "log");
    let devices = harness
        .expect(
            Method::GET,
            "/api/v1/devices",
            "/api/v1/devices",
            admin,
            None,
        )
        .await;
    assert_eq!(devices["devices"].as_array().unwrap().len(), 2, "{devices}");
    harness
        .expect(
            Method::DELETE,
            "/api/v1/devices/lab/chem-1",
            "/api/v1/devices/{channel}/{device}",
            admin,
            None,
        )
        .await;
    let changed = channel.replace("name: Laboratory", "name: Core laboratory");
    harness
        .expect(
            Method::PUT,
            "/api/v1/channels/lab",
            "/api/v1/channels/{id}",
            admin,
            Some(("application/yaml", changed)),
        )
        .await;
    let history = harness
        .expect(
            Method::GET,
            "/api/v1/channels/lab/history",
            "/api/v1/channels/{id}/history",
            admin,
            None,
        )
        .await;
    assert_eq!(
        history["versions"].as_array().unwrap().len(),
        2,
        "{history}"
    );
    let first = harness
        .expect(
            Method::GET,
            "/api/v1/channels/lab/history/1",
            "/api/v1/channels/{id}/history/{version}",
            admin,
            None,
        )
        .await;
    assert_eq!(first["yaml"], channel);
    let restored = harness
        .expect(
            Method::POST,
            "/api/v1/channels/lab/history/1/restore",
            "/api/v1/channels/{id}/history/{version}/restore",
            admin,
            None,
        )
        .await;
    assert_eq!(restored["version"], 3);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("channels/lab.yaml")).unwrap(),
        channel
    );
    let maintenance = harness
        .expect(
            Method::POST,
            "/api/v1/system/maintenance",
            "/api/v1/system/maintenance",
            admin,
            json_body(json!({ "enabled": true, "reason": "LIS upgrade" })),
        )
        .await;
    assert_eq!(maintenance["enabled"], true);
    let system = harness
        .expect(Method::GET, "/api/v1/system", "/api/v1/system", admin, None)
        .await;
    assert_eq!(system["maintenance"]["reason"], "LIS upgrade");
    harness
        .expect(
            Method::POST,
            "/api/v1/system/maintenance",
            "/api/v1/system/maintenance",
            admin,
            json_body(json!({ "enabled": false })),
        )
        .await;
    let created = harness
        .expect(
            Method::POST,
            "/api/v1/backups",
            "/api/v1/backups",
            admin,
            None,
        )
        .await;
    let name = created["name"].as_str().unwrap().to_owned();
    let backups = harness
        .expect(
            Method::GET,
            "/api/v1/backups",
            "/api/v1/backups",
            admin,
            None,
        )
        .await;
    assert_eq!(backups["backups"][0]["name"], name.as_str());
    let (status, _) = harness
        .call(Method::GET, &format!("/api/v1/backups/{name}"), admin, None)
        .await;
    assert_eq!(status, StatusCode::OK);
    // Viewers see devices but not backups (their earlier sessions ended).
    let viewer_session = harness
        .expect(
            Method::POST,
            "/api/v1/auth/login",
            "/api/v1/auth/login",
            None,
            login("viewer"),
        )
        .await;
    let viewer = viewer_session["token"].as_str().unwrap().to_owned();
    let (status, _) = harness
        .call(Method::GET, "/api/v1/devices", Some(&viewer), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = harness
        .call(Method::GET, "/api/v1/backups", Some(&viewer), None)
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // The first server-sent event has the documented shape.
    let request = Request::builder()
        .uri("/api/v1/events")
        .header(header::AUTHORIZATION, format!("Bearer {admin_token}"))
        .body(Body::empty())
        .unwrap();
    let response = harness.app.clone().oneshot(request).await.unwrap();
    let mut body = response.into_body();
    let frame = tokio::time::timeout(Duration::from_secs(5), body.frame())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let text = String::from_utf8(frame.into_data().unwrap().to_vec()).unwrap();
    let data = text
        .lines()
        .find_map(|line| line.strip_prefix("data: "))
        .unwrap();
    let event: Value = serde_json::from_str(data).unwrap();
    let schemas = &harness.document["components"]["schemas"];
    check(schemas, &schemas["StatsEvent"], &event, "").unwrap();
    engine.shutdown().await;
}
