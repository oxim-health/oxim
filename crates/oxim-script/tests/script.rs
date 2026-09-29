//! Script steps on synthetic messages (no real patient data).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use oxim_core::{
    ChannelConfig, DocumentParser, Encoder, Filter, MessageContext, Registry, StepConfig,
    Transformer,
};
use oxim_model::{
    ChannelId, ClinicalContent, CodeableConcept, Coding, ConnectorId, DataType, Envelope,
    MessageId, Observation, ResultGroup, Timestamp,
};
use oxim_script::{ScriptEncoder, ScriptEnvironment, ScriptFilter, ScriptTransformer};

const HL7: &[u8] = b"MSH|^~\\&|LAB|FAC|LIS|FAC|20260929120000||ORU^R01|MSG1|P|2.5.1\r\
PID|1||P100^^^LAB~P200^^^HIS||DOE^JANE||19800101|U\r\
OBR|1|ORD1||GLU\r\
OBX|1|NM|GLU^Glucose||5.4|mmol/L\r\
OBX|2|NM|K^Potassium||4.1|mmol/L\r";

const ASTM: &[u8] = b"H|\\^&|||ANALYZER\rP|1|P100\rO|1|S1||^^^GLU\rR|1|^^^GLU|5.4|mmol/L\rL|1|N\r";

fn context_with(data_type: DataType, raw: &[u8], channel: &str) -> MessageContext {
    let envelope = Envelope::new(
        MessageId::from_parts(1_790_000_000_000, 7),
        ChannelId::new(channel).unwrap(),
        ConnectorId::new("source").unwrap(),
        Timestamp::from_unix_nanos(1_790_000_000_000_000_000),
        data_type,
        raw.to_vec(),
    );
    MessageContext {
        document: DocumentParser::new(data_type, None)
            .unwrap()
            .parse(raw)
            .unwrap(),
        envelope,
        clinical: None,
        variables: BTreeMap::new(),
        response: None,
    }
}

fn context(data_type: DataType, raw: &[u8]) -> MessageContext {
    context_with(data_type, raw, "lab")
}

fn step(settings: serde_json::Value) -> StepConfig {
    StepConfig {
        kind: "script".into(),
        settings: match settings {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        },
    }
}

fn env() -> ScriptEnvironment {
    ScriptEnvironment::in_memory()
}

fn transformer(settings: serde_json::Value) -> ScriptTransformer {
    ScriptTransformer::from_step(&step(settings), &env()).unwrap()
}

fn filter(settings: serde_json::Value) -> ScriptFilter {
    ScriptFilter::from_step(&step(settings), &env()).unwrap()
}

fn encoder(settings: serde_json::Value) -> ScriptEncoder {
    ScriptEncoder::from_step(&step(settings), &env()).unwrap()
}

#[test]
fn reads_and_writes_hl7_paths() {
    let script = transformer(serde_json::json!({
        "source": "
            const family = msg.get('PID-5.1');
            msg.set('PID-5.1', family.toLowerCase());
            msg.set('OBX[2]-5', '4.2');
            vars.type = msg.dataType;
            vars.second = msg.get('PID-3[2].1');
            vars.missing = String(msg.get('ZZZ-1'));
            vars.raw = msg.raw.split('\\r')[1];
        "
    }));
    let mut ctx = context(DataType::Hl7V2, HL7);
    script.apply(&mut ctx).unwrap();
    assert_eq!(ctx.document.get("PID-5.1").unwrap().as_deref(), Some("doe"));
    assert_eq!(
        ctx.document.get("OBX[2]-5").unwrap().as_deref(),
        Some("4.2")
    );
    assert_eq!(ctx.variables["type"], "hl7v2");
    assert_eq!(ctx.variables["second"], "P200");
    assert_eq!(ctx.variables["missing"], "null");
    assert_eq!(
        ctx.variables["raw"],
        "PID|1||P100^^^LAB~P200^^^HIS||doe^JANE||19800101|U"
    );
}

