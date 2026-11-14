//! Suspend/resume (`NtSuspendProcess` / `NtResumeProcess`, what Process Explorer uses; suspend
//! counts nest) and Job Objects for subtree kills.

use windows::Win32::Foundation::{HANDLE, NTSTATUS};
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, TerminateJobObject,
};
use windows::core::PCWSTR;

use super::handle::OwnedHandle;
use crate::pin::{Job, JobHandle, PinHandle, Pinned};
use crate::{CoreError, Result};

// Audited declarations: not in the `windows` crate. Both take a process handle opened with
// PROCESS_SUSPEND_RESUME and return an NTSTATUS.
windows_link::link!("ntdll.dll" "system" fn NtSuspendProcess(processhandle: HANDLE) -> NTSTATUS);
windows_link::link!("ntdll.dll" "system" fn NtResumeProcess(processhandle: HANDLE) -> NTSTATUS);

fn proc_handle(p: &Pinned) -> Result<HANDLE> {
    match &p.handle {
        PinHandle::Windows(h) => Ok(h.raw()),
        PinHandle::Fake => Err(CoreError::Unsupported),
    }
}

fn job_handle(j: &Job) -> Result<HANDLE> {
    match &j.handle {
        JobHandle::Windows(h) => Ok(h.raw()),
        JobHandle::Fake(_) => Err(CoreError::Unsupported),
    }
}

fn nt(api: &'static str, st: NTSTATUS) -> Result<()> {
    if st.is_ok() {
        Ok(())
    } else {
        Err(CoreError::Os { api, code: st.0 })
    }
}

fn win(api: &'static str, e: &windows::core::Error) -> CoreError {
    CoreError::Os {
        api,
        code: e.code().0 & 0xFFFF,
    }
}

pub(crate) fn suspend(p: &Pinned) -> Result<()> {
    let h = proc_handle(p)?;
    // SAFETY: a live process handle; `pin` requested PROCESS_SUSPEND_RESUME for this access.
    nt("NtSuspendProcess", unsafe { NtSuspendProcess(h) })
}

pub(crate) fn resume(p: &Pinned) -> Result<()> {
    let h = proc_handle(p)?;
    // SAFETY: as in `suspend`.
    nt("NtResumeProcess", unsafe { NtResumeProcess(h) })
}

pub(crate) fn create() -> Result<Job> {
    // SAFETY: default security, unnamed; the handle is owned by `OwnedHandle`.
    let h = unsafe { CreateJobObjectW(None, PCWSTR::null()) }
        .map_err(|e| win("CreateJobObjectW", &e))?;
    Ok(Job {
        handle: JobHandle::Windows(OwnedHandle::new(h)),
    })
}

pub(crate) fn assign(job: &Job, p: &Pinned) -> Result<()> {
    let (j, h) = (job_handle(job)?, proc_handle(p)?);
    // SAFETY: live job and process handles (process opened with SET_QUOTA | TERMINATE).
    unsafe { AssignProcessToJobObject(j, h) }.map_err(|e| win("AssignProcessToJobObject", &e))
}

pub(crate) fn terminate(job: &Job) -> Result<()> {
    let j = job_handle(job)?;
    // SAFETY: a live job handle.
    unsafe { TerminateJobObject(j, 1) }.map_err(|e| win("TerminateJobObject", &e))
}
