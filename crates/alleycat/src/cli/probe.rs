//! `alleycat probe` — local debug client that connects to the daemon over iroh
//! exactly the way the phone does, runs the JSON-RPC initialize handshake
//! against an agent, and invokes a method (default `thread/list`).
//!
//! Two modes:
//! - No `--agent`: round-trip a `list_agents` over the alleycat protocol and
//!   print the agent table.
//! - With `--agent <name>`: open a `connect`-style stream, send `initialize`
//!   + `initialized` + the user-supplied method, and dump every JSON-RPC frame
//!   in/out.
//!
//! Identity: reads the daemon's local `host.toml` + `host.key` so the probe
//! authenticates with the same node id and token a phone holding the QR
//! payload would. Generates a fresh client iroh identity each run.

use std::time::Duration;

use anyhow::{Context, anyhow};
use clap::Args;
use iroh::endpoint::presets;
use iroh::{Endpoint, EndpointAddr, PublicKey, SecretKey};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::cli;
use crate::daemon::control::Request as ControlRequest;
use crate::framing::{read_json_frame, write_json_frame};
use crate::host;
use crate::protocol::{ALLEYCAT_ALPN, PROTOCOL_VERSION, PairPayload, Request, Response, Resume};

#[derive(Args, Debug)]
pub struct ProbeArgs {
    /// Agent to connect to (`pi`, `opencode`, `codex`). Omit to round-trip a
    /// `list_agents` call instead.
    #[arg(long)]
    pub agent: Option<String>,
    /// JSON-RPC method to invoke after `initialize` succeeds. Ignored when
    /// `--agent` is omitted. Defaults to `thread/list`.
    #[arg(long)]
    pub method: Option<String>,
    /// JSON params for the method. Defaults to `{}`.
    #[arg(long, default_value = "{}")]
    pub params: String,
    /// Override the node id to dial. Defaults to the local daemon's node id
    /// (read from `host.key`). Useful for probing a remote alleycat.
    #[arg(long)]
    pub node_id: Option<String>,
    /// Override the auth token. Defaults to the local daemon's token (read
    /// from `host.toml`). Pair this with `--node-id` to probe a remote.
    #[arg(long)]
    pub token: Option<String>,
    /// Override the relay URL. By default local probes use the daemon's live
    /// pair payload relay, matching the QR path used by mobile clients.
    #[arg(long)]
    pub relay: Option<String>,
    /// How long to wait for additional JSON-RPC frames after the method
    /// response before exiting, in seconds. Streaming methods may push
    /// notifications; raise this to capture them.
    #[arg(long, default_value_t = 5)]
    pub linger_secs: u64,
    /// Timeout for the JSON-RPC method response, in seconds.
    #[arg(long, default_value_t = 30)]
    pub timeout_secs: u64,
    /// Send an explicit alleycat resume cursor on connect. Useful for
    /// debugging reconnect/replay behavior; clients normally use the highest
    /// `_alleycat_seq` they observed before reconnecting.
    #[arg(long)]
    pub resume_from: Option<u64>,
    /// After the first probe finishes, open a second connect stream on the
    /// same iroh connection with this resume cursor. This simulates a client
    /// reconnect from the same endpoint identity, so the host can attach the
    /// existing session and exercise replay/drift paths.
    #[arg(long)]
    pub repeat_resume_from: Option<u64>,
}

pub async fn run(args: ProbeArgs) -> anyhow::Result<()> {
    if args.node_id.is_none() {
        cli::ensure_current_daemon().await?;
    }

    let cfg = crate::config::load_or_init().await?;
    let server_secret = crate::state::load_or_create_secret_key().await?;
    let local_payload = load_local_pair_payload(&server_secret, &cfg, args.node_id.is_none()).await;

    let token = match &args.token {
        Some(t) => t.clone(),
        None => local_payload.token.clone(),
    };
    let node_id: PublicKey = match &args.node_id {
        Some(s) => s
            .parse()
            .with_context(|| format!("parsing --node-id {s:?} as iroh public key"))?,
        None => local_payload
            .node_id
            .parse()
            .with_context(|| format!("parsing pair payload node_id {:?}", local_payload.node_id))?,
    };
    let relay = match (&args.relay, &args.node_id) {
        (Some(relay), _) => Some(relay.clone()),
        (None, None) => local_payload.relay.clone(),
        (None, Some(_)) => None,
    };

    eprintln!(
        "probe: dialing node_id={} token={} relay={}",
        node_id,
        short_token(&token),
        relay.as_deref().unwrap_or("<iroh default>")
    );

    let endpoint = build_client_endpoint().await?;
    let result = probe_with_endpoint(&endpoint, node_id, relay.as_deref(), &token, &args).await;
    endpoint.close().await;
    result
}

