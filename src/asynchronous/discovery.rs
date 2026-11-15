//! Owned discovery adapters sharing the receivers' blocking-pool stop rule.
use super::{private, with_deadline_budget, with_timeout_budget, AsyncRuntime};
use crate::{
    finder::{validate_host_query, validate_source_name},
    timeout::Deadline,
    Finder, Result, Source, SourceCache,
};
use std::{cell::Cell, marker::PhantomData, sync::Arc, time::Duration};

mod sealed {
    pub trait Discovery {
        fn finder(&self) -> &crate::Finder;
    }
    impl Discovery for crate::Finder {
        fn finder(&self) -> &crate::Finder {
            self
        }
    }
    impl Discovery for crate::SourceCache {
        fn finder(&self) -> &crate::Finder {
            self.finder()
        }
    }
}

/// Discovery state an [`AsyncFinder`] can own: [`Finder`] or [`SourceCache`].
///
/// This sealed, itemless trait selects the existing discovery owner. It adds no
/// finder, cache or worker and cannot be implemented outside this crate.
pub trait Discovery: sealed::Discovery + Send + Sync + 'static {}
impl Discovery for Finder {}
impl Discovery for SourceCache {}

/// Async discovery through one owned [`Finder`] or [`SourceCache`].
///
/// `R` selects `Tokio` or `Smol`, when its feature is enabled.
/// Clones share the owner through an `Arc`; [`Self::finder`] lends its configured
/// finder for synchronous controls. Constructing this adapter does not recreate
/// a finder or change its groups, IP hints, local-source policy or runtime.
///
/// Wait budgets start on first poll and include blocking-pool queue delay and
/// snapshot-lock contention. Dropping a future skips queued work and stops a
/// started wait within its next native attempt, at most 100 ms. A snapshot copy
/// or predicate already running finishes, and can outlast the budget. Native
/// waits and condvar waits are sliced only for these stoppable calls; synchronous
/// discovery keeps its full-budget wait, avoiding the extra native calls.
/// On idle Linux x86-64 finders with Standard 6.3.2/6.4.0.2, slicing measured
/// about 0.1–0.2 ms of calling-thread CPU per second of waiting; other workloads
/// and platforms can differ.
/// Predicates run outside SDK locks; their panics resume on the awaiting task.
///
/// ```no_run
/// # #[cfg(feature = "tokio")]
/// # async fn example() -> grafton_ndi::Result<()> {
/// use grafton_ndi::{asynchronous::tokio::AsyncFinder, Finder, FinderOptions, Ndi};
/// use std::time::Duration;
/// let ndi = Ndi::new()?;
/// let finder = AsyncFinder::new(Finder::new(&ndi, &FinderOptions::new())?);
/// let source = finder.find_source_by_name("STUDIO (Program)", Duration::from_secs(5)).await?;
/// println!("{source:?}");
/// # Ok(())
/// # }
/// ```
pub struct AsyncFinder<R: AsyncRuntime, D: Discovery = Finder> {
    inner: Arc<D>,
    _runtime: PhantomData<R>,
}

impl<R: AsyncRuntime, D: Discovery> Clone for AsyncFinder<R, D> {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
            _runtime: PhantomData,
        }
    }
}

impl<R: AsyncRuntime, D: Discovery> AsyncFinder<R, D> {
    /// Share an existing discovery owner without recreating its native finder.
    #[must_use]
    pub fn new(discovery: D) -> Self {
        Self {
            inner: Arc::new(discovery),
            _runtime: PhantomData,
        }
    }

    /// The configured finder shared by every lookup and cursor of this adapter.
    #[must_use]
    pub fn finder(&self) -> &Finder {
        sealed::Discovery::finder(&*self.inner)
    }

    /// Create one owned cursor that can move into a spawned async task.
    ///
    /// Each consumer needs its own cursor. This records the current generation
    /// and clones an `Arc`, without allocating or calling the SDK.
    #[must_use]
    pub fn watch(&self) -> AsyncSourceWatch<R, D> {
        AsyncSourceWatch {
            inner: Arc::clone(&self.inner),
            observed: Cell::new(self.finder().generation()),
            _runtime: PhantomData,
        }
    }

