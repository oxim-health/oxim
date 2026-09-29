//! Compiled scripts and the pool of QuickJS runtimes that run them.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use oxim_core::{Document, Encoded, EngineError, StepError};
use oxim_model::{ClinicalContent, DataType, Envelope};
use rquickjs::context::EvalOptions;
use rquickjs::{Context, Ctx, Exception, FromJs, Function, Object, Runtime, Value};
use tracing::{debug, error, info, warn};

use crate::environment::GlobalMaps;
use crate::settings::{Kind, ScriptSettings};

const PRELUDE: &str = include_str!("prelude.js");
const PRELUDE_NAME: &str = "oxim-prelude";
/// Name of every step type this crate registers.
pub(crate) const STEP: &str = "script";

/// The message a script works on while it runs.
#[derive(Debug)]
pub(crate) struct RunState {
    pub(crate) document: Document,
    /// Whether the script may change the message (transformers only).
    pub(crate) writable: bool,
    /// The reply set with `reply()`.
    pub(crate) response: Option<Encoded>,
    channel: String,
    message: String,
}

impl RunState {
    pub(crate) fn new(document: Document, writable: bool, envelope: &Envelope) -> Self {
        Self {
            document,
            writable,
            response: None,
            channel: envelope.channel.to_string(),
            message: envelope.id.to_string(),
        }
    }
}

/// What a script returned.
#[derive(Debug)]
pub(crate) enum Returned {
    Nothing,
    Bool(bool),
    Bytes(Vec<u8>),
}

/// The result of one run.
#[derive(Debug)]
pub(crate) struct Outcome {
    pub(crate) value: Returned,
    /// The replaced normalized content, when the script changed it.
    pub(crate) clinical: Option<Option<ClinicalContent>>,
    pub(crate) variables: BTreeMap<String, String>,
}

/// Why a run failed.
#[derive(Debug)]
enum Failure {
    Timeout,
    Memory,
    Stack,
    Script(String),
}

impl Failure {
    /// Whether the runtime must be discarded instead of reused.
    fn is_fatal(&self) -> bool {
        !matches!(self, Self::Script(_))
    }
}

/// State shared between a runtime's native functions, its interrupt
/// handler and the thread that runs it.
struct Shared {
    state: Mutex<Option<RunState>>,
    /// Nanoseconds after `base` at which the script is interrupted;
    /// `u64::MAX` when no script runs.
    deadline: AtomicU64,
    base: Instant,
    globals: Arc<GlobalMaps>,
    script: String,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn arm(&self, timeout: Duration) {
        let at = self.base.elapsed().saturating_add(timeout);
        let nanos = u64::try_from(at.as_nanos()).unwrap_or(u64::MAX - 1);
        self.deadline.store(nanos, Ordering::Relaxed);
    }

    fn disarm(&self) {
        self.deadline.store(u64::MAX, Ordering::Relaxed);
    }

    fn expired(&self) -> bool {
        let deadline = self.deadline.load(Ordering::Relaxed);
        deadline != u64::MAX
            && u64::try_from(self.base.elapsed().as_nanos()).unwrap_or(u64::MAX) >= deadline
    }

    /// Runs `f` on the current message; its error becomes a JavaScript
    /// exception.
    fn with_state<'js, R>(
        &self,
        ctx: &Ctx<'js>,
        f: impl FnOnce(&mut RunState) -> Result<R, String>,
    ) -> rquickjs::Result<R> {
        let mut guard = lock(&self.state);
        let Some(state) = guard.as_mut() else {
            return Err(Exception::throw_internal(
                ctx,
                "no message is being processed",
            ));
        };
        let result = f(state);
        drop(guard);
        result.map_err(|message| Exception::throw_message(ctx, &message))
    }
}

fn data_type(name: Option<String>, fallback: DataType) -> Result<DataType, String> {
    match name {
        None => Ok(fallback),
        Some(name) => serde_json::from_value(serde_json::Value::String(name.clone()))
            .map_err(|_| format!("unknown data type {name:?}")),
    }
}

fn data_type_name(data_type: DataType) -> String {
    match serde_json::to_value(data_type) {
        Ok(serde_json::Value::String(name)) => name,
        _ => format!("{data_type:?}"),
    }
}

fn hl7(document: &Document) -> Result<&oxim_hl7::Message, String> {
    match document {
        Document::Hl7(message) => Ok(message),
        _ => Err("this operation is only available for HL7 v2 messages".into()),
    }
}

