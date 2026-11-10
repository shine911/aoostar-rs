// SPDX-License-Identifier: MIT OR Apache-2.0

//! Ties the sensor children's lifetime to the launcher's, so a crashed or
//! killed launcher (Task Manager "End task", `kill -9`, a panic in a
//! foreign callback) cannot leave them running hidden — and, on Windows,
//! elevated. Before this, a relaunch after such a death started a second
//! aster-sysinfo / HwBridge next to the orphaned one: two writers on the
//! same `AOOSTAR_HW_STATS` seqlock slot.
//!
//! - Windows: one Job Object with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`,
//!   created at startup ([`init`]); each sensor child is assigned to it
//!   right after spawn ([`adopt`]). The launcher holds the only handle
//!   (not inheritable) and never closes it, so the kernel closes it — and
//!   kills every member — exactly when the launcher process dies, however
//!   it dies. A clean quit is unchanged: the watchers kill the children
//!   first.
//! - Linux: `PR_SET_PDEATHSIG(SIGTERM)` in the child between fork and exec
//!   ([`configure_command`]).
//!
//! `asterctl` is deliberately NOT tied to the launcher: when the launcher
//! dies it must stay alive long enough to notice the stale `display.state`
//! heartbeat and blank the LCD (CloseTFT) before exiting on its own (within
//! ~10s, `LAUNCHER_HEARTBEAT_TIMEOUT` in asterctl). Killing it together
//! with the launcher would freeze the last frame on the panel forever.
//!
//! Failures are logged and otherwise ignored: a child outside the job is
//! exactly the previous behavior, never a reason to not start it.
//!
//! A previous launcher's orphans are NOT hunted down at startup: the only
//! handle on them is their image name, and killing every `aster-sysinfo` /
//! `HwBridge.exe` could hit instances the user started by hand (debugging,
//! another install). With the job object in place new orphans no longer
//! appear, and the single-instance lock already prevents two live
//! launchers.

// `main.rs` denies `unsafe_code` crate-wide; this module is the narrow,
// deliberate exception — Win32 job-object FFI and the Linux `pre_exec`
// hook — scoped the same way as `power.rs` and `display.rs`.
#![allow(unsafe_code)]

/// True for the children that must die with the launcher: the sensor
/// producers. `asterctl` is excluded on purpose (see the module docs: it
/// blanks the LCD after the launcher dies, then exits by itself).
#[cfg_attr(not(any(windows, target_os = "linux")), allow(dead_code))]
pub(crate) fn dies_with_launcher(child_name: &str) -> bool {
    matches!(child_name, "aster-sysinfo" | "hwbridge")
}

/// The launcher's kill-on-close job, as a `usize` because a raw `HANDLE`
/// is not `Send`/`Sync`. `None` inside when creation failed (logged once).
/// Never closed on purpose: closing the last handle is what kills the
/// members, which must happen only when the launcher process itself ends.
#[cfg(windows)]
static JOB: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();

/// Creates the launcher's kill-on-close Job Object. Call once at startup,
/// before any child is spawned; later calls are no-ops. Never fails: on an
/// error it logs to `log_path` and children simply run outside a job.
#[cfg(windows)]
pub(crate) fn init(log_path: &std::path::Path) {
    JOB.get_or_init(|| create_kill_on_close_job(log_path));
}

#[cfg(windows)]
fn create_kill_on_close_job(log_path: &std::path::Path) -> Option<usize> {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    // SAFETY: plain Win32 calls with valid arguments: null security
    // attributes (→ handle not inheritable, so children cannot keep the job
    // alive) and no name (private job); `info` outlives the call and its
    // size is passed exactly.
    unsafe {
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job.is_null() {
            crate::logging::append_line(
                log_path,
                &format!(
                    "job: CreateJobObjectW failed ({}); children will not be \
                     killed if the launcher crashes",
                    std::io::Error::last_os_error()
                ),
            );
            return None;
        }
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            crate::logging::append_line(
                log_path,
                &format!(
                    "job: SetInformationJobObject(KILL_ON_JOB_CLOSE) failed ({}); \
                     children will not be killed if the launcher crashes",
                    std::io::Error::last_os_error()
                ),
            );
            CloseHandle(job);
            return None;
        }
        Some(job as usize)
    }
}

