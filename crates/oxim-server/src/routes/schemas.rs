//! JSON schemas of the request and response bodies, for the OpenAPI
//! document. The web UI generates its TypeScript types from them, so they
//! must match what the handlers produce; the API tests check real responses
//! against these schemas.

use oxim_auth::{Permission, Role};
use serde_json::{Map, Value, json};

fn string() -> Value {
    json!({ "type": "string" })
}

fn nullable_string() -> Value {
    json!({ "type": ["string", "null"] })
}

fn time() -> Value {
    json!({ "type": "string", "format": "date-time" })
}

fn nullable_time() -> Value {
    json!({ "type": ["string", "null"], "format": "date-time" })
}

fn integer() -> Value {
    json!({ "type": "integer", "minimum": 0 })
}

fn reference(name: &str) -> Value {
    json!({ "$ref": format!("#/components/schemas/{name}") })
}

fn array(items: Value) -> Value {
    json!({ "type": "array", "items": items })
}

/// An object whose listed properties are all required.
fn object(properties: &[(&str, Value)]) -> Value {
    object_with(properties, &[])
}

/// An object with required and optional properties.
fn object_with(required: &[(&str, Value)], optional: &[(&str, Value)]) -> Value {
    let mut map = Map::new();
    for (name, schema) in required.iter().chain(optional) {
        map.insert((*name).to_owned(), schema.clone());
    }
    let names: Vec<&str> = required.iter().map(|(name, _)| *name).collect();
    json!({
        "type": "object",
        "required": names,
        "properties": map,
        "additionalProperties": false,
    })
}

fn names<T: serde::Serialize>(values: impl IntoIterator<Item = T>) -> Vec<Value> {
    values
        .into_iter()
        .filter_map(|value| serde_json::to_value(value).ok())
        .collect()
}

const MESSAGE_STATUSES: &[&str] = &["received", "filtered", "transformed", "completed", "error"];
const DESTINATION_STATUSES: &[&str] = &[
    "queued", "sending", "sent", "filtered", "retrying", "failed",
];
const STAGES: &[&str] = &[
    "raw",
    "normalized",
    "transformed",
    "encoded",
    "response",
    "reply",
];

