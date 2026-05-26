use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::{
    evidence::EvidenceEvent,
    session::{ScreenSnapshot, SessionConfig, SessionManager, SessionObservation, WaitCondition},
};

static DANGEROUS_COMMANDS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    vec![
        (
            Regex::new(r"(?i)(?:^|[;&|]\s*)(?:sudo\s+)?rm\s+-(?:[a-z]*r[a-z]*f|[a-z]*f[a-z]*r)\b")
                .expect("valid rm -rf policy regex"),
            "rm -rf",
        ),
        (
            Regex::new(r"(?i)\bgit\s+push\b[^\n]*\s--force(?:-with-lease)?\b")
                .expect("valid git push --force policy regex"),
            "git push --force",
        ),
        (
            Regex::new(r"(?i)\bterraform\s+apply\b").expect("valid terraform apply policy regex"),
            "terraform apply",
        ),
        (
            Regex::new(r"(?i)\bkubectl\s+delete\b").expect("valid kubectl delete policy regex"),
            "kubectl delete",
        ),
        (
            Regex::new(r"(?i)\bvault\s+write\b").expect("valid vault write policy regex"),
            "vault write",
        ),
        (
            Regex::new(r"(?i)\bgh\s+pr\s+merge\b").expect("valid gh pr merge policy regex"),
            "gh pr merge",
        ),
    ]
});

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    New {
        id: String,
        repo: PathBuf,
        shell: PathBuf,
        rows: u16,
        cols: u16,
        env: BTreeMap<String, String>,
    },
    Send {
        id: String,
        text: String,
        enter: bool,
    },
    Screen {
        id: String,
    },
    Wait {
        id: String,
        until: String,
        timeout_ms: u64,
    },
    Kill {
        id: String,
    },
    Replay {
        id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum ResponsePayload {
    SessionCreated { id: String },
    Sent,
    Screen(ScreenSnapshot),
    Observation(SessionObservation),
    Killed,
    Replay(Vec<EvidenceEvent>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireResponse {
    pub ok: bool,
    pub data: Option<ResponsePayload>,
    pub error: Option<String>,
}

impl WireResponse {
    pub fn ok(data: ResponsePayload) -> Self {
        Self {
            ok: true,
            data: Some(data),
            error: None,
        }
    }

    pub fn error(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            data: None,
            error: Some(error.into()),
        }
    }
}

pub fn handle_request(manager: &SessionManager, request: Request) -> Result<ResponsePayload> {
    match request {
        Request::New {
            id,
            repo,
            shell,
            rows,
            cols,
            env,
        } => {
            manager.open(SessionConfig {
                id: id.clone(),
                workspace: repo,
                shell,
                env,
                rows,
                cols,
            })?;
            Ok(ResponsePayload::SessionCreated { id })
        }
        Request::Send { id, text, enter } => {
            enforce_action_policy(&text)?;
            if enter {
                manager.send_line(&id, &text)?;
            } else {
                manager.send(&id, text.as_bytes())?;
            }
            Ok(ResponsePayload::Sent)
        }
        Request::Screen { id } => Ok(ResponsePayload::Screen(manager.screen(&id)?)),
        Request::Wait {
            id,
            until,
            timeout_ms,
        } => {
            let condition = parse_wait_condition(&until)?;
            let timeout = Duration::from_millis(timeout_ms);
            Ok(ResponsePayload::Observation(
                manager.wait(&id, condition, timeout)?,
            ))
        }
        Request::Kill { id } => {
            manager.kill(&id)?;
            Ok(ResponsePayload::Killed)
        }
        Request::Replay { id } => Ok(ResponsePayload::Replay(manager.replay(&id)?)),
    }
}

pub fn enforce_action_policy(text: &str) -> Result<()> {
    for (regex, label) in DANGEROUS_COMMANDS.iter() {
        if regex.is_match(text) {
            bail!("{label} requires approval before it can be sent to the PTY");
        }
    }
    Ok(())
}

pub fn serve_unix(socket_path: impl AsRef<Path>, log_dir: impl Into<PathBuf>) -> Result<()> {
    let socket_path = socket_path.as_ref();
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create socket directory {}", parent.display()))?;
    }
    if socket_path.exists() {
        std::fs::remove_file(socket_path)
            .with_context(|| format!("remove stale socket {}", socket_path.display()))?;
    }

    let listener = UnixListener::bind(socket_path)
        .with_context(|| format!("bind unix socket {}", socket_path.display()))?;
    let manager = Arc::new(SessionManager::new(log_dir)?);

    for stream in listener.incoming() {
        let stream = stream.context("accept unix socket connection")?;
        let manager = Arc::clone(&manager);
        thread::spawn(move || {
            let _ = handle_stream(stream, manager);
        });
    }

    Ok(())
}

pub fn request_unix(socket_path: impl AsRef<Path>, request: &Request) -> Result<ResponsePayload> {
    let socket_path = socket_path.as_ref();
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("connect to {}", socket_path.display()))?;
    serde_json::to_writer(&mut stream, request).context("serialize daemon request")?;
    stream.write_all(b"\n").context("write request newline")?;
    stream.flush().context("flush daemon request")?;

    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .context("read daemon response")?;
    let response: WireResponse =
        serde_json::from_str(&response).context("parse daemon response")?;

    if response.ok {
        response.data.context("daemon response missing data")
    } else {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "daemon request failed".to_string())
        );
    }
}

pub fn parse_wait_condition(value: &str) -> Result<WaitCondition> {
    if value == "prompt" {
        return Ok(WaitCondition::Prompt);
    }
    if value == "exit" {
        return Ok(WaitCondition::Exit);
    }
    if let Some(duration) = value.strip_prefix("idle:") {
        return Ok(WaitCondition::Idle {
            quiet_for: parse_duration(duration)?,
        });
    }
    if let Some(pattern) = value.strip_prefix("regex:") {
        return Ok(WaitCondition::Regex {
            pattern: pattern.to_string(),
        });
    }

    Ok(WaitCondition::Regex {
        pattern: regex::escape(value),
    })
}

pub fn parse_duration(value: &str) -> Result<Duration> {
    let value = value.trim();
    if let Some(ms) = value.strip_suffix("ms") {
        return Ok(Duration::from_millis(
            ms.parse().context("parse millisecond duration")?,
        ));
    }
    if let Some(seconds) = value.strip_suffix('s') {
        return Ok(Duration::from_secs(
            seconds.parse().context("parse second duration")?,
        ));
    }
    if let Some(minutes) = value.strip_suffix('m') {
        let minutes: u64 = minutes.parse().context("parse minute duration")?;
        return Ok(Duration::from_secs(minutes * 60));
    }
    Ok(Duration::from_secs(
        value.parse().context("parse second duration")?,
    ))
}

fn handle_stream(mut stream: UnixStream, manager: Arc<SessionManager>) -> Result<()> {
    let mut request = String::new();
    BufReader::new(stream.try_clone().context("clone unix stream")?)
        .read_line(&mut request)
        .context("read daemon request")?;

    let response = match serde_json::from_str::<Request>(&request) {
        Ok(request) => match handle_request(&manager, request) {
            Ok(data) => WireResponse::ok(data),
            Err(error) => WireResponse::error(format!("{error:#}")),
        },
        Err(error) => WireResponse::error(format!("invalid request: {error}")),
    };

    serde_json::to_writer(&mut stream, &response).context("serialize daemon response")?;
    stream.write_all(b"\n").context("write daemon response")?;
    Ok(())
}
