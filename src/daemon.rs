use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, mpsc},
    thread,
    time::Duration,
};

use anyhow::{Context, Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    evidence::EvidenceEvent,
    session::{
        ForkResult, ProofBundle, ScreenSnapshot, SessionConfig, SessionManager, SessionMetadata,
        SessionObservation, TraceEvent, WaitCondition,
    },
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
        #[serde(default)]
        backend: crate::session::SessionBackend,
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
    List,
    Proof {
        id: String,
    },
    Fork {
        id: String,
        name: String,
        copy_worktree: bool,
    },
    Attach {
        id: String,
        read_only: bool,
        history_bytes: usize,
    },
    TracePath,
    Shutdown,
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
    Sessions(Vec<SessionMetadata>),
    Proof(ProofBundle),
    Forked(ForkResult),
    Attached,
    TracePath(PathBuf),
    Shutdown,
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
    let request_name = request.name();
    let session_id = request.session_id();
    let started_at = chrono::Utc::now();
    let result = handle_request_inner(manager, request);
    let ended_at = chrono::Utc::now();
    let mut attributes = BTreeMap::new();
    attributes.insert("component".to_string(), "agent-pty".to_string());
    if let Some(session_id) = session_id {
        attributes.insert("session.id".to_string(), session_id);
    }
    let _ = manager.append_trace(TraceEvent {
        trace_id: Uuid::new_v4().to_string(),
        span_id: Uuid::new_v4().to_string(),
        name: request_name,
        started_at,
        ended_at,
        status: if result.is_ok() { "ok" } else { "error" }.to_string(),
        attributes,
    });
    result
}

fn handle_request_inner(manager: &SessionManager, request: Request) -> Result<ResponsePayload> {
    match request {
        Request::New {
            id,
            repo,
            shell,
            rows,
            cols,
            env,
            backend,
        } => {
            manager.open(SessionConfig {
                id: id.clone(),
                workspace: repo,
                shell,
                env,
                rows,
                cols,
                backend,
            })?;
            Ok(ResponsePayload::SessionCreated { id })
        }
        Request::Send { id, text, enter } => {
            if let Some(rule) = action_policy_violation(&text) {
                manager.record_policy_denial(&id, &text, rule)?;
                bail!("{rule} requires approval before it can be sent to the PTY");
            }
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
        Request::List => Ok(ResponsePayload::Sessions(manager.list_sessions()?)),
        Request::Proof { id } => Ok(ResponsePayload::Proof(manager.proof(&id)?)),
        Request::Fork {
            id,
            name,
            copy_worktree,
        } => Ok(ResponsePayload::Forked(manager.fork(
            &id,
            &name,
            copy_worktree,
        )?)),
        Request::Attach { .. } => bail!("attach is only supported by the Unix socket stream"),
        Request::TracePath => Ok(ResponsePayload::TracePath(
            manager.trace_path().to_path_buf(),
        )),
        Request::Shutdown => bail!("shutdown is only supported by the Unix socket daemon"),
    }
}

pub fn enforce_action_policy(text: &str) -> Result<()> {
    if let Some(label) = action_policy_violation(text) {
        bail!("{label} requires approval before it can be sent to the PTY");
    }
    Ok(())
}

pub fn action_policy_violation(text: &str) -> Option<&'static str> {
    for (regex, label) in DANGEROUS_COMMANDS.iter() {
        if regex.is_match(text) {
            return Some(label);
        }
    }
    None
}

impl Request {
    fn name(&self) -> String {
        match self {
            Request::New { .. } => "terminal.new",
            Request::Send { .. } => "terminal.send",
            Request::Screen { .. } => "terminal.screen",
            Request::Wait { .. } => "terminal.wait",
            Request::Kill { .. } => "terminal.kill",
            Request::Replay { .. } => "terminal.replay",
            Request::List => "terminal.list",
            Request::Proof { .. } => "terminal.proof",
            Request::Fork { .. } => "terminal.fork",
            Request::Attach { .. } => "terminal.attach",
            Request::TracePath => "terminal.trace_path",
            Request::Shutdown => "terminal.shutdown",
        }
        .to_string()
    }

