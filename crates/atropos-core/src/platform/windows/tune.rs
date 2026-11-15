//! Priority class, affinity and `EcoQoS` through a pinned handle (spec Phase 6).
//! The pin carries `PROCESS_SET_INFORMATION` (see `pin::mask` with `Access::tune`).
//!
//! - [`SetPriorityClass`] / [`GetPriorityClass`].
//! - [`SetProcessAffinityMask`] / [`GetProcessAffinityMask`]: one processor group only; with
//!   more than one active group (> 64 logical CPUs) this refuses with `Unsupported` rather than
//!   apply a mask that would mean something else.
//! - [`SetProcessInformation`]`(ProcessPowerThrottling)` with `EXECUTION_SPEED` (`EcoQoS`,
//!   Windows 11; Windows 10 1709+ accepts it as plain throttling). Reading it back with
//!   `GetProcessInformation` needs Windows 11; earlier, `eco` is `None`.

use std::ffi::c_void;

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::Threading::{
    ABOVE_NORMAL_PRIORITY_CLASS, BELOW_NORMAL_PRIORITY_CLASS, GetActiveProcessorGroupCount,
    GetPriorityClass, GetProcessAffinityMask, GetProcessInformation, HIGH_PRIORITY_CLASS,
    IDLE_PRIORITY_CLASS, NORMAL_PRIORITY_CLASS, PROCESS_CREATION_FLAGS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, ProcessPowerThrottling, REALTIME_PRIORITY_CLASS,
    SetPriorityClass, SetProcessAffinityMask, SetProcessInformation,
};

use super::pin::raw;
use crate::pin::Pinned;
use crate::tune::{Priority, Tune, Tuning};
use crate::{CoreError, Result};

fn os(api: &'static str, e: &windows::core::Error) -> CoreError {
    CoreError::Os {
        api,
        code: e.code().0,
    }
}

fn class(p: Priority) -> PROCESS_CREATION_FLAGS {
    match p {
        Priority::Idle => IDLE_PRIORITY_CLASS,
        Priority::BelowNormal => BELOW_NORMAL_PRIORITY_CLASS,
        Priority::Normal => NORMAL_PRIORITY_CLASS,
        Priority::AboveNormal => ABOVE_NORMAL_PRIORITY_CLASS,
        Priority::High => HIGH_PRIORITY_CLASS,
        Priority::Realtime => REALTIME_PRIORITY_CLASS,
    }
}

fn single_group() -> bool {
    // SAFETY: no preconditions.
    (unsafe { GetActiveProcessorGroupCount() }) <= 1
}

fn throttling(on: bool) -> PROCESS_POWER_THROTTLING_STATE {
    PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        // Off = take the bit out of the control mask: Windows decides again.
        ControlMask: if on {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
        StateMask: if on {
            PROCESS_POWER_THROTTLING_EXECUTION_SPEED
        } else {
            0
        },
    }
}

#[allow(clippy::cast_possible_truncation)] // a 12-byte struct
const STATE_LEN: u32 = std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32;

pub(crate) fn tune(p: &Pinned, t: Tune) -> Result<()> {
    let h: HANDLE = raw(p).ok_or(CoreError::Unsupported)?;
    match t {
        Tune::Priority(Priority::Realtime) => Err(CoreError::Unsupported),
        // SAFETY: a live handle with PROCESS_SET_INFORMATION (pinned with `tune`).
        Tune::Priority(pr) => {
            unsafe { SetPriorityClass(h, class(pr)) }.map_err(|e| os("SetPriorityClass", &e))
        }
        Tune::Affinity(_) if !single_group() => Err(CoreError::Unsupported),
        Tune::Affinity(mask) => {
            let mask = usize::try_from(mask).map_err(|_| CoreError::Unsupported)?;
            // SAFETY: as above; a zero mask is refused by the caller and by the OS.
            unsafe { SetProcessAffinityMask(h, mask) }.map_err(|e| os("SetProcessAffinityMask", &e))
        }
        Tune::Eco(on) => {
            let state = throttling(on);
            // SAFETY: `state` is a valid PROCESS_POWER_THROTTLING_STATE of STATE_LEN bytes.
            unsafe {
                SetProcessInformation(
                    h,
                    ProcessPowerThrottling,
                    (&raw const state).cast::<c_void>(),
                    STATE_LEN,
                )
            }
            .map_err(|e| os("SetProcessInformation", &e))
        }
    }
}

pub(crate) fn tuning(p: &Pinned) -> Result<Tuning> {
    let h: HANDLE = raw(p).ok_or(CoreError::Unsupported)?;
    // SAFETY: a live handle with PROCESS_QUERY_LIMITED_INFORMATION.
    let c = unsafe { GetPriorityClass(h) };
    let priority = [
        Priority::Idle,
        Priority::BelowNormal,
        Priority::Normal,
        Priority::AboveNormal,
        Priority::High,
        Priority::Realtime,
    ]
    .into_iter()
    .find(|p| class(*p).0 == c)
    .ok_or(CoreError::Os {
        api: "GetPriorityClass",
        code: 0,
    })?;
    let (mut proc_mask, mut sys_mask) = (0usize, 0usize);
    // SAFETY: as above; both out pointers are valid.
    unsafe { GetProcessAffinityMask(h, &raw mut proc_mask, &raw mut sys_mask) }
        .map_err(|e| os("GetProcessAffinityMask", &e))?;
    let mut state = throttling(false);
    // SAFETY: `state` is a writable PROCESS_POWER_THROTTLING_STATE of STATE_LEN bytes.
    let eco = unsafe {
        GetProcessInformation(
            h,
            ProcessPowerThrottling,
            (&raw mut state).cast::<c_void>(),
            STATE_LEN,
        )
    }
    .ok()
    .map(|()| state.StateMask & PROCESS_POWER_THROTTLING_EXECUTION_SPEED != 0);
    Ok(Tuning {
        priority,
        affinity: proc_mask as u64,
        system_affinity: sys_mask as u64,
        eco,
    })
}
