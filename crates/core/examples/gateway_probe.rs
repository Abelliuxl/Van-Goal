//! Wire-level probe for an OpenClaw Gateway.
//!
//! Session isolation is decided by what a frame says about *which* session it
//! belongs to, and the only way to know that is to look at the frames. This
//! connects with the same device identity the app uses, prints every frame with
//! the session label it carries, and reports what the client-side filter in
//! [`van_goal_core::agent::openclaw`] would do with it.
//!
//! ```text
//! cargo run -p van-goal-core --example gateway_probe -- wss://host:18789
//! cargo run -p van-goal-core --example gateway_probe -- --seconds 120
//! cargo run -p van-goal-core --example gateway_probe -- --subscribe agent:main:me
//! cargo run -p van-goal-core --example gateway_probe -- --poke
//! ```
//!
//! Nothing here writes to a session; `--poke` is the only flag that sends a
//! prompt, and it does so to [`POKE_KEY`], a session of its own. `--delete`
//! cleans such a session up, but needs the `operator.admin` scope, which this
//! client's device is not granted — so a poked session has to be removed from
//! the Gateway's own side.

use futures::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use tokio_tungstenite::tungstenite::Message;
use van_goal_core::agent::device_identity::{
    OpenClawDeviceIdentity, OPENCLAW_CLIENT_ID, OPENCLAW_CLIENT_MODE, OPENCLAW_DEVICE_FAMILY,
    OPENCLAW_PLATFORM, OPENCLAW_ROLE, OPENCLAW_SCOPES,
};

/// The event names the adapter filters on. Anything else is passed through
/// whatever its session label says.
const TURN_EVENTS: [&str; 4] = ["chat", "session.message", "session.tool", "agent"];

/// The session `--poke` writes into. Fixed, so a repeat run continues the same
/// throwaway conversation instead of filling the gateway with new ones.
const POKE_KEY: &str = "agent:main:van-goal-probe:isolation-probe";

/// Short and unmistakable, so a probe turn appearing in another chat is obvious
/// at a glance in a screenshot.
const POKE_PROMPT: &str = "reply with exactly: PROBE-OK";

struct Args {
    url: String,
    seconds: u64,
    subscribe: Vec<String>,
    delete: Vec<String>,
    history: Vec<String>,
    poke: bool,
    list: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        url: "wss://liuxl.com.cn:18789".into(),
        seconds: 90,
        subscribe: Vec::new(),
        delete: Vec::new(),
        history: Vec::new(),
        poke: false,
        list: false,
    };
    let mut positional = true;
    let mut rest = std::env::args().skip(1).peekable();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--seconds" => args.seconds = rest.next().and_then(|v| v.parse().ok()).unwrap_or(90),
            "--subscribe" => {
                if let Some(key) = rest.next() {
                    args.subscribe.push(key);
                }
            }
            "--delete" => {
                if let Some(key) = rest.next() {
                    args.delete.push(key);
                }
            }
            "--history" => {
                if let Some(key) = rest.next() {
                    args.history.push(key);
                }
            }
            "--poke" => args.poke = true,
            "--list" => args.list = true,
            other if positional && !other.starts_with("--") => {
                args.url = other.to_string();
                positional = false;
            }
            other if !other.starts_with("--") => args.subscribe.push(other.to_string()),
            _ => {}
        }
    }
    args
}