/// The Rust functions the prelude receives as `native`.
fn natives<'js>(ctx: &Ctx<'js>, shared: &Arc<Shared>) -> rquickjs::Result<Object<'js>> {
    let native = Object::new(ctx.clone())?;

    let s = shared.clone();
    native.set(
        "get",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            s.with_state(&ctx, |state| {
                state.document.get(&path).map_err(|e| e.to_string())
            })
        })?,
    )?;

    let s = shared.clone();
    native.set(
        "set",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, path: String, value: String| {
                s.with_state(&ctx, |state| {
                    if !state.writable {
                        return Err("filters and encoders cannot change the message".into());
                    }
                    state.document.set(&path, &value).map_err(|e| e.to_string())
                })
            },
        )?,
    )?;

    let s = shared.clone();
    native.set(
        "raw",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            s.with_state(&ctx, |state| {
                Ok(String::from_utf8_lossy(&state.document.to_bytes()).into_owned())
            })
        })?,
    )?;

    let s = shared.clone();
    native.set(
        "dataType",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>| {
            s.with_state(&ctx, |state| Ok(data_type_name(state.document.data_type())))
        })?,
    )?;

    let s = shared.clone();
    native.set(
        "count",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, path: String| {
            s.with_state(&ctx, |state| {
                let message = hl7(&state.document)?;
                let count = if path.len() == 3 && !path.contains('-') {
                    message.segments_named(&path).count()
                } else {
                    message
                        .get(&path)
                        .map_or(0, |value| value.repetitions().count())
                };
                Ok(u32::try_from(count).unwrap_or(u32::MAX))
            })
        })?,
    )?;

    let s = shared.clone();
    native.set(
        "segment",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, id: String, occurrence: u32| {
                s.with_state(&ctx, |state| {
                    let message = hl7(&state.document)?;
                    let occurrence = usize::try_from(occurrence.max(1)).unwrap_or(1);
                    Ok(message.segment(&id, occurrence).map(|segment| {
                        let bytes = segment.to_bytes();
                        String::from_utf8_lossy(&bytes)
                            .trim_end_matches(['\r', '\n'])
                            .to_owned()
                    }))
                })
            },
        )?,
    )?;

    let reply = |shared: &Arc<Shared>| {
        let s = shared.clone();
        move |ctx: &Ctx<'js>, data: Vec<u8>, name: Option<String>| {
            s.with_state(ctx, |state| {
                if !state.writable {
                    return Err("reply() is only available in transformers".into());
                }
                let data_type = data_type(name, state.document.data_type())?;
                state.response = Some(Encoded { data_type, data });
                Ok(())
            })
        }
    };
    let text_reply = reply(shared);
    native.set(
        "replyText",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, data: String, name: Option<String>| {
                text_reply(&ctx, data.into_bytes(), name)
            },
        )?,
    )?;
    let bytes_reply = reply(shared);
    native.set(
        "replyBytes",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, data: Vec<u8>, name: Option<String>| bytes_reply(&ctx, data, name),
        )?,
    )?;

    let s = shared.clone();
    native.set(
        "log",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, level: String, text: String| {
                let script = s.script.clone();
                s.with_state(&ctx, |state| {
                    let (channel, message) = (&state.channel, &state.message);
                    match level.as_str() {
                        "debug" => debug!(%channel, message_id = %message, %script, "{text}"),
                        "warn" => warn!(%channel, message_id = %message, %script, "{text}"),
                        "error" => error!(%channel, message_id = %message, %script, "{text}"),
                        _ => info!(%channel, message_id = %message, %script, "{text}"),
                    }
                    Ok(())
                })
            },
        )?,
    )?;

    let s = shared.clone();
    native.set(
        "globalGet",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, scope: String, key: String| {
                let globals = s.globals.clone();
                s.with_state(&ctx, |state| {
                    globals.with(&scope, &state.channel, |map| map.get(&key).cloned())
                })
            },
        )?,
    )?;

    let s = shared.clone();
    native.set(
        "globalPut",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, scope: String, key: String, value: String| {
                let globals = s.globals.clone();
                s.with_state(&ctx, |state| {
                    globals.with(&scope, &state.channel, |map| {
                        map.insert(key, value);
                    })
                })
            },
        )?,
    )?;

    let s = shared.clone();
    native.set(
        "globalRemove",
        Function::new(
            ctx.clone(),
            move |ctx: Ctx<'js>, scope: String, key: String| {
                let globals = s.globals.clone();
                s.with_state(&ctx, |state| {
                    globals.with(&scope, &state.channel, |map| {
                        map.remove(&key);
                    })
                })
            },
        )?,
    )?;

    let s = shared.clone();
    native.set(
        "globalKeys",
        Function::new(ctx.clone(), move |ctx: Ctx<'js>, scope: String| {
            let globals = s.globals.clone();
            s.with_state(&ctx, |state| {
                globals.with(&scope, &state.channel, |map| {
                    map.keys().cloned().collect::<Vec<String>>()
                })
            })
        })?,
    )?;

    Ok(native)
}