/// The `components.schemas` section.
pub(crate) fn schemas() -> Value {
    let queue = object(&[
        ("queued", integer()),
        ("sending", integer()),
        ("retrying", integer()),
        ("failed", integer()),
        ("oldest_pending_at", nullable_time()),
    ]);
    let destination_state = object(&[
        ("destination", string()),
        ("status", reference("DestinationStatus")),
        ("attempts", integer()),
        ("last_error", nullable_string()),
        ("next_attempt_at", nullable_time()),
        ("updated_at", time()),
    ]);
    let record_fields = [
        ("id", string()),
        ("channel", string()),
        ("connector", string()),
        ("received_at", time()),
        ("data_type", string()),
        ("status", reference("MessageStatus")),
        ("peer", nullable_string()),
        ("device", nullable_string()),
        ("correlation_id", nullable_string()),
        (
            "metadata",
            json!({ "type": "object", "additionalProperties": { "type": "string" } }),
        ),
        ("error", nullable_string()),
        ("destinations", array(reference("DestinationState"))),
    ];
    let content_info = object(&[
        ("stage", reference("Stage")),
        ("destination", nullable_string()),
        ("data_type", nullable_string()),
        ("size", integer()),
    ]);
    let mut detail_fields = record_fields.to_vec();
    detail_fields.push(("contents", array(reference("ContentInfo"))));
    let principal = object(&[
        ("username", string()),
        ("display_name", string()),
        ("role", reference("Role")),
        (
            "kind",
            json!({ "type": "string", "enum": ["session", "api_token"] }),
        ),
        ("permissions", array(reference("Permission"))),
    ]);
    let user = object(&[
        ("username", string()),
        ("display_name", string()),
        ("role", reference("Role")),
        ("disabled", json!({ "type": "boolean" })),
        ("created_at", time()),
        ("updated_at", time()),
        ("last_login_at", nullable_time()),
    ]);
    let token = object(&[
        ("id", integer()),
        ("name", string()),
        ("role", reference("Role")),
        ("created_by", string()),
        ("created_at", time()),
        ("expires_at", nullable_time()),
        ("last_used_at", nullable_time()),
        ("revoked", json!({ "type": "boolean" })),
    ]);
    let channel = object_with(
        &[
            ("id", string()),
            ("file", string()),
            ("enabled", json!({ "type": "boolean" })),
            ("deployed", json!({ "type": "boolean" })),
            (
                "source",
                object(&[
                    ("id", string()),
                    ("type", string()),
                    ("data_type", string()),
                ]),
            ),
            (
                "destinations",
                array(object(&[
                    ("id", string()),
                    ("type", string()),
                    (
                        "queue",
                        json!({ "oneOf": [reference("QueueStats"), { "type": "null" }] }),
                    ),
                ])),
            ),
        ],
        &[
            ("name", nullable_string()),
            ("description", nullable_string()),
        ],
    );
    let broken_channel = object(&[("file", string()), ("error", string())]);
    let mut map = Map::new();
    map.insert(
        "Error".to_owned(),
        json!({
            "type": "object",
            "required": ["error"],
            "properties": {
                "error": {
                    "type": "object",
                    "required": ["code", "message"],
                    "properties": {
                        "code": { "type": "string" },
                        "message": { "type": "string" },
                    },
                },
            },
        }),
    );
    map.insert(
        "Permission".to_owned(),
        json!({ "type": "string", "enum": names(Permission::ALL) }),
    );
    map.insert(
        "Role".to_owned(),
        json!({ "type": "string", "enum": names(Role::ALL) }),
    );
    map.insert(
        "MessageStatus".to_owned(),
        json!({ "type": "string", "enum": MESSAGE_STATUSES }),
    );
    map.insert(
        "DestinationStatus".to_owned(),
        json!({ "type": "string", "enum": DESTINATION_STATUSES }),
    );
    map.insert(
        "Stage".to_owned(),
        json!({ "type": "string", "enum": STAGES }),
    );
    map.insert("Principal".to_owned(), principal);
    map.insert(
        "LoginRequest".to_owned(),
        object(&[("username", string()), ("password", string())]),
    );
    map.insert(
        "LoginResponse".to_owned(),
        object(&[
            ("token", string()),
            ("csrf_token", string()),
            ("expires_at", time()),
            ("user", reference("Principal")),
        ]),
    );
    map.insert(
        "PasswordChange".to_owned(),
        object(&[("current_password", string()), ("new_password", string())]),
    );
    map.insert("QueueStats".to_owned(), queue);
    map.insert("Channel".to_owned(), channel);
    map.insert("BrokenChannelFile".to_owned(), broken_channel);
    map.insert(
        "ChannelList".to_owned(),
        object(&[(
            "channels",
            array(json!({ "oneOf": [reference("Channel"), reference("BrokenChannelFile")] })),
        )]),
    );
    map.insert(
        "ChannelYaml".to_owned(),
        object(&[
            ("id", string()),
            ("file", string()),
            ("yaml", string()),
            ("deployed", json!({ "type": "boolean" })),
        ]),
    );
    map.insert(
        "ChannelSaved".to_owned(),
        object(&[("id", string()), ("file", string())]),
    );
    map.insert("DestinationState".to_owned(), destination_state);
    map.insert("MessageRecord".to_owned(), object(&record_fields));
    map.insert(
        "MessageList".to_owned(),
        object(&[
            ("messages", array(reference("MessageRecord"))),
            ("next_before", nullable_string()),
        ]),
    );
    map.insert("ContentInfo".to_owned(), content_info);
    map.insert("MessageDetail".to_owned(), object(&detail_fields));
    map.insert(
        "Content".to_owned(),
        object(&[
            ("stage", reference("Stage")),
            ("destination", nullable_string()),
            ("data_type", nullable_string()),
            ("masked", json!({ "type": "boolean" })),
            ("withheld", json!({ "type": "boolean" })),
            (
                "encoding",
                json!({ "type": ["string", "null"], "enum": ["utf8", "base64", null] }),
            ),
            ("data", nullable_string()),
        ]),
    );
    map.insert(
        "BreakGlassRequest".to_owned(),
        object_with(
            &[("stage", reference("Stage")), ("reason", string())],
            &[("destination", nullable_string())],
        ),
    );
    map.insert(
        "Reprocessed".to_owned(),
        object(&[
            ("id", string()),
            ("scheduled", json!({ "type": "boolean" })),
        ]),
    );
    map.insert(
        "RequeueRequest".to_owned(),
        object(&[("destination", string())]),
    );
    map.insert("EraseRequest".to_owned(), object(&[("reason", string())]));
    map.insert(
        "TableList".to_owned(),
        object(&[(
            "tables",
            array(object(&[
                ("name", string()),
                ("size", integer()),
                (
                    "entries",
                    json!({ "type": ["integer", "null"], "minimum": 0 }),
                ),
                ("error", nullable_string()),
            ])),
        )]),
    );
    map.insert(
        "Table".to_owned(),
        object(&[("name", string()), ("csv", string())]),
    );
    map.insert(
        "TableSaved".to_owned(),
        object(&[("name", string()), ("entries", integer())]),
    );
    map.insert("User".to_owned(), user);
    map.insert(
        "UserList".to_owned(),
        object(&[("users", array(reference("User")))]),
    );
    map.insert(
        "CreateUser".to_owned(),
        object_with(
            &[
                ("username", string()),
                ("password", string()),
                ("role", reference("Role")),
            ],
            &[("display_name", nullable_string())],
        ),
    );
    map.insert(
        "UpdateUser".to_owned(),
        object_with(
            &[],
            &[
                ("display_name", nullable_string()),
                ("role", reference("Role")),
                ("disabled", json!({ "type": "boolean" })),
            ],
        ),
    );
    map.insert("SetPassword".to_owned(), object(&[("password", string())]));
    map.insert("SessionsEnded".to_owned(), object(&[("ended", integer())]));
    map.insert("ApiToken".to_owned(), token);
    map.insert(
        "TokenList".to_owned(),
        object(&[("tokens", array(reference("ApiToken")))]),
    );
    map.insert(
        "CreateToken".to_owned(),
        object_with(
            &[("name", string()), ("role", reference("Role"))],
            &[("expires_at", nullable_time())],
        ),
    );
    map.insert(
        "CreatedToken".to_owned(),
        object(&[
            ("id", integer()),
            ("name", string()),
            ("role", reference("Role")),
            ("expires_at", nullable_time()),
            ("token", string()),
        ]),
    );
    map.insert(
        "AuditEvent".to_owned(),
        object(&[
            ("at", time()),
            ("action", string()),
            ("actor", string()),
            ("message_id", nullable_string()),
            ("channel", nullable_string()),
            ("detail", nullable_string()),
        ]),
    );
    map.insert(
        "AuditList".to_owned(),
        object(&[("events", array(reference("AuditEvent")))]),
    );
    map.insert(
        "SystemInfo".to_owned(),
        object(&[
            ("name", string()),
            ("version", string()),
            ("started_at", time()),
            ("uptime_seconds", integer()),
            ("now", time()),
            ("deployed_channels", array(string())),
            ("tls", json!({ "type": "boolean" })),
            ("session_idle_seconds", integer()),
            ("session_max_seconds", integer()),
            (
                "component_types",
                json!({ "type": "object", "additionalProperties": array(string()) }),
            ),
        ]),
    );
    map.insert(
        "StatsEvent".to_owned(),
        object(&[
            ("deployed", array(string())),
            (
                "messages",
                array(object(&[
                    ("channel", string()),
                    ("status", reference("MessageStatus")),
                    ("count", integer()),
                ])),
            ),
            (
                "deliveries",
                array(object(&[
                    ("channel", string()),
                    ("destination", string()),
                    ("status", reference("DestinationStatus")),
                    ("count", integer()),
                ])),
            ),
        ]),
    );
    Value::Object(map)
}
