use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Write},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    process::{Command as ProcessCommand, ExitCode},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use serde::Serialize;

use agent_pty::{
    daemon::{Request, ResponsePayload, WireResponse, parse_duration, request_unix, serve_unix},
    demo::{DemoOptions, run_demo},
    evidence::{Action, EventKind},
    http::serve_http,
    mcp::serve_mcp_stdio,
};

#[derive(Debug, Parser)]
#[command(name = "agent-pty")]
#[command(about = "Agent terminal substrate with persistent PTYs and evidence logs")]
struct Cli {
    #[arg(long, global = true, default_value = "~/.agent-pty.sock")]
    socket: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the Unix-socket daemon for persistent terminal sessions.
    Serve {
        #[arg(long, default_value = "~/.agent-pty/runs")]
        log_dir: PathBuf,
    },
    /// Start the HTTP/JSON daemon.
    ServeHttp {
        #[arg(long, default_value = "127.0.0.1:4319")]
        addr: String,
        #[arg(long, default_value = "~/.agent-pty/runs")]
        log_dir: PathBuf,
    },
    /// Serve MCP-compatible terminal tools over stdio JSON-RPC.
    McpStdio {
        #[arg(long, default_value = "~/.agent-pty/runs")]
        log_dir: PathBuf,
    },
    /// Run a self-contained five-minute demo and emit proof artifacts.
    Demo {
        /// Directory where demo runs and artifacts are written.
        #[arg(long, default_value = "~/.agent-pty/demos")]
        root: PathBuf,
        /// Session name to use in the demo artifacts.
        #[arg(long, default_value = "demo")]
        name: String,
        /// Shell used for the demo PTY session.
        #[arg(long, default_value = "/bin/sh")]
        shell: PathBuf,
        /// Print a machine-readable summary.
        #[arg(long)]
        json: bool,
    },
    /// Report whether the Unix-socket daemon is reachable.
    Status {
        /// Print a machine-readable daemon status summary.
        #[arg(long)]
        json: bool,
    },
    /// Ask the Unix-socket daemon to shut down cleanly.
    Stop,
    /// Check local prerequisites, paths, and daemon reachability.
    Doctor {
        /// Session log directory to check for writability.
        #[arg(long, default_value = "~/.agent-pty/runs")]
        log_dir: PathBuf,
        /// Shell path to validate.
        #[arg(long, default_value = "/bin/sh")]
        shell: PathBuf,
        /// Print a machine-readable diagnostics summary.
        #[arg(long)]
        json: bool,
    },
    /// Create a persistent terminal session in a repository.
    New {
        /// Workspace/repository path for the session.
        #[arg(long)]
        repo: PathBuf,
        /// Stable session name.
        #[arg(long)]
        name: String,
        /// Shell to spawn inside the PTY.
        #[arg(long, default_value = "/bin/sh")]
        shell: PathBuf,
        #[arg(long, default_value_t = 24)]
        rows: u16,
        #[arg(long, default_value_t = 80)]
        cols: u16,
    },
    /// Send text or keys to a session.
    Send {
        name: String,
        text: String,
        /// Send bytes without appending Enter.
        #[arg(long)]
        no_enter: bool,
    },
    /// Attach this terminal to a live session over the Unix socket.
    Attach {
        name: String,
        /// Stream output without forwarding stdin into the session.
        #[arg(long)]
        read_only: bool,
        /// Exit automatically after a duration, useful for scripts and smoke tests.
        #[arg(long)]
        timeout: Option<String>,
        /// Initial transcript bytes to replay before live output.
        #[arg(long, default_value_t = 12_000)]
        history_bytes: usize,
    },
    /// Read the current semantic screen snapshot.
    Screen {
        name: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    /// Wait until a predicate is true.
    Wait {
        name: String,
        /// Predicate: literal text, regex:<pattern>, prompt, exit, or idle:<duration>.
        #[arg(long)]
        until: String,
        #[arg(long, default_value = "30s")]
        timeout: String,
    },
    /// Kill a running session.
    Kill { name: String },
    /// Replay append-only evidence events.
    Replay {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// List known sessions.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Write a JSON and Markdown proof bundle for a session.
    Proof {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Fork a session, optionally with a git worktree-backed workspace.
    Fork {
        name: String,
        #[arg(long)]
        new_name: String,
        #[arg(long)]
        copy_worktree: bool,
    },
    /// Print the OTEL-style JSONL trace log path.
    TracePath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum OutputFormat {
    Text,
    Markdown,
    Json,
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    let cli = Cli::parse();
    let socket = expand_tilde(cli.socket);

    match cli.command {
        Command::Serve { log_dir } => {
            let log_dir = expand_tilde(log_dir);
            println!("agent-pty listening on {}", socket.display());
            serve_unix(socket, log_dir)
        }
        Command::ServeHttp { addr, log_dir } => {
            let log_dir = expand_tilde(log_dir);
            println!("agent-pty HTTP listening on {addr}");
            serve_http(addr, log_dir)
        }
        Command::McpStdio { log_dir } => serve_mcp_stdio(expand_tilde(log_dir)),
        Command::Demo {
            root,
            name,
            shell,
            json,
        } => {
            let summary = run_demo(DemoOptions {
                root: expand_tilde(root),
                name,
                shell: expand_tilde(shell),
            })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("agent-pty demo complete");
                println!("workspace: {}", summary.workspace.display());
                println!("report: {}", summary.report_path.display());
                println!("proof: {}", summary.proof_markdown_path.display());
                println!("event log: {}", summary.event_log_path.display());
            }
            Ok(())
        }
        Command::Status { json } => {
            let summary = daemon_status(&socket);
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else if summary.running {
                println!(
                    "agent-pty daemon running: {} session{}",
                    summary.session_count,
                    plural(summary.session_count)
                );
                if let Some(path) = &summary.trace_path {
                    println!("trace log: {}", path.display());
                }
            } else {
                println!(
                    "agent-pty daemon not running: {}",
                    summary.error.as_deref().unwrap_or("unreachable")
                );
            }
            Ok(())
        }
        Command::Stop => {
            let payload = request_unix(socket, &Request::Shutdown)?;
            print_payload(payload, OutputFormat::Text, false)
        }
        Command::Doctor {
            log_dir,
            shell,
            json,
        } => {
            let summary = run_doctor(&socket, expand_tilde(log_dir), expand_tilde(shell));
            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("doctor: {}", if summary.ok { "ok" } else { "failed" });
                println!(
                    "daemon: {}",
                    if summary.daemon_running {
                        "running"
                    } else {
                        "not running"
                    }
                );
                for check in &summary.checks {
                    println!(
                        "- {}: {} ({})",
                        check.name,
                        if check.ok { "ok" } else { "failed" },
                        check.detail
                    );
                }
            }
            if summary.ok {
                Ok(())
            } else {
                anyhow::bail!("doctor found failing checks")
            }
        }
        Command::New {
            repo,
            name,
            shell,
            rows,
            cols,
        } => {
            let payload = request_unix(
                socket,
                &Request::New {
                    id: name,
                    repo: expand_tilde(repo),
                    shell: expand_tilde(shell),
                    rows,
                    cols,
                    env: BTreeMap::new(),
                },
            )?;
            print_payload(payload, OutputFormat::Text, false)
        }
        Command::Send {
            name,
            text,
            no_enter,
        } => {
            let payload = request_unix(
                socket,
                &Request::Send {
                    id: name,
                    text,
                    enter: !no_enter,
                },
            )?;
            print_payload(payload, OutputFormat::Text, false)
        }
        Command::Attach {
            name,
            read_only,
            timeout,
            history_bytes,
        } => {
            let timeout = timeout
                .as_deref()
                .map(parse_duration)
                .transpose()
                .with_context(|| format!("invalid attach timeout {timeout:?}"))?;
            attach_unix(&socket, &name, read_only, timeout, history_bytes)
        }
        Command::Screen { name, format } => {
            let payload = request_unix(socket, &Request::Screen { id: name })?;
            print_payload(payload, format, false)
        }
        Command::Wait {
            name,
            until,
            timeout,
        } => {
            let timeout = parse_duration(&timeout)
                .with_context(|| format!("invalid timeout {timeout:?}"))?
                .as_millis() as u64;
            let payload = request_unix(
                socket,
                &Request::Wait {
                    id: name,
                    until,
                    timeout_ms: timeout,
                },
            )?;
            print_payload(payload, OutputFormat::Text, false)
        }
        Command::Kill { name } => {
            let payload = request_unix(socket, &Request::Kill { id: name })?;
            print_payload(payload, OutputFormat::Text, false)
        }
        Command::Replay { name, json } => {
            let payload = request_unix(socket, &Request::Replay { id: name })?;
            let format = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Text
            };
            print_payload(payload, format, false)
        }
        Command::List { json } => {
            let payload = request_unix(socket, &Request::List)?;
            let format = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Text
            };
            print_payload(payload, format, false)
        }
        Command::Proof { name, json } => {
            let payload = request_unix(socket, &Request::Proof { id: name })?;
            let format = if json {
                OutputFormat::Json
            } else {
                OutputFormat::Text
            };
            print_payload(payload, format, false)
        }
        Command::Fork {
            name,
            new_name,
            copy_worktree,
        } => {
            let payload = request_unix(
                socket,
                &Request::Fork {
                    id: name,
                    name: new_name,
                    copy_worktree,
                },
            )?;
            print_payload(payload, OutputFormat::Text, false)
        }
        Command::TracePath => {
            let payload = request_unix(socket, &Request::TracePath)?;
            print_payload(payload, OutputFormat::Text, false)
        }
    }
}