/// The session key the adapter reads off a frame: the payload's own top-level
/// fields, which is the only place it looks.
fn adapter_session_key(payload: &Value) -> Option<String> {
    ["sessionKey", "key", "session_id", "sessionId"]
        .iter()
        .find_map(|field| payload.get(*field).and_then(Value::as_str))
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

/// Every path in the payload that looks like it names a session. Used to report
/// labels the adapter's four field names would miss.
fn session_like_paths(value: &Value) -> Vec<String> {
    fn looks_like_key(text: &str) -> bool {
        text.contains(":main:") || text.starts_with("agent:") || text.starts_with("session:")
    }
    fn walk(value: &Value, path: &str, found: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    let lower = key.to_ascii_lowercase();
                    let named = lower.contains("session")
                        || lower == "key"
                        || lower == "runid"
                        || lower == "sessionkey";
                    let child_path = format!("{path}.{key}");
                    if named {
                        if let Some(text) = child.as_str() {
                            found.push(format!("{child_path}={text}"));
                        } else if child.is_object() {
                            found.push(format!("{child_path}=<object>"));
                        }
                    }
                    walk(child, &child_path, found);
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    walk(child, &format!("{path}[{index}]"), found);
                }
            }
            Value::String(text) if looks_like_key(text) => {
                found.push(format!("{path}={text}"));
            }
            _ => {}
        }
    }
    let mut found = Vec::new();
    walk(value, "", &mut found);
    found
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    println!("probing {} for {}s", args.url, args.seconds);
    for key in &args.subscribe {
        println!("  will subscribe to {key}");
    }

    let (mut sink, mut stream) = match tokio_tungstenite::connect_async(args.url.as_str()).await {
        Ok((ws, _)) => ws.split(),
        Err(error) => {
            eprintln!("could not open the socket: {error}");
            std::process::exit(1);
        }
    };

    let started = Instant::now();
    let mut handshake_id: Option<String> = None;
    let mut pending: BTreeMap<String, String> = BTreeMap::new();
    let mut followed_up = false;
    let mut poke_key: Option<String> = None;

    // Frames by event name, and by the session label the adapter would see.
    let mut by_event: BTreeMap<String, usize> = BTreeMap::new();
    let mut by_label: BTreeMap<String, usize> = BTreeMap::new();
    let mut unlabelled_turn_frames = 0usize;
    let mut missed_labels: Vec<String> = Vec::new();
    // Every turn frame as (label) so the two rules can be scored on the same
    // traffic afterwards, using the real functions rather than a paraphrase.
    let mut turn_frames: Vec<Option<String>> = Vec::new();

    let deadline = tokio::time::sleep(Duration::from_secs(args.seconds));
    tokio::pin!(deadline);

    loop {
        tokio::select! {
            _ = &mut deadline => break,
            frame = stream.next() => {
                let Some(Ok(frame)) = frame else { break };
                let Message::Text(text) = frame else { continue };
                let Ok(object) = serde_json::from_str::<Value>(&text) else { continue };
                let elapsed = started.elapsed().as_secs_f32();
                let frame_type = object.get("type").and_then(Value::as_str).unwrap_or("");

                if frame_type == "res" {
                    let id = object.get("id").and_then(Value::as_str).unwrap_or("?").to_string();
                    let label = pending.remove(&id).unwrap_or_else(|| "?".into());
                    let ok = object.get("ok").and_then(Value::as_bool).unwrap_or(false);
                    if ok {
                        let payload = object.get("payload").cloned().unwrap_or(Value::Null);
                        println!("[{elapsed:>6.1}s] res  {label} ok {}", summarize(&payload));
                        if label == "chat.history" {
                            for row in history_rows(&payload) {
                                println!("[{elapsed:>6.1}s]   {row}");
                            }
                        }
                    } else {
                        let error = object.get("error").map(|e| e.to_string()).unwrap_or_default();
                        println!("[{elapsed:>6.1}s] res  {label} FAILED {error}");
                    }
                    // The handshake landed: now do what the client does next.
                    if Some(&id) == handshake_id.as_ref() && ok && !followed_up {
                        followed_up = true;
                        if args.list {
                            send_request(&mut sink, &mut pending, "sessions.list", json!({ "limit": 100 })).await;
                        }
                        for key in &args.subscribe {
                            send_request(
                                &mut sink,
                                &mut pending,
                                "sessions.messages.subscribe",
                                json!({ "key": key }),
                            )
                            .await;
                        }
                        if args.poke {
                            // A session of its own, under a fixed key so a repeat
                            // run continues it rather than making another one.
                            println!("[{elapsed:>6.1}s] creating {POKE_KEY} to make traffic in a session nobody is watching");
                            send_request(
                                &mut sink,
                                &mut pending,
                                "sessions.create",
                                json!({ "key": POKE_KEY }),
                            )
                            .await;
                        }
                        for key in &args.delete {
                            println!("[{elapsed:>6.1}s] deleting {key}");
                            send_request(
                                &mut sink,
                                &mut pending,
                                "sessions.delete",
                                json!({ "key": key }),
                            )
                            .await;
                        }
                        for key in &args.history {
                            println!("[{elapsed:>6.1}s] reading the history of {key}");
                            send_request(
                                &mut sink,
                                &mut pending,
                                "chat.history",
                                json!({ "sessionKey": key, "limit": 200 }),
                            )
                            .await;
                        }
                    }
                    // `sessions.create` answered — ok or already-exists, either
                    // way the session is there: put a turn in it.
                    if label == "sessions.create" && poke_key.is_none() {
                        poke_key = Some(POKE_KEY.to_string());
                        println!("[{elapsed:>6.1}s] sending a turn to {POKE_KEY}");
                        send_request(
                            &mut sink,
                            &mut pending,
                            "chat.send",
                            json!({
                                "sessionKey": POKE_KEY,
                                "message": POKE_PROMPT,
                                "thinking": "off",
                                "deliver": false,
                                "idempotencyKey": uuid::Uuid::new_v4().to_string()
                            }),
                        )
                        .await;
                    }
                    continue;
                }

                if frame_type != "event" {
                    continue;
                }
                let event = object.get("event").and_then(Value::as_str).unwrap_or("?").to_string();
                let payload = object.get("payload").cloned().unwrap_or(Value::Null);
                *by_event.entry(event.clone()).or_default() += 1;

                if event == "connect.challenge" {
                    let nonce = payload.get("nonce").and_then(Value::as_str).unwrap_or("").to_string();
                    let signed_at = payload.get("ts").and_then(Value::as_i64).unwrap_or(0);
                    let Ok(identity) = OpenClawDeviceIdentity::load_or_create() else {
                        eprintln!("could not load the device identity");
                        return;
                    };
                    // The paired-device token is filed under the *parsed* URL,
                    // which is what the adapter keeps as its gateway scope. A
                    // raw string would hash differently and silently look like a
                    // device that was never paired.
                    let Ok(parsed) = url::Url::parse(&args.url) else {
                        eprintln!("could not parse the gateway url");
                        return;
                    };
                    let scope = parsed.to_string();
                    let device_token = OpenClawDeviceIdentity::load_device_token(&scope)
                        .ok()
                        .flatten();
                    let signature_token = device_token.clone().unwrap_or_default();
                    let device = identity.signed_connect_device(&nonce, signed_at, &signature_token);
                    let id = uuid::Uuid::new_v4().to_string();
                    handshake_id = Some(id.clone());
                    pending.insert(id.clone(), "connect".into());
                    let connect = json!({
                        "type": "req",
                        "id": id,
                        "method": "connect",
                        "params": {
                            "minProtocol": 4,
                            "maxProtocol": 4,
                            "client": {
                                "id": OPENCLAW_CLIENT_ID,
                                "displayName": "Van-Goal probe",
                                "version": env!("CARGO_PKG_VERSION"),
                                "platform": OPENCLAW_PLATFORM,
                                "deviceFamily": OPENCLAW_DEVICE_FAMILY,
                                "mode": OPENCLAW_CLIENT_MODE
                            },
                            "role": OPENCLAW_ROLE,
                            "scopes": OPENCLAW_SCOPES,
                            "caps": ["tool-events"],
                            "commands": [],
                            "permissions": {},
                            "auth": match (&device_token, signature_token.is_empty()) {
                                (Some(token), _) => json!({ "deviceToken": token }),
                                (None, _) => json!({ "token": signature_token }),
                            },
                            "locale": "en-US",
                            "userAgent": format!("van-goal-probe/{}", env!("CARGO_PKG_VERSION")),
                            "device": device
                        }
                    });
                    println!("[{elapsed:>6.1}s] challenge nonce={} ts={signed_at} -> connect", nonce.len());
                    let _ = sink.send(Message::Text(connect.to_string())).await;
                    continue;
                }

                // After the handshake is accepted, subscribe and (optionally)
                // look at the session list.
                let adapter_key = adapter_session_key(&payload);
                let deep = session_like_paths(&payload);
                let is_turn = TURN_EVENTS.contains(&event.as_str());
                let label = adapter_key.clone().unwrap_or_else(|| "<none>".into());
                if is_turn {
                    *by_label.entry(label.clone()).or_default() += 1;
                    turn_frames.push(adapter_key.clone());
                    if adapter_key.is_none() {
                        unlabelled_turn_frames += 1;
                        let report = format!("{event} paths={deep:?}");
                        if !missed_labels.contains(&report) {
                            missed_labels.push(report);
                        }
                    }
                }

                // What the client does with this frame.
                let verdict = if !is_turn {
                    "pass(bookkeeping)"
                } else if adapter_key.is_none() {
                    "PASS (no label -> nothing to compare against -> admitted)"
                } else {
                    "pass(if subscribed to it)"
                };
                println!(
                    "[{elapsed:>6.1}s] EVENT {event:<24} label={label:<40} {verdict}{}",
                    if is_turn { format!(" deep={deep:?}") } else { String::new() }
                );
            }
        }
    }

    println!(
        "\n--- summary over {:.0}s ---",
        started.elapsed().as_secs_f32()
    );
    println!("by event: {by_event:?}");
    println!("turn frames by session label as the adapter reads it: {by_label:?}");
    println!(
        "turn frames with NO label at the four fields the adapter reads: {unlabelled_turn_frames}"
    );
    if !missed_labels.is_empty() {
        println!("those frames carried labels elsewhere:");
        for report in &missed_labels {
            println!("  {report}");
        }
    }

    // The point of the whole exercise: how much of what the Gateway pushed to a
    // client that had subscribed to nothing would each rule have admitted.
    println!(
        "\n--- the same {} turn frames, scored by the real rules ---",
        turn_frames.len()
    );
    let mut scopes: Vec<Option<String>> = vec![None];
    for label in by_label.keys() {
        if label != "<none>" {
            scopes.push(Some(label.clone()));
        }
    }
    println!(
        "{:<44} {:>10} {:>10}",
        "session the client is on", "lenient", "strict"
    );
    for scope in scopes {
        let ids: Vec<&str> = scope.as_deref().into_iter().collect();
        let lenient = turn_frames
            .iter()
            .filter(|label| {
                van_goal_core::agent::session_scope::belongs_to_session(
                    ids.clone(),
                    label.as_deref(),
                )
            })
            .count();
        let strict = turn_frames
            .iter()
            .filter(|label| {
                van_goal_core::agent::session_scope::belongs_to_open_session(
                    ids.clone(),
                    label.as_deref(),
                )
            })
            .count();
        let name = match &scope {
            None => "<none — the window after a reconnect>".to_string(),
            Some(id) => {
                if id.len() > 42 {
                    format!("{}…", &id[..42])
                } else {
                    id.clone()
                }
            }
        };
        println!("{name:<44} {lenient:>10} {strict:>10}");
    }
    println!("(a frame admitted for the wrong session is another chat's reply drawn in this one)");
    let _ = args.poke;
}