async fn load_local_pair_payload(
    server_secret: &SecretKey,
    cfg: &crate::config::HostConfig,
    prefer_daemon: bool,
) -> PairPayload {
    if prefer_daemon
        && let Ok(resp) = cli::send(ControlRequest::Pair).await
        && let Ok(payload) = cli::decode_data::<PairPayload>(resp)
    {
        return payload;
    }

    host::pair_payload(server_secret, cfg, None)
}

async fn probe_with_endpoint(
    endpoint: &Endpoint,
    node_id: PublicKey,
    relay: Option<&str>,
    token: &str,
    args: &ProbeArgs,
) -> anyhow::Result<()> {
    let _ = tokio::time::timeout(Duration::from_secs(8), endpoint.online()).await;

    let addr = endpoint_addr(node_id, relay)?;
    let conn = endpoint
        .connect(addr, ALLEYCAT_ALPN)
        .await
        .with_context(|| format!("dialing alleycat node {node_id}"))?;
    eprintln!("probe: iroh connection established");

    let result = match args.agent.as_deref() {
        None => list_agents(&conn, token).await,
        Some(agent) => {
            probe_agent(&conn, token, agent, args, args.resume_from).await?;
            if let Some(resume_from) = args.repeat_resume_from {
                eprintln!("probe: opening second connect stream with resume_from={resume_from}");
                probe_agent(&conn, token, agent, args, Some(resume_from)).await?;
            }
            Ok(())
        }
    };
    conn.close(iroh::endpoint::VarInt::from_u32(0), b"probe complete");
    result
}

fn endpoint_addr(node_id: PublicKey, relay: Option<&str>) -> anyhow::Result<EndpointAddr> {
    let mut addr = EndpointAddr::new(node_id);
    if let Some(relay) = relay {
        let relay_url = relay
            .parse()
            .with_context(|| format!("parsing relay URL {relay:?}"))?;
        addr = addr.with_relay_url(relay_url);
    }
    Ok(addr)
}

async fn list_agents(conn: &iroh::endpoint::Connection, token: &str) -> anyhow::Result<()> {
    let (mut send, mut recv) = conn.open_bi().await.context("opening list_agents stream")?;
    write_json_frame(
        &mut send,
        &Request::ListAgents {
            v: PROTOCOL_VERSION,
            token: token.to_string(),
        },
    )
    .await?;
    send.finish().ok();
    let resp: Response = read_json_frame(&mut recv).await?;
    println!("{}", serde_json::to_string_pretty(&resp)?);
    Ok(())
}

