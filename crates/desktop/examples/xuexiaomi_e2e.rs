//! End-to-end check of the desktop bridge against a live 雪小米 gateway, through
//! the same C ABI the Go client calls. Runs in a throwaway app-data directory,
//! so it never touches the developer's settings.
//!
//! ```bash
//! VG_XXM_URL=https://host:port/ VG_XXM_TOKEN_FILE=path/to/token \
//!   cargo run -p van-goal-desktop --example xuexiaomi_e2e
//! ```
//!
//! It creates one session on the gateway (named `van-goal-…`) and runs two
//! short turns in it: one answered, one stopped.

use std::ffi::{CStr, CString};
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use van_goal_desktop::{vgd_command, vgd_free, vgd_poll, vgd_set_data_dir, vgd_wait};

fn command(value: Value) -> Value {
    let text = CString::new(value.to_string()).unwrap();
    unsafe {
        let ptr = vgd_command(text.as_ptr());
        let reply = CStr::from_ptr(ptr).to_string_lossy().to_string();
        vgd_free(ptr);
        serde_json::from_str(&reply).unwrap()
    }
}

fn ok(value: Value) -> Value {
    let reply = command(value.clone());
    assert_eq!(reply["ok"], true, "{value} → {reply}");
    reply
}

/// The latest view, as the Go side keeps it: every poll merged into it.
#[derive(Default)]
struct View {
    state: Value,
    messages: Vec<Value>,
    composer: Vec<Value>,
}

impl View {
    fn apply(&mut self, update: Value) {
        if let Some(state) = update.get("state") {
            self.state = state.clone();
        }
        if let Some(messages) = update.get("messages").and_then(Value::as_array) {
            self.messages = messages.clone();
        }
        if let Some(deltas) = update.get("deltas").and_then(Value::as_array) {
            for delta in deltas {
                if let Some(message) = self.messages.iter_mut().find(|m| m["id"] == delta["id"]) {
                    message["content"] = delta["content"].clone();
                }
            }
        }
        if let Some(actions) = update.get("composer").and_then(Value::as_array) {
            self.composer.extend(actions.iter().cloned());
        }
    }

    fn poll(&mut self) {
        let ptr = vgd_poll();
        let text = unsafe { CStr::from_ptr(ptr).to_string_lossy().to_string() };
        unsafe { vgd_free(ptr) };
        self.apply(serde_json::from_str(&text).unwrap());
    }

    /// Wait until `done` holds for the view, the way the Go side waits.
    fn until(&mut self, what: &str, limit: Duration, done: impl Fn(&View) -> bool) {
        let start = Instant::now();
        loop {
            self.poll();
            if done(self) {
                println!("  ✓ {what} ({:.1}s)", start.elapsed().as_secs_f32());
                return;
            }
            assert!(
                start.elapsed() < limit,
                "timed out waiting for {what}; state: {}",
                self.state
            );
            vgd_wait(500);
        }
    }

    fn connection(&self) -> &str {
        self.state["connection"]["state"].as_str().unwrap_or("")
    }

    fn sending(&self) -> bool {
        self.state["sending"] == true
    }
}

