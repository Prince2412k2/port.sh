use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(test)]
thread_local! {
    static TEST_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Default, Deserialize, Serialize)]
struct Ledger {
    day: u64,
    allocated: f64,
    #[serde(default)]
    visits: HashMap<String, u32>,
}

struct Lock(File);

impl Drop for Lock {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn path() -> PathBuf {
    if let Ok(path) = std::env::var("PORTFOLIO_AI_BUDGET") {
        return path.into();
    }
    crate::visits::path()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("ai-budget.json")
}

fn setting(name: &str, fallback: f64) -> f64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|value| *value > 0.0)
        .unwrap_or(fallback)
}

fn update(mut change: impl FnMut(&mut Ledger) -> Result<(), String>) -> Result<(), String> {
    let path = path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| error.to_string())?;
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error().to_string());
    }
    let _lock = Lock(file.try_clone().map_err(|error| error.to_string())?);
    let mut raw = String::new();
    file.read_to_string(&mut raw).map_err(|error| error.to_string())?;
    let day = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_secs()
        / 86_400;
    let mut ledger: Ledger = if raw.trim().is_empty() {
        Ledger::default()
    } else {
        serde_json::from_str(&raw).map_err(|error| format!("invalid budget ledger: {error}"))?
    };
    if ledger.day != day {
        ledger = Ledger { day, ..Default::default() };
    }
    change(&mut ledger)?;
    file.set_len(0).map_err(|error| error.to_string())?;
    file.rewind().map_err(|error| error.to_string())?;
    serde_json::to_writer(&mut file, &ledger).map_err(|error| error.to_string())?;
    file.write_all(b"\n").map_err(|error| error.to_string())?;
    file.sync_data().map_err(|error| error.to_string())?;
    Ok(())
}

pub fn reserve_ai() -> Result<(), String> {
    #[cfg(test)]
    if !TEST_ACTIVE.get() {
        return Ok(());
    }
    let limit = setting("PORTFOLIO_DAILY_AI_USD", 10.0);
    let reservation = setting("PORTFOLIO_AI_REQUEST_USD", 0.25);
    update(|ledger| {
        if ledger.allocated + reservation > limit {
            return Err("The portfolio's daily AI allowance has been used. Try again tomorrow.".into());
        }
        ledger.allocated += reservation;
        Ok(())
    })
}

pub fn admit_visit(keys: &[String]) -> bool {
    #[cfg(test)]
    if !TEST_ACTIVE.get() {
        return true;
    }
    let limit = setting("PORTFOLIO_DAILY_VISITS", 24.0) as u32;
    update(|ledger| {
        if keys.iter().any(|key| ledger.visits.get(key).copied().unwrap_or(0) >= limit) {
            return Err("daily visit limit reached".into());
        }
        for key in keys {
            *ledger.visits.entry(key.clone()).or_default() += 1;
        }
        Ok(())
    })
    .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_limits_are_shared_by_identity_and_spend() {
        let _held = crate::visits::ENV_LOCK.lock().unwrap_or_else(|error| error.into_inner());
        let path = std::env::temp_dir().join(format!("portfolio-budget-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        unsafe {
            std::env::set_var("PORTFOLIO_AI_BUDGET", &path);
            std::env::set_var("PORTFOLIO_DAILY_AI_USD", "1");
            std::env::set_var("PORTFOLIO_AI_REQUEST_USD", "0.6");
            std::env::set_var("PORTFOLIO_DAILY_VISITS", "2");
        }
        TEST_ACTIVE.set(true);

        assert!(reserve_ai().is_ok());
        assert!(reserve_ai().is_err(), "spend crossed the daily ceiling");
        let keys = vec!["ip:203.0.113.4".to_string(), "web-id:visitor".to_string()];
        assert!(admit_visit(&keys));
        assert!(admit_visit(&keys));
        assert!(!admit_visit(&keys), "identity crossed the daily visit ceiling");

        unsafe {
            std::env::remove_var("PORTFOLIO_AI_BUDGET");
            std::env::remove_var("PORTFOLIO_DAILY_AI_USD");
            std::env::remove_var("PORTFOLIO_AI_REQUEST_USD");
            std::env::remove_var("PORTFOLIO_DAILY_VISITS");
        }
        TEST_ACTIVE.set(false);
        let _ = std::fs::remove_file(path);
    }
}
