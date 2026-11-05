//! Shared test support.

/// Run an async test body on an 8 MiB stack. In unoptimized builds the frames
/// of `NexusNode::poll_harness_status_and_spawn_agents_inner` and
/// `VesselNode::post` alone take 1.3-1.7 MB, so tests that drive them sit at
/// the edge of the default 2 MiB test-thread stack.
pub(crate) fn on_large_stack<F, Fut>(test: F)
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .stack_size(8 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(test())
        })
        .unwrap()
        .join()
        .unwrap();
}