fn main() {
    let url = std::env::var("VG_XXM_URL").expect("VG_XXM_URL");
    let token_file = std::env::var("VG_XXM_TOKEN_FILE").expect("VG_XXM_TOKEN_FILE");
    let token = std::fs::read_to_string(token_file)
        .expect("token file")
        .trim()
        .to_string();

    let dir = std::env::temp_dir().join(format!("van-goal-xxm-e2e-{}", std::process::id()));
    let path = CString::new(dir.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { vgd_set_data_dir(path.as_ptr()) }, 1);

    let mut view = View::default();
    view.apply(ok(json!({ "cmd": "start" })));
    println!("1. connect");
    ok(json!({ "cmd": "enable_backend", "backend": "xuexiaomi" }));
    ok(json!({ "cmd": "settings", "host": url, "credential": token }));
    ok(json!({ "cmd": "connect" }));
    view.until("connected", Duration::from_secs(30), |v| {
        v.connection() == "connected"
    });
    assert_eq!(view.state["backend"]["name"], "雪小米");

    println!("2. sessions");
    view.until("sessions listed", Duration::from_secs(30), |v| {
        v.state["sessions"]
            .as_array()
            .is_some_and(|s| !s.is_empty())
    });
    let sessions = view.state["sessions"].as_array().unwrap().clone();
    println!(
        "  {} sessions, first: {}",
        sessions.len(),
        sessions[0]["title"]
    );

    println!("3. open a channel snapshot (read-only history)");
    if let Some(snapshot) = sessions
        .iter()
        .find(|s| s["subtitle"].as_str().is_some_and(|t| t.contains("只读")))
    {
        ok(json!({ "cmd": "open_session", "id": snapshot["id"] }));
        view.until("history loaded", Duration::from_secs(30), |v| {
            v.state["selected"]["id"] == snapshot["id"] && !v.messages.is_empty()
        });
        let first = view.messages[0]["content"].as_str().unwrap_or("");
        assert!(
            !first.starts_with("[网关可信来源"),
            "relay prefix leaked: {first}"
        );
        println!(
            "  {} messages, title {}",
            view.messages.len(),
            snapshot["title"]
        );
    } else {
        println!("  (no read-only snapshot listed; skipped)");
    }

    println!("4. new chat, one answered turn");
    ok(json!({ "cmd": "new_chat" }));
    view.until("blank chat", Duration::from_secs(5), |v| {
        v.messages.is_empty() && v.state["selected"].is_null()
    });
    ok(
        json!({ "cmd": "send", "text": "这是 Van-Goal 桌面端端到端自检：不调用工具，只回复 VG_E2E_OK。" }),
    );
    view.until(
        "prompt and placeholder shown",
        Duration::from_secs(5),
        |v| v.messages.len() == 2 && v.sending(),
    );
    view.until("reply finished", Duration::from_secs(120), |v| !v.sending());
    let reply = view.messages.last().unwrap();
    let text = reply["content"].as_str().unwrap_or("");
    println!("  reply: {text:?}");
    assert_eq!(reply["role"], "assistant");
    assert!(
        text.contains("VG_E2E_OK"),
        "unexpected reply {text:?}; error {}",
        view.state["error"]
    );
    let session_id = view.state["selected"]["id"].as_str().unwrap().to_string();
    assert!(session_id.starts_with("van-goal-"), "{session_id}");
    view.until("listed by the gateway", Duration::from_secs(30), |v| {
        v.state["sessions"]
            .as_array()
            .is_some_and(|s| s.iter().any(|row| row["id"] == session_id.as_str()))
    });

    println!("5. a turn that is stopped");
    ok(json!({ "cmd": "send", "text": "端到端自检：写一首 40 行关于雪的长诗，不调用工具。" }));
    view.until("streaming", Duration::from_secs(60), |v| {
        v.sending()
            && v.messages.last().is_some_and(|m| {
                m["content"]
                    .as_str()
                    .is_some_and(|t| t.chars().count() > 10)
            })
    });
    ok(json!({ "cmd": "stop" }));
    view.until("stopped", Duration::from_secs(20), |v| !v.sending());
    println!(
        "  last message: {:?}",
        view.messages.last().map(|m| m["content"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(60)
            .collect::<String>())
    );

    println!("6. reopen the session: history comes back from the gateway");
    ok(json!({ "cmd": "new_chat" }));
    view.until("blank again", Duration::from_secs(5), |v| {
        v.messages.is_empty()
    });
    ok(json!({ "cmd": "open_session", "id": session_id }));
    view.until("history merged", Duration::from_secs(30), |v| {
        v.messages.iter().any(|m| {
            m["content"]
                .as_str()
                .is_some_and(|t| t.contains("VG_E2E_OK"))
        })
    });

    println!("7. disconnect");
    ok(json!({ "cmd": "disconnect" }));
    view.until("disconnected", Duration::from_secs(5), |v| {
        v.connection() == "disconnected"
    });

    let _ = std::fs::remove_dir_all(&dir);
    println!("all good — session {session_id} was created on the gateway");
}