async fn probe_agent(
    conn: &iroh::endpoint::Connection,
    token: &str,
    agent: &str,
    args: &ProbeArgs,
    resume_from: Option<u64>,
) -> anyhow::Result<()> {
    let method = args
        .method
        .clone()
        .unwrap_or_else(|| "thread/list".to_string());
    let params: Value = serde_json::from_str(&args.params)
        .with_context(|| format!("parsing --params {:?} as JSON", args.params))?;

    let (mut send, recv) = conn
        .open_bi()
        .await
        .with_context(|| format!("opening connect stream for agent `{agent}`"))?;
    write_json_frame(
        &mut send,
        &Request::Connect {
            v: PROTOCOL_VERSION,
            token: token.to_string(),
            agent: agent.to_string(),
            resume: resume_from.map(|last_seq| Resume { last_seq }),
        },
    )
    .await?;

    // First read the length-prefixed connect ack on the same recv handle,
    // then keep recv (BufReader-wrapped) for the JSONL phase.
    let mut recv = recv;
    let resp: Response = read_json_frame(&mut recv).await?;
    if !resp.ok {
        anyhow::bail!(
            "connect rejected: {}",
            resp.error.unwrap_or_else(|| "<no error>".to_string())
        );
    }
    if let Some(session) = resp.session.as_ref() {
        eprintln!(
            "probe: connect ok agent={agent} attached={:?} current_seq={} floor_seq={} resume_from={:?}; switching to JSONL",
            session.attached, session.current_seq, session.floor_seq, resume_from
        );
    } else {
        eprintln!(
            "probe: connect ok agent={agent} resume_from={:?}; switching to JSONL",
            resume_from
        );
    }

    let mut reader = BufReader::new(recv);

    // initialize
    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "clientInfo": {
                "name": "alleycat-probe",
                "version": env!("CARGO_PKG_VERSION"),
                "title": "alleycat-probe"
            },
            "capabilities": {}
        }
    });
    print_outbound(&init);
    write_jsonl(&mut send, &init).await?;

    // Read until we see a response with id=1.
    loop {
        let frame = read_jsonl_with_timeout(&mut reader, Duration::from_secs(args.timeout_secs))
            .await
            .context("reading initialize response")?;
        print_inbound(&frame);
        if frame.get("id").is_some() {
            break;
        }
    }

    // initialized notification
    let initialized = json!({
        "jsonrpc": "2.0",
        "method": "initialized",
        "params": {}
    });
    print_outbound(&initialized);
    write_jsonl(&mut send, &initialized).await?;

    // user method
    let method_req = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": method,
        "params": params,
    });
    print_outbound(&method_req);
    write_jsonl(&mut send, &method_req).await?;

    // Drain frames until we see id=2 response, then linger for late
    // notifications.
    let mut got_response = false;
    let response_deadline = tokio::time::Instant::now() + Duration::from_secs(args.timeout_secs);
    while !got_response && tokio::time::Instant::now() < response_deadline {
        match read_jsonl_with_timeout(
            &mut reader,
            response_deadline.saturating_duration_since(tokio::time::Instant::now()),
        )
        .await
        {
            Ok(frame) => {
                print_inbound(&frame);
                if frame.get("id") == Some(&json!(2)) {
                    got_response = true;
                }
            }
            Err(error) => {
                eprintln!("probe: read error: {error:#}");
                break;
            }
        }
    }
    if !got_response {
        eprintln!(
            "probe: did not receive response to id=2 ({method}) within {}s",
            args.timeout_secs
        );
    }

    // Linger window — capture any trailing notifications the bridge pushes.
    if args.linger_secs > 0 {
        eprintln!("probe: lingering {}s for trailing frames", args.linger_secs);
        let linger_deadline = tokio::time::Instant::now() + Duration::from_secs(args.linger_secs);
        while tokio::time::Instant::now() < linger_deadline {
            match read_jsonl_with_timeout(
                &mut reader,
                linger_deadline.saturating_duration_since(tokio::time::Instant::now()),
            )
            .await
            {
                Ok(frame) => print_inbound(&frame),
                Err(_) => break,
            }
        }
    }

    let _ = send.finish();
    Ok(())
}

async fn write_jsonl(stream: &mut iroh::endpoint::SendStream, value: &Value) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    stream.write_all(&bytes).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_jsonl_with_timeout<R>(
    reader: &mut BufReader<R>,
    timeout: Duration,
) -> anyhow::Result<Value>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut line = String::new();
    let n = tokio::time::timeout(timeout, reader.read_line(&mut line))
        .await
        .map_err(|_| anyhow!("timed out waiting for JSON line"))??;
    if n == 0 {
        return Err(anyhow!("stream closed by peer"));
    }
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("empty JSON line"));
    }
    serde_json::from_str(trimmed).with_context(|| format!("decoding JSON-RPC line: {trimmed}"))
}

fn print_outbound(value: &Value) {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    eprintln!("→ {pretty}");
}

fn print_inbound(value: &Value) {
    let pretty = serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string());
    println!("← {pretty}");
}

async fn build_client_endpoint() -> anyhow::Result<Endpoint> {
    let secret = SecretKey::generate();
    Endpoint::builder(presets::N0)
        .secret_key(secret)
        .alpns(vec![ALLEYCAT_ALPN.to_vec()])
        .bind()
        .await
        .context("binding probe client endpoint")
}

fn short_token(token: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(&Sha256::digest(token.as_bytes())[..4])
}