/// Line and column of the first stack frame in `file`.
fn position(stack: &str, file: &str, offset: u32) -> Option<(u32, u32)> {
    for needle in [format!("({file}:"), format!("at {file}:")] {
        let Some(start) = stack.find(&needle) else {
            continue;
        };
        let rest = &stack[start + needle.len()..];
        let mut numbers = rest
            .split(|c: char| !c.is_ascii_digit())
            .take(2)
            .map(str::parse::<u32>);
        if let (Some(Ok(line)), Some(Ok(column))) = (numbers.next(), numbers.next()) {
            return Some((line.saturating_sub(offset).max(1), column));
        }
    }
    None
}

/// Turns the pending exception into a failure.
fn caught(ctx: &Ctx<'_>, error: rquickjs::Error, file: &str, offset: u32) -> Failure {
    if !matches!(error, rquickjs::Error::Exception) {
        return Failure::Script(error.to_string());
    }
    let value = ctx.catch();
    if value.is_undefined() || value.is_null() {
        // QuickJS could not even allocate the error object.
        return Failure::Memory;
    }
    let Some(exception) = value.as_object().cloned().and_then(Exception::from_object) else {
        let text = rquickjs::Coerced::<String>::from_js(ctx, value)
            .map(|text| text.0)
            .unwrap_or_else(|_| "a value that is not an Error".into());
        return Failure::Script(format!("uncaught exception: {text}"));
    };
    let name: String = exception
        .get::<_, Option<String>>("name")
        .ok()
        .flatten()
        .unwrap_or_else(|| "Error".into());
    let message = exception.message().unwrap_or_default();
    match (name.as_str(), message.as_str()) {
        ("InternalError", "interrupted") => return Failure::Timeout,
        ("InternalError", "out of memory") => return Failure::Memory,
        ("RangeError", "Maximum call stack size exceeded") => return Failure::Stack,
        _ => {}
    }
    let stack = exception.stack().unwrap_or_default();
    Failure::Script(match position(&stack, file, offset) {
        Some((line, column)) => format!("{name}: {message} (line {line}, column {column})"),
        None => format!("{name}: {message}"),
    })
}

/// One QuickJS runtime with the prelude and the compiled script.
struct Instance {
    context: Context,
    // Kept alive for the context; dropped after it.
    _runtime: Runtime,
    shared: Arc<Shared>,
    /// Lines OXIM added in front of the script when wrapping it.
    offset: u32,
}

fn eval_options(file: &str) -> EvalOptions {
    let mut options = EvalOptions::default();
    options.global = true;
    options.strict = false;
    options.backtrace_barrier = true;
    options.filename = Some(file.to_owned());
    options
}

impl Instance {
    fn new(
        settings: &ScriptSettings,
        kind: Kind,
        globals: &Arc<GlobalMaps>,
    ) -> Result<Self, Failure> {
        let runtime = Runtime::new().map_err(|e| Failure::Script(e.to_string()))?;
        runtime.set_memory_limit(settings.memory_limit);
        runtime.set_max_stack_size(settings.max_stack);
        let shared = Arc::new(Shared {
            state: Mutex::new(None),
            deadline: AtomicU64::new(u64::MAX),
            base: Instant::now(),
            globals: globals.clone(),
            script: settings.name.clone(),
        });
        let clock = shared.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || clock.expired())));
        let context = Context::full(&runtime).map_err(|_| Failure::Memory)?;
        shared.arm(settings.timeout);
        let offset = context.with(|ctx| -> Result<u32, Failure> {
            let prelude: Function = ctx
                .eval_with_options(PRELUDE, eval_options(PRELUDE_NAME))
                .map_err(|e| caught(&ctx, e, PRELUDE_NAME, 0))?;
            let native = natives(&ctx, &shared).map_err(|e| caught(&ctx, e, PRELUDE_NAME, 0))?;
            prelude
                .call::<_, ()>((native, settings.mirth))
                .map_err(|e| caught(&ctx, e, PRELUDE_NAME, 0))?;
            let (main, offset) = compile(&ctx, settings, kind)?;
            let install: Function = ctx
                .globals()
                .get("__oxim_install")
                .map_err(|e| caught(&ctx, e, PRELUDE_NAME, 0))?;
            install
                .call::<_, ()>((main,))
                .map_err(|e| caught(&ctx, e, PRELUDE_NAME, 0))?;
            Ok(offset)
        });
        shared.disarm();
        Ok(Self {
            context,
            _runtime: runtime,
            shared,
            offset: offset?,
        })
    }
}