/// Assigns a freshly spawned child to the launcher's kill-on-close job if
/// it is one of the [`dies_with_launcher`] children. No-op when [`init`]
/// was not called or failed. A failure (e.g. a policy forbidding nested
/// jobs) is logged to the child's log; the child keeps running unassigned.
///
/// The child runs for a moment before the assignment; that window only
/// matters if the launcher dies within it, and neither sensor child spawns
/// processes of its own that could escape.
#[cfg(windows)]
pub(crate) fn adopt(child: &std::process::Child, child_name: &str, log_path: &std::path::Path) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

    if !dies_with_launcher(child_name) {
        return;
    }
    let Some(Some(job)) = JOB.get().copied() else {
        return;
    };
    // SAFETY: `job` is the live job handle created in `init` (never
    // closed); the process handle belongs to `child`, which is borrowed for
    // the duration of the call. CreateProcess handles carry
    // PROCESS_ALL_ACCESS, which includes the PROCESS_SET_QUOTA |
    // PROCESS_TERMINATE rights the call needs.
    let ok = unsafe {
        AssignProcessToJobObject(
            job as windows_sys::Win32::Foundation::HANDLE,
            child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE,
        )
    };
    if ok == 0 {
        crate::logging::append_line(
            log_path,
            &format!(
                "job: could not tie {child_name} to the launcher ({}); it may \
                 outlive a crashed launcher",
                std::io::Error::last_os_error()
            ),
        );
    }
}

/// Linux: makes the kernel send SIGTERM to the child when the launcher
/// dies, for the [`dies_with_launcher`] children.
///
/// Caveat: `PR_SET_PDEATHSIG` fires when the *thread* that forked the child
/// exits, not the process. Children are spawned by their watcher thread
/// (`process::spawn_and_watch`), which lives until the launcher quits and
/// only exits after it has killed its child itself, so this matches
/// "launcher died".
///
/// The hook also closes the fork/prctl race: if the launcher died before
/// `prctl` took effect, the child has already been reparented, so it
/// fails the exec instead of running orphaned.
#[cfg(target_os = "linux")]
pub(crate) fn configure_command(command: &mut std::process::Command, child_name: &str) {
    use std::os::unix::process::CommandExt;

    if !dies_with_launcher(child_name) {
        return;
    }
    // Read before the fork; the closure must stay allocation-free.
    // SAFETY: getpid has no preconditions.
    let launcher_pid = unsafe { libc::getpid() };
    // SAFETY: the closure runs in the forked child before exec and only
    // makes async-signal-safe calls (prctl, getppid) and builds io::Errors
    // from raw OS codes, which does not allocate.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(
                libc::PR_SET_PDEATHSIG,
                libc::SIGTERM as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
                0 as libc::c_ulong,
            ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            if libc::getppid() != launcher_pid {
                return Err(std::io::Error::from_raw_os_error(libc::ESRCH));
            }
            Ok(())
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_sensor_children_die_with_the_launcher() {
        assert!(dies_with_launcher("aster-sysinfo"));
        assert!(dies_with_launcher("hwbridge"));
        // asterctl must outlive a dead launcher to blank the LCD.
        assert!(!dies_with_launcher("asterctl"));
        assert!(!dies_with_launcher("unknown"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pdeathsig_terminates_the_child_when_its_spawning_thread_exits() {
        use std::os::unix::process::ExitStatusExt;

        // Spawn from a short-lived thread: PR_SET_PDEATHSIG fires when that
        // thread exits (the same mechanism that fires when the launcher
        // dies), so the child must be SIGTERMed long before `sleep 30` ends.
        let mut child = std::thread::spawn(|| {
            let mut command = std::process::Command::new("sleep");
            command.arg("30");
            configure_command(&mut command, "aster-sysinfo");
            command.spawn().unwrap()
        })
        .join()
        .unwrap();

        let started = std::time::Instant::now();
        let status = child.wait().unwrap();
        assert_eq!(status.signal(), Some(libc::SIGTERM));
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn asterctl_gets_no_death_signal() {
        // Same setup, but asterctl must survive its spawner and exit on its
        // own (it blanks the LCD after the launcher is gone).
        let mut child = std::thread::spawn(|| {
            let mut command = std::process::Command::new("sleep");
            command.arg("1");
            configure_command(&mut command, "asterctl");
            command.spawn().unwrap()
        })
        .join()
        .unwrap();
        assert!(child.wait().unwrap().success());
    }
}