    /// Find the first source accepted by `select`, as [`Finder::find_source`].
    ///
    /// The predicate is owned by the blocking worker and must be `Send + 'static`.
    /// A predicate already running finishes even after cancellation.
    ///
    /// # Errors
    ///
    /// Invalid timeouts are refused before submitting work. Snapshot and resource
    /// errors propagate; [`crate::Error::SpawnFailed`] reports dispatch failure.
    pub async fn find_source(
        &self,
        timeout: Duration,
        mut select: impl FnMut(&Source) -> bool + Send + 'static,
    ) -> Result<Option<Source>> {
        let inner = Arc::clone(&self.inner);
        with_timeout_budget::<R, _, _>(timeout, move |deadline, stop| {
            sealed::Discovery::finder(&*inner).find_source_until(deadline, stop, &mut select)
        })
        .await
    }

    /// Find an exact complete name, as [`Finder::find_source_by_name`].
    ///
    /// # Errors
    ///
    /// Invalid timeouts and empty or NUL-containing names are refused, in that
    /// order, before submitting work. Other errors match [`Self::find_source`].
    pub async fn find_source_by_name(
        &self,
        name: &str,
        timeout: Duration,
    ) -> Result<Option<Source>> {
        let deadline = Deadline::after(timeout)?;
        validate_source_name(name)?;
        let name = name.to_owned();
        let inner = Arc::clone(&self.inner);
        with_deadline_budget::<R, _, _>(deadline, move |deadline, stop| {
            sealed::Discovery::finder(&*inner)
                .find_source_until(deadline, stop, |source| source.name() == name)
        })
        .await
    }

    /// Observe the full discovery window, as [`Finder::find_sources`].
    ///
    /// # Errors
    ///
    /// Invalid timeouts are refused before submitting work. Bounded snapshot
    /// and accumulated-result errors propagate, as does dispatch failure.
    pub async fn find_sources(&self, timeout: Duration) -> Result<Vec<Source>> {
        let inner = Arc::clone(&self.inner);
        with_timeout_budget::<R, _, _>(timeout, move |deadline, stop| {
            sealed::Discovery::finder(&*inner).find_sources_until(deadline, stop)
        })
        .await
    }
}

impl<R: AsyncRuntime> AsyncFinder<R, SourceCache> {
    /// The cache shared by this adapter, for invalidation and synchronous access.
    #[must_use]
    pub fn cache(&self) -> &SourceCache {
        &self.inner
    }

    /// Return cached feeds or observe a host's full discovery window.
    ///
    /// As [`SourceCache::sources_by_host`], including best-effort publication.
    /// A cancelled miss does not publish an incomplete discovery window. Cache
    /// hits also use the blocking pool; returned descriptors are owned copies.
    ///
    /// # Errors
    ///
    /// Invalid timeouts and hosts outside [`Source::matches_host`]'s grammar
    /// are refused, in that order, before submitting work. Discovery, bounded
    /// result-copy and dispatch errors propagate.
    pub async fn sources_by_host(&self, host: &str, timeout: Duration) -> Result<Vec<Source>> {
        let deadline = Deadline::after(timeout)?;
        validate_host_query(host)?;
        let host = host.to_owned();
        let inner = Arc::clone(&self.inner);
        with_deadline_budget::<R, _, _>(deadline, move |deadline, stop| {
            inner.sources_by_host_until(&host, deadline, stop)
        })
        .await
    }
}

