//! The analysis threads' stacks (rigor-rs#412, rigor-rs#431, ADR-0045).
//!
//! Every thread that parses, lowers or infers runs on a reserved
//! [`ANALYSIS_STACK_BYTES`] stack: the command thread [`run_command_thread`]
//! starts first and reserves its stack, then builds the rayon pool
//! ([`configure_global_pool`]) so workers cannot spend the reservation
//! budget first. `coverage --workers` uses the same spawn handler. A host
//! that cannot reserve 256 MiB falls back to 64 MiB, then:
//! - pool workers: Rust's 2 MiB spawn default;
//! - the command thread on Unix: the process main thread (8 MiB), never a
//!   spawned thread smaller than that;
//! - the command thread on Windows: Rust's 2 MiB spawn default, then the
//!   process main thread (1 MiB).
//!
//! A large stack never stops a command from starting. A pool that cannot
//! be built at all prints one error and the command returns failure; it
//! does not continue into a later `GlobalPoolAlreadyInitialized` panic.

use std::io;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, Once, PoisonError};

use rigor_parse::ANALYSIS_STACK_BYTES;

/// One spawn attempt's closure.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// The stack sizes tried in turn for pool workers; `None` is Rust's 2 MiB
/// spawn default (`thread::Builder` with no `stack_size`), not the OS
/// main-thread stack.
pub(crate) const STACK_FALLBACK: [Option<usize>; 3] =
    [Some(ANALYSIS_STACK_BYTES), Some(64 << 20), None];

/// Stack sizes tried for the command thread. On Unix the process main
/// thread is already 8 MiB, so we never spawn at Rust's 2 MiB default: if
/// 256 MiB and 64 MiB both fail, the command runs inline.
#[cfg(unix)]
pub(crate) const COMMAND_THREAD_SIZES: &[Option<usize>] =
    &[Some(ANALYSIS_STACK_BYTES), Some(64 << 20)];

/// On Windows the process main thread is 1 MiB, smaller than Rust's 2 MiB
/// spawn default, so the command thread still tries that default.
#[cfg(not(unix))]
pub(crate) const COMMAND_THREAD_SIZES: &[Option<usize>] = &STACK_FALLBACK;

/// What a fallback actually used, for the one stderr line per process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StackKind {
    Reserved(usize),
    /// `thread::Builder` with no `stack_size`: Rust's 2 MiB spawn default.
    RustSpawnDefault,
    /// The command ran on the process main thread because no spawn worked.
    ProcessMainThread,
}

/// Spawn `job` with the first size in `sizes` that `spawn` accepts, and return
/// the spawn's handle and that size.
///
/// `std::thread::Builder::spawn` consumes its closure even when it fails, so
/// the job waits in a shared slot. Every attempt receives a fresh closure that
/// takes the job out of the slot only once its thread runs. The job therefore
/// runs at most once: on the thread of the first attempt that succeeds.
///
/// `spawn` is injected so a test can fail chosen sizes.
pub(crate) fn spawn_fallback<J, H, S>(
    sizes: &[Option<usize>],
    job: J,
    mut spawn: S,
) -> io::Result<(H, Option<usize>)>
where
    J: FnOnce() + Send + 'static,
    S: FnMut(Option<usize>, Job) -> io::Result<H>,
{
    let slot = Arc::new(Mutex::new(Some(job)));
    let mut last_err = None;
    for &size in sizes {
        let slot = Arc::clone(&slot);
        let attempt = Box::new(move || {
            let job = slot.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(job) = job {
                job();
            }
        });
        match spawn(size, attempt) {
            Ok(handle) => return Ok((handle, size)),
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("no stack size to try")))
}

/// A `std::thread::Builder` for one attempt, detached.
fn std_spawn(name: Option<String>) -> impl FnMut(Option<usize>, Job) -> io::Result<()> {
    move |size, job| {
        let mut builder = std::thread::Builder::new();
        if let Some(name) = &name {
            builder = builder.name(name.clone());
        }
        if let Some(size) = size {
            builder = builder.stack_size(size);
        }
        builder.spawn(job).map(drop)
    }
}

