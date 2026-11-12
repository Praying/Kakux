use std::thread::JoinHandle;

/// On macOS the closure runs inside an NSAutoreleasePool so Objective-C
/// objects created on the worker are drained instead of accumulating until
/// shutdown (use-after-free crashes). Other platforms have no pool to
/// manage, so the closure runs directly.
#[cfg(target_os = "macos")]
pub fn spawn_with_pool<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(move || unsafe {
        use cocoa::foundation::NSAutoreleasePool;
        let pool = NSAutoreleasePool::new(cocoa::base::nil);
        let result = f();
        pool.drain();
        result
    })
}

#[cfg(not(target_os = "macos"))]
pub fn spawn_with_pool<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(f)
}