/// One owned consumer's cursor over an [`AsyncFinder`]'s change generations.
///
/// Unlike [`crate::SourceWatch`], this cursor owns its discovery state through
/// an `Arc`, so it can move across awaits and into spawned tasks. It is `Send`
/// but not `Sync`: one consumer advances it through `&mut self` methods.
/// A failed or dropped operation leaves its generation unchanged. Reports
/// published during a snapshot or an abandoned wait remain owed to the cursor.
/// This also keeps the finder and its runtime alive until the cursor is dropped.
pub struct AsyncSourceWatch<R: AsyncRuntime, D: Discovery = Finder> {
    inner: Arc<D>,
    observed: Cell<u64>,
    _runtime: PhantomData<R>,
}

impl<R: AsyncRuntime, D: Discovery> AsyncSourceWatch<R, D> {
    /// Copy current sources and acknowledge the generation read before copying.
    ///
    /// Snapshot-lock acquisition can be stopped when this future is dropped;
    /// a native snapshot or bounded copy already running finishes. There is no
    /// timeout, as with [`crate::SourceWatch::current_sources`].
    ///
    /// # Errors
    ///
    /// Snapshot, resource and dispatch errors propagate. Failure leaves this
    /// cursor unchanged.
    pub async fn current_sources(&mut self) -> Result<Vec<Source>> {
        let inner = Arc::clone(&self.inner);
        let (generation, sources) = R::spawn_stoppable(private::Token, move |stop| {
            let finder = sealed::Discovery::finder(&*inner);
            let generation = finder.generation();
            finder
                .current_sources_stoppable(stop)
                .map(|sources| (generation, sources))
        })
        .await??;
        self.observed.set(generation);
        Ok(sources)
    }