fn describe_stack(kind: StackKind) -> String {
    match kind {
        StackKind::Reserved(bytes) => format!("{} MiB", bytes >> 20),
        StackKind::RustSpawnDefault => "Rust's 2 MiB spawn default".to_string(),
        StackKind::ProcessMainThread => "the process main thread".to_string(),
    }
}

/// The one stderr line a fallback prints. Named so tests can pin the wording.
pub(crate) fn stack_warning(kind: StackKind) -> String {
    format!(
        "rigor: could not reserve {} MiB thread stacks, using {}; \
         deeply nested files may overflow",
        ANALYSIS_STACK_BYTES >> 20,
        describe_stack(kind)
    )
}

fn size_to_kind(size: Option<usize>) -> StackKind {
    match size {
        Some(bytes) => StackKind::Reserved(bytes),
        None => StackKind::RustSpawnDefault,
    }
}

/// One stderr line per process when a thread got less than
/// [`ANALYSIS_STACK_BYTES`]. Silent on the normal path.
fn note_stack(kind: StackKind) {
    static WARNED: Once = Once::new();
    if matches!(kind, StackKind::Reserved(bytes) if bytes == ANALYSIS_STACK_BYTES) {
        return;
    }
    WARNED.call_once(|| {
        eprintln!("{}", stack_warning(kind));
    });
}

/// The one error a pool that cannot be built at all prints. Named so tests
/// can pin the wording; the command then returns failure instead of
/// continuing into rayon's `GlobalPoolAlreadyInitialized`.
pub(crate) fn pool_build_error_line(err: &dyn std::fmt::Display) -> String {
    format!("rigor: could not start the analysis thread pool: {err}")
}

/// Run `command` on the command thread: named `main`, so panic and overflow
/// messages read as they did when the command ran on the process's main
/// thread. If no thread of at least 64 MiB (Unix) / including Rust's 2 MiB
/// spawn default (elsewhere) can be created, it runs on the main thread
/// itself.
pub(crate) fn run_command_thread(command: fn() -> ExitCode) -> ExitCode {
    run_command_thread_with(COMMAND_THREAD_SIZES, command, |size, job| {
        let mut builder = std::thread::Builder::new().name("main".to_string());
        if let Some(size) = size {
            builder = builder.stack_size(size);
        }
        builder.spawn(job)
    })
}

/// [`run_command_thread`] with the spawn injected (tests).
fn run_command_thread_with<S, F>(sizes: &[Option<usize>], command: F, spawn: S) -> ExitCode
where
    S: FnMut(Option<usize>, Job) -> io::Result<std::thread::JoinHandle<()>>,
    F: FnOnce() -> ExitCode + Send + 'static,
{
    let command = Arc::new(Mutex::new(Some(command)));
    let code = Arc::new(Mutex::new(None));
    let out = Arc::clone(&code);
    let cmd = Arc::clone(&command);
    match spawn_fallback(
        sizes,
        move || {
            let command = cmd.lock().unwrap_or_else(PoisonError::into_inner).take();
            if let Some(command) = command {
                *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(command());
            }
        },
        spawn,
    ) {
        Ok((handle, size)) => {
            note_stack(size_to_kind(size));
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
            let code = code.lock().unwrap_or_else(PoisonError::into_inner).take();
            code.unwrap_or(ExitCode::FAILURE)
        }
        Err(_) => {
            note_stack(StackKind::ProcessMainThread);
            let command = command
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            command.map(|c| c()).unwrap_or(ExitCode::FAILURE)
        }
    }
}

/// The rayon `spawn_handler` for every analysis pool: each worker gets the
/// first stack size the host can reserve.
pub(crate) fn spawn_pool_thread(thread: rayon::ThreadBuilder) -> io::Result<()> {
    let name = thread.name().map(str::to_string);
    let size = spawn_pool_thread_with(thread, &STACK_FALLBACK, std_spawn(name))?;
    note_stack(size_to_kind(size));
    Ok(())
}