#[test]
fn reads_and_writes_astm_and_json_paths() {
    let script = transformer(serde_json::json!({
        "source": "msg.set('R[1]-4', '6.0'); vars.test = msg.get('R-3');"
    }));
    let mut ctx = context(DataType::Astm, ASTM);
    script.apply(&mut ctx).unwrap();
    assert_eq!(ctx.document.get("R-4").unwrap().as_deref(), Some("6.0"));
    assert_eq!(ctx.variables["test"], "^^^GLU");

    let script = transformer(serde_json::json!({
        "source": "msg.set('results[0].value', msg.get('results[0].value') + '0');"
    }));
    let mut ctx = context(DataType::Json, br#"{"results":[{"value":"5.4"}]}"#);
    script.apply(&mut ctx).unwrap();
    assert_eq!(
        ctx.document.get("results[0].value").unwrap().as_deref(),
        Some("5.40")
    );
}

#[test]
fn filters_accept_expressions_and_bodies() {
    let ctx = context(DataType::Hl7V2, HL7);
    assert!(
        filter(serde_json::json!({"source": "msg.get('MSH-9.1') === 'ORU';"}))
            .accept(&ctx)
            .unwrap()
    );
    assert!(!filter(serde_json::json!({
        "source": "// Only admissions.\nif (msg.get('MSH-9.1') === 'ADT') {\n  return true;\n}\nreturn false;"
    }))
    .accept(&ctx)
    .unwrap());
    let error = filter(serde_json::json!({"source": "msg.get('MSH-9.1')"}))
        .accept(&ctx)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("must return true or false, not string"),
        "{error}"
    );
    // Filters cannot change the message.
    let error = filter(serde_json::json!({"source": "msg.set('PID-5.1', 'X'); return true;"}))
        .accept(&ctx)
        .unwrap_err();
    assert!(
        error.to_string().contains("cannot change the message"),
        "{error}"
    );
}

fn results() -> ClinicalContent {
    ClinicalContent::Results {
        device: None,
        groups: vec![ResultGroup {
            observations: vec![Observation {
                code: CodeableConcept::from_coding(Coding::new("GLU")),
                ..Observation::default()
            }],
            ..ResultGroup::default()
        }],
    }
}

#[test]
fn edits_and_replaces_normalized_content() {
    let mut ctx = context(DataType::Hl7V2, HL7);
    ctx.clinical = Some(results());
    transformer(serde_json::json!({
        "source": "clinical.groups[0].observations[0].code.codings[0].code = 'GLU-LIS';"
    }))
    .apply(&mut ctx)
    .unwrap();
    let Some(ClinicalContent::Results { groups, .. }) = &ctx.clinical else {
        panic!("expected results");
    };
    assert_eq!(
        groups[0].observations[0].code.primary_code(),
        Some("GLU-LIS")
    );

    // A script that leaves the content alone does not touch it.
    let before = ctx.clinical.clone();
    transformer(serde_json::json!({"source": "vars.kind = clinical.kind;"}))
        .apply(&mut ctx)
        .unwrap();
    assert_eq!(ctx.clinical, before);
    assert_eq!(ctx.variables["kind"], "results");

    let error = transformer(serde_json::json!({"source": "clinical = {kind: 'gossip'};"}))
        .apply(&mut ctx)
        .unwrap_err();
    assert!(
        error.to_string().contains("not valid normalized content"),
        "{error}"
    );
    assert_eq!(ctx.clinical, before);

    transformer(serde_json::json!({"source": "clinical = null;"}))
        .apply(&mut ctx)
        .unwrap();
    assert!(ctx.clinical.is_none());
}

#[test]
fn encoders_return_text_or_bytes() {
    let ctx = context(DataType::Hl7V2, HL7);
    let encoded = encoder(serde_json::json!({
        "data_type": "json",
        "source": "JSON.stringify({id: msg.get('MSH-10'), patient: msg.get('PID-3.1')})"
    }))
    .encode(&ctx)
    .unwrap();
    assert_eq!(encoded.data_type, DataType::Json);
    assert_eq!(encoded.data, br#"{"id":"MSG1","patient":"P100"}"#);

    let encoded = encoder(serde_json::json!({
        "source": "return new Uint8Array([0x0b, 0x41, 0x1c, 0x0d]);"
    }))
    .encode(&ctx)
    .unwrap();
    assert_eq!(encoded.data_type, DataType::Hl7V2);
    assert_eq!(encoded.data, [0x0b, 0x41, 0x1c, 0x0d]);

    let error = encoder(serde_json::json!({"source": "42"}))
        .encode(&ctx)
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("string or a Uint8Array, not number"),
        "{error}"
    );
    assert!(
        ScriptEncoder::from_step(
            &step(serde_json::json!({"source": "''", "data_type": "gibberish"})),
            &env()
        )
        .is_err()
    );
}