    /// Wait for a report newer than this cursor's last completed observation.
    ///
    /// A zero timeout sees an already published report even while another
    /// native waiter is active; otherwise it makes one nonblocking native query.
    /// Dropping the future leaves the cursor unchanged. A report consumed by
    /// its worker is still published for the next wait and other consumers.
    ///
    /// # Errors
    ///
    /// Invalid timeouts are refused before submitting work; dispatch failure is
    /// [`crate::Error::SpawnFailed`]. Failure leaves this cursor unchanged.
    pub async fn wait_for_change(&mut self, timeout: Duration) -> Result<bool> {
        let observed = self.observed.get();
        let inner = Arc::clone(&self.inner);
        let (changed, generation) =
            with_timeout_budget::<R, _, _>(timeout, move |deadline, stop| {
                let finder = sealed::Discovery::finder(&*inner);
                let changed = finder.wait_for_sources_since(observed, deadline, stop);
                Ok((changed, finder.generation()))
            })
            .await?;
        if changed {
            self.observed.set(generation);
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FinderOptions, Ndi};
    use std::{
        future::Future,
        task::{Context, Poll, Waker},
        thread,
        time::Instant,
    };

    fn finder(ndi: &Ndi) -> Finder {
        Finder::new(
            ndi,
            &FinderOptions::new()
                .groups(crate::test_support::unique_name("async discovery"))
                .show_local_sources(false),
        )
        .unwrap()
    }

    fn futures_are_send<R: AsyncRuntime>(
        finder: &AsyncFinder<R>,
        cache: &AsyncFinder<R, SourceCache>,
        watch: &mut AsyncSourceWatch<R>,
    ) {
        fn send<T: Send>(_: T) {}
        send(finder.find_source(Duration::ZERO, |_| true));
        send(finder.find_source_by_name("camera", Duration::ZERO));
        send(finder.find_sources(Duration::ZERO));
        send(cache.sources_by_host("camera", Duration::ZERO));
        send(watch.current_sources());
        send(watch.wait_for_change(Duration::ZERO));
    }

    #[test]
    fn discovery_futures_are_send() {
        #[cfg(feature = "tokio")]
        let _ = futures_are_send::<super::super::Tokio>;
        #[cfg(feature = "smol")]
        let _ = futures_are_send::<super::super::Smol>;
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn invalid_discovery_inputs_are_refused_without_runtime_context() {
        use super::super::Tokio;
        use crate::{Error, InputDefect};
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let adapter = AsyncFinder::<Tokio>::new(finder(&ndi));
        let cache = AsyncFinder::<Tokio, SourceCache>::new(SourceCache::new(finder(&ndi)));
        let mut context = Context::from_waker(Waker::noop());
        let excessive = crate::MAX_TIMEOUT + Duration::from_nanos(1);
        assert!(matches!(
            Box::pin(adapter.find_source(excessive, |_| true))
                .as_mut()
                .poll(&mut context),
            Poll::Ready(Err(Error::InvalidInput(InputDefect::TimeoutTooLong { .. })))
        ));
        assert!(matches!(
            Box::pin(adapter.find_sources(excessive))
                .as_mut()
                .poll(&mut context),
            Poll::Ready(Err(Error::InvalidInput(InputDefect::TimeoutTooLong { .. })))
        ));
        for name in ["", "bad\0name"] {
            assert!(matches!(
                Box::pin(adapter.find_source_by_name(name, Duration::ZERO))
                    .as_mut()
                    .poll(&mut context),
                Poll::Ready(Err(Error::InvalidInput(InputDefect::InvalidSourceName)))
            ));
        }
        for host in ["", "http://camera", "bad\0host"] {
            assert!(matches!(
                Box::pin(cache.sources_by_host(host, Duration::ZERO))
                    .as_mut()
                    .poll(&mut context),
                Poll::Ready(Err(Error::InvalidInput(InputDefect::InvalidHostQuery)))
            ));
        }
        let mut watch = adapter.watch();
        assert!(matches!(
            Box::pin(watch.wait_for_change(excessive))
                .as_mut()
                .poll(&mut context),
            Poll::Ready(Err(Error::InvalidInput(InputDefect::TimeoutTooLong { .. })))
        ));
    }

    async fn cancelled_native_discovery_releases_its_owner<R: AsyncRuntime>() {
        let Some(ndi) = crate::test_support::ndi() else {
            return;
        };
        let adapter = AsyncFinder::<R>::new(finder(&ndi));
        let cache = AsyncFinder::<R, SourceCache>::new(SourceCache::new(finder(&ndi)));
        fn abandon<T>(
            operation: impl Future<Output = T>,
            references: impl Fn() -> usize,
            owners: usize,
        ) {
            let mut operation = Box::pin(operation);
            let mut context = Context::from_waker(Waker::noop());
            assert!(matches!(
                operation.as_mut().poll(&mut context),
                Poll::Pending
            ));
            thread::sleep(Duration::from_millis(50));
            drop(operation);
            let deadline = Instant::now() + Duration::from_millis(150);
            while references() != owners {
                assert!(
                    Instant::now() < deadline,
                    "abandoned discovery kept its blocking worker"
                );
                thread::sleep(Duration::from_millis(1));
            }
        }
        abandon(
            adapter.find_source(Duration::from_secs(3), |_| false),
            || Arc::strong_count(&adapter.inner),
            1,
        );
        abandon(
            cache.sources_by_host("missing.invalid", Duration::from_secs(3)),
            || Arc::strong_count(&cache.inner),
            1,
        );
        assert!(cache.cache().is_empty());
        let mut watch = adapter.watch();
        // Drain startup reports before measuring a wait in this empty group.
        while adapter
            .finder()
            .watch()
            .wait_for_change(Duration::ZERO)
            .unwrap()
        {}
        watch.current_sources().await.unwrap();
        abandon(
            watch.wait_for_change(Duration::from_secs(3)),
            || Arc::strong_count(&adapter.inner),
            2,
        );
    }

    #[cfg(feature = "tokio")]
    #[test]
    fn tokio_abandoned_native_discovery_releases_its_owner() {
        ::tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(cancelled_native_discovery_releases_its_owner::<
                super::super::Tokio,
            >());
    }

    #[cfg(feature = "smol")]
    #[test]
    fn smol_abandoned_native_discovery_releases_its_owner() {
        ::smol::block_on(cancelled_native_discovery_releases_its_owner::<
            super::super::Smol,
        >());
    }
}