/// Compiles the script into a function. Filters and encoders may be a
/// single expression; otherwise the script is a function body.
fn compile<'js>(
    ctx: &Ctx<'js>,
    settings: &ScriptSettings,
    kind: Kind,
) -> Result<(Function<'js>, u32), Failure> {
    let file = settings.name.as_str();
    if kind != Kind::Transformer {
        let expression = settings.source.trim_end().trim_end_matches(';').trim_end();
        if !expression.trim().is_empty() {
            let wrapped = format!("(function () {{\nreturn (\n{expression}\n);\n}})");
            match ctx.eval_with_options::<Function, _>(wrapped, eval_options(file)) {
                Ok(function) => return Ok((function, 2)),
                Err(_) => {
                    // Not a single expression: compile it as a body below.
                    let _ = ctx.catch();
                }
            }
        }
    }
    let wrapped = format!("(function () {{\n{}\n}})", settings.source);
    ctx.eval_with_options::<Function, _>(wrapped, eval_options(file))
        .map(|function| (function, 1))
        .map_err(|e| caught(ctx, e, file, 1))
}

/// A compiled script and its pool of runtimes.
pub(crate) struct Script {
    settings: ScriptSettings,
    kind: Kind,
    globals: Arc<GlobalMaps>,
    /// Whether the script mentions `clinical`; otherwise the normalized
    /// content is not converted for it.
    uses_clinical: bool,
    idle: Mutex<Vec<Instance>>,
    max_idle: usize,
}

impl fmt::Debug for Script {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Script")
            .field("name", &self.settings.name)
            .field("kind", &self.kind)
            .field("mirth", &self.settings.mirth)
            .field("timeout", &self.settings.timeout)
            .field("memory_limit", &self.settings.memory_limit)
            .finish_non_exhaustive()
    }
}

impl Script {
    /// Compiles the script; syntax errors are reported here, when the
    /// channel is deployed.
    pub(crate) fn new(
        settings: ScriptSettings,
        kind: Kind,
        globals: Arc<GlobalMaps>,
    ) -> Result<Self, EngineError> {
        let uses_clinical = !settings.mirth && settings.source.contains("clinical");
        let max_idle = std::thread::available_parallelism()
            .map_or(4, usize::from)
            .clamp(2, 32);
        let script = Self {
            settings,
            kind,
            globals,
            uses_clinical,
            idle: Mutex::new(Vec::new()),
            max_idle,
        };
        let instance = Instance::new(&script.settings, kind, &script.globals)
            .map_err(|failure| EngineError::Config(script.describe(&failure)))?;
        lock(&script.idle).push(instance);
        Ok(script)
    }

    /// The data type an encoder declares for its output.
    pub(crate) fn output_type(&self) -> Option<DataType> {
        self.settings.data_type
    }

    fn describe(&self, failure: &Failure) -> String {
        let name = &self.settings.name;
        match failure {
            Failure::Timeout => format!(
                "script {name:?} ran longer than its timeout of {:?}",
                self.settings.timeout
            ),
            Failure::Memory => format!(
                "script {name:?} exceeded its memory limit of {} bytes",
                self.settings.memory_limit
            ),
            Failure::Stack => format!(
                "script {name:?} exceeded its stack limit of {} bytes (recursion too deep)",
                self.settings.max_stack
            ),
            Failure::Script(message) => format!("script {name:?}: {message}"),
        }
    }