#[test]
fn transformers_set_replies() {
    let mut ctx = context(DataType::Hl7V2, HL7);
    transformer(serde_json::json!({
        "source": "reply('MSA|AA|' + msg.get('MSH-10'), 'raw');"
    }))
    .apply(&mut ctx)
    .unwrap();
    let response = ctx.response.unwrap();
    assert_eq!(response.data_type, DataType::Raw);
    assert_eq!(response.data, b"MSA|AA|MSG1");

    let error = filter(serde_json::json!({"source": "reply('x'); return true;"}))
        .accept(&context(DataType::Hl7V2, HL7))
        .unwrap_err();
    assert!(
        error.to_string().contains("only available in transformers"),
        "{error}"
    );
}

#[test]
fn runaway_scripts_are_stopped() {
    let ctx = context(DataType::Hl7V2, HL7);
    let looping = filter(serde_json::json!({
        "source": "while (true) {}",
        "timeout": "100ms"
    }));
    let started = Instant::now();
    let error = looping.accept(&ctx).unwrap_err();
    assert!(
        error.to_string().contains("ran longer than its timeout"),
        "{error}"
    );
    assert!(started.elapsed().as_secs() < 5);
    // The script cannot swallow the interruption.
    let stubborn = filter(serde_json::json!({
        "source": "try { while (true) {} } catch (e) {} return true;",
        "timeout": "100ms"
    }));
    assert!(stubborn.accept(&ctx).is_err());
    // A fresh runtime replaces the interrupted one.
    assert!(stubborn.accept(&ctx).is_err());

    let hungry = transformer(serde_json::json!({
        "source": "const chunks = []; while (true) { chunks.push(new Array(100000).fill(chunks.length)); }",
        "memory_limit": "8MiB",
        "timeout": "10s"
    }));
    let mut ctx = context(DataType::Hl7V2, HL7);
    let error = hungry.apply(&mut ctx).unwrap_err();
    assert!(
        error.to_string().contains("memory limit of 8388608 bytes"),
        "{error}"
    );
    // The message is intact after a failure.
    assert_eq!(ctx.document.get("MSH-10").unwrap().as_deref(), Some("MSG1"));

    let deep = transformer(serde_json::json!({
        "source": "function down(n) { return down(n + 1) + 1; } down(0);"
    }));
    let error = deep.apply(&mut ctx).unwrap_err();
    assert!(error.to_string().contains("stack limit"), "{error}");
}

#[test]
fn exceptions_name_the_line() {
    let mut ctx = context(DataType::Hl7V2, HL7);
    let error = transformer(serde_json::json!({
        "source": "const a = 1;\n\n    throw new TypeError('bad ' + a);"
    }))
    .apply(&mut ctx)
    .unwrap_err();
    assert!(
        error
            .to_string()
            .starts_with("script: script \"inline\": TypeError: bad 1 (line 3, column "),
        "{error}"
    );
    let error = transformer(serde_json::json!({"source": "throw 'plain';"}))
        .apply(&mut ctx)
        .unwrap_err();
    assert!(
        error.to_string().contains("uncaught exception: plain"),
        "{error}"
    );
    let error = transformer(serde_json::json!({"source": "msg.set('PID-0', 'x');"}))
        .apply(&mut ctx)
        .unwrap_err();
    assert!(error.to_string().contains("PID-0"), "{error}");

    // Syntax errors are found when the channel is deployed.
    let error = ScriptTransformer::from_step(
        &step(serde_json::json!({"source": "let ok = 1;\nlet broken = ;"})),
        &env(),
    )
    .unwrap_err();
    assert!(error.to_string().contains("SyntaxError"), "{error}");
    assert!(error.to_string().contains("line 2"), "{error}");
}