    fn session_id(&self) -> Option<String> {
        match self {
            Request::New { id, .. }
            | Request::Send { id, .. }
            | Request::Screen { id }
            | Request::Wait { id, .. }
            | Request::Kill { id }
            | Request::Replay { id }
            | Request::Proof { id }
            | Request::Fork { id, .. }
            | Request::Attach { id, .. } => Some(id.clone()),
            Request::List | Request::TracePath | Request::Shutdown => None,
        }
    }
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
    listener
        .set_nonblocking(true)
        .context("set unix listener nonblocking")?;
    let manager = Arc::new(SessionManager::new(log_dir)?);
    let (shutdown_tx, shutdown_rx) = mpsc::channel::<()>();

    loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }

        match listener.accept() {
            Ok((stream, _addr)) => {
                let manager = Arc::clone(&manager);
                let shutdown_tx = shutdown_tx.clone();
                thread::spawn(move || {
                    if handle_stream(stream, manager).unwrap_or(false) {
                        let _ = shutdown_tx.send(());
                    }
                });
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error).context("accept unix socket connection"),
        }
    }

    let _ = std::fs::remove_file(socket_path);
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

fn handle_stream(mut stream: UnixStream, manager: Arc<SessionManager>) -> Result<bool> {
    let mut request = String::new();
    BufReader::new(stream.try_clone().context("clone unix stream")?)
        .read_line(&mut request)
        .context("read daemon request")?;

    let mut shutdown = false;
    let response = match serde_json::from_str::<Request>(&request) {
        Ok(request) => {
            shutdown = matches!(request, Request::Shutdown);
            if shutdown {
                WireResponse::ok(ResponsePayload::Shutdown)
            } else if let Request::Attach {
                id,
                read_only,
                history_bytes,
            } = request
            {
                return handle_attach_stream(stream, manager, id, read_only, history_bytes);
            } else {
                match handle_request(&manager, request) {
                    Ok(data) => WireResponse::ok(data),
                    Err(error) => WireResponse::error(format!("{error:#}")),
                }
            }
        }
        Err(error) => WireResponse::error(format!("invalid request: {error}")),
    };

    serde_json::to_writer(&mut stream, &response).context("serialize daemon response")?;
    stream.write_all(b"\n").context("write daemon response")?;
    Ok(shutdown)
}

fn handle_attach_stream(
    mut stream: UnixStream,
    manager: Arc<SessionManager>,
    session_id: String,
    read_only: bool,
    history_bytes: usize,
) -> Result<bool> {
    if let Err(error) = manager.record_attach(&session_id) {
        serde_json::to_writer(&mut stream, &WireResponse::error(format!("{error:#}")))
            .context("serialize attach error response")?;
        stream
            .write_all(b"\n")
            .context("write attach error response newline")?;
        return Ok(false);
    }
    let (mut cursor, initial) = match manager.transcript_snapshot(&session_id, history_bytes) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            serde_json::to_writer(&mut stream, &WireResponse::error(format!("{error:#}")))
                .context("serialize attach error response")?;
            stream
                .write_all(b"\n")
                .context("write attach error response newline")?;
            return Ok(false);
        }
    };
    serde_json::to_writer(&mut stream, &WireResponse::ok(ResponsePayload::Attached))
        .context("serialize attach response")?;
    stream
        .write_all(b"\n")
        .context("write attach response newline")?;
    if !initial.is_empty() {
        stream
            .write_all(initial.as_bytes())
            .context("write initial attach transcript")?;
        stream.flush().context("flush initial attach transcript")?;
    }

    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .context("set attach read timeout")?;
    let mut input = [0_u8; 8192];
    loop {
        match stream.read(&mut input) {
            Ok(0) => break,
            Ok(count) => {
                if !read_only {
                    manager.send(&session_id, &input[..count])?;
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error).context("read attach input"),
        }

        let (next_cursor, chunk) = manager.transcript_since(&session_id, cursor)?;
        cursor = next_cursor;
        if !chunk.is_empty() {
            if let Err(error) = stream.write_all(chunk.as_bytes()) {
                if matches!(
                    error.kind(),
                    ErrorKind::BrokenPipe | ErrorKind::ConnectionReset
                ) {
                    break;
                }
                return Err(error).context("write attach output");
            }
            stream.flush().context("flush attach output")?;
        }

        thread::sleep(Duration::from_millis(25));
    }

    Ok(false)
}
