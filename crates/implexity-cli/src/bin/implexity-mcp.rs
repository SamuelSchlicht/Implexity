// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



#![forbid(unsafe_code)]

use std::process::ExitCode;

fn preserve_on_sigint() -> std::io::Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<std::io::Result<()>>();
    std::thread::Builder::new().name("implexity-mcp-sigint".into()).spawn(move || {
        runtime.block_on(async move {
            match tokio::signal::ctrl_c().await {
                Ok(()) => {}
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            }
            loop {
                if tokio::signal::ctrl_c().await.is_err() {
                    return;
                }
            }
        });
    })?;

    std::thread::sleep(std::time::Duration::from_millis(20));
    match ready_rx.try_recv() {
        Ok(Err(e)) => Err(e),
        _ => Ok(()),
    }
}

fn main() -> ExitCode {
    if std::env::var("IMPLEXITY_MCP_PRESERVE_ON_SIGINT").as_deref() == Ok("1")
        && let Err(e) = preserve_on_sigint()
    {
        eprintln!("Implexity MCP configuration error: cannot preserve on SIGINT: {e}");
        return ExitCode::from(2);
    }
    let mut client = match implexity_mcp::client_from_environment() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("Implexity MCP configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    let stdin = std::io::stdin().lock();
    let stdout = std::io::stdout().lock();
    match implexity_mcp::serve(&mut client, stdin, stdout) {
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::from(1),
    }
}
