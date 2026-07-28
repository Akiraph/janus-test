//! Sleeper abstraction for retries/cooldowns so tests can drive the delay
//! deterministically without real sleeps. Real productions use `SystemSleeper`
//! (tokio::time::sleep); tests inject `FakeSleeper` which records waits and
//! returns immediately.

use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

type SleepFut<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub trait Sleeper: Send + Sync {
    fn sleep<'a>(&'a self, d: Duration) -> SleepFut<'a>;
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SystemSleeper;

impl Sleeper for SystemSleeper {
    fn sleep<'a>(&'a self, d: Duration) -> SleepFut<'a> {
        Box::pin(async move {
            if d.is_zero() {
                return;
            }
            tokio::time::sleep(d).await;
        })
    }
}

#[derive(Debug, Default, Clone)]
pub struct FakeSleeper {
    /// Milliseconds awaited, in call order.
    pub waits: Arc<Mutex<Vec<u64>>>,
}

impl Sleeper for FakeSleeper {
    fn sleep<'a>(&'a self, d: Duration) -> SleepFut<'a> {
        let waits = self.waits.clone();
        Box::pin(async move {
            waits
                .lock()
                .expect("FakeSleeper lock")
                .push(d.as_millis() as u64);
        })
    }
}
