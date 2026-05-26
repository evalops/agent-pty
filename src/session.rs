use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, LazyLock, Mutex},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
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
    pub backend: SessionBackend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMetadata {
    pub id: String,
    pub workspace: PathBuf,
    pub shell: PathBuf,
    pub env: BTreeMap<String, String>,
    pub rows: u16,
    pub cols: u16,
    pub started_at: DateTime<Utc>,
    pub process_id: Option<u32>,
    pub active: bool,
    pub log_path: PathBuf,
    #[serde(default)]
    pub backend: SessionBackend,
    #[serde(default)]
    pub tmux_session: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionBackend {
    #[default]
    Pty,
    Tmux,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForkResult {
    pub id: String,
    pub source_id: String,
    pub workspace: PathBuf,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofBundle {
    pub session_id: String,
    pub generated_at: DateTime<Utc>,
    pub event_count: usize,
    pub commands_run: Vec<String>,
    pub files_changed: Vec<FileChange>,
    pub risk_flags: Vec<String>,
    pub latest_git_status: Option<String>,
    pub git_diff: Option<String>,
    pub screen_tail: String,
    pub log_path: PathBuf,
    pub json_path: PathBuf,
    pub markdown_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceEvent {
    pub trace_id: String,
    pub span_id: String,
    pub name: String,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub status: String,
    pub attributes: BTreeMap<String, String>,
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
    pub process_snapshot: Option<ProcessSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessSnapshot {
    pub root_pid: u32,
    pub processes: Vec<ProcessInfo>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub ppid: u32,
    pub stat: String,
    pub command: String,
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
    index_path: PathBuf,
    proof_dir: PathBuf,
    trace_path: PathBuf,
    sessions: Mutex<HashMap<String, Arc<SessionHandle>>>,
}

impl SessionManager {
    pub fn new(log_dir: impl Into<PathBuf>) -> Result<Self> {
        let log_dir = log_dir.into();
        std::fs::create_dir_all(&log_dir)
            .with_context(|| format!("create session log dir {}", log_dir.display()))?;
        let proof_dir = log_dir.join("proofs");
        std::fs::create_dir_all(&proof_dir)
            .with_context(|| format!("create proof dir {}", proof_dir.display()))?;
        Ok(Self {
            index_path: log_dir.join("sessions.json"),
            trace_path: log_dir.join("traces.jsonl"),
            proof_dir,
            log_dir,
            sessions: Mutex::new(HashMap::new()),
        })
    }

    pub fn open(&self, config: SessionConfig) -> Result<()> {
        if config.backend == SessionBackend::Tmux {
            return self.open_tmux(config);
        }

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
        let process_id = child.process_id();
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().context("clone pty reader")?;
        let writer = pair.master.take_writer().context("take pty writer")?;
        let log_path = self.log_path(&config.id);
        let log = EventLog::open(log_path.clone())?;
        let state = Arc::new(Mutex::new(SessionState::new(config.rows, config.cols)));
        let started_at = Utc::now();

        let handle = Arc::new(SessionHandle {
            config: config.clone(),
            started_at,
            process_id,
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
            .insert(config.id.clone(), Arc::clone(&handle));
        self.persist_metadata(&SessionMetadata {
            id: config.id,
            workspace: config.workspace,
            shell: config.shell,
            env: config.env,
            rows: config.rows,
            cols: config.cols,
            started_at,
            process_id,
            active: true,
            log_path,
            backend: SessionBackend::Pty,
            tmux_session: None,
        })?;
        Ok(())
    }

    fn open_tmux(&self, config: SessionConfig) -> Result<()> {
        if self
            .read_index()?
            .iter()
            .any(|session| session.id == config.id && session.active)
        {
            bail!("session {} already exists", config.id);
        }

        let tmux_session = safe_filename(&config.id);
        if tmux_session_exists(&tmux_session) {
            bail!("tmux session {tmux_session} already exists");
        }

        let cols = config.cols.to_string();
        let rows = config.rows.to_string();
        let mut command = Command::new("tmux");
        command
            .args(["new-session", "-d", "-s", &tmux_session, "-c"])
            .arg(&config.workspace);
        for (key, value) in &config.env {
            command.arg("-e").arg(format!("{key}={value}"));
        }
        let output = command
            .args(["-x", &cols, "-y", &rows])
            .arg(&config.shell)
            .output()
            .with_context(|| format!("start tmux session {tmux_session}"))?;
        if !output.status.success() {
            bail!(
                "tmux new-session failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }

        let log_path = self.log_path(&config.id);
        if let Err(error) = start_tmux_pipe(&tmux_session, &self.tmux_transcript_path(&config.id)) {
            let _ = Command::new("tmux")
                .args(["kill-session", "-t", &tmux_session])
                .output();
            return Err(error);
        }
        EventLog::open(log_path.clone())?.append_action(
            &config.id,
            Action::Exec {
                argv: vec![
                    "tmux".to_string(),
                    "new-session".to_string(),
                    "-s".to_string(),
                    tmux_session.clone(),
                    config.shell.display().to_string(),
                ],
                cwd: config.workspace.clone(),
            },
            capture_git(&config.workspace),
        )?;
        self.persist_metadata(&SessionMetadata {
            id: config.id,
            workspace: config.workspace,
            shell: config.shell,
            env: config.env,
            rows: config.rows,
            cols: config.cols,
            started_at: Utc::now(),
            process_id: None,
            active: true,
            log_path,
            backend: SessionBackend::Tmux,
            tmux_session: Some(tmux_session),
        })?;
        Ok(())
    }

    pub fn send(&self, session_id: &str, bytes: &[u8]) -> Result<()> {
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    return self.send_tmux(&metadata, bytes);
                }
                return Err(handle_error);
            }
        };
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
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    return self.screen_tmux(&metadata);
                }
                return Err(handle_error);
            }
        };
        Ok(handle
            .state
            .lock()
            .expect("session state lock poisoned")
            .screen_snapshot())
    }

    pub fn transcript_snapshot(
        &self,
        session_id: &str,
        max_bytes: usize,
    ) -> Result<(usize, String)> {
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    return self.transcript_snapshot_tmux(&metadata, max_bytes);
                }
                return Err(handle_error);
            }
        };
        let state = handle.state.lock().expect("session state lock poisoned");
        let len = state.transcript.len();
        let start = len.saturating_sub(max_bytes);
        Ok((
            len,
            slice_from_boundary(&state.transcript, start).to_string(),
        ))
    }

    pub fn transcript_since(&self, session_id: &str, offset: usize) -> Result<(usize, String)> {
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    return self.transcript_since_tmux(&metadata, offset);
                }
                return Err(handle_error);
            }
        };
        let state = handle.state.lock().expect("session state lock poisoned");
        let len = state.transcript.len();
        if offset >= len {
            return Ok((len, String::new()));
        }
        Ok((
            len,
            slice_from_boundary(&state.transcript, offset).to_string(),
        ))
    }

    pub fn wait(
        &self,
        session_id: &str,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<SessionObservation> {
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    return self.wait_tmux(session_id, &metadata, condition, timeout);
                }
                return Err(handle_error);
            }
        };
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
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    return self.kill_tmux(&metadata);
                }
                return Err(handle_error);
            }
        };
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

    pub fn record_attach(&self, session_id: &str) -> Result<()> {
        let handle = match self.handle(session_id) {
            Ok(handle) => handle,
            Err(handle_error) => {
                if let Some(metadata) = self.metadata(session_id)?
                    && metadata.backend == SessionBackend::Tmux
                {
                    EventLog::open(self.log_path(session_id))?.append_action(
                        session_id,
                        Action::AttachHuman,
                        capture_git(&metadata.workspace),
                    )?;
                    return Ok(());
                }
                return Err(handle_error);
            }
        };
        handle.log.append_action(
            session_id,
            Action::AttachHuman,
            capture_git(&handle.config.workspace),
        )?;
        Ok(())
    }

    pub fn replay(&self, session_id: &str) -> Result<Vec<EvidenceEvent>> {
        if let Some(handle) = self
            .sessions
            .lock()
            .expect("session registry lock poisoned")
            .get(session_id)
            .cloned()
        {
            return handle.log.replay();
        }

        EventLog::open(self.log_path(session_id))?.replay()
    }

    pub fn list_sessions(&self) -> Result<Vec<SessionMetadata>> {
        let mut by_id = self
            .read_index()?
            .into_iter()
            .map(|mut metadata| {
                metadata.active = match metadata.backend {
                    SessionBackend::Pty => false,
                    SessionBackend::Tmux => metadata
                        .tmux_session
                        .as_deref()
                        .is_some_and(tmux_session_exists),
                };
                (metadata.id.clone(), metadata)
            })
            .collect::<BTreeMap<_, _>>();

        for handle in self
            .sessions
            .lock()
            .expect("session registry lock poisoned")
            .values()
        {
            by_id.insert(handle.config.id.clone(), self.metadata_for_handle(handle));
        }

        Ok(by_id.into_values().collect())
    }

    pub fn record_policy_denial(&self, session_id: &str, command: &str, rule: &str) -> Result<()> {
        let git_snapshot = if let Some(handle) = self
            .sessions
            .lock()
            .expect("session registry lock poisoned")
            .get(session_id)
        {
            capture_git(&handle.config.workspace)
        } else {
            self.metadata(session_id)?
                .and_then(|metadata| capture_git(&metadata.workspace))
        };
        EventLog::open(self.log_path(session_id))?.append_action(
            session_id,
            Action::PolicyDenied {
                command: command.to_string(),
                rule: rule.to_string(),
            },
            git_snapshot,
        )?;
        Ok(())
    }

    pub fn fork(
        &self,
        source_id: &str,
        target_id: &str,
        copy_worktree: bool,
    ) -> Result<ForkResult> {
        let source = self.handle(source_id)?;
        let mut target_config = source.config.clone();
        target_config.id = target_id.to_string();

        let (workspace, branch) = if copy_worktree {
            create_worktree_fork(&source.config.workspace, target_id)?
        } else {
            (source.config.workspace.clone(), None)
        };
        target_config.workspace = workspace.clone();

        self.open(target_config)?;
        source.log.append_action(
            source_id,
            Action::Fork {
                source: source_id.to_string(),
                target: target_id.to_string(),
                workspace: workspace.clone(),
            },
            capture_git(&source.config.workspace),
        )?;

        Ok(ForkResult {
            id: target_id.to_string(),
            source_id: source_id.to_string(),
            workspace,
            branch,
        })
    }

    pub fn proof(&self, session_id: &str) -> Result<ProofBundle> {
        let events = self.replay(session_id)?;
        let mut commands_run = Vec::new();
        let mut files_changed = BTreeMap::<PathBuf, FileChange>::new();
        let mut risk_flags = BTreeSet::<String>::new();
        let mut latest_git_status = None;
        let mut git_diff = None;
        let mut screen_tail = String::new();

        for event in &events {
            if let Some(snapshot) = &event.git_snapshot {
                latest_git_status = Some(snapshot.status_short.clone());
                git_diff = Some(snapshot.diff.clone());
                for change in files_changed_from_git(snapshot) {
                    files_changed.insert(change.path.clone(), change);
                }
            }

            match &event.kind {
                crate::evidence::EventKind::Action(action) => match action {
                    Action::SendKeys { bytes } => {
                        let command = String::from_utf8_lossy(bytes).trim().to_string();
                        if !command.is_empty() {
                            commands_run.push(command);
                        }
                    }
                    Action::Exec { argv, cwd } => {
                        commands_run.push(format!("{} # cwd={}", argv.join(" "), cwd.display()));
                    }
                    Action::PolicyDenied { command, rule } => {
                        risk_flags.insert(format!("blocked {rule}: {command}"));
                    }
                    _ => {}
                },
                crate::evidence::EventKind::Observation(observation) => {
                    if let Some(screen) = &observation.screen_text {
                        screen_tail = tail_chars(screen, 8_000);
                    } else if !observation.stdout_tail.is_empty() {
                        screen_tail = tail_chars(&observation.stdout_tail, 8_000);
                    }
                    if let Some(snapshot) = &observation.git_snapshot {
                        latest_git_status = Some(snapshot.status_short.clone());
                        git_diff = Some(snapshot.diff.clone());
                    }
                    for change in &observation.files_changed {
                        files_changed.insert(change.path.clone(), change.clone());
                    }
                    if let Some(status) = observation.exit_status
                        && status != 0
                    {
                        risk_flags.insert(format!("nonzero exit status {status}"));
                    }
                }
            }
        }

        let json_path = self
            .proof_dir
            .join(format!("{}.proof.json", safe_filename(session_id)));
        let markdown_path = self
            .proof_dir
            .join(format!("{}.proof.md", safe_filename(session_id)));
        let proof = ProofBundle {
            session_id: session_id.to_string(),
            generated_at: Utc::now(),
            event_count: events.len(),
            commands_run,
            files_changed: files_changed.into_values().collect(),
            risk_flags: risk_flags.into_iter().collect(),
            latest_git_status,
            git_diff,
            screen_tail,
            log_path: self.log_path(session_id),
            json_path,
            markdown_path,
        };

        fs::write(&proof.json_path, serde_json::to_vec_pretty(&proof)?)
            .with_context(|| format!("write proof json {}", proof.json_path.display()))?;
        fs::write(&proof.markdown_path, render_proof_markdown(&proof))
            .with_context(|| format!("write proof markdown {}", proof.markdown_path.display()))?;
        EventLog::open(self.log_path(session_id))?.append_action(
            session_id,
            Action::Proof {
                json_path: proof.json_path.clone(),
                markdown_path: proof.markdown_path.clone(),
            },
            None,
        )?;

        Ok(proof)
    }

    pub fn append_trace(&self, event: TraceEvent) -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.trace_path)
            .with_context(|| format!("open trace log {}", self.trace_path.display()))?;
        serde_json::to_writer(&mut file, &event).context("serialize trace event")?;
        file.write_all(b"\n")
            .with_context(|| format!("write trace log {}", self.trace_path.display()))?;
        Ok(())
    }

    pub fn trace_path(&self) -> &Path {
        &self.trace_path
    }

    fn send_tmux(&self, metadata: &SessionMetadata, bytes: &[u8]) -> Result<()> {
        let session = metadata
            .tmux_session
            .as_deref()
            .with_context(|| format!("session {} missing tmux session name", metadata.id))?;
        ensure_tmux_session(session)?;
        self.ensure_tmux_pipe(metadata)?;
        send_tmux_bytes(session, bytes)?;
        EventLog::open(self.log_path(&metadata.id))?.append_action(
            &metadata.id,
            Action::SendKeys {
                bytes: bytes.to_vec(),
            },
            capture_git(&metadata.workspace),
        )?;
        Ok(())
    }

    fn transcript_snapshot_tmux(
        &self,
        metadata: &SessionMetadata,
        max_bytes: usize,
    ) -> Result<(usize, String)> {
        self.ensure_tmux_pipe(metadata)?;
        let text = self.tmux_transcript_text(&metadata.id)?;
        let len = text.len();
        if len == 0 {
            let screen = self.screen_tmux(metadata)?.text;
            let start = screen.len().saturating_sub(max_bytes);
            return Ok((0, slice_from_boundary(&screen, start).to_string()));
        }
        let start = len.saturating_sub(max_bytes);
        Ok((len, slice_from_boundary(&text, start).to_string()))
    }

    fn transcript_since_tmux(
        &self,
        metadata: &SessionMetadata,
        offset: usize,
    ) -> Result<(usize, String)> {
        self.ensure_tmux_pipe(metadata)?;
        let text = self.tmux_transcript_text(&metadata.id)?;
        let len = text.len();
        if offset >= len {
            return Ok((len, String::new()));
        }
        Ok((len, slice_from_boundary(&text, offset).to_string()))
    }

    fn screen_tmux(&self, metadata: &SessionMetadata) -> Result<ScreenSnapshot> {
        let session = metadata
            .tmux_session
            .as_deref()
            .with_context(|| format!("session {} missing tmux session name", metadata.id))?;
        ensure_tmux_session(session)?;
        let output = Command::new("tmux")
            .args(["capture-pane", "-p", "-t", session, "-S", "-3000"])
            .output()
            .with_context(|| format!("capture tmux session {session}"))?;
        if !output.status.success() {
            bail!(
                "tmux capture-pane failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = String::from_utf8_lossy(&output.stdout).to_string();
        Ok(ScreenSnapshot {
            rows: metadata.rows,
            cols: metadata.cols,
            spans: spans_for_screen(&text),
            text,
        })
    }

    fn wait_tmux(
        &self,
        session_id: &str,
        metadata: &SessionMetadata,
        condition: WaitCondition,
        timeout: Duration,
    ) -> Result<SessionObservation> {
        let log = EventLog::open(self.log_path(session_id))?;
        log.append_action(
            session_id,
            Action::WaitFor {
                predicate: condition.to_predicate(),
                timeout,
            },
            capture_git(&metadata.workspace),
        )?;
        let matcher = condition.matcher()?;
        let deadline = Instant::now() + timeout;
        let mut last_text = String::new();
        let mut last_change_at = Instant::now();

        loop {
            let mut observation = self.observation_from_tmux(metadata)?;
            if observation.screen.text != last_text {
                last_text = observation.screen.text.clone();
                last_change_at = Instant::now();
            }
            if matcher.matches_tmux(&observation, last_change_at) {
                log.append_observation(session_id, observation.to_evidence())?;
                return Ok(observation);
            }

            if Instant::now() >= deadline {
                observation = self.observation_from_tmux(metadata)?;
                log.append_observation(session_id, observation.to_evidence())?;
                bail!("timed out waiting for {condition:?} in session {session_id}");
            }

            thread::sleep(Duration::from_millis(25));
        }
    }

    fn kill_tmux(&self, metadata: &SessionMetadata) -> Result<()> {
        let session = metadata
            .tmux_session
            .as_deref()
            .with_context(|| format!("session {} missing tmux session name", metadata.id))?;
        if tmux_session_exists(session) {
            let output = Command::new("tmux")
                .args(["kill-session", "-t", session])
                .output()
                .with_context(|| format!("kill tmux session {session}"))?;
            if !output.status.success() {
                bail!(
                    "tmux kill-session failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
        EventLog::open(self.log_path(&metadata.id))?.append_action(
            &metadata.id,
            Action::Kill {
                signal: "kill".to_string(),
            },
            capture_git(&metadata.workspace),
        )?;
        let mut updated = metadata.clone();
        updated.active = false;
        self.persist_metadata(&updated)?;
        Ok(())
    }

    fn observation_from_tmux(&self, metadata: &SessionMetadata) -> Result<SessionObservation> {
        let screen = self.screen_tmux(metadata)?;
        let stdout_tail = tail_chars(&screen.text, 12_000);
        let git_snapshot = capture_git(&metadata.workspace);
        let files_changed = git_snapshot
            .as_ref()
            .map(files_changed_from_git)
            .unwrap_or_default();
        Ok(SessionObservation {
            screen,
            stdout_tail,
            stderr_tail: String::new(),
            exit_status: None,
            files_changed,
            git_snapshot,
            process_snapshot: metadata
                .tmux_session
                .as_deref()
                .and_then(tmux_root_pid)
                .and_then(process_snapshot),
        })
    }

    fn ensure_tmux_pipe(&self, metadata: &SessionMetadata) -> Result<()> {
        let session = metadata
            .tmux_session
            .as_deref()
            .with_context(|| format!("session {} missing tmux session name", metadata.id))?;
        ensure_tmux_session(session)?;
        start_tmux_pipe(session, &self.tmux_transcript_path(&metadata.id))
    }

    fn tmux_transcript_text(&self, session_id: &str) -> Result<String> {
        let path = self.tmux_transcript_path(session_id);
        match fs::read_to_string(&path) {
            Ok(text) => Ok(text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
            Err(error) => {
                Err(error).with_context(|| format!("read tmux transcript {}", path.display()))
            }
        }
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
            process_snapshot: handle.process_id.and_then(process_snapshot),
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

    fn tmux_transcript_path(&self, session_id: &str) -> PathBuf {
        self.log_dir
            .join(format!("{}.tmux.log", safe_filename(session_id)))
    }

    fn metadata_for_handle(&self, handle: &SessionHandle) -> SessionMetadata {
        SessionMetadata {
            id: handle.config.id.clone(),
            workspace: handle.config.workspace.clone(),
            shell: handle.config.shell.clone(),
            env: handle.config.env.clone(),
            rows: handle.config.rows,
            cols: handle.config.cols,
            started_at: handle.started_at,
            process_id: handle.process_id,
            active: true,
            log_path: self.log_path(&handle.config.id),
            backend: SessionBackend::Pty,
            tmux_session: None,
        }
    }

    fn metadata(&self, session_id: &str) -> Result<Option<SessionMetadata>> {
        Ok(self
            .read_index()?
            .into_iter()
            .find(|metadata| metadata.id == session_id))
    }

    fn read_index(&self) -> Result<Vec<SessionMetadata>> {
        if !self.index_path.exists() {
            return Ok(Vec::new());
        }
        let bytes = fs::read(&self.index_path)
            .with_context(|| format!("read session index {}", self.index_path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("parse session index {}", self.index_path.display()))
    }

    fn write_index(&self, sessions: &[SessionMetadata]) -> Result<()> {
        fs::write(&self.index_path, serde_json::to_vec_pretty(sessions)?)
            .with_context(|| format!("write session index {}", self.index_path.display()))
    }

    fn persist_metadata(&self, metadata: &SessionMetadata) -> Result<()> {
        let mut sessions = self.read_index()?;
        if let Some(existing) = sessions
            .iter_mut()
            .find(|session| session.id == metadata.id)
        {
            *existing = metadata.clone();
        } else {
            sessions.push(metadata.clone());
        }
        sessions.sort_by(|left, right| left.id.cmp(&right.id));
        self.write_index(&sessions)
    }
}

struct SessionHandle {
    config: SessionConfig,
    started_at: DateTime<Utc>,
    process_id: Option<u32>,
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

    fn matches_tmux(&self, observation: &SessionObservation, last_change_at: Instant) -> bool {
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
            WaitMatcher::Exit => false,
            WaitMatcher::Idle { quiet_for } => last_change_at.elapsed() >= *quiet_for,
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

fn create_worktree_fork(
    source_workspace: &Path,
    target_id: &str,
) -> Result<(PathBuf, Option<String>)> {
    let safe_target = safe_filename(target_id);
    let fork_root = source_workspace
        .parent()
        .unwrap_or(source_workspace)
        .join(format!(
            "{}.agent-pty-forks",
            source_workspace
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("workspace")
        ));
    fs::create_dir_all(&fork_root)
        .with_context(|| format!("create fork root {}", fork_root.display()))?;
    let target_workspace = fork_root.join(&safe_target);

    if ensure_git_worktree(source_workspace).is_ok() {
        if target_workspace.exists() {
            bail!(
                "fork workspace already exists: {}",
                target_workspace.display()
            );
        }
        let branch = format!("agent-pty/{safe_target}");
        let output = Command::new("git")
            .args(["worktree", "add", "-b", &branch])
            .arg(&target_workspace)
            .arg("HEAD")
            .current_dir(source_workspace)
            .output()
            .with_context(|| format!("create git worktree {}", target_workspace.display()))?;
        if !output.status.success() {
            bail!(
                "git worktree add failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        return Ok((target_workspace, Some(branch)));
    }

    copy_dir(source_workspace, &target_workspace)?;
    Ok((target_workspace, None))
}

fn ensure_git_worktree(workspace: &Path) -> Result<()> {
    let output = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .current_dir(workspace)
        .output()
        .with_context(|| format!("run git in {}", workspace.display()))?;

    if output.status.success() && String::from_utf8_lossy(&output.stdout).trim() == "true" {
        Ok(())
    } else {
        bail!("{} is not a git worktree", workspace.display());
    }
}

fn copy_dir(source: &Path, target: &Path) -> Result<()> {
    if target.exists() {
        bail!("fork workspace already exists: {}", target.display());
    }
    fs::create_dir_all(target).with_context(|| format!("create {}", target.display()))?;
    for entry in fs::read_dir(source).with_context(|| format!("read {}", source.display()))? {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&source_path, &target_path)?;
        } else {
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "copy {} to {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }
    Ok(())
}

fn render_proof_markdown(proof: &ProofBundle) -> String {
    let mut output = String::new();
    output.push_str(&format!("# agent-pty proof: {}\n\n", proof.session_id));
    output.push_str(&format!("- Generated: {}\n", proof.generated_at));
    output.push_str(&format!("- Events: {}\n", proof.event_count));
    output.push_str(&format!("- Event log: `{}`\n", proof.log_path.display()));
    output.push_str("\n## Commands\n\n");
    if proof.commands_run.is_empty() {
        output.push_str("- none recorded\n");
    } else {
        for command in &proof.commands_run {
            output.push_str(&format!("- `{}`\n", command.replace('`', "\\`")));
        }
    }
    output.push_str("\n## Files Changed\n\n");
    if proof.files_changed.is_empty() {
        output.push_str("- none recorded\n");
    } else {
        for change in &proof.files_changed {
            output.push_str(&format!(
                "- `{}` {}\n",
                change.path.display(),
                change.status
            ));
        }
    }
    output.push_str("\n## Risk Flags\n\n");
    if proof.risk_flags.is_empty() {
        output.push_str("- none\n");
    } else {
        for flag in &proof.risk_flags {
            output.push_str(&format!("- {flag}\n"));
        }
    }
    output.push_str("\n## Screen Tail\n\n```text\n");
    output.push_str(proof.screen_tail.trim_end());
    output.push_str("\n```\n");
    output
}

fn process_snapshot(root_pid: u32) -> Option<ProcessSnapshot> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,stat=,command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let all = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(parse_process_info)
        .collect::<Vec<_>>();
    let mut wanted = HashSet::from([root_pid]);
    let mut changed = true;
    while changed {
        changed = false;
        for process in &all {
            if wanted.contains(&process.ppid) && wanted.insert(process.pid) {
                changed = true;
            }
        }
    }

    let processes = all
        .into_iter()
        .filter(|process| wanted.contains(&process.pid))
        .collect::<Vec<_>>();
    if processes.is_empty() {
        None
    } else {
        Some(ProcessSnapshot {
            root_pid,
            processes,
        })
    }
}

fn tmux_session_exists(session: &str) -> bool {
    Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn ensure_tmux_session(session: &str) -> Result<()> {
    if tmux_session_exists(session) {
        Ok(())
    } else {
        bail!("tmux session {session} is not running")
    }
}

fn start_tmux_pipe(session: &str, transcript_path: &Path) -> Result<()> {
    let command = format!("cat >> {}", shell_quote(&transcript_path.to_string_lossy()));
    let output = Command::new("tmux")
        .args(["pipe-pane", "-o", "-t", session])
        .arg(command)
        .output()
        .with_context(|| format!("start tmux transcript pipe for session {session}"))?;
    if !output.status.success() {
        bail!(
            "tmux pipe-pane failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn send_tmux_bytes(session: &str, bytes: &[u8]) -> Result<()> {
    let text = String::from_utf8_lossy(bytes);
    let parts = text.split('\n').collect::<Vec<_>>();
    for (index, part) in parts.iter().enumerate() {
        if !part.is_empty() {
            let output = Command::new("tmux")
                .args(["send-keys", "-t", session, "-l", part])
                .output()
                .with_context(|| format!("send literal keys to tmux session {session}"))?;
            if !output.status.success() {
                bail!(
                    "tmux send-keys failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
        if index < parts.len().saturating_sub(1) {
            let output = Command::new("tmux")
                .args(["send-keys", "-t", session, "Enter"])
                .output()
                .with_context(|| format!("send Enter to tmux session {session}"))?;
            if !output.status.success() {
                bail!(
                    "tmux send-keys Enter failed: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                );
            }
        }
    }
    Ok(())
}

fn tmux_root_pid(session: &str) -> Option<u32> {
    let output = Command::new("tmux")
        .args(["display-message", "-p", "-t", session, "#{pane_pid}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
}

fn parse_process_info(line: &str) -> Option<ProcessInfo> {
    let mut parts = line.split_whitespace();
    let pid = parts.next()?.parse().ok()?;
    let ppid = parts.next()?.parse().ok()?;
    let stat = parts.next()?.to_string();
    let command = parts.collect::<Vec<_>>().join(" ");
    Some(ProcessInfo {
        pid,
        ppid,
        stat,
        command,
    })
}

fn tail_chars(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    chars.reverse();
    chars.into_iter().collect()
}

fn slice_from_boundary(value: &str, mut start: usize) -> &str {
    if start >= value.len() {
        return "";
    }
    while start > 0 && !value.is_char_boundary(start) {
        start -= 1;
    }
    &value[start..]
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
