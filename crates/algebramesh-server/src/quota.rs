//! The bytes each client token may upload as blobs (ADR 0021, B3).
//!
//! Each token has an allowance of `per_hour` bytes that refills evenly over
//! an hour (a token bucket). An upload takes its declared size from the
//! allowance before any byte of it is stored; it is refused if the allowance
//! does not hold that much, and the bytes of an upload that fails are given
//! back. The allowance is kept in memory on each scheduler: a restart gives
//! every token its full allowance again, and a client that uploads to several
//! schedulers has an allowance on each.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

const HOUR: Duration = Duration::from_secs(3600);

#[derive(Debug)]
pub struct UploadQuota {
    /// 0: no bound.
    per_hour: u64,
    /// The bytes each token may still upload, as of an instant.
    allowances: Mutex<HashMap<String, (f64, Instant)>>,
    refused: AtomicU64,
}

impl UploadQuota {
    pub fn new(per_hour: u64) -> Self {
        Self {
            per_hour,
            allowances: Mutex::new(HashMap::new()),
            refused: AtomicU64::new(0),
        }
    }

    /// Whether uploads are bounded at all.
    pub fn is_bounded(&self) -> bool {
        self.per_hour > 0
    }

    /// Takes `bytes` from the allowance of `token`, or says why not.
    pub fn take(&self, token: &str, bytes: u64) -> Result<(), String> {
        self.take_at(token, bytes, Instant::now())
    }

    fn take_at(&self, token: &str, bytes: u64, now: Instant) -> Result<(), String> {
        if self.per_hour == 0 {
            return Ok(());
        }
        let refuse = |message: String| {
            self.refused.fetch_add(1, Ordering::Relaxed);
            Err(message)
        };
        if bytes > self.per_hour {
            return refuse(format!(
                "a blob of {bytes} bytes exceeds the {} bytes a client token may upload in an \
                 hour (blob_upload_bytes_per_token_per_hour)",
                self.per_hour
            ));
        }
        let mut allowances = self
            .allowances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let left = self.left(&allowances, token, now);
        if (bytes as f64) > left {
            let wait = (bytes as f64 - left) / self.per_hour as f64 * HOUR.as_secs_f64();
            return refuse(format!(
                "this client token has uploaded its allowance of {} bytes an hour \
                 (blob_upload_bytes_per_token_per_hour); {bytes} bytes are available again in \
                 {:.0} s",
                self.per_hour,
                wait.ceil()
            ));
        }
        allowances.insert(token.to_owned(), (left - bytes as f64, now));
        Ok(())
    }

    /// Gives `bytes` back to `token`, after an upload that failed.
    pub fn give_back(&self, token: &str, bytes: u64) {
        if self.per_hour == 0 {
            return;
        }
        let now = Instant::now();
        let mut allowances = self
            .allowances
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let left = (self.left(&allowances, token, now) + bytes as f64).min(self.per_hour as f64);
        allowances.insert(token.to_owned(), (left, now));
    }

    /// Uploads refused since the scheduler started.
    pub fn refused(&self) -> u64 {
        self.refused.load(Ordering::Relaxed)
    }

    fn left(&self, allowances: &HashMap<String, (f64, Instant)>, token: &str, now: Instant) -> f64 {
        let full = self.per_hour as f64;
        allowances.get(token).map_or(full, |(left, at)| {
            let refilled = now.saturating_duration_since(*at).as_secs_f64() / HOUR.as_secs_f64();
            (left + refilled * full).min(full)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_a_bound_everything_is_taken() {
        let q = UploadQuota::new(0);
        assert!(!q.is_bounded());
        q.take("a", u64::MAX).unwrap();
        assert_eq!(q.refused(), 0);
    }

    #[test]
    fn the_allowance_refills_over_an_hour() {
        let q = UploadQuota::new(3600);
        let t = Instant::now();
        q.take_at("a", 3000, t).unwrap();
        let e = q.take_at("a", 1000, t).unwrap_err();
        assert!(e.contains("400 s"), "{e}");
        // Another token has its own allowance.
        q.take_at("b", 3600, t).unwrap();
        // 400 bytes come back in 400 s.
        q.take_at("a", 1000, t + Duration::from_secs(400)).unwrap();
        // Never more than the hourly allowance.
        q.take_at("a", 3600, t + Duration::from_secs(100_000))
            .unwrap();
        assert_eq!(q.refused(), 1);
    }

    #[test]
    fn a_blob_larger_than_the_allowance_is_refused_outright() {
        let q = UploadQuota::new(100);
        let e = q.take("a", 101).unwrap_err();
        assert!(e.contains("exceeds"), "{e}");
        assert_eq!(q.refused(), 1);
    }

    #[test]
    fn a_failed_upload_gives_its_bytes_back() {
        let q = UploadQuota::new(100);
        q.take("a", 100).unwrap();
        q.give_back("a", 100);
        q.take("a", 100).unwrap();
    }
}
