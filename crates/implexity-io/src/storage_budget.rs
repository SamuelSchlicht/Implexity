// SPDX-License-Identifier: Apache-2.0
// METAPLEXIS-DISCLAIMER-BEGIN sha256=c7c02258f245d1dcd6db1e8066ef4f6bc1bf36430d91bf9aab026671723c058f
// Open-access statement and disclaimer: see DISCLAIMER.md.
// METAPLEXIS-DISCLAIMER-END



use std::path::Path;

#[must_use]
pub fn memory_capacity_bytes() -> Option<u64> {
    let observation = implexity_core::runtime_environment::memory_observation(Path::new("/proc"));
    if let Some(bytes) = observation["effective_capacity_bytes"].as_u64().filter(|v| *v > 0) {
        return Some(bytes);
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("/usr/sbin/sysctl").args(["-n", "hw.memsize"]).output().ok()?;
        if output.status.success() { return String::from_utf8(output.stdout).ok()?.trim().parse().ok(); }
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
            .output().ok()?;
        if output.status.success() { return String::from_utf8(output.stdout).ok()?.trim().parse().ok(); }
    }
    None
}

#[must_use]
pub fn available_disk_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    #[cfg(unix)]
    {
        let status = rustix::fs::statvfs(existing).ok()?;
        return status.f_bavail.checked_mul(status.f_frsize);
    }
    #[cfg(windows)]
    {
        let drive = existing.components().next()?.as_os_str().to_str()?;
        if drive.len() != 2 || !drive.as_bytes()[0].is_ascii_alphabetic() || !drive.ends_with(':') { return None; }
        let expression = format!("(Get-CimInstance Win32_LogicalDisk -Filter \"DeviceID='{drive}'\").FreeSpace");
        let output = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &expression]).output().ok()?;
        if output.status.success() { return String::from_utf8(output.stdout).ok()?.trim().parse().ok(); }
    }
    #[allow(unreachable_code)]
    None
}

#[must_use]
pub fn default_memory_bytes() -> u64 {
    memory_capacity_bytes().map_or(1 << 30, |bytes| (bytes / 8).max(8))
}

#[must_use]
pub fn default_disk_bytes(root: &Path) -> u64 {
    available_disk_bytes(root).map_or(0, |bytes| bytes / 4)
}
