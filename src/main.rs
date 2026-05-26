use std::{collections::BTreeMap, path::PathBuf, process::ExitCode};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};

use agent_pty::{
    daemon::{Request, ResponsePayload, parse_duration, request_unix, serve_unix},
    evidence::{Action, EventKind},
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
    Serve {
        #[arg(long, default_value = "~/.agent-pty/runs")]
        log_dir: PathBuf,
    },
    New {
        #[arg(long)]
        repo: PathBuf,
        #[arg(long)]
        name: String,
        #[arg(long, default_value = "/bin/sh")]
        shell: PathBuf,
        #[arg(long, default_value_t = 24)]
        rows: u16,
        #[arg(long, default_value_t = 80)]
        cols: u16,
    },
    Send {
        name: String,
        text: String,
        #[arg(long)]
        no_enter: bool,
    },
    Screen {
        name: String,
        #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
        format: OutputFormat,
    },
    Wait {
        name: String,
        #[arg(long)]
        until: String,
        #[arg(long, default_value = "30s")]
        timeout: String,
    },
    Kill {
        name: String,
    },
    Replay {
        name: String,
        #[arg(long)]
        json: bool,
    },
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
