use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

/// Per-thread dispatch serialization locks.
///
/// Maps each Discord `thread_id` to an `Arc<Mutex<()>>` that callers
/// must hold while dispatching a primary or mid-turn message.  Holding
/// the lock ensures that only one dispatch runs at a time per thread,
/// preventing the primary/mid-turn branch-inversion race described in
/// GitHub #258.
///
/// # Invariants
///
/// - `get_or_create` always returns the **same** `Arc<Mutex<()>>` for a
///   given `thread_id` as long as the entry lives in the map.
/// - `remove` is **cooperative** — it only drops the map entry when no
///   other task holds or is waiting on the inner mutex (checked via
///   `Arc::strong_count == 1`).  If there are active holders/waiters,
///   the entry is preserved and the next cleanup cycle retries.  This
///   prevents the following teardown race:
///   1. Task A holds the per-thread guard while dispatching.
///   2. Cleanup fires and would otherwise evict the entry.
///   3. Task B starts a new dispatch for the same thread, calls
///      `get_or_create`, and receives a **fresh** mutex.  A and B would
///      then run concurrently against the same thread, reopening the
///      very race this type exists to prevent.
/// - Because `remove` holds the outer `Mutex<HashMap>`, the
///   `strong_count` check is atomic with the subsequent `HashMap::remove`.
/// - Entries are not leaked: TTL sweep, LRU eviction, and
///   `cleanup_session_state` each re-invoke `remove`, so once all holders
///   drop their `Arc` the entry is reclaimed on a later cleanup tick.
pub struct ThreadDispatchLocks {
    inner: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl ThreadDispatchLocks {
    /// Creates a new, empty `ThreadDispatchLocks`.
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    /// Returns the dispatch lock for `thread_id`, creating one if it does
    /// not yet exist.
    ///
    /// The returned `Arc` keeps the mutex alive even if `remove` is called
    /// concurrently.
    pub async fn get_or_create(&self, thread_id: &str) -> Arc<Mutex<()>> {
        let mut map = self.inner.lock().await;
        map.entry(thread_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Cooperatively removes the dispatch lock entry for `thread_id`.
    ///
    /// The entry is only dropped when the map's `Arc` is the **only**
    /// reference (`strong_count == 1`).  If other tasks still hold the
    /// `Arc` — either because they are inside a guard or waiting on
    /// `.lock()` — the entry is left in place so all current and future
    /// dispatchers on this thread continue to serialize on the **same**
    /// mutex.  A later cleanup cycle (TTL sweep, LRU eviction, next
    /// `cleanup_session_state`) will retry and eventually reclaim the
    /// slot once all holders drop their `Arc`.
    pub async fn remove(&self, thread_id: &str) {
        let mut map = self.inner.lock().await;
        // Hold the outer Mutex<HashMap> for the whole check-and-remove — this
        // is atomic with concurrent `get_or_create` calls on the same key.
        // `Arc::strong_count == 1` means the map holds the only reference.
        if let Some(arc) = map.get(thread_id)
            && Arc::strong_count(arc) == 1
        {
            map.remove(thread_id);
        }
    }

    /// Returns the number of entries currently in the map.
    ///
    /// Only available in test builds.
    #[cfg(test)]
    pub async fn len(&self) -> usize {
        self.inner.lock().await.len()
    }
}

impl Default for ThreadDispatchLocks {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    // ---------------------------------------------------------------------------
    // Integration tests: ThreadDispatchLocks + try_acquire_session DB CAS
    // ---------------------------------------------------------------------------
    mod integration_with_db {
        use super::*;
        use crate::db::repository::{
            create_session, register_project, try_acquire_session, update_session_status,
        };
        use sqlx::PgPool;

        async fn setup_test_pool() -> PgPool {
            let database_url = std::env::var("TEST_DATABASE_URL")
                .expect("TEST_DATABASE_URL must be set for db integration tests");
            let pool = PgPool::connect(&database_url).await.unwrap();
            sqlx::migrate!("./migrations").run(&pool).await.unwrap();
            sqlx::query("TRUNCATE sessions, projects RESTART IDENTITY CASCADE")
                .execute(&pool)
                .await
                .unwrap();
            pool
        }

        /// Two tasks competing for the same `thread_id` must produce exactly
        /// one `true` and one `false` per iteration.
        ///
        /// The per-thread `Mutex<()>` in `ThreadDispatchLocks` serializes the
        /// two tasks so only one reaches `try_acquire_session` while the
        /// session is still `idle`; the other arrives after the status has
        /// been flipped to `running` and therefore gets `false`.
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        #[ignore = "requires TEST_DATABASE_URL"]
        async fn concurrent_acquire_serialized_with_db() {
            let pool = setup_test_pool().await;
            register_project(&pool, "ch-test", "/tmp", None)
                .await
                .unwrap();
            create_session(&pool, "th-race", "ch-test").await.unwrap();

            let locks = Arc::new(ThreadDispatchLocks::new());
            const ITERATIONS: usize = 50;

            for iter in 0..ITERATIONS {
                let barrier = Arc::new(tokio::sync::Barrier::new(2));
                let results: Arc<tokio::sync::Mutex<Vec<bool>>> =
                    Arc::new(tokio::sync::Mutex::new(Vec::with_capacity(2)));

                let mut handles = Vec::with_capacity(2);
                for _ in 0..2 {
                    let locks = locks.clone();
                    let pool = pool.clone();
                    let barrier = barrier.clone();
                    let results = results.clone();
                    handles.push(tokio::spawn(async move {
                        // Both tasks reach the barrier before either proceeds,
                        // maximising contention on the per-thread mutex.
                        barrier.wait().await;
                        let mutex = locks.get_or_create("th-race").await;
                        let _guard = mutex.lock().await;
                        let acquired = try_acquire_session(&pool, "th-race").await.unwrap();
                        results.lock().await.push(acquired);
                    }));
                }

                for h in handles {
                    h.await.unwrap();
                }

                let r = results.lock().await;
                assert_eq!(
                    r.len(),
                    2,
                    "iter {iter}: expected 2 results, got {}",
                    r.len()
                );
                assert!(
                    r.contains(&true) && r.contains(&false),
                    "iter {iter}: expected (true, false) pair, got {:?}",
                    *r
                );
                drop(r);

                // Reset status to `idle` so the next iteration can race again.
                update_session_status(&pool, "th-race", "idle")
                    .await
                    .unwrap();
            }
        }
    }

    /// Two tasks dispatching on the **same** thread_id must run serially.
    /// Each holds the lock for 100 ms, so the total elapsed time must be
    /// at least 180 ms (200 ms target with 20 ms CI slack).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn serialize_same_key() {
        let locks = Arc::new(ThreadDispatchLocks::new());

        let l1 = locks.clone();
        let t1 = tokio::spawn(async move {
            let mutex = l1.get_or_create("thread-a").await;
            let _guard = mutex.lock().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        // Give t1 a moment to acquire the lock before t2 tries.
        tokio::time::sleep(Duration::from_millis(10)).await;

        let l2 = locks.clone();
        let start = Instant::now();
        let t2 = tokio::spawn(async move {
            let mutex = l2.get_or_create("thread-a").await;
            let _guard = mutex.lock().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        t1.await.unwrap();
        t2.await.unwrap();

        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(180),
            "expected >= 180ms for serial execution, got {:?}",
            elapsed
        );
    }

    /// Two tasks dispatching on **different** thread_ids must run in
    /// parallel.  Each holds its lock for 100 ms, so the total elapsed
    /// time must be less than 160 ms (100 ms + 60 ms slack).
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn parallel_different_keys() {
        let locks = Arc::new(ThreadDispatchLocks::new());

        let l1 = locks.clone();
        let l2 = locks.clone();

        let start = Instant::now();

        let t1 = tokio::spawn(async move {
            let mutex = l1.get_or_create("thread-x").await;
            let _guard = mutex.lock().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        let t2 = tokio::spawn(async move {
            let mutex = l2.get_or_create("thread-y").await;
            let _guard = mutex.lock().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        });

        t1.await.unwrap();
        t2.await.unwrap();

        let elapsed = start.elapsed();
        assert!(
            elapsed < Duration::from_millis(160),
            "expected < 160ms for parallel execution, got {:?}",
            elapsed
        );
    }

    /// After `remove`, `len()` must be 0.
    #[tokio::test]
    async fn remove_cleanup() {
        let locks = ThreadDispatchLocks::new();

        locks.get_or_create("thread-1").await;
        locks.get_or_create("thread-2").await;
        assert_eq!(locks.len().await, 2);

        locks.remove("thread-1").await;
        locks.remove("thread-2").await;
        assert_eq!(locks.len().await, 0);
    }

    /// `remove` while another task holds the `Arc` must be cooperative —
    /// the entry is preserved so concurrent dispatchers keep serializing
    /// on the **same** mutex.  After the holder drops the `Arc`, a later
    /// `remove` reclaims the slot.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn remove_cooperative_with_active_holder() {
        let locks = Arc::new(ThreadDispatchLocks::new());

        // Holder keeps an Arc clone + the guard.
        let arc = locks.get_or_create("thread-z").await;
        let guard = arc.lock().await;

        let locks_clone = locks.clone();
        let remover = tokio::spawn(async move {
            locks_clone.remove("thread-z").await;
        });

        // Removal must not deadlock, even when skipped.
        remover.await.unwrap();

        // Cooperative remove: entry is preserved because the holder still owns `arc`.
        assert_eq!(locks.len().await, 1);

        // Same mutex is returned — dispatchers keep serializing.
        let arc_same = locks.get_or_create("thread-z").await;
        assert!(Arc::ptr_eq(&arc, &arc_same));

        drop(guard);
        drop(arc);
        drop(arc_same);

        // Now only the map holds the Arc. remove should succeed.
        locks.remove("thread-z").await;
        assert_eq!(locks.len().await, 0);

        // A fresh get_or_create allocates a brand new mutex.
        let arc_new = locks.get_or_create("thread-z").await;
        assert_eq!(locks.len().await, 1);
        drop(arc_new);
    }
}
