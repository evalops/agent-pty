use std::{
    collections::{BTreeMap, HashMap},
    io::{Read, Write},
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::evidence::{
    Action, EventLog, EvidenceEvent, FileChange, GitSnapshot, Observation, Predicate,
};

static URL_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"https?://[^\s)>\]]+").expect("valid URL regex"));
static FILE_PATH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?x)(?:\./|\../|/)?[[:alnum:]_.-]+(?:/[[:alnum:]_.-]+)+(?::\d+)?")
        .expect("valid file path regex")
});

#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub id: String,
    pub workspace: PathBuf,
    pub shell: PathBuf,
    pub env: BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WaitCondition {
    Regex { pattern: String },
    Prompt,
    Exit,
    Idle { quiet_for: Duration },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionObservation {
    pub screen: ScreenSnapshot,
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub exit_status: Option<i32>,
    pub files_changed: Vec<FileChange>,
    pub git_snapshot: Option<GitSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSnapshot {
    pub rows: u16,
    pub cols: u16,
    pub text: String,
    pub spans: Vec<ScreenSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSpan {
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub kind: SpanKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    Output,
    Prompt,
    ErrorLike,
    Url,
    FilePath,
}

pub struct SessionManager {
    log_dir: PathBuf,
    sessions: Mutex<HashMap<String, Arc<SessionHandle>>>,
}

impl SessionManager {
    pub fn new(log_dir: impl Into<PathBuf>) -> Result<Self> {
        let log_dir = log_dir.into();
        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("create session log dir {}", log_dir.display()))?;
        Ok(Self {
            log_dir,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn open(&self, config: SessionConfig) -> Result<()> {
        if self
            .sessions
            .lock()
            .expect("session registry lock poisoned")
            .contains_key(&config.id)
        {
            bail!("session {} already exists", config.id);
        }

        let pty_system = native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows: config.rows,
                cols: config.cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("open pty")?;

        let mut command = CommandBuilder::new(&config.shell);
        command.cwd(&config.workspace);
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let child = pair
            .slave
            .spawn_command(command)
            .with_context(|| format!("spawn shell {}", config.shell.display()))?;
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;
        let log = EventLog::open(self.log_path(&config.id))?;
        let state = Arc::new(Mutex::new(SessionState::new(config.rows, config.cols)));

        let handle = Arc::new(SessionHandle {
            config: config.clone(),
            writer: Mutex::new(writer),
            child: Mutex::new(child),
            state: Arc::clone(&state),
            log,
        });

        let reader_state = Arc::clone(&state);
        thread::Builder::new()
            .name(format!("agent-pty-reader-{}", config.id))
            .spawn(move || {
                let mut buffer = [0_u8; 8192];
                loop {
                    match reader.read(&mut buffer) {
                        Ok(0) => break,
                        Ok(count) => {
                            let mut state =
                                reader_state.lock().expect("session state lock poisoned");
                            state.record_output(&buffer[..count]);
                        }
                        Err(_) => break,
                    }
                }
            })
            .context("spawn pty reader thread")?;

        handle.log.append_action(
            &config.id,
            Action::Exec {
                argv: vec![config.shell.display().to_string()],
                cwd: config.workspace.clone(),
            },
            capture_git(&config.workspace),
        )?;

        self.sessions
            .lock()
            .expect("session registry lock poisoned")
            .insert(config.id, handle);
        Ok(())
    }

    pub fn send(&self, session_id: &str, bytes: &[u8]) -> Result<()> {
        let handle = self.handle(session_id)?;
        {
            let mut writer = handle.writer.lock().expect("session writer lock poisoned");
            writer
                .write_all(bytes)
                .with_context(|| format!("send bytes to session {session_id}"))?;
            writer
                .flush()
                .with_context(|| format!("flush session {session_id}"))?;
        }

        handle.log.append_action(
            session_id,
            Action::SendKeys {
                bytes: bytes.to_vec(),
            },
            capture_git(&handle.config.workspace),
        )?;
        Ok(())
    }

    pub fn send_line(&self, session_id: &str, line: &str) -> Result<()> {
        let mut bytes = line.as_bytes().to_vec();
        bytes.push(b'\n');
        self.send(session_id, &bytes)
    }

    pub fn screen(&self, session_id: &str) -> Result<ScreenSnapshot> {
        let handle = self.handle(session_id)?;
        Ok(handle
            .state
            .lock()
            .expect("session state lock poisoned")
            .screen_snapshot())
    }

    pub fn wait(
        &self,
        session_id: &str,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<SessionObservation> {
        let handle = self.handle(session_id)?;
        handle.log.append_action(
            session_id,
            Action::WaitFor {
                predicate: condition.to_predicate(),
                timeout,
            },
            capture_git(&handle.config.workspace),
        )?;

        let matcher = condition.matcher()?;
        let deadline = Instant::now() + timeout;

        loop {
            self.refresh_exit(&handle)?;
            let observation = self.observation_from(&handle);
            if matcher.matches(&observation, &handle) {
                handle
                    .log
                    .append_observation(session_id, observation.to_evidence())?;
                return Ok(observation);
            }

            if Instant::now() >= deadline {
                handle
                    .log
                    .append_observation(session_id, observation.to_evidence())?;
                bail!("timed out waiting for {condition:?} in session {session_id}");
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    pub fn kill(&self, session_id: &str) -> Result<()> {
        let handle = self.handle(session_id)?;
        handle
            .child
            .lock()
            .expect("session child lock poisoned")
            .kill()
            .with_context(|| format!("kill session {session_id}"))?;
        handle.log.append_action(
            session_id,
            Action::Kill {
                signal: "kill".to_string(),
            },
            capture_git(&handle.config.workspace),
        )?;
        Ok(())
    }

    pub fn replay(&self, session_id: &str) -> Result<Vec<EvidenceEvent>> {
        let handle = self.handle(session_id)?;
        handle.log.replay()
    }

    fn observation_from(&self, handle: &SessionHandle) -> SessionObservation {
        let state = handle.state.lock().expect("session state lock poisoned");
        let screen = state.screen_snapshot();
        let stdout_tail = tail_chars(&state.transcript, 12_000);
        let exit_status = state.exit_status;
        drop(state);

        let git_snapshot = capture_git(&handle.config.workspace);
        let files_changed = git_snapshot
            .as_ref()
            .map(files_changed_from_git)
            .unwrap_or_default();

        SessionObservation {
            screen,
            stdout_tail,
            stderr_tail: String::new(),
            exit_status,
            files_changed,
            git_snapshot,
        }
    }

    fn refresh_exit(&self, handle: &SessionHandle) -> Result<()> {
        if handle
            .state
            .lock()
            .expect("session state lock poisoned")
            .exit_status
            .is_some()
        {
            return Ok(());
        }

        let maybe_status = handle
            .child
            .lock()
            .expect("session child lock poisoned")
            .try_wait()
            .context("poll child process")?;
        if let Some(status) = maybe_status {
            handle
                .state
                .lock()
                .expect("session state lock poisoned")
                .exit_status = Some(status.exit_code() as i32);
        }
        Ok(())
    }

    fn handle(&self, session_id: &str) -> Result<Arc<SessionHandle>> {
        self.sessions
            .lock()
            .expect("session registry lock poisoned")
            .get(session_id)
            .cloned()
            .with_context(|| format!("unknown session {session_id}"))
    }

    fn log_path(&self, session_id: &str) -> PathBuf {
        self.log_dir
            .join(format!("{}.jsonl", safe_filename(session_id)))
    }
}

struct SessionHandle {
    config: SessionConfig,
    writer: Mutex<Box<dyn Write + Send>>,
    child: Mutex<Box<dyn portable_pty::Child + Send + Sync>>,
    state: Arc<Mutex<SessionState>>,
    log: EventLog,
}

struct SessionState {
    parser: vt100::Parser,
    rows: u16,
    cols: u16,
    transcript: String,
    last_output_at: Instant,
    exit_status: Option<i32>,
}

impl SessionState {
    fn new(rows: u16, cols: u16) -> Self {
        Self {
            parser: vt100::Parser::new(rows, cols, 10_000),
            rows,
            cols,
            transcript: String::new(),
            last_output_at: Instant::now(),
            exit_status: None,
        }
    }

    fn record_output(&mut self, bytes: &[u8]) {
        self.parser.process(bytes);
        self.transcript.push_str(&String::from_utf8_lossy(bytes));
        if self.transcript.len() > 1_000_000 {
            self.transcript = tail_chars(&self.transcript, 750_000);
        }
        self.last_output_at = Instant::now();
    }

    fn screen_snapshot(&self) -> ScreenSnapshot {
        let text = self.parser.screen().contents();
        ScreenSnapshot {
            rows: self.rows,
            cols: self.cols,
            spans: spans_for_screen(&text),
            text,
        }
    }
}

impl std::fmt::Debug for SessionState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionState")
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("transcript_len", &self.transcript.len())
            .field("exit_status", &self.exit_status)
            .finish()
    }
}

impl WaitCondition {
    fn matcher(&self) -> Result<WaitMatcher> {
        match self {
            WaitCondition::Regex { pattern } => Ok(WaitMatcher::Regex(
                Regex::new(pattern).with_context(|| format!("compile wait regex {pattern:?}"))?,
            )),
            WaitCondition::Prompt => Ok(WaitMatcher::Prompt),
            WaitCondition::Exit => Ok(WaitMatcher::Exit),
            WaitCondition::Idle { quiet_for } => Ok(WaitMatcher::Idle {
                quiet_for: *quiet_for,
            }),
        }
    }

    fn to_predicate(&self) -> Predicate {
        match self {
            WaitCondition::Regex { pattern } => Predicate::Regex {
                pattern: pattern.clone(),
            },
            WaitCondition::Prompt => Predicate::Prompt,
            WaitCondition::Exit => Predicate::Exit,
            WaitCondition::Idle { quiet_for } => Predicate::Idle {
                quiet_for: *quiet_for,
            },
        }
    }
}

enum WaitMatcher {
    Regex(Regex),
    Prompt,
    Exit,
    Idle { quiet_for: Duration },
}

impl WaitMatcher {
    fn matches(&self, observation: &SessionObservation, handle: &SessionHandle) -> bool {
        match self {
            WaitMatcher::Regex(regex) => {
                regex.is_match(&observation.screen.text) || regex.is_match(&observation.stdout_tail)
            }
            WaitMatcher::Prompt => observation
                .screen
                .text
                .lines()
                .last()
                .is_some_and(looks_like_prompt),
            WaitMatcher::Exit => observation.exit_status.is_some(),
            WaitMatcher::Idle { quiet_for } => {
                handle
                    .state
                    .lock()
                    .expect("session state lock poisoned")
                    .last_output_at
                    .elapsed()
                    >= *quiet_for
            }
        }
    }
}

impl SessionObservation {
    fn to_evidence(&self) -> Observation {
        Observation {
            screen_text: Some(self.screen.text.clone()),
            stdout_tail: self.stdout_tail.clone(),
            stderr_tail: self.stderr_tail.clone(),
            exit_status: self.exit_status,
            files_changed: self.files_changed.clone(),
            git_snapshot: self.git_snapshot.clone(),
        }
    }
}

fn spans_for_screen(text: &str) -> Vec<ScreenSpan> {
    let mut spans = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(start) = line.find(trimmed) {
            spans.push(ScreenSpan {
                line: line_index,
                start_col: start,
                end_col: start + trimmed.len(),
                kind: if looks_like_prompt(line) {
                    SpanKind::Prompt
                } else if looks_error_like(line) {
                    SpanKind::ErrorLike
                } else {
                    SpanKind::Output
                },
                text: trimmed.to_string(),
            });
        }

        for match_ in URL_RE.find_iter(line) {
            spans.push(ScreenSpan {
                line: line_index,
                start_col: match_.start(),
                end_col: match_.end(),
                kind: SpanKind::Url,
                text: match_.as_str().to_string(),
            });
        }

        for match_ in FILE_PATH_RE.find_iter(line) {
            spans.push(ScreenSpan {
                line: line_index,
                start_col: match_.start(),
                end_col: match_.end(),
                kind: SpanKind::FilePath,
                text: match_.as_str().to_string(),
            });
        }
    }
    spans
}

fn looks_error_like(line: &str) -> bool {
    let line = line.to_ascii_lowercase();
    line.contains("error")
        || line.contains("failed")
        || line.contains("panic")
        || line.contains("exception")
}

fn looks_like_prompt(line: &str) -> bool {
    let trimmed = line.trim_end();
    trimmed.ends_with("$")
        || trimmed.ends_with("$ ")
        || trimmed.ends_with("%")
        || trimmed.ends_with("% ")
        || trimmed.ends_with("#")
        || trimmed.ends_with("# ")
        || trimmed.ends_with("> ")
}

fn files_changed_from_git(snapshot: &GitSnapshot) -> Vec<FileChange> {
    snapshot
        .status_short
        .lines()
        .filter_map(|line| {
            let status = line.get(..2)?.trim().to_string();
            let path = line.get(3..)?.trim();
            if path.is_empty() {
                None
            } else {
                Some(FileChange {
                    path: PathBuf::from(path),
                    status,
                })
            }
        })
        .collect()
}

fn capture_git(workspace: &std::path::Path) -> Option<GitSnapshot> {
    GitSnapshot::capture(workspace).ok()
}

fn tail_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

fn safe_filename(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}
