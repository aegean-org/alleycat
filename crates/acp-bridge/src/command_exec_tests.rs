use std::sync::Arc;

use alleycat_bridge_core::LocalLauncher;
use alleycat_codex_proto as p;

use super::*;

#[tokio::test]
async fn buffered_command_exec_returns_stdout_and_exit_code() {
    let response = handle_command_exec(Arc::new(LocalLauncher), echo_params("acp-ok"))
        .await
        .expect("command exec");

    assert_eq!(response.exit_code, 0);
    assert!(response.stdout.contains("acp-ok"));
}

#[tokio::test]
async fn command_exec_rejects_tty_mode() {
    let mut params = echo_params("unused");
    params.tty = true;

    let err = handle_command_exec(Arc::new(LocalLauncher), params)
        .await
        .unwrap_err();

    assert_eq!(err.rpc_code(), p::error_codes::METHOD_NOT_FOUND);
    assert!(err.to_string().contains("tty mode is not supported"));
}

fn echo_params(text: &str) -> p::CommandExecParams {
    let command = if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "echo".to_string(),
            text.to_string(),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            format!("printf %s {text}"),
        ]
    };
    p::CommandExecParams {
        command,
        tty: false,
        stream_stdin: false,
        stream_stdout_stderr: false,
        ..Default::default()
    }
}
