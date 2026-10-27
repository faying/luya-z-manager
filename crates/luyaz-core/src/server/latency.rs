//! `GET /v2/latency/direct`: the direct exit's latency, measured only when asked (the touch asks
//! while Home is on a lit screen; nothing probes on a timer, so an idle device never wakes for
//! it). One probe ([`crate::api::watch::direct_ms`], the deep diagnosis' own) answers every
//! caller for [`TTL`]; callers that come while it runs wait for it instead of starting another.
//! Also while roaming: it is one small request; `roaming` says so.

use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

pub const TTL: Duration = Duration::from_secs(60);

/// The last result: when (monotonic), and `{ms, at, ok}`.
pub struct Cache(Mutex<Option<(Instant, Value)>>);

impl Cache {
    pub const fn new() -> Cache {
        Cache(Mutex::const_new(None))
    }

    /// The cached result while fresh, else `probe`'s (stamped with `wall()`); the lock is held
    /// across the probe, so concurrent callers share it.
    pub async fn get<F, Fut>(&self, probe: F, wall: impl Fn() -> i64) -> (Value, u64)
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Option<u32>>,
    {
        let mut slot = self.0.lock().await;
        if let Some((at, v)) = slot.as_ref()
            && at.elapsed() < TTL
        {
            return (v.clone(), at.elapsed().as_secs());
        }
        let ms = probe().await;
        let v = json!({"ms": ms, "at": wall(), "ok": ms.is_some()});
        *slot = Some((Instant::now(), v.clone()));
        (v, 0)
    }
}

/// One latency probe: the round trip in ms, `None` when nothing answered.
pub type ProbeFn = Arc<dyn Fn() -> Pin<Box<dyn Future<Output = Option<u32>> + Send>> + Send + Sync>;

/// The server's direct-latency state: the cache and the probe it runs (tests inject their own).
pub struct Direct {
    cache: Cache,
    probe: ProbeFn,
}

impl Direct {
    pub fn new(probe: ProbeFn) -> Direct {
        Direct {
            cache: Cache::new(),
            probe,
        }
    }

    /// The device's: [`crate::api::watch::direct_ms`].
    pub fn real() -> Direct {
        Direct::new(Arc::new(|| Box::pin(crate::api::watch::direct_ms())))
    }

    /// The reply's `data`: `{ms, at, ok, age_s, roaming}` (`roaming` null when the signal block
    /// is stale or missing).
    pub async fn data(&self, roaming: Option<bool>) -> Value {
        let (mut v, age) = self
            .cache
            .get(|| (self.probe)(), luyaz_api::alerts::now)
            .await;
        v["age_s"] = json!(age);
        v["roaming"] = json!(roaming);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn one_probe_for_concurrent_callers_and_cached() {
        let cache = Arc::new(Cache::new());
        let n = Arc::new(AtomicU32::new(0));
        let call = |cache: Arc<Cache>, n: Arc<AtomicU32>| async move {
            cache
                .get(
                    || async move {
                        n.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Some(42)
                    },
                    || 1000,
                )
                .await
        };
        let (a, b, c) = tokio::join!(
            call(cache.clone(), n.clone()),
            call(cache.clone(), n.clone()),
            call(cache.clone(), n.clone())
        );
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert_eq!(a.0, json!({"ms": 42, "at": 1000, "ok": true}));
        assert_eq!((b.0.clone(), c.0.clone()), (a.0.clone(), a.0));
        // an unanswered probe is ok:false, ms null (and cached too)
        let cold = Cache::new();
        let (v, _) = cold.get(|| async { None }, || 5).await;
        assert_eq!(v, json!({"ms": null, "at": 5, "ok": false}));
        let (v, _) = cold.get(|| async { Some(1) }, || 6).await;
        assert_eq!(v["ok"], false, "within the TTL");
    }

    #[tokio::test]
    async fn race_takes_the_first_answer() {
        use crate::api::watch::first_some;
        use std::future::{pending, ready};
        let late = || async {
            tokio::time::sleep(Duration::from_millis(30)).await;
            Some(9)
        };
        assert_eq!(first_some(ready(Some(1)), pending()).await, Some(1));
        assert_eq!(first_some(pending(), ready(Some(2))).await, Some(2));
        // one side fails: wait for the other
        assert_eq!(first_some(ready(None), late()).await, Some(9));
        assert_eq!(first_some(late(), ready(None)).await, Some(9));
        // both fail: None
        assert_eq!(first_some(ready(None::<u32>), ready(None)).await, None);
        // the faster answer wins
        let slow = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            Some(200)
        };
        assert_eq!(first_some(slow, late()).await, Some(9));
    }
}