/// A request frame, registered so its response can be reported by name.
async fn send_request<S>(
    sink: &mut S,
    pending: &mut BTreeMap<String, String>,
    method: &str,
    params: Value,
) where
    S: futures::Sink<Message> + Unpin,
{
    let id = uuid::Uuid::new_v4().to_string();
    pending.insert(id.clone(), method.to_string());
    let frame = json!({ "type": "req", "id": id, "method": method, "params": params });
    let _ = sink.send(Message::Text(frame.to_string())).await;
}

/// One message from a `chat.history` reply, as `role  time  first line`.
///
/// The point of asking: a turn in a session nobody in this process sent has to
/// be attributable, and a reply with no user message in front of it means the
/// prompt came from another client on the same session.
fn history_rows(payload: &Value) -> Vec<String> {
    payload
        .get("messages")
        .and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .map(|row| {
                    let role = row.get("role").and_then(Value::as_str).unwrap_or("?");
                    let when = row
                        .get("timestamp")
                        .or_else(|| row.get("ts"))
                        .or_else(|| row.get("at"))
                        .and_then(Value::as_i64)
                        .map(|ms| {
                            let ms = if ms < 10_000_000_000 { ms * 1000 } else { ms };
                            chrono::DateTime::from_timestamp_millis(ms)
                                .map(|t| {
                                    t.with_timezone(&chrono::Local)
                                        .format("%H:%M:%S")
                                        .to_string()
                                })
                                .unwrap_or_else(|| ms.to_string())
                        })
                        .unwrap_or_else(|| "-".into());
                    let text = row
                        .get("text")
                        .or_else(|| row.get("content"))
                        .map(|text| match text {
                            Value::String(text) => text.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_default();
                    let first_line: String =
                        text.lines().next().unwrap_or("").chars().take(80).collect();
                    format!("    {role:<10} {when}  {first_line}")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// One line describing an RPC result, without dumping a whole session list.
///
/// The session list is the exception: one row verbatim, because the fields it
/// carries are how to tell which client a session came from. A Gateway names a
/// session after the `displayName` of the client that created it, and this is
/// where that name lands.
fn summarize(payload: &Value) -> String {
    if let Some(sessions) = payload.get("sessions").and_then(Value::as_array) {
        // Counted per namespace: that is how the naming is checked. A session
        // started by one frontend files itself under that frontend's namespace,
        // and the point of naming the frontends is that the two are never the
        // same bucket.
        let mut namespaces: BTreeMap<&str, usize> = BTreeMap::new();
        for row in sessions {
            let namespace = row
                .get("key")
                .and_then(Value::as_str)
                .and_then(|key| key.split(':').nth(2))
                .unwrap_or("<none>");
            *namespaces.entry(namespace).or_default() += 1;
        }
        let first = sessions
            .first()
            .map(|row| row.to_string())
            .unwrap_or_default();
        return format!(
            "{} sessions {namespaces:?}, first row: {first}",
            sessions.len()
        );
    }
    let text = payload.to_string();
    if text.len() > 200 {
        format!("{}…", &text[..200])
    } else {
        text
    }
}