#[test]
fn the_sandbox_has_no_escape_hatches() {
    let ctx = context(DataType::Hl7V2, HL7);
    for (name, source) in [
        ("eval", "typeof eval === 'undefined'"),
        ("require", "typeof require === 'undefined'"),
        (
            "timers",
            "typeof setTimeout === 'undefined' && typeof setInterval === 'undefined'",
        ),
        (
            "std",
            "typeof std === 'undefined' && typeof os === 'undefined'",
        ),
        (
            "Function",
            "try { new Function('return 1'); return false; } catch (e) { return e instanceof EvalError; }",
        ),
        (
            "constructor",
            "try { (function () {}).constructor('return 1')(); return false; } catch (e) { return e instanceof EvalError; }",
        ),
        (
            "generator",
            "try { (function* () {}).constructor('yield 1'); return false; } catch (e) { return e instanceof EvalError; }",
        ),
        ("instanceof", "(function () {}) instanceof Function"),
    ] {
        assert!(
            filter(serde_json::json!({ "source": source }))
                .accept(&ctx)
                .unwrap(),
            "{name}"
        );
    }
}

const MIRTH_TRANSFORMER: &str = "
var family = msg['PID']['PID.5']['PID.5.1'].toString();
if (msg['MSH']['MSH.9']['MSH.9.1'] == 'ORU') {
    msg['PID']['PID.5']['PID.5.1'] = family.toUpperCase() + '-X';
}
msg['PID']['PID.8'] = 'F';
channelMap.put('results', msg['OBX'].length());
channelMap.put('second', msg['OBX'][1]['OBX.3']['OBX.3.1']);
$c('otherId', msg['PID']['PID.3'][1]['PID.3.1']);
connectorMap.put('seen', true);
responseMap.put('status', 'ok');
var count = globalChannelMap.get('count') || 0;
globalChannelMap.put('count', count + 1);
globalMap.put('shared', {last: msg['MSH']['MSH.10'].toString()});
tmp['PID']['PID.7'] = '19800102';
logger.info('processed ' + $('results') + ' results');
channelMap.put('segment', msg['PID'].toString().substring(0, 6));
channelMap.put('missing', msg['ZZZ']['ZZZ.1'].toString() === '' && msg['ZZZ'].length() === 0);
";

#[test]
fn mirth_scripts_run_unchanged() {
    let environment = ScriptEnvironment::in_memory();
    let settings = serde_json::json!({"mirth": true, "source": MIRTH_TRANSFORMER});
    let script = ScriptTransformer::from_step(&step(settings), &environment).unwrap();
    let mut ctx = context(DataType::Hl7V2, HL7);
    script.apply(&mut ctx).unwrap();
    assert_eq!(
        ctx.document.get("PID-5.1").unwrap().as_deref(),
        Some("DOE-X")
    );
    assert_eq!(ctx.document.get("PID-8").unwrap().as_deref(), Some("F"));
    assert_eq!(
        ctx.document.get("PID-7").unwrap().as_deref(),
        Some("19800102")
    );
    assert_eq!(ctx.variables["results"], "2");
    assert_eq!(ctx.variables["second"], "K");
    assert_eq!(ctx.variables["otherId"], "P200");
    assert_eq!(ctx.variables["connectorMap.seen"], "true");
    assert_eq!(ctx.variables["responseMap.status"], "ok");
    assert_eq!(ctx.variables["segment"], "PID|1|");
    assert_eq!(ctx.variables["missing"], "true");

    // globalChannelMap survives between messages of the channel and is
    // separate per channel; globalMap is shared.
    let mut second = context(DataType::Hl7V2, HL7);
    script.apply(&mut second).unwrap();
    let reader = ScriptTransformer::from_step(
        &step(serde_json::json!({
            "mirth": true,
            "source": "channelMap.put('count', $gc('count')); channelMap.put('last', globalMap.get('shared').last);"
        })),
        &environment,
    )
    .unwrap();
    let mut same = context(DataType::Hl7V2, HL7);
    reader.apply(&mut same).unwrap();
    assert_eq!(same.variables["count"], "2");
    assert_eq!(same.variables["last"], "MSG1");
    let mut other = context_with(DataType::Hl7V2, HL7, "other");
    reader.apply(&mut other).unwrap();
    assert_eq!(other.variables["count"], "");
    assert_eq!(other.variables["last"], "MSG1");

    // Mirth filter rules are function bodies returning a boolean.
    let rule = ScriptFilter::from_step(
        &step(serde_json::json!({
            "mirth": true,
            "source": "if (msg['OBX'][0]['OBX.5']['OBX.5.1'] == '5.4') { return true; }\nreturn false;"
        })),
        &environment,
    )
    .unwrap();
    assert!(rule.accept(&context(DataType::Hl7V2, HL7)).unwrap());
}