    /// Runs the script on one message. The state (with the document) is
    /// always handed back, also when the script fails.
    pub(crate) fn run(
        &self,
        state: RunState,
        clinical: Option<&ClinicalContent>,
        variables: &BTreeMap<String, String>,
    ) -> (RunState, Result<Outcome, StepError>) {
        let error = |message: String| StepError::new(STEP, message);
        let instance = lock(&self.idle).pop();
        let instance = match instance {
            Some(instance) => instance,
            None => match Instance::new(&self.settings, self.kind, &self.globals) {
                Ok(instance) => instance,
                Err(failure) => return (state, Err(error(self.describe(&failure)))),
            },
        };
        let clinical_in = if self.uses_clinical {
            match serde_json::to_string(&clinical) {
                Ok(json) => json,
                Err(e) => return (state, Err(error(format!("clinical: {e}")))),
            }
        } else {
            "null".to_owned()
        };
        let vars_in = match serde_json::to_string(variables) {
            Ok(json) => json,
            Err(e) => return (state, Err(error(format!("vars: {e}")))),
        };
        let input = format!("{{\"clinical\":{clinical_in},\"vars\":{vars_in}}}");

        *lock(&instance.shared.state) = Some(state);
        instance.shared.arm(self.settings.timeout);
        let result = instance
            .context
            .with(|ctx| self.call(&ctx, &input, instance.offset));
        instance.shared.disarm();
        let state = lock(&instance.shared.state).take();
        let fatal = matches!(&result, Err(failure) if failure.is_fatal());
        if !fatal {
            let mut idle = lock(&self.idle);
            if idle.len() < self.max_idle {
                idle.push(instance);
            }
        }
        let Some(state) = state else {
            return (
                RunState {
                    document: Document::Raw(Vec::new()),
                    writable: false,
                    response: None,
                    channel: String::new(),
                    message: String::new(),
                },
                Err(error("the script lost the message".into())),
            );
        };
        let (value, clinical_out, vars_out) = match result {
            Ok(result) => result,
            Err(failure) => return (state, Err(error(self.describe(&failure)))),
        };
        let clinical = if self.uses_clinical && clinical_out != clinical_in {
            match serde_json::from_str::<Option<ClinicalContent>>(&clinical_out) {
                Ok(content) => Some(content),
                Err(e) => {
                    return (
                        state,
                        Err(error(format!(
                            "script {:?} set clinical to content that is not valid normalized content: {e}",
                            self.settings.name
                        ))),
                    );
                }
            }
        } else {
            None
        };
        let variables = match serde_json::from_str(&vars_out) {
            Ok(variables) => variables,
            Err(e) => return (state, Err(error(format!("vars: {e}")))),
        };
        (
            state,
            Ok(Outcome {
                value,
                clinical,
                variables,
            }),
        )
    }

    fn call(
        &self,
        ctx: &Ctx<'_>,
        input: &str,
        offset: u32,
    ) -> Result<(Returned, String, String), Failure> {
        let file = self.settings.name.as_str();
        let fail = |e| caught(ctx, e, file, offset);
        let globals = ctx.globals();
        let begin: Function = globals.get("__oxim_begin").map_err(fail)?;
        begin.call::<_, ()>((input,)).map_err(fail)?;
        let main: Function = globals.get("__oxim_main").map_err(fail)?;
        let value: Value = main.call(()).map_err(fail)?;
        let returned = match self.kind {
            Kind::Transformer => Returned::Nothing,
            Kind::Filter => Returned::Bool(value.as_bool().ok_or_else(|| {
                Failure::Script(format!(
                    "a filter script must return true or false, not {}",
                    value.type_name()
                ))
            })?),
            Kind::Encoder => match value.as_string() {
                Some(text) => Returned::Bytes(text.to_string().map_err(fail)?.into_bytes()),
                None => {
                    let bytes: Function = globals.get("__oxim_bytes").map_err(fail)?;
                    Returned::Bytes(bytes.call::<_, Vec<u8>>((value,)).map_err(fail)?)
                }
            },
        };
        let end: Function = globals.get("__oxim_end").map_err(fail)?;
        let output: Vec<String> = end.call(()).map_err(fail)?;
        let mut output = output.into_iter();
        match (output.next(), output.next()) {
            (Some(clinical), Some(vars)) => Ok((returned, clinical, vars)),
            _ => Err(Failure::Script("the script state could not be read".into())),
        }
    }
}
