//! The analysis threads' stacks (rigor-rs#412, rigor-rs#431, ADR-0045).
//!
//! Every thread that parses, lowers or infers runs on a reserved
//! [`ANALYSIS_STACK_BYTES`] stack: the command thread [`run_command_thread`]
//! starts first and reserves its stack, then builds the rayon pool
//! ([`configure_global_pool`]) so workers cannot spend the reservation
//! budget first. `coverage --workers` uses the same spawn handler. A host
//! that cannot reserve 256 MiB falls back to 64 MiB, then:
//! - pool workers: an explicit 2 MiB stack;
//! - the command thread on Unix: the process main thread (`ulimit -s`,
//!   8 MiB by default), never a spawned thread smaller than that could be;
//! - the command thread on Windows: an explicit 2 MiB stack, then the
//!   process main thread (1 MiB).
//!
//! The command's own stack never stops a command from starting — it can
//! always fall back to the process main thread. A pool with no worker at
//! all does: it prints one error and the command exits 70 (the reference's
//! `EXIT_INTERNAL_ERROR`, rigor-rs#678); it does not continue into a later
//! `GlobalPoolAlreadyInitialized` panic. Commands that only print static
//! text (`help`, `version`, `upgrade`) skip the pool build and poison the
//! one-shot registry instead ([`disable_global_pool`]), so they still
//! answer on a host too starved for even one worker — and a skipped path
//! that reached rayon would panic rather than build a default pool.

use std::io;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, Once, PoisonError};

use rigor_parse::ANALYSIS_STACK_BYTES;

/// One spawn attempt's closure.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// The stack sizes tried in turn for pool workers. The last resort is an
/// explicit 2 MiB `stack_size`, never `thread::Builder`'s no-size default:
/// the default honours `RUST_MIN_STACK`, so it could be larger or smaller
/// than 2 MiB, and a large value could fail the last-resort spawn itself
/// (rigor-rs#679).
pub(crate) const STACK_FALLBACK: [usize; 3] =
    [ANALYSIS_STACK_BYTES, 64 << 20, 2 << 20];

/// Stack sizes tried for the command thread. On Unix the process main
/// thread is already `ulimit -s` (8 MiB by default), so we never spawn at
/// 2 MiB: if 256 MiB and 64 MiB both fail, the command runs inline.
#[cfg(unix)]
pub(crate) const COMMAND_THREAD_SIZES: &[usize] =
    &[ANALYSIS_STACK_BYTES, 64 << 20];

/// On Windows the process main thread is 1 MiB, smaller than 2 MiB, so the
/// command thread still tries that spawn before running inline.
#[cfg(not(unix))]
pub(crate) const COMMAND_THREAD_SIZES: &[usize] = &STACK_FALLBACK;

/// What a fallback actually used, for the one stderr line per process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StackKind {
    Reserved(usize),
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
    sizes: &[usize],
    job: J,
    mut spawn: S,
) -> io::Result<(H, usize)>
where
    J: FnOnce() + Send + 'static,
    S: FnMut(usize, Job) -> io::Result<H>,
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
fn std_spawn(name: Option<String>) -> impl FnMut(usize, Job) -> io::Result<()> {
    move |size, job| {
        let mut builder = std::thread::Builder::new().stack_size(size);
        if let Some(name) = &name {
            builder = builder.name(name.clone());
        }
        builder.spawn(job).map(drop)
    }
}

fn describe_stack(kind: StackKind) -> String {
    match kind {
        StackKind::Reserved(bytes) => format!("{} MiB", bytes >> 20),
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
/// thread. If no thread of at least 64 MiB (Unix) / down to an explicit
/// 2 MiB (elsewhere) can be created, it runs on the main thread itself.
pub(crate) fn run_command_thread(command: fn() -> ExitCode) -> ExitCode {
    run_command_thread_with(COMMAND_THREAD_SIZES, command, |size, job| {
        std::thread::Builder::new()
            .name("main".to_string())
            .stack_size(size)
            .spawn(job)
    })
}

/// [`run_command_thread`] with the spawn injected (tests).
fn run_command_thread_with<S, F>(sizes: &[usize], command: F, spawn: S) -> ExitCode
where
    S: FnMut(usize, Job) -> io::Result<std::thread::JoinHandle<()>>,
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
            note_stack(StackKind::Reserved(size));
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
            let code = code.lock().unwrap_or_else(PoisonError::into_inner).take();
            // Unreachable (the command runs exactly once); if reached, the
            // command never produced a code — a could-not-run, so 70.
            code.unwrap_or(ExitCode::from(crate::EXIT_INTERNAL_ERROR))
        }
        Err(_) => {
            note_stack(StackKind::ProcessMainThread);
            let command = command
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            // Unreachable (the command is taken exactly once); if reached,
            // the command never ran — a could-not-run, so 70.
            command
                .map(|c| c())
                .unwrap_or(ExitCode::from(crate::EXIT_INTERNAL_ERROR))
        }
    }
}