#[test]
fn scripts_load_from_the_scripts_directory() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("lab")).unwrap();
    std::fs::write(
        dir.path().join("lab").join("upper.js"),
        "\u{feff}msg.set('PID-5.1', msg.get('PID-5.1').toUpperCase());\nthrow new Error('here');",
    )
    .unwrap();
    let environment = ScriptEnvironment::new(dir.path());
    let script = ScriptTransformer::from_step(
        &step(serde_json::json!({"file": "lab/upper.js"})),
        &environment,
    )
    .unwrap();
    let error = script
        .apply(&mut context(DataType::Hl7V2, HL7))
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("\"lab/upper.js\": Error: here (line 2"),
        "{error}"
    );

    let outside = dir.path().join("outside.js");
    std::fs::write(&outside, "true").unwrap();
    for settings in [
        serde_json::json!({"file": "../outside.js"}),
        serde_json::json!({"file": outside.to_str().unwrap()}),
        serde_json::json!({"file": "missing.js"}),
        serde_json::json!({"file": "lab/upper.js", "source": "true"}),
        serde_json::json!({}),
        serde_json::json!({"source": "true", "timeout": "soon"}),
        serde_json::json!({"source": "true", "memory_limit": 1000}),
        serde_json::json!({"source": "true", "max_stack": "64MiB"}),
        serde_json::json!({"source": "true", "mirth": "yes"}),
        serde_json::json!({"source": "true", "unknown": 1}),
        serde_json::json!({"source": "true", "data_type": "json"}),
    ] {
        assert!(
            ScriptTransformer::from_step(&step(settings.clone()), &environment).is_err(),
            "{settings}"
        );
    }
}

#[test]
fn registers_filter_transformer_and_encoder() {
    let mut registry = Registry::new();
    oxim_script::register(&mut registry, ScriptEnvironment::in_memory());
    let config = ChannelConfig::from_yaml(
        "id: scripted
source: {type: mllp, data_type: hl7v2, settings: {listen: 127.0.0.1:0}}
filters:
  - {type: script, source: \"msg.get('MSH-9.1') === 'ORU'\"}
transformers:
  - {type: script, source: \"msg.set('MSH-3', 'OXIM')\"}
destinations:
  - id: out
    type: file
    encoder: {type: script, source: \"msg.raw.toLowerCase()\"}
    settings: {directory: out}
",
    )
    .unwrap();
    let pipeline = registry.compile(&config).unwrap();
    assert_eq!(pipeline.filters.len(), 1);
    assert_eq!(pipeline.transformers.len(), 1);
}

#[test]
fn runs_concurrently_and_quickly() {
    let script = Arc::new(transformer(serde_json::json!({
        "source": "msg.set('PID-5.1', msg.get('PID-5.1').toLowerCase()); vars.n = String(Number(vars.n || 0) + 1);"
    })));
    let threads: Vec<_> = (0..4)
        .map(|_| {
            let script = script.clone();
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let mut ctx = context(DataType::Hl7V2, HL7);
                    script.apply(&mut ctx).unwrap();
                    assert_eq!(ctx.variables["n"], "1");
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    let runs = 2000;
    let mut ctx = context(DataType::Hl7V2, HL7);
    let started = Instant::now();
    for _ in 0..runs {
        script.apply(&mut ctx).unwrap();
    }
    let per_message = started.elapsed() / runs;
    eprintln!("script transformer: {per_message:?} per message");
    assert!(per_message.as_millis() < 20, "{per_message:?}");
}