fn print_payload(payload: ResponsePayload, format: OutputFormat, force_json: bool) -> Result<()> {
    if force_json {
        println!("{}", serde_json::to_string_pretty(&payload)?);
        return Ok(());
    }

    match payload {
        ResponsePayload::SessionCreated { id } => println!("created session {id}"),
        ResponsePayload::Sent => println!("sent"),
        ResponsePayload::Killed => println!("killed"),
        ResponsePayload::Sessions(sessions) => {
            if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&sessions)?);
            } else {
                for session in sessions {
                    println!(
                        "{} active={} pid={} workspace={}",
                        session.id,
                        session.active,
                        session
                            .process_id
                            .map(|pid| pid.to_string())
                            .unwrap_or_else(|| "-".to_string()),
                        session.workspace.display()
                    );
                }
            }
        }
        ResponsePayload::Proof(proof) => {
            if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&proof)?);
            } else {
                println!("{}", proof.markdown_path.display());
            }
        }
        ResponsePayload::Forked(fork) => {
            println!(
                "forked {} from {} at {}",
                fork.id,
                fork.source_id,
                fork.workspace.display()
            );
        }
        ResponsePayload::TracePath(path) => println!("{}", path.display()),
        ResponsePayload::Shutdown => println!("stopped"),
        ResponsePayload::Attached => println!("attached"),
        ResponsePayload::Screen(screen) => match format {
            OutputFormat::Text => print!("{}", screen.text),
            OutputFormat::Markdown => println!("```text\n{}\n```", screen.text.trim_end()),
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&screen)?),
        },
        ResponsePayload::Observation(observation) => match format {
            OutputFormat::Text => print!("{}", observation.screen.text),
            OutputFormat::Markdown => {
                println!("```text\n{}\n```", observation.screen.text.trim_end())
            }
            OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&observation)?),
        },
        ResponsePayload::Replay(events) => {
            if format == OutputFormat::Json {
                println!("{}", serde_json::to_string_pretty(&events)?);
            } else {
                for event in events {
                    println!(
                        "{} {} {}",
                        event.timestamp,
                        event.session_id,
                        event_label(&event.kind)
                    );
                }
            }
        }
    }
    Ok(())
}