/// Poison the global pool for a command that never runs analysis. A failed
/// `build_global` still consumes the one-shot registry (see
/// [`configure_global_pool`]), so it leaves the registry permanently unset:
/// any rayon use after this point panics instead of silently building a
/// default pool on std's small worker stacks — the #412 regression a
/// whitelisted command that grew analysis would otherwise reintroduce
/// (rigor-rs#678). The spawn handler fails before any worker exists, so no
/// thread is spawned.
pub(crate) fn disable_global_pool() {
    let _ = rayon::ThreadPoolBuilder::new()
        .spawn_handler(|_| Err(io::Error::other("a static-text command")))
        .build_global();
}

/// The rayon `spawn_handler` for every analysis pool: each worker gets the
/// first stack size the host can reserve.
pub(crate) fn spawn_pool_thread(thread: rayon::ThreadBuilder) -> io::Result<()> {
    let name = thread.name().map(str::to_string);
    let size = spawn_pool_thread_with(thread, &STACK_FALLBACK, std_spawn(name))?;
    note_stack(StackKind::Reserved(size));
    Ok(())
}

/// [`spawn_pool_thread`] with the sizes and the spawn injected (tests).
fn spawn_pool_thread_with<S>(
    thread: rayon::ThreadBuilder,
    sizes: &[usize],
    spawn: S,
) -> io::Result<usize>
where
    S: FnMut(usize, Job) -> io::Result<()>,
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
/// worker can be created even at 2 MiB, this prints
/// one error and returns `false` so the command can exit 70
/// (`EXIT_INTERNAL_ERROR`); a later parallel stage would otherwise panic
/// with `GlobalPoolAlreadyInitialized`.
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
        refuse: Vec<usize>,
        asked: Arc<Mutex<Vec<usize>>>,
    ) -> impl FnMut(usize, Job) -> io::Result<std::thread::JoinHandle<()>> {
        move |size, job| {
            asked.lock().unwrap().push(size);
            if refuse.contains(&size) {
                return Err(io::Error::other("refused"));
            }
            std::thread::Builder::new().stack_size(size).spawn(job)
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
            refusing(vec![ANALYSIS_STACK_BYTES], Arc::clone(&asked)),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(size, 64 << 20);
        assert_eq!(runs.load(Ordering::SeqCst), 1);
        assert_eq!(
            *asked.lock().unwrap(),
            vec![ANALYSIS_STACK_BYTES, 64 << 20]
        );
    }

    #[test]
    fn an_explicit_two_mib_is_the_last_pool_resort() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let refuse = vec![ANALYSIS_STACK_BYTES, 64 << 20];
        let (handle, size) = spawn_fallback(
            &STACK_FALLBACK,
            move || {
                counted.fetch_add(1, Ordering::SeqCst);
            },
            refusing(refuse, Arc::new(Mutex::new(Vec::new()))),
        )
        .unwrap();
        handle.join().unwrap();
        assert_eq!(size, 2 << 20);
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
                vec![ANALYSIS_STACK_BYTES],
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
            vec![ANALYSIS_STACK_BYTES, 64 << 20]
        );
        assert!(
            !asked.lock().unwrap().contains(&(2 << 20)),
            "Unix must not spawn the command at an explicit 2 MiB"
        );
    }

    #[test]
    fn command_thread_is_spawned_before_the_command_body() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let spawn_log = Arc::clone(&events);
        let body_log = Arc::clone(&events);
        let code = run_command_thread_with(
            &[64 << 20],
            move || {
                body_log.lock().unwrap().push("body");
                ExitCode::SUCCESS
            },
            move |size, job| {
                spawn_log.lock().unwrap().push("spawn");
                std::thread::Builder::new().stack_size(size).spawn(job)
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

        let two_mib = stack_warning(StackKind::Reserved(2 << 20));
        assert!(two_mib.contains("using 2 MiB"), "{two_mib}");
        assert!(!two_mib.contains("platform default"), "{two_mib}");

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
                let mut spawn = refusing(vec![ANALYSIS_STACK_BYTES], Arc::clone(&asked_in));
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
        assert_eq!(*sizes.lock().unwrap(), vec![64 << 20; 2]);
        assert_eq!(asked.lock().unwrap().len(), 4);
    }
}
