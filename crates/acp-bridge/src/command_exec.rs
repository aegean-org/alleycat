use std::collections::HashMap;
use std::ffi::OsString;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use alleycat_bridge_core::{
    ChildProcess, ChildStderr, ChildStdout, ProcessLauncher, ProcessRole, ProcessSpec, StdioMode,
};
use alleycat_codex_proto as p;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::oneshot;
use tokio::time::timeout;

const DEFAULT_OUTPUT_BYTES_CAP: usize = 256 * 1024;
const DEFAULT_TIMEOUT_MS: i64 = 60_000;

static EXEC_REGISTRY: LazyLock<ExecRegistry> = LazyLock::new(ExecRegistry::default);

#[derive(Default)]
struct ExecRegistry {
    inner: Mutex<HashMap<String, ExecHandle>>,
}

struct ExecHandle {
    terminate_tx: oneshot::Sender<()>,
}

impl ExecRegistry {
    fn insert(&self, id: String, handle: ExecHandle) {
        self.inner.lock().unwrap().insert(id, handle);
    }

    fn take(&self, id: &str) -> Option<ExecHandle> {
        self.inner.lock().unwrap().remove(id)
    }
}

pub async fn handle_command_exec(
    launcher: Arc<dyn ProcessLauncher>,
    params: p::CommandExecParams,
) -> Result<p::CommandExecResponse, ExecError> {
    validate_params(&params)?;
    let mut child = launcher
        .launch(process_spec(&params))
        .await
        .map_err(ExecError::spawn)?;
    let cap = output_cap(&params);
    let timeout_dur = timeout_duration(&params);
    let terminate_rx = register_termination(params.process_id.as_ref());
    let stdout_task = tokio::spawn(read_pipe(take_stdout(&mut child)?, cap));
    let stderr_task = tokio::spawn(read_pipe(take_stderr(&mut child)?, cap));
    let exit_status = run_with_supervisor(child, timeout_dur, terminate_rx).await;
    unregister_process(params.process_id.as_ref());
    let stdout_bytes = stdout_task.await.unwrap_or_default();
    let stderr_bytes = stderr_task.await.unwrap_or_default();
    let exit_status = exit_status?;
    Ok(p::CommandExecResponse {
        exit_code: exit_status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout_bytes).into_owned(),
        stderr: String::from_utf8_lossy(&stderr_bytes).into_owned(),
    })
}

pub fn handle_command_exec_terminate(params: p::CommandExecTerminateParams) {
    if let Some(handle) = EXEC_REGISTRY.take(&params.process_id) {
        let _ = handle.terminate_tx.send(());
    }
}

fn validate_params(params: &p::CommandExecParams) -> Result<(), ExecError> {
    if params.command.is_empty() {
        return Err(ExecError::InvalidParams("empty command argv".into()));
    }
    if params.tty {
        return Err(ExecError::Unsupported("tty mode is not supported".into()));
    }
    if params.stream_stdin {
        return Err(ExecError::Unsupported(
            "stream_stdin is not supported".into(),
        ));
    }
    if params.stream_stdout_stderr {
        return Err(ExecError::Unsupported(
            "stream_stdout_stderr is not supported".into(),
        ));
    }
    validate_limits(params)
}

fn validate_limits(params: &p::CommandExecParams) -> Result<(), ExecError> {
    if params.disable_output_cap && params.output_bytes_cap.is_some() {
        return Err(ExecError::InvalidParams(
            "disable_output_cap cannot be combined with output_bytes_cap".into(),
        ));
    }
    if params.disable_timeout && params.timeout_ms.is_some() {
        return Err(ExecError::InvalidParams(
            "disable_timeout cannot be combined with timeout_ms".into(),
        ));
    }
    Ok(())
}

fn process_spec(params: &p::CommandExecParams) -> ProcessSpec {
    let argv = params.command.clone();
    ProcessSpec {
        role: ProcessRole::ToolCommand,
        program: argv[0].clone().into(),
        args: argv[1..].iter().map(OsString::from).collect(),
        cwd: params.cwd.clone(),
        env: env_overrides(params),
        env_clear: false,
        stdin: StdioMode::Null,
        stdout: StdioMode::Piped,
        stderr: StdioMode::Piped,
    }
}

fn env_overrides(params: &p::CommandExecParams) -> Vec<(OsString, OsString)> {
    params
        .env
        .as_ref()
        .into_iter()
        .flat_map(|env| env.iter())
        .filter_map(|(key, value)| value.as_ref().map(|v| (key.into(), v.into())))
        .collect()
}

fn output_cap(params: &p::CommandExecParams) -> usize {
    if params.disable_output_cap {
        usize::MAX
    } else {
        params.output_bytes_cap.unwrap_or(DEFAULT_OUTPUT_BYTES_CAP)
    }
}

fn timeout_duration(params: &p::CommandExecParams) -> Option<Duration> {
    if params.disable_timeout {
        None
    } else {
        let timeout_ms = params.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS).max(0) as u64;
        Some(Duration::from_millis(timeout_ms))
    }
}