/// [`spawn_pool_thread`] with the sizes and the spawn injected (tests).
fn spawn_pool_thread_with<S>(
    thread: rayon::ThreadBuilder,
    sizes: &[Option<usize>],
    spawn: S,
) -> io::Result<Option<usize>>
where
    S: FnMut(Option<usize>, Job) -> io::Result<()>,
{
    spawn_fallback(sizes, move || thread.run(), spawn).map(|((), size)| size)
}

/// Build the global rayon pool with [`spawn_pool_thread`]. It must run on
/// the command thread, after that thread has reserved its stack and before
/// anything uses rayon (rigor-rs#431).
///
/// `build_global` is one-shot: rayon marks the global registry as set even
/// when building it fails, so it is never retried with other settings. The
/// stack fallback happens per thread, inside the spawn handler. If no
/// worker can be created even at Rust's 2 MiB spawn default, this prints
/// one error and returns `false` so the command can exit; a later parallel
/// stage would otherwise panic with `GlobalPoolAlreadyInitialized`.
pub(crate) fn configure_global_pool() -> bool {
    match rayon::ThreadPoolBuilder::new()
        .spawn_handler(spawn_pool_thread)
        .build_global()
    {
        Ok(()) => true,
        Err(e) => {
            eprintln!("{}", pool_build_error_line(&e));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An injected spawn that refuses every size in `refuse` and otherwise
    /// spawns a real thread at that size, recording what it was asked for.
    fn refusing(
        refuse: Vec<Option<usize>>,
        asked: Arc<Mutex<Vec<Option<usize>>>>,
    ) -> impl FnMut(Option<usize>, Job) -> io::Result<std::thread::JoinHandle<()>> {
        move |size, job| {
            asked.lock().unwrap().push(size);
            if refuse.contains(&size) {
                return Err(io::Error::other("refused"));
            }
            let mut builder = std::thread::Builder::new();
            if let Some(size) = size {
                builder = builder.stack_size(size);
            }
            builder.spawn(job)
        }
    }

    #[test]
    fn the_job_runs_once_at_the_first_size_that_spawns() {
        let runs = Arc::new(AtomicUsize::new(0));
        let asked = Arc::new(Mutex::new(Vec::new()));
        let counted = Arc::clone(&runs);
        let (handle, size) = spawn_fallback(
            &STACK_FALLBACK,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
            refusing(vec![Some(ANALYSIS_STACK_BYTES)], Arc::clone(&asked)),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(size, Some(64 << 20));
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            *asked.lock().unwrap(),
            vec![Some(ANALYSIS_STACK_BYTES), Some(64 << 20)]
        );
    }

    #[test]
    fn rust_spawn_default_is_the_last_pool_resort() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let refuse = vec![Some(ANALYSIS_STACK_BYTES), Some(64 << 20)];
        let (handle, size) = spawn_fallback(
            &STACK_FALLBACK,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
            refusing(refuse, Arc::new(Mutex::new(Vec::new()))),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(size, None);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn when_every_size_fails_the_job_never_runs() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let result = spawn_fallback(
            &STACK_FALLBACK,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
            refusing(STACK_FALLBACK.to_vec(), Arc::new(Mutex::new(Vec::new()))),
        );
        assert!(result.is_err());
        assert_eq!(runs.load(Ordering::SeqCst), 0);
    }

    fn exit_42() -> ExitCode {
        ExitCode::from(42)
    }

    #[test]
    fn the_command_runs_on_a_fallback_thread_or_inline() {
        // `ExitCode` has no `PartialEq`; compare its `Debug` text.
        let fallback = run_command_thread_with(
            COMMAND_THREAD_SIZES,
            exit_42,
            refusing(
                vec![Some(ANALYSIS_STACK_BYTES)],
                Arc::new(Mutex::new(Vec::new())),
            ),
        );
        assert_eq!(format!("{fallback:?}"), format!("{:?}", ExitCode::from(42)));
        let inline = run_command_thread_with(
            COMMAND_THREAD_SIZES,
            exit_42,
            refusing(
                COMMAND_THREAD_SIZES.to_vec(),
                Arc::new(Mutex::new(Vec::new())),
            ),
        );
        assert_eq!(format!("{inline:?}"), format!("{:?}", ExitCode::from(42)));
    }

    #[cfg(unix)]
    #[test]
    fn unix_command_thread_does_not_spawn_below_8_mib() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let inline = run_command_thread_with(
            COMMAND_THREAD_SIZES,
            exit_42,
            refusing(COMMAND_THREAD_SIZES.to_vec(), Arc::clone(&asked)),
        );
        assert_eq!(format!("{inline:?}"), format!("{:?}", ExitCode::from(42)));
        assert_eq!(
            *asked.lock().unwrap(),
            vec![Some(ANALYSIS_STACK_BYTES), Some(64 << 20)]
        );
        assert!(
            !asked.lock().unwrap().contains(&None),
            "Unix must not spawn the command at Rust's 2 MiB default"
        );
    }

    #[test]
    fn command_thread_is_spawned_before_the_command_body() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spawn_log = Arc::clone(&events);
        let body_log = Arc::clone(&events);
        let code = run_command_thread_with(
            &[Some(64 << 20)],
            move || {
                body_log.lock().unwrap().push("body");
                ExitCode::SUCCESS
            },
            move |size, job| {
                spawn_log.lock().unwrap().push("spawn");
                std::thread::Builder::new()
                    .stack_size(size.unwrap())
                    .spawn(job)
            },
        );
        assert_eq!(format!("{code:?}"), format!("{:?}", ExitCode::SUCCESS));
        assert_eq!(*events.lock().unwrap(), ["spawn", "body"]);
    }

    #[test]
    fn fallback_messages_name_the_actual_stack() {
        let mib64 = stack_warning(StackKind::Reserved(64 << 20));
        assert!(mib64.contains("using 64 MiB"), "{mib64}");
        assert!(!mib64.contains("platform default"), "{mib64}");

        let rust_default = stack_warning(StackKind::RustSpawnDefault);
        assert!(
            rust_default.contains("using Rust's 2 MiB spawn default"),
            "{rust_default}"
        );
        assert!(!rust_default.contains("platform default"), "{rust_default}");

        let inline = stack_warning(StackKind::ProcessMainThread);
        assert!(inline.contains("using the process main thread"), "{inline}");
        assert!(!inline.contains("platform default"), "{inline}");
    }

    #[test]
    fn a_failed_pool_build_is_one_error_line() {
        assert_eq!(
            pool_build_error_line(&"thread spawn failed"),
            "rigor: could not start the analysis thread pool: thread spawn failed"
        );
    }

    #[test]
    fn a_pool_whose_workers_fall_back_still_runs_work() {
        let asked = Arc::new(Mutex::new(Vec::new()));
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let (asked_in, sizes_in) = (Arc::clone(&asked), Arc::clone(&sizes));
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .spawn_handler(move |thread| {
                let mut spawn = refusing(vec![Some(ANALYSIS_STACK_BYTES)], Arc::clone(&asked_in));
                let size = spawn_pool_thread_with(thread, &STACK_FALLBACK, move |size, job| {
                    spawn(size, job).map(drop)
                })?;
                sizes_in.lock().unwrap().push(size);
                Ok(())
            })
            .build()
            .unwrap();
        let sum: u64 = pool.install(|| {
            use rayon::prelude::*;
            (1..=100u64).into_par_iter().sum()
        });
        assert_eq!(sum, 5050);
        assert_eq!(*sizes.lock().unwrap(), vec![Some(64 << 20); 2]);
        assert_eq!(asked.lock().unwrap().len(), 4);
    }
}