fn attach_unix(
    socket: &Path,
    session_id: &str,
    read_only: bool,
    timeout: Option<Duration>,
    history_bytes: usize,
) -> Result<()> {
    let mut stream =
        UnixStream::connect(socket).with_context(|| format!("connect to {}", socket.display()))?;
    serde_json::to_writer(
        &mut stream,
        &Request::Attach {
            id: session_id.to_string(),
            read_only,
            history_bytes,
        },
    )
    .context("serialize attach request")?;
    stream.write_all(b"\n").context("write attach newline")?;
    stream.flush().context("flush attach request")?;

    let response = read_wire_response(&mut stream)?;
    if !response.ok {
        bail!(
            "{}",
            response
                .error
                .unwrap_or_else(|| "attach failed".to_string())
        );
    }
    if !matches!(response.data, Some(ResponsePayload::Attached)) {
        bail!("daemon did not enter attach mode");
    }

    if !read_only {
        let mut input_stream = stream.try_clone().context("clone attach input stream")?;
        thread::spawn(move || {
            let mut stdin = std::io::stdin().lock();
            let _ = std::io::copy(&mut stdin, &mut input_stream);
        });
    }

    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .context("set attach output timeout")?;
    let deadline = timeout.map(|duration| Instant::now() + duration);
    let mut output = std::io::stdout().lock();
    let mut buffer = [0_u8; 8192];
    loop {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            break;
        }
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => {
                output
                    .write_all(&buffer[..count])
                    .context("write attach output")?;
                output.flush().context("flush attach output")?;
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error).context("read attach output"),
        }
    }
    Ok(())
}