fn register_termination(process_id: Option<&String>) -> Option<oneshot::Receiver<()>> {
    process_id.map(|id| {
        let (terminate_tx, terminate_rx) = oneshot::channel::<()>();
        EXEC_REGISTRY.insert(id.clone(), ExecHandle { terminate_tx });
        terminate_rx
    })
}

fn unregister_process(process_id: Option<&String>) {
    if let Some(id) = process_id {
        EXEC_REGISTRY.take(id);
    }
}

fn take_stdout(child: &mut Box<dyn ChildProcess>) -> Result<ChildStdout, ExecError> {
    child
        .take_stdout()
        .ok_or_else(|| ExecError::internal("child has no stdout pipe"))
}

fn take_stderr(child: &mut Box<dyn ChildProcess>) -> Result<ChildStderr, ExecError> {
    child
        .take_stderr()
        .ok_or_else(|| ExecError::internal("child has no stderr pipe"))
}

async fn read_pipe<R>(mut reader: R, cap: usize) -> Vec<u8>
where
    R: AsyncRead + Send + Unpin + 'static,
{
    let mut buf = Vec::new();
    let _ = read_capped(&mut reader, &mut buf, cap).await;
    buf
}

async fn run_with_supervisor(
    mut child: Box<dyn ChildProcess>,
    timeout_dur: Option<Duration>,
    terminate_rx: Option<oneshot::Receiver<()>>,
) -> Result<std::process::ExitStatus, ExecError> {
    match (timeout_dur, terminate_rx) {
        (Some(dur), Some(term)) => run_with_timeout_and_terminate(child, dur, term).await,
        (Some(dur), None) => run_with_timeout(child, dur).await,
        (None, Some(term)) => run_with_terminate(child, term).await,
        (None, None) => child.wait().await.map_err(ExecError::wait),
    }
}

async fn run_with_timeout_and_terminate(
    mut child: Box<dyn ChildProcess>,
    dur: Duration,
    term: oneshot::Receiver<()>,
) -> Result<std::process::ExitStatus, ExecError> {
    tokio::select! {
        res = child.wait() => res.map_err(ExecError::wait),
        _ = term => kill_then_wait(child).await,
        _ = tokio::time::sleep(dur) => timeout_after_kill(child).await,
    }
}

async fn run_with_timeout(
    mut child: Box<dyn ChildProcess>,
    dur: Duration,
) -> Result<std::process::ExitStatus, ExecError> {
    match timeout(dur, child.wait()).await {
        Ok(res) => res.map_err(ExecError::wait),
        Err(_) => timeout_after_kill(child).await,
    }
}

async fn run_with_terminate(
    mut child: Box<dyn ChildProcess>,
    term: oneshot::Receiver<()>,
) -> Result<std::process::ExitStatus, ExecError> {
    tokio::select! {
        res = child.wait() => res.map_err(ExecError::wait),
        _ = term => kill_then_wait(child).await,
    }
}

async fn timeout_after_kill(
    mut child: Box<dyn ChildProcess>,
) -> Result<std::process::ExitStatus, ExecError> {
    let _ = child.kill().await;
    let _ = child.wait().await;
    Err(ExecError::Timeout)
}

async fn kill_then_wait(
    mut child: Box<dyn ChildProcess>,
) -> Result<std::process::ExitStatus, ExecError> {
    let _ = child.kill().await;
    child.wait().await.map_err(ExecError::wait)
}

async fn read_capped<R>(reader: &mut R, dest: &mut Vec<u8>, cap: usize) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
{
    let mut buf = vec![0u8; 8 * 1024];
    while dest.len() < cap {
        let n = reader.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        let remaining = cap - dest.len();
        let take = n.min(remaining);
        dest.extend_from_slice(&buf[..take]);
        if take < n {
            drain_reader(reader, &mut buf).await?;
            break;
        }
    }
    Ok(())
}

async fn drain_reader<R>(reader: &mut R, buf: &mut [u8]) -> std::io::Result<()>
where
    R: AsyncReadExt + Unpin,
{
    loop {
        if reader.read(buf).await? == 0 {
            return Ok(());
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExecError {
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("command timed out")]
    Timeout,
    #[error("spawn failed: {0}")]
    Spawn(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl ExecError {
    fn spawn(err: std::io::Error) -> Self {
        Self::Spawn(err.to_string())
    }

    fn wait(err: std::io::Error) -> Self {
        Self::Internal(format!("waiting on child: {err}"))
    }

    fn internal<E: std::fmt::Display>(err: E) -> Self {
        Self::Internal(err.to_string())
    }

    pub fn rpc_code(&self) -> i64 {
        match self {
            ExecError::InvalidParams(_) => p::error_codes::INVALID_PARAMS,
            ExecError::Unsupported(_) => p::error_codes::METHOD_NOT_FOUND,
            ExecError::Timeout | ExecError::Spawn(_) | ExecError::Internal(_) => {
                p::error_codes::INTERNAL_ERROR
            }
        }
    }
}

#[cfg(test)]
#[path = "command_exec_tests.rs"]
mod tests;
