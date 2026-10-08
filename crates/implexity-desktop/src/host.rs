// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const APP_NAME: &str = "Implexity";

pub(crate) fn install_root() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(p).ok())
        .and_then(|p| p.parent().map(Path::to_path_buf))
        .unwrap_or_else(|| PathBuf::from("."))
}



pub(crate) fn user_root() -> std::io::Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()).map_or_else(
        || {
            let home = std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map_or_else(|| PathBuf::from("."), PathBuf::from);
            home.join("AppData").join("Local")
        },
        PathBuf::from,
    );
    let root = base.join(APP_NAME).join("Workbench");
    for name in ["logs", "state", "recovery", "projects"] {
        std::fs::create_dir_all(root.join(name))?;
    }
    Ok(root)
}

fn civil(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;

    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let small = |v: i64| u32::try_from(v).unwrap_or(0);
    let r = |v: u64| u32::try_from(v).unwrap_or(0);
    (year, small(month), small(day), r(rem / 3600), r(rem / 60 % 60), r(rem % 60))
}

fn asctime(now: SystemTime) -> String {
    let d = now.duration_since(UNIX_EPOCH).unwrap_or_default();
    let (y, mo, da, h, mi, s) = civil(d.as_secs());
    format!("{y:04}-{mo:02}-{da:02} {h:02}:{mi:02}:{s:02},{:03}", d.subsec_millis())
}

#[derive(Debug)]
pub(crate) struct Log {
    path: PathBuf,
    file: Mutex<Option<File>>,
}

impl Log {
    const MAX_BYTES: u64 = 4_000_000;
    const BACKUPS: u32 = 5;

    pub(crate) fn open(root: &Path) -> Self {
        let path = root.join("logs").join("workbench.log");
        let file = OpenOptions::new().create(true).append(true).open(&path).ok();
        Self { path, file: Mutex::new(file) }
    }

    fn rollover(&self, file: &mut Option<File>) {
        *file = None;
        let name = |i: u32| {
            let mut p = self.path.clone().into_os_string();
            p.push(format!(".{i}"));
            PathBuf::from(p)
        };
        for i in (1..Self::BACKUPS).rev() {
            if name(i).exists() {
                let _ = std::fs::remove_file(name(i + 1));
                let _ = std::fs::rename(name(i), name(i + 1));
            }
        }
        let _ = std::fs::remove_file(name(1));
        let _ = std::fs::rename(&self.path, name(1));
        *file = OpenOptions::new().create(true).append(true).open(&self.path).ok();
    }

    fn emit(&self, level: &str, message: &str) {
        let line = format!("{} {level} {message}\n", asctime(SystemTime::now()));
        let Ok(mut guard) = self.file.lock() else { return };
        let size = guard.as_ref().and_then(|f| f.metadata().ok()).map_or(0, |m| m.len());
        if size > 0 && size + line.len() as u64 >= Self::MAX_BYTES {
            self.rollover(&mut guard);
        }
        if let Some(f) = guard.as_mut() {
            let _ = f.write_all(line.as_bytes());
            let _ = f.flush();
        }
    }

    pub(crate) fn info(&self, message: &str) {
        self.emit("INFO", message);
    }

    pub(crate) fn error(&self, message: &str, cause: &str) {
        self.emit("ERROR", &format!("{message}\n{cause}"));
    }
}

#[derive(Debug)]
pub(crate) struct SingleInstance {
    lock: Option<File>,
    endpoint: PathBuf,
}

impl SingleInstance {


    pub(crate) fn acquire(root: &Path) -> std::io::Result<(Self, bool)> {
        let state = root.join("state");
        std::fs::create_dir_all(&state)?;
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(state.join("workbench.lock"))?;
        let endpoint = state.join("workbench.activate");
        match file.try_lock() {
            Ok(()) => Ok((Self { lock: Some(file), endpoint }, true)),
            Err(std::fs::TryLockError::WouldBlock) => Ok((Self { lock: None, endpoint }, false)),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    pub(crate) fn serve_activation(&self, activate: impl Fn() + Send + 'static) {
        if self.lock.is_none() {
            return;
        }
        let Ok(listener) = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))) else { return };
        let Ok(port) = listener.local_addr().map(|a| a.port()) else { return };
        if std::fs::write(&self.endpoint, port.to_string()).is_err() {
            return;
        }
        let _ = std::thread::Builder::new().name("workbench-activation".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = [0u8; 16];
                let n = stream.read(&mut buf).unwrap_or(0);
                if &buf[..n] == b"activate\n" {
                    activate();
                }
            }
        });
    }

    pub(crate) fn activate_existing(&self) -> bool {
        let Some(port) =
            std::fs::read_to_string(&self.endpoint).ok().and_then(|p| p.trim().parse::<u16>().ok())
        else {
            return false;
        };
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
        TcpStream::connect_timeout(&addr, Duration::from_secs(2))
            .and_then(|mut s| s.write_all(b"activate\n"))
            .is_ok()
    }

    pub(crate) fn close(&mut self) {
        if let Some(lock) = self.lock.take() {
            let _ = std::fs::remove_file(&self.endpoint);
            let _ = lock.unlock();
        }
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        self.close();
    }
}

