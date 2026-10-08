// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END


use std::io::Read as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const PROVIDER_JOB: &str = "provider-job";
pub const PROVIDER: &str = "provider";
pub const MANAGED_PROVIDER: &str = "managed-provider";
pub const ACCELERATED_MANAGED_PROVIDER: &str = "accelerated-managed-provider";
pub const QUALIFICATION_MANAGED_PROVIDER: &str = "qualification-managed-provider";
pub const ACCELERATED_PROVIDER_JOB: &str = "accelerated-provider-job";
pub const OPTIMIZE: &str = "optimize";

pub const KINDS: [&str; 7] = [
    PROVIDER_JOB,
    PROVIDER,
    MANAGED_PROVIDER,
    ACCELERATED_MANAGED_PROVIDER,
    QUALIFICATION_MANAGED_PROVIDER,
    ACCELERATED_PROVIDER_JOB,
    OPTIMIZE,
];

#[must_use]
pub fn main(args: &[String]) -> i32 {
    let Some((kind, rest)) = args.split_first() else {
        let mut err = std::io::stderr().lock();
        let _ = std::io::Write::write_fmt(
            &mut err,
            format_args!("usage: implexity worker {{{}}} ...\n", KINDS.join(",")),
        );
        return 2;
    };
    match kind.as_str() {
        PROVIDER_JOB => crate::provider_job::main(rest),
        PROVIDER => crate::provider_worker::main(rest),
        MANAGED_PROVIDER => crate::managed_workers::managed_provider_worker(rest),
        ACCELERATED_MANAGED_PROVIDER => crate::managed_workers::accelerated_managed_provider_worker(rest),
        QUALIFICATION_MANAGED_PROVIDER => crate::managed_workers::qualification_managed_provider_worker(rest),
        ACCELERATED_PROVIDER_JOB => crate::managed_workers::accelerated_provider_job(rest),
        OPTIMIZE => crate::optimize::main(rest),
        other => {
            let mut err = std::io::stderr().lock();
            let _ = std::io::Write::write_fmt(
                &mut err,
                format_args!("implexity worker: unknown worker {other:?}; one of {}\n", KINDS.join(", ")),
            );
            2
        }
    }
}

#[must_use]
pub fn duration_from_secs(seconds: f64) -> Duration {
    if seconds.is_nan() || seconds <= 0.0 {
        return Duration::ZERO;
    }
    Duration::try_from_secs_f64(seconds).unwrap_or(Duration::MAX)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildOutput {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}


pub fn run_captured(
    command: &[String],
    cwd: &Path,
    env: &std::collections::BTreeMap<String, String>,
    timeout: Duration,
) -> std::io::Result<Option<ChildOutput>> {
    let (program, args) =
        command.split_first().ok_or_else(|| std::io::Error::other("empty worker command"))?;
    let mut child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().ok_or_else(|| std::io::Error::other("worker stdout unavailable"))?;
    let mut stderr = child.stderr.take().ok_or_else(|| std::io::Error::other("worker stderr unavailable"))?;
    let (out_tx, out_rx) = std::sync::mpsc::channel();
    let (err_tx, err_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        let _ = out_tx.send(buf);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        let _ = err_tx.send(buf);
    });
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let Some(status) = status else {

        return Ok(None);
    };

    let stdout = out_rx.recv().unwrap_or_default();
    let stderr = err_rx.recv().unwrap_or_default();
    Ok(Some(status).map(|s| ChildOutput {
        returncode: s.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    }))
}

