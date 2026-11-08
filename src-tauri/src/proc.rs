//! Starting and stopping the helper programs (yt-dlp and the FFmpeg and
//! Deno processes it starts) the same way on macOS and Windows.

#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, BorrowedHandle, FromRawHandle, OwnedHandle, RawHandle};
use tokio::process::{Child, Command};
#[cfg(windows)]
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
    TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{TerminateProcess, CREATE_NO_WINDOW};

/// Keeps a black console window from flashing up for every helper program
/// on Windows. The processes yt-dlp starts share its hidden console (and
/// yt-dlp hides their windows too), so only Ambar's own commands need this.
pub fn hide_console(cmd: &mut Command) {
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    #[cfg(not(windows))]
    let _ = cmd;
}

/// A started program together with every process it starts, so they can
/// be stopped as one: yt-dlp and the FFmpeg and Deno processes it runs. A
/// process group on macOS, a Job Object on Windows.
pub struct ProcessTree {
    #[cfg(unix)]
    pgid: i32,
    #[cfg(windows)]
    target: Target,
}

impl ProcessTree {
    /// Spawns `cmd` as the root of a new tree. The tree is None only when
    /// the program could not be tracked (it already exited).
    pub fn spawn(cmd: &mut Command) -> std::io::Result<(Child, Option<ProcessTree>)> {
        #[cfg(unix)]
        cmd.process_group(0);
        let child = cmd.spawn()?;
        let tree = ProcessTree::attach(&child);
        Ok((child, tree))
    }

    /// Asks every process in the tree to finish. Windows has no such request
    /// for a program without a console window, so there they stop at once;
    /// yt-dlp's partial files stay either way, so "Tekrar dene" resumes.
    pub fn stop(&self) {
        #[cfg(unix)]
        self.signal(libc::SIGTERM);
        #[cfg(windows)]
        self.terminate();
    }

    /// Stops every process in the tree at once.
    pub fn kill(&self) {
        #[cfg(unix)]
        self.signal(libc::SIGKILL);
        #[cfg(windows)]
        self.terminate();
    }
}

#[cfg(unix)]
impl ProcessTree {
    fn attach(child: &Child) -> Option<ProcessTree> {
        // process_group(0) made the child the leader of a new group, so the
        // group id is its pid.
        child.id().map(|pid| pid as i32).filter(|pid| *pid > 0).map(|pgid| ProcessTree { pgid })
    }

    fn signal(&self, signal: i32) {
        // The negative id reaches the whole group, so this also reaches the
        // ffmpeg processes yt-dlp spawned.
        // SAFETY: kill() only takes integers; a group that is already gone
        // just makes it fail with ESRCH.
        unsafe {
            libc::kill(-self.pgid, signal);
        }
    }
}

#[cfg(windows)]
enum Target {
    /// A Job Object holding the program and every process it starts later.
    /// Closing its last handle kills them all, so nothing is left running
    /// even if Ambar itself crashes.
    Job(OwnedHandle),
    /// The program alone, for the unlikely case that no job could be made.
    Process(OwnedHandle),
}

#[cfg(windows)]
impl ProcessTree {
    fn attach(child: &Child) -> Option<ProcessTree> {
        let process = child.raw_handle()?;
        let target = match new_job(process) {
            Some(job) => Target::Job(job),
            None => {
                // SAFETY: the handle belongs to `child`, which outlives this
                // borrow; the clone is a separate handle we own.
                let borrowed = unsafe { BorrowedHandle::borrow_raw(process) };
                Target::Process(borrowed.try_clone_to_owned().ok()?)
            }
        };
        Some(ProcessTree { target })
    }

    fn terminate(&self) {
        // SAFETY: the handle stays open for as long as `self` lives.
        let ok = unsafe {
            match &self.target {
                Target::Job(job) => TerminateJobObject(job.as_raw_handle(), 1),
                Target::Process(process) => TerminateProcess(process.as_raw_handle(), 1),
            }
        };
        // Also fails when the program already exited, which is fine.
        if ok == 0 {
            eprintln!("[kova] could not stop helper: {}", std::io::Error::last_os_error());
        }
    }
}

/// A new Job Object with `process` in it, set to kill everything inside
/// once its last handle is closed. yt-dlp starts nothing until Python is up,
/// long after this ran, so its FFmpeg and Deno processes land in the job too.
#[cfg(windows)]
fn new_job(process: RawHandle) -> Option<OwnedHandle> {
    // SAFETY: both arguments may be null (default security, no name).
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw.is_null() {
        return None;
    }
    // SAFETY: `raw` is a fresh, valid handle that nothing else owns; the
    // OwnedHandle closes it on every path from here on.
    let job = unsafe { OwnedHandle::from_raw_handle(raw) };
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the pointer and size describe `limits`, which outlives the call.
    let limited = unsafe {
        SetInformationJobObject(
            job.as_raw_handle(),
            JobObjectExtendedLimitInformation,
            std::ptr::from_ref(&limits).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    // SAFETY: both handles are open; `process` belongs to the caller's child.
    if limited == 0 || unsafe { AssignProcessToJobObject(job.as_raw_handle(), process) } == 0 {
        eprintln!("[kova] job object not set up: {}", std::io::Error::last_os_error());
        return None;
    }
    Some(job)
}
