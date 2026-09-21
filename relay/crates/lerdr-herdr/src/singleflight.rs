//! Singleflight for identical in-flight read-only requests (doc 08 rule 2):
//! N watchers of the same pane produce one `pane.read` on the socket and the
//! result fans out.
//!
//! Leader/follower over a `broadcast::Sender` of capacity 1. If the leader's
//! future is dropped (caller cancelled), a drop guard evicts the map entry so
//! followers observe `Closed` and one re-runs as the new leader — no waiter is
//! stranded and no stale sender lingers in the map.

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::broadcast;

use crate::error::HerdrError;

type SharedResult = Result<Value, Arc<HerdrError>>;
type Map = Mutex<HashMap<String, broadcast::Sender<SharedResult>>>;

#[derive(Debug, Default)]
pub(crate) struct Singleflight {
    in_flight: Map,
}

/// Evicts the flight's map entry when the leader future completes *or* is
/// dropped. Without this, a cancelled leader would leave its `Sender` in the
/// map and followers would hang waiting on a channel that can never deliver.
struct LeaderGuard<'a> {
    map: &'a Map,
    key: &'a str,
}

impl Drop for LeaderGuard<'_> {
    fn drop(&mut self) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(self.key);
    }
}

impl Singleflight {
    /// Run `fut` once per `key` while it is in flight; concurrent callers share
    /// the result. A leader whose future is dropped hands leadership to a
    /// waiting follower.
    pub async fn execute<F, Fut>(&self, key: String, fut: F) -> Result<Value, HerdrError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, HerdrError>>,
    {
        let mut fut = Some(fut);
        loop {
            enum Role {
                Leader(broadcast::Sender<SharedResult>),
                Follower(broadcast::Receiver<SharedResult>),
            }
            let role = {
                let mut map = self.in_flight.lock().unwrap_or_else(|e| e.into_inner());
                match map.get(&key) {
                    Some(tx) => Role::Follower(tx.subscribe()),
                    None => {
                        let (tx, _) = broadcast::channel(1);
                        map.insert(key.clone(), tx.clone());
                        Role::Leader(tx)
                    }
                }
            };
            match role {
                Role::Leader(tx) => {
                    let result = {
                        let _guard = LeaderGuard {
                            map: &self.in_flight,
                            key: &key,
                        };
                        fut.take().expect("leader future taken exactly once")()
                            .await
                            .map_err(Arc::new)
                        // _guard drops on normal completion AND on
                        // cancellation of this future.
                    };
                    // Send before the guard's removal could interleave:
                    // receivers subscribed by now get the result; a caller
                    // arriving in the gap just misses the dedupe.
                    let _ = tx.send(result.clone());
                    return result.map_err(|e| (*e).clone());
                }
                Role::Follower(mut rx) => match rx.recv().await {
                    Ok(result) => return result.map_err(|e| (*e).clone()),
                    // Leader gone before completing — loop to claim
                    // leadership or join the next flight.
                    Err(broadcast::error::RecvError::Closed)
                    | Err(broadcast::error::RecvError::Lagged(_)) => continue,
                },
            }
        }
    }

