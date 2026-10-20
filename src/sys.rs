//! Process, cache-file and time helpers shared by the stages.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A finished command: its exit code (None when a signal ended it) and stdout.
pub struct Output {
    pub code: Option<i32>,
    pub stdout: String,
}

/// Runs `cmd` with stdin closed and stderr dropped.
///
/// Returns None when the command cannot start or outlives `timeout` seconds; it is then killed.
pub fn run(cmd: &[&str], timeout: f64) -> Option<Output> {
    let mut child = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    // A full pipe would block the child, so stdout drains while we wait.
    let mut pipe = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = pipe.read_to_end(&mut out);
        out
    });
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().unwrap_or_default();
                return Some(Output {
                    code: status.code(),
                    stdout: String::from_utf8_lossy(&out).into_owned(),
                });
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// stdout of a command that exited with 0.
pub fn stdout_of(cmd: &[&str], timeout: f64) -> Option<String> {
    run(cmd, timeout)
        .filter(|o| o.code == Some(0))
        .map(|o| o.stdout)
}

pub fn home() -> PathBuf {
    PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
}

/// ~/.cache/pr-map, shared with the full map.
pub fn cache_dir() -> PathBuf {
    crate::config::get().project().cache.clone()
}

/// Writes a cache file atomically, so a concurrent reader never sees half of it.
pub fn write_cache(name: &str, data: &[u8]) {
    write_cache_in(&cache_dir(), name, data);
}

/// write_cache into another project's cache directory.
pub fn write_cache_in(dir: &Path, name: &str, data: &[u8]) {
    let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
    let _ = std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&tmp, data))
        .and_then(|_| std::fs::rename(&tmp, dir.join(name)));
}

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// A file's modification time in seconds since the epoch.
pub fn mtime(path: &Path) -> Option<f64> {
    let d = std::fs::metadata(path)
        .ok()?
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?;
    Some(d.as_secs() as f64 + d.subsec_nanos() as f64 * 1e-9)
}

/// Seconds since the epoch of an ISO 8601 time such as '2026-10-18T13:37:56Z' or
/// '2026-10-18T13:37:56.267+00:00'. Digits past microseconds are dropped, and a time without
/// an offset is taken as UTC.
pub fn epoch(stamp: &str) -> Option<f64> {
    let b = stamp.as_bytes();
    if b.len() < 19
        || b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let num = |from: usize, to: usize| -> Option<i64> {
        let s = stamp.get(from..to)?;
        s.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| s.parse().ok())?
    };
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, s) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    let mut rest = &stamp[19..];
    let mut micros = 0;
    if let Some(frac) = rest.strip_prefix('.') {
        let digits = frac.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        let six: String = frac[..digits]
            .chars()
            .chain(std::iter::repeat('0'))
            .take(6)
            .collect();
        micros = six.parse::<i64>().ok()?;
        rest = &frac[digits..];
    }
    let offset = match rest {
        "" | "Z" => 0,
        _ => {
            let sign = match rest.as_bytes()[0] {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let hm = rest[1..].replace(':', "");
            if hm.len() != 4 || !hm.bytes().all(|c| c.is_ascii_digit()) {
                return None;
            }
            sign * (hm[..2].parse::<i64>().ok()? * 3600 + hm[2..].parse::<i64>().ok()? * 60)
        }
    };
    let seconds = days_from_civil(y, mo, d) * 86400 + h * 3600 + mi * 60 + s - offset;
    Some((seconds * 1_000_000 + micros) as f64 / 1e6)
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * ((m + 9) % 12) + 2) / 5 + d - 1;
    era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468
}

/// Herdr exposes its own executable to providers, including separately installed forks.
pub fn herdr_bin() -> String {
    std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".into())
}

#[cfg(test)]
mod tests {
    use super::epoch;

    #[test]
    fn epoch_reads_utc_offsets_and_fractions() {
        assert_eq!(epoch("1970-01-01T00:00:00Z"), Some(0.0));
        assert_eq!(epoch("2026-10-18T13:37:56Z"), Some(1792330676.0));
        assert_eq!(epoch("2026-10-18T13:37:56.267Z"), Some(1792330676.267));
        assert_eq!(epoch("2026-10-18T15:37:56.267+02:00"), Some(1792330676.267));
        assert_eq!(
            epoch("2026-10-18T13:37:56.2671239Z"),
            Some(1792330676.267123)
        );
        assert_eq!(epoch("yesterday"), None);
    }
}