fn read_wire_response(stream: &mut UnixStream) -> Result<WireResponse> {
    let mut line = Vec::new();
    let mut byte = [0_u8; 1];
    loop {
        let count = stream.read(&mut byte).context("read attach response")?;
        if count == 0 {
            bail!("daemon closed before attach response");
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            break;
        }
    }
    serde_json::from_slice(&line).context("parse attach response")
}

#[derive(Debug, Clone, Serialize)]
struct StatusSummary {
    socket: PathBuf,
    running: bool,
    session_count: usize,
    trace_path: Option<PathBuf>,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorSummary {
    ok: bool,
    daemon_running: bool,
    socket: PathBuf,
    log_dir: PathBuf,
    checks: Vec<DoctorCheck>,
    status: StatusSummary,
}

#[derive(Debug, Clone, Serialize)]
struct DoctorCheck {
    name: String,
    ok: bool,
    detail: String,
}

fn daemon_status(socket: &Path) -> StatusSummary {
    match request_unix(socket, &Request::List) {
        Ok(ResponsePayload::Sessions(sessions)) => {
            let trace_path = match request_unix(socket, &Request::TracePath) {
                Ok(ResponsePayload::TracePath(path)) => Some(path),
                _ => None,
            };
            StatusSummary {
                socket: socket.to_path_buf(),
                running: true,
                session_count: sessions.len(),
                trace_path,
                error: None,
            }
        }
        Ok(other) => StatusSummary {
            socket: socket.to_path_buf(),
            running: true,
            session_count: 0,
            trace_path: None,
            error: Some(format!("unexpected response: {other:?}")),
        },
        Err(error) => StatusSummary {
            socket: socket.to_path_buf(),
            running: false,
            session_count: 0,
            trace_path: None,
            error: Some(format!("{error:#}")),
        },
    }
}

fn run_doctor(socket: &Path, log_dir: PathBuf, shell: PathBuf) -> DoctorSummary {
    let status = daemon_status(socket);
    let checks = vec![
        check_command("git", &["--version"]),
        check_shell(&shell),
        check_writable_dir("log_dir", &log_dir),
        check_writable_dir(
            "socket_parent",
            socket.parent().unwrap_or_else(|| Path::new(".")),
        ),
    ];
    let ok = checks.iter().all(|check| check.ok);
    DoctorSummary {
        ok,
        daemon_running: status.running,
        socket: socket.to_path_buf(),
        log_dir,
        checks,
        status,
    }
}

fn check_command(name: &str, args: &[&str]) -> DoctorCheck {
    match ProcessCommand::new(name).args(args).output() {
        Ok(output) if output.status.success() => DoctorCheck {
            name: name.to_string(),
            ok: true,
            detail: String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("available")
                .to_string(),
        },
        Ok(output) => DoctorCheck {
            name: name.to_string(),
            ok: false,
            detail: String::from_utf8_lossy(&output.stderr)
                .lines()
                .next()
                .unwrap_or("command failed")
                .to_string(),
        },
        Err(error) => DoctorCheck {
            name: name.to_string(),
            ok: false,
            detail: error.to_string(),
        },
    }
}

fn check_shell(shell: &Path) -> DoctorCheck {
    match ProcessCommand::new(shell).arg("-c").arg("exit 0").output() {
        Ok(output) if output.status.success() => DoctorCheck {
            name: "shell".to_string(),
            ok: true,
            detail: shell.display().to_string(),
        },
        Ok(output) => DoctorCheck {
            name: "shell".to_string(),
            ok: false,
            detail: format!("{} exited with {}", shell.display(), output.status),
        },
        Err(error) => DoctorCheck {
            name: "shell".to_string(),
            ok: false,
            detail: format!("{}: {error}", shell.display()),
        },
    }
}

fn check_writable_dir(name: &str, path: &Path) -> DoctorCheck {
    let result = (|| -> Result<()> {
        fs::create_dir_all(path).with_context(|| format!("create {}", path.display()))?;
        let probe = path.join(format!(".agent-pty-doctor-{}", std::process::id()));
        let mut file =
            fs::File::create(&probe).with_context(|| format!("write {}", probe.display()))?;
        file.write_all(b"ok")
            .with_context(|| format!("write {}", probe.display()))?;
        drop(file);
        fs::remove_file(&probe).with_context(|| format!("remove {}", probe.display()))?;
        Ok(())
    })();

    match result {
        Ok(()) => DoctorCheck {
            name: name.to_string(),
            ok: true,
            detail: path.display().to_string(),
        },
        Err(error) => DoctorCheck {
            name: name.to_string(),
            ok: false,
            detail: format!("{error:#}"),
        },
    }
}

fn expand_tilde(path: PathBuf) -> PathBuf {
    let Some(value) = path.to_str() else {
        return path;
    };
    if value == "~" {
        return home_dir().unwrap_or(path);
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = home_dir() {
            return home.join(rest);
        }
    }
    path
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn event_label(kind: &EventKind) -> String {
    match kind {
        EventKind::Action(action) => match action {
            Action::SendKeys { bytes } => {
                format!("send {} byte{}", bytes.len(), plural(bytes.len()))
            }
            Action::Exec { argv, cwd } => {
                format!("exec {} in {}", argv.join(" "), cwd.display())
            }
            Action::WaitFor { predicate, timeout } => {
                format!("wait {:?} for {:?}", predicate, timeout)
            }
            Action::SnapshotScreen => "snapshot screen".to_string(),
            Action::SnapshotFiles { globs } => format!("snapshot files {}", globs.join(",")),
            Action::Kill { signal } => format!("kill {signal}"),
            Action::AttachHuman => "attach human".to_string(),
            Action::PolicyDenied { command, rule } => {
                format!("policy denied {rule}: {command}")
            }
            Action::Fork {
                source,
                target,
                workspace,
            } => {
                format!("fork {source} -> {target} at {}", workspace.display())
            }
            Action::Proof {
                json_path,
                markdown_path,
            } => {
                format!(
                    "proof json={} markdown={}",
                    json_path.display(),
                    markdown_path.display()
                )
            }
        },
        EventKind::Observation(observation) => {
            let changed = observation.files_changed.len();
            let exit = observation
                .exit_status
                .map(|status| format!(" exit={status}"))
                .unwrap_or_default();
            format!(
                "observe screen={} changed={}{}",
                observation
                    .screen_text
                    .as_ref()
                    .map(|screen| screen.lines().count())
                    .unwrap_or_default(),
                changed,
                exit
            )
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
