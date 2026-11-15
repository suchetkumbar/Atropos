//! Stand-in for `elevate.rs` outside Windows: there is no elevated helper (the UAC + named
//! pipe design is Windows-only). Actions never offer the retry there; a request that still
//! arrives answers with a clear failure.

use crossbeam_channel::{Receiver, Sender};

use atropos_core::ProcKey;
use atropos_helper::protocol::Reply;

use crate::toast::Retry;

#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)] // the shared `Actions` code matches on both variants
pub enum ElevatedEvent {
    Done(ProcKey, Reply),
    Failed(String),
}

pub struct Elevated {
    tx: Sender<ElevatedEvent>,
    pub events: Receiver<ElevatedEvent>,
}

impl Elevated {
    #[must_use]
    pub fn spawn(_me: ProcKey, _notify: Box<dyn Fn() + Send>) -> Self {
        let (tx, events) = crossbeam_channel::unbounded();
        Self { tx, events }
    }

    pub fn request(&self, _r: Retry) {
        let _ = self.tx.send(ElevatedEvent::Failed(
            "Retrying with administrator rights is available on Windows only.".into(),
        ));
    }
}
