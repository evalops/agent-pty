use std::{
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::PathBuf,
    sync::Arc,
    thread,
};

use anyhow::{Context, Result, bail};

use crate::{
    daemon::{Request, WireResponse, handle_request},
    session::SessionManager,
};

pub fn handle_http_request(manager: &SessionManager, raw_request: &str) -> Result<String> {
    let (head, body) = raw_request
        .split_once("\r\n\r\n")
        .context("HTTP request missing header/body separator")?;
    let request_line = head
        .lines()
        .next()
        .context("HTTP request missing request line")?;
    if !request_line.starts_with("POST /request ") {
        return Ok(http_response(
            404,
            WireResponse::error("only POST /request is supported"),
        )?);
    }

    let request = match serde_json::from_str::<Request>(body.trim()) {
        Ok(request) => request,
        Err(error) => {
            return Ok(http_response(
                400,
                WireResponse::error(format!("invalid JSON request: {error}")),
            )?);
        }
    };

    let response = match handle_request(manager, request) {
        Ok(payload) => http_response(200, WireResponse::ok(payload))?,
        Err(error) => http_response(409, WireResponse::error(format!("{error:#}")))?,
    };
    Ok(response)
}

pub fn serve_http<A: ToSocketAddrs>(addr: A, log_dir: impl Into<PathBuf>) -> Result<()> {
    let listener = TcpListener::bind(addr).context("bind HTTP listener")?;
    serve_http_listener(listener, log_dir)
}

pub fn serve_http_listener(listener: TcpListener, log_dir: impl Into<PathBuf>) -> Result<()> {
    let manager = Arc::new(SessionManager::new(log_dir)?);
    for stream in listener.incoming() {
        let mut stream = stream.context("accept HTTP connection")?;
        let manager = Arc::clone(&manager);
        thread::spawn(move || {
            let _ = handle_stream(&manager, &mut stream);
        });
    }
    Ok(())
}

fn handle_stream(manager: &SessionManager, stream: &mut TcpStream) -> Result<()> {
    let mut reader = BufReader::new(stream.try_clone().context("clone HTTP stream")?);
    let mut request = String::new();
    let mut content_length = 0_usize;

    loop {
        let mut line = String::new();
        let bytes = reader.read_line(&mut line).context("read HTTP header")?;
        if bytes == 0 {
            break;
        }
        let lower = line.to_ascii_lowercase();
        if let Some(value) = lower.strip_prefix("content-length:") {
            content_length = value.trim().parse().context("parse content-length")?;
        }
        request.push_str(&line);
        if line == "\r\n" {
            break;
        }
    }

    let mut body = vec![0_u8; content_length];
    reader.read_exact(&mut body).context("read HTTP body")?;
    request.push_str(&String::from_utf8_lossy(&body));

    let response = handle_http_request(manager, &request)?;
    stream
        .write_all(response.as_bytes())
        .context("write HTTP response")?;
    Ok(())
}

fn http_response(status: u16, response: WireResponse) -> Result<String> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        409 => "Conflict",
        _ => bail!("unsupported HTTP status {status}"),
    };
    let body = serde_json::to_string(&response)?;
    Ok(format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        body.len(),
        body
    ))
}
