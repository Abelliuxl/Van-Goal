//! Wire-level probe for the OpenCode-family backends, MiMoCode in particular.
//!
//! MiMoCode is driven through the same HTTP + SSE interface as OpenCode, but the
//! two servers do not behave identically: `mimo serve` picks a random port
//! unless it is told which one, scopes every session to the directory it was
//! started in, pairs its password with the `mimocode` user rather than
//! `opencode`, and reports a text part as the whole text so far with no `delta`
//! field. Each of those is invisible from the source, so this runs the same
//! managed-server path the app does and prints what came back.
//!
//! ```text
//! cargo run -p van-goal-core --example mimocode_probe
//! cargo run -p van-goal-core --example mimocode_probe -- --port 4096 --workspace ~/code/project
//! cargo run -p van-goal-core --example mimocode_probe -- --prompt "reply with exactly: PROBE-OK"
//! cargo run -p van-goal-core --example mimocode_probe -- --keep-running
//! ```
//!
//! Nothing here writes to an existing session: the turn it can send goes to a
//! session it creates itself, and `--keep-running` is the only way the server it
//! starts outlives the probe.

use futures::channel::mpsc::unbounded;
use futures::StreamExt;
use std::time::Duration;
use van_goal_core::agent::Backend;
use van_goal_core::local_server::{LocalServerManager, ManagedServer};
use van_goal_core::models::{AgentEvent, BackendConfig};
use van_goal_core::settings::BackendKind;

struct Args {
    port: u16,
    workspace: Option<String>,
    credential: String,
    prompt: Option<String>,
    seconds: u64,
    keep_running: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        port: 4096,
        workspace: std::env::current_dir()
            .ok()
            .map(|path| path.display().to_string()),
        credential: String::new(),
        prompt: None,
        seconds: 60,
        keep_running: false,
    };
    let mut parts = std::env::args().skip(1);
    while let Some(flag) = parts.next() {
        match flag.as_str() {
            "--port" => {
                if let Some(value) = parts.next().and_then(|v| v.parse().ok()) {
                    args.port = value;
                }
            }
            "--workspace" => args.workspace = parts.next(),
            "--password" => args.credential = parts.next().unwrap_or_default(),
            "--prompt" => args.prompt = parts.next(),
            "--seconds" => {
                if let Some(value) = parts.next().and_then(|v| v.parse().ok()) {
                    args.seconds = value;
                }
            }
            "--keep-running" => args.keep_running = true,
            other => eprintln!("ignoring unknown argument {other}"),
        }
    }
    args
}

#[tokio::main]
async fn main() {
    let args = parse_args();
    let server = match ManagedServer::for_kind(BackendKind::MiMoCode) {
        Some(server) => server,
        None => {
            eprintln!("MiMoCode is not a managed server in this build");
            std::process::exit(1);
        }
    };
    println!(
        "managed server: {} on port {} in {}",
        server.label(),
        args.port,
        args.workspace.as_deref().unwrap_or("<app directory>")
    );

    let manager = LocalServerManager::default();
    let base_url = match manager
        .ensure_running(
            server,
            args.port,
            args.workspace.as_deref(),
            &args.credential,
        )
        .await
    {
        Ok(url) => url,
        Err(error) => {
            eprintln!("the server never became ready: {error}");
            std::process::exit(1);
        }
    };
    println!("manager says: {}", manager.take_message());
    println!("base url: {base_url}");

    let config = BackendConfig {
        base_url: base_url.clone(),
        credential: args.credential.clone(),
        profile: None,
        workspace: args.workspace.clone(),
    };
    let mut backend = Backend::make(BackendKind::MiMoCode);

    match backend.probe(&config).await {
        Ok(()) => println!("health: ok"),
        Err(error) => {
            eprintln!("health: {error}");
            std::process::exit(1);
        }
    }

    match backend.list_sessions(&config).await {
        Ok(sessions) => {
            println!("sessions: {}", sessions.len());
            for session in sessions.iter().take(5) {
                println!(
                    "  {}  {}",
                    session.id,
                    session.title.as_deref().unwrap_or("<untitled>")
                );
            }
        }
        Err(error) => eprintln!("session list failed: {error}"),
    }

    let (tx, mut rx) = unbounded();
    if let Err(error) = backend.connect(config.clone(), tx).await {
        eprintln!("event stream failed: {error}");
        std::process::exit(1);
    }
    println!("event stream: open");

    let session_id = match backend.create_session(&config).await {
        Ok(ids) => {
            println!("created session: {}", ids.live_id);
            ids.live_id
        }
        Err(error) => {
            eprintln!("could not create a session: {error}");
            std::process::exit(1);
        }
    };

    match backend.messages(&config, &session_id).await {
        Ok(messages) => println!("history of the new session: {} message(s)", messages.len()),
        Err(error) => eprintln!("history failed: {error}"),
    }

    if let Some(prompt) = args.prompt.clone() {
        println!("prompt: {prompt:?}");
        match backend.submit_prompt(&session_id, &prompt).await {
            Ok(()) => println!("prompt accepted"),
            Err(error) => eprintln!("prompt refused: {error}"),
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(args.seconds);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), rx.next()).await {
                Ok(Some(event)) => println!("  event: {}", describe(&event)),
                Ok(None) => break,
                Err(_) => continue,
            }
        }
        match backend.messages(&config, &session_id).await {
            Ok(messages) => {
                for message in &messages {
                    println!(
                        "  {:?}: {}",
                        message.role,
                        message.content.replace('\n', " ").chars().take(200).collect::<String>()
                    );
                }
            }
            Err(error) => eprintln!("history after the turn failed: {error}"),
        }
    }

    if args.keep_running {
        println!("leaving {} running on {base_url}", server.label());
        return;
    }
    manager.stop().await;
    println!("{}", manager.take_message());
}

/// One line per event, with the part that matters to a reader of the transcript.
fn describe(event: &AgentEvent) -> String {
    match event {
        AgentEvent::Connected => "connected".into(),
        AgentEvent::Disconnected => "disconnected".into(),
        AgentEvent::MessageStart => "message start".into(),
        AgentEvent::MessageDelta { text, .. } => format!("delta {text:?}"),
        AgentEvent::MessageComplete(text) => format!("complete {text:?}"),
        AgentEvent::Tool(record) => format!("tool {} [{}]", record.name, record.status),
        AgentEvent::TurnFailed(detail) => format!("turn failed: {detail}"),
        AgentEvent::SessionInfo(id) => format!("session {id}"),
        AgentEvent::Clarify { question, .. } => format!("asks: {question}"),
        other => format!("{other:?}"),
    }
}