    /// Number of distinct in-flight keys — diagnostics only.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// Followers parked on `key`'s broadcast channel — lets tests wait until
    /// all dedupe participants are actually subscribed before releasing the
    /// leader.
    #[cfg(test)]
    pub fn receiver_count(&self, key: &str) -> usize {
        self.in_flight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(key)
            .map(|tx| tx.receiver_count())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::DispatchPhase;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn dedupes_concurrent_calls() {
        let sf = Arc::new(Singleflight::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let calls = calls.clone();
            let gate_rx = gate_rx.clone();
            let sf = sf.clone();
            handles.push(tokio::spawn(async move {
                sf.execute("k".to_string(), || async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    // Gate the leader so all followers actually overlap.
                    if let Some(rx) = gate_rx.lock().await.take() {
                        let _ = rx.await;
                    }
                    Ok(Value::from(42))
                })
                .await
            }));
        }
        // Deterministic overlap: wait until every follower is parked on the
        // leader's channel before releasing it.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sf.receiver_count("k") < 7 {
            assert!(
                std::time::Instant::now() < deadline,
                "followers never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        gate_tx.send(()).unwrap();
        for h in handles {
            let r = tokio::time::timeout(Duration::from_secs(10), h)
                .await
                .expect("deduped call hung")
                .unwrap();
            assert_eq!(r.unwrap(), Value::from(42));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(sf.len(), 0);
    }

    #[tokio::test]
    async fn followers_get_the_same_error() {
        let sf = Arc::new(Singleflight::default());
        let (gate_tx, gate_rx) = tokio::sync::oneshot::channel::<()>();
        let gate_rx = Arc::new(tokio::sync::Mutex::new(Some(gate_rx)));
        let mut handles = Vec::new();
        for _ in 0..4 {
            let gate_rx = gate_rx.clone();
            let sf = sf.clone();
            handles.push(tokio::spawn(async move {
                sf.execute("k".to_string(), || async move {
                    if let Some(rx) = gate_rx.lock().await.take() {
                        let _ = rx.await;
                    }
                    Err(HerdrError::refused("pane_not_found", "no"))
                })
                .await
            }));
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sf.receiver_count("k") < 3 {
            assert!(
                std::time::Instant::now() < deadline,
                "followers never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        gate_tx.send(()).unwrap();
        for h in handles {
            let err = tokio::time::timeout(Duration::from_secs(10), h)
                .await
                .expect("deduped call hung")
                .unwrap()
                .unwrap_err();
            assert_eq!(err.phase(), DispatchPhase::Refused);
            assert_eq!(err.refusal_code(), Some("pane_not_found"));
        }
    }

    #[tokio::test]
    async fn cancelled_leader_hands_off() {
        let sf = Arc::new(Singleflight::default());
        let calls = Arc::new(AtomicUsize::new(0));

        // Leader that blocks until cancelled.
        let leader_sf = sf.clone();
        let leader_calls = calls.clone();
        let leader = tokio::spawn(async move {
            leader_sf
                .execute("k".to_string(), || async move {
                    leader_calls.fetch_add(1, Ordering::SeqCst);
                    futures_pending().await
                })
                .await
        });
        // Let the leader insert its key.
        tokio::task::yield_now().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
        assert_eq!(sf.len(), 1);

        // A follower joins the same key.
        let follower_sf = sf.clone();
        let follower_calls = calls.clone();
        let follower = tokio::spawn(async move {
            follower_sf
                .execute("k".to_string(), || async move {
                    follower_calls.fetch_add(1, Ordering::SeqCst);
                    Ok(Value::from("re-leadered"))
                })
                .await
        });
        // Wait until the follower is actually parked on the leader's channel.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while sf.receiver_count("k") < 1 {
            assert!(
                std::time::Instant::now() < deadline,
                "follower never subscribed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        // Cancel the leader — the follower must take over, not hang.
        leader.abort();
        let result = tokio::time::timeout(Duration::from_secs(5), follower)
            .await
            .expect("follower must not hang after leader cancellation")
            .unwrap();
        assert_eq!(result.unwrap(), Value::from("re-leadered"));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    async fn futures_pending() -> Result<Value, HerdrError> {
        std::future::pending::<()>().await;
        unreachable!()
    }

    #[tokio::test]
    async fn distinct_keys_do_not_dedupe() {
        let sf = Singleflight::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let a = sf.execute("a".to_string(), {
            let calls = calls.clone();
            || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Value::from(1))
            }
        });
        let b = sf.execute("b".to_string(), {
            let calls = calls.clone();
            || async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Value::from(2))
            }
        });
        let (ra, rb) = tokio::join!(a, b);
        assert_eq!(ra.unwrap(), Value::from(1));
        assert_eq!(rb.unwrap(), Value::from(2));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
