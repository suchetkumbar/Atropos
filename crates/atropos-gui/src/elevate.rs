//! "Retry as administrator": one background thread owns the helper session. The first retry
//! launches `Atropos.exe --elevated-helper` through UAC (which may wait on the user); later
//! retries reuse the session until the helper idles out, then the next one relaunches.

use crossbeam_channel::{Receiver, Sender};

use atropos_core::ProcKey;
use atropos_helper::client::{Session, launch};
use atropos_helper::protocol::Reply;

use crate::toast::Retry;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevatedEvent {
    Done(ProcKey, Reply),
    /// UAC declined, helper missing, or the pipe broke.
    Failed(String),
}

pub struct Elevated {
    tx: Sender<Retry>,
    pub events: Receiver<ElevatedEvent>,
}

impl Elevated {
    /// Starts the worker thread (the helper itself starts on the first retry).
    #[must_use]
    pub fn spawn(me: ProcKey, notify: Box<dyn Fn() + Send>) -> Self {
        let (tx, rx) = crossbeam_channel::unbounded::<Retry>();
        let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
        let _ = std::thread::Builder::new()
            .name("atropos-elevated".into())
            .spawn(move || {
                let mut session: Option<Session> = None;
                for retry in rx {
                    if session.is_none() {
                        match std::env::current_exe().and_then(|exe| launch(&exe, me)) {
                            Ok(s) => session = Some(s),
                            Err(e) => {
                                tracing::warn!("elevation: {e}");
                                let _ = ev_tx.send(ElevatedEvent::Failed(e.to_string()));
                                notify();
                                continue;
                            }
                        }
                    }
                    for key in retry.keys {
                        let Some(s) = session.as_mut() else { break };
                        let ev = match s.send(retry.op.clone(), key) {
                            Ok(reply) => ElevatedEvent::Done(key, reply),
                            Err(e) => {
                                session = None; // helper gone (idle) or broken: relaunch next time
                                ElevatedEvent::Failed(e.to_string())
                            }
                        };
                        let _ = ev_tx.send(ev);
                        notify();
                    }
                }
            });
        Self { tx, events: ev_rx }
    }

    pub fn request(&self, r: Retry) {
        let _ = self.tx.send(r);
    }
}
