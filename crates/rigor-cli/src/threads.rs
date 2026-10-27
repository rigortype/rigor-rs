//! The analysis threads' stacks (rigor-rs#412, ADR-0045).
//!
//! Every thread that parses, lowers or infers runs on a reserved
//! [`ANALYSIS_STACK_BYTES`] stack: the command thread [`run_command_thread`]
//! starts, and every rayon worker ([`configure_global_pool`],
//! `coverage --workers`). A host that cannot reserve that much falls back to
//! 64 MiB, then to the platform default. A large stack never stops a command
//! from starting.

use std::io;
use std::process::ExitCode;
use std::sync::{Arc, Mutex, Once, PoisonError};

use rigor_parse::ANALYSIS_STACK_BYTES;

/// One spawn attempt's closure.
pub(crate) type Job = Box<dyn FnOnce() + Send + 'static>;

/// The stack sizes tried in turn; `None` is the platform default.
pub(crate) const STACK_FALLBACK: [Option<usize>; 3] =
    [Some(ANALYSIS_STACK_BYTES), Some(64 << 20), None];

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

/// One stderr line per process when a thread got less than
/// [`ANALYSIS_STACK_BYTES`]. Silent on the normal path.
fn note_stack(size: Option<usize>) {
    static WARNED: Once = Once::new();
    if size == Some(ANALYSIS_STACK_BYTES) {
        return;
    }
    WARNED.call_once(|| {
        let got = size.map_or_else(|| "the platform default".to_string(), |s| format!("{} MiB", s >> 20));
        eprintln!(
            "rigor: could not reserve {} MiB thread stacks, using {got}; \
             deeply nested files may overflow",
            ANALYSIS_STACK_BYTES >> 20
        );
    });
}

/// Run `command` on the command thread: named `main`, so panic and overflow
/// messages read as they did when the command ran on the process's main
/// thread. If no thread can be created at all, it runs on the main thread
/// itself.
pub(crate) fn run_command_thread(command: fn() -> ExitCode) -> ExitCode {
    run_command_thread_with(&STACK_FALLBACK, command, |size, job| {
        let mut builder = std::thread::Builder::new().name("main".to_string());
        if let Some(size) = size {
            builder = builder.stack_size(size);
        }
        builder.spawn(job)
    })
}

/// [`run_command_thread`] with the spawn injected (tests).
fn run_command_thread_with<S>(
    sizes: &[Option<usize>],
    command: fn() -> ExitCode,
    spawn: S,
) -> ExitCode
where
    S: FnMut(Option<usize>, Job) -> io::Result<std::thread::JoinHandle<()>>,
{
    let code = Arc::new(Mutex::new(None));
    let out = Arc::clone(&code);
    match spawn_fallback(sizes, move || *out.lock().unwrap_or_else(PoisonError::into_inner) = Some(command()), spawn) {
        Ok((handle, size)) => {
            note_stack(size);
            if let Err(payload) = handle.join() {
                std::panic::resume_unwind(payload);
            }
            let code = code.lock().unwrap_or_else(PoisonError::into_inner).take();
            code.unwrap_or(ExitCode::FAILURE)
        }
        Err(_) => {
            note_stack(None);
            command()
        }
    }
}

/// The rayon `spawn_handler` for every analysis pool: each worker gets the
/// first stack size the host can reserve.
pub(crate) fn spawn_pool_thread(thread: rayon::ThreadBuilder) -> io::Result<()> {
    let name = thread.name().map(str::to_string);
    let size = spawn_pool_thread_with(thread, &STACK_FALLBACK, std_spawn(name))?;
    note_stack(size);
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

/// Build the global rayon pool with [`spawn_pool_thread`]. It must run before
/// anything uses rayon.
///
/// `build_global` is one-shot: rayon marks the global registry as set even
/// when building it fails, so it is never retried with other settings. The
/// stack fallback happens per thread, inside the spawn handler. It only fails
/// when no thread can be created even at the platform default; the command
/// then still starts, and a later parallel stage fails as it would have on
/// master.
pub(crate) fn configure_global_pool() {
    if let Err(e) = rayon::ThreadPoolBuilder::new().spawn_handler(spawn_pool_thread).build_global() {
        eprintln!("rigor: could not start the analysis thread pool: {e}");
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
        assert_eq!(*asked.lock().unwrap(), vec![Some(ANALYSIS_STACK_BYTES), Some(64 << 20)]);
    }

    #[test]
    fn the_platform_default_is_the_last_resort() {
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
            &STACK_FALLBACK,
            exit_42,
            refusing(vec![Some(ANALYSIS_STACK_BYTES)], Arc::new(Mutex::new(Vec::new()))),
        );
        assert_eq!(format!("{fallback:?}"), format!("{:?}", ExitCode::from(42)));
        let inline = run_command_thread_with(
            &STACK_FALLBACK,
            exit_42,
            refusing(STACK_FALLBACK.to_vec(), Arc::new(Mutex::new(Vec::new()))),
        );
        assert_eq!(format!("{inline:?}"), format!("{:?}", ExitCode::from(42)));
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
