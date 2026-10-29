//! Bounded, status-only fan-out. No output, screen replay or socket IO runs here.
use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use super::control::{ControlEvent, LinkShutdown, PaneEventMetadata, server_instance};
use super::protocol::DaemonMsg;

const EVENT_QUEUE: usize = 1024;

#[derive(Clone, Default)]
pub struct PaneEventHub(Arc<Mutex<HubState>>);

#[derive(Default)]
struct HubState {
    next_subscription: u64,
    sequence: u64,
    subscribers: HashMap<u64, Subscriber>,
}

struct Subscriber {
    tx: smol::channel::Sender<ControlEvent>,
    shutdown: Option<Arc<dyn LinkShutdown>>,
}

pub struct PaneEventSubscription {
    id: u64,
    hub: Weak<Mutex<HubState>>,
    pub(crate) receiver: smol::channel::Receiver<ControlEvent>,
}

impl PaneEventSubscription {
    pub(crate) fn disconnect_on_overflow(&self, shutdown: Arc<dyn LinkShutdown>) {
        if let Some(hub) = self.hub.upgrade()
            && let Some(subscriber) = hub.lock().unwrap().subscribers.get_mut(&self.id)
        {
            subscriber.shutdown = Some(shutdown);
        }
    }
}

impl Drop for PaneEventSubscription {
    fn drop(&mut self) {
        self.receiver.close();
        if let Some(hub) = self.hub.upgrade() {
            hub.lock().unwrap().subscribers.remove(&self.id);
        }
    }
}

impl PaneEventHub {
    pub fn subscribe(&self) -> PaneEventSubscription {
        let (tx, receiver) = smol::channel::bounded(EVENT_QUEUE);
        let mut state = self.0.lock().unwrap();
        state.next_subscription += 1;
        let id = state.next_subscription;
        state
            .subscribers
            .insert(id, Subscriber { tx, shutdown: None });
        PaneEventSubscription {
            id,
            hub: Arc::downgrade(&self.0),
            receiver,
        }
    }

    pub(crate) fn publish(&self, pane_id: u64, msg: &DaemonMsg) -> Option<PaneEventMetadata> {
        if !matches!(msg, DaemonMsg::Exited { .. } | DaemonMsg::AgentStatus(_)) {
            return None;
        }
        // Serializing publication under one lock makes sequence order identical
        // for every receiver, even when different panes report concurrently.
        let mut state = self.0.lock().unwrap();
        state.sequence += 1;
        let metadata = PaneEventMetadata {
            instance: server_instance().to_string(),
            sequence: state.sequence,
            timestamp_ms: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64,
        };
        let event = match msg {
            DaemonMsg::Exited { code } => ControlEvent::PaneExited {
                pane_id,
                code: *code,
                metadata: metadata.clone(),
            },
            DaemonMsg::AgentStatus(session) => ControlEvent::AgentStatus {
                pane_id,
                json: serde_json::to_value(session).expect("agent state is JSON serializable"),
                metadata: metadata.clone(),
            },
            _ => unreachable!(),
        };
        let mut disconnect = Vec::new();
        state.subscribers.retain(|_, subscriber| {
            if subscriber.tx.try_send(event.clone()).is_ok() {
                return true;
            }
            subscriber.tx.close();
            if let Some(shutdown) = &subscriber.shutdown {
                disconnect.push(Arc::clone(shutdown));
            }
            false
        });
        drop(state);
        for shutdown in disconnect {
            // A forwarder may be blocked inside a socket write. Cancel from a
            // separate thread, never under the hub/pane locks or on that writer.
            let result = std::thread::Builder::new()
                .name("tty7-pane-event-disconnect".into())
                .spawn(move || {
                    let _ = shutdown.shutdown_link();
                });
            if let Err(error) = result {
                log::warn!("could not disconnect a lagging pane event consumer: {error}");
            }
        }
        Some(metadata)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_only_events_are_ordered_and_broadcast() {
        let hub = PaneEventHub::default();
        let a = hub.subscribe();
        let b = hub.subscribe();
        let _ = hub.publish(3, &DaemonMsg::Output(b"not an event".to_vec()));
        assert!(a.receiver.is_empty());
        let _ = hub.publish(3, &DaemonMsg::AgentStatus(None));
        let _ = hub.publish(4, &DaemonMsg::Exited { code: Some(7) });
        for subscription in [&a, &b] {
            let first = subscription.receiver.try_recv().unwrap();
            let second = subscription.receiver.try_recv().unwrap();
            let ControlEvent::AgentStatus { json, metadata, .. } = first else {
                panic!("expected agent status");
            };
            assert!(json.is_null());
            assert_eq!(metadata.sequence, 1);
            assert!(!metadata.instance.is_empty());
            assert!(metadata.timestamp_ms > 0);
            assert!(matches!(second, ControlEvent::PaneExited {
                pane_id: 4, code: Some(7), metadata
            } if metadata.sequence == 2));
        }
    }

    #[test]
    fn concurrent_panes_have_the_same_order_for_every_consumer() {
        let hub = PaneEventHub::default();
        let a = hub.subscribe();
        let b = hub.subscribe();
        std::thread::scope(|scope| {
            for pane in 0..4 {
                let hub = hub.clone();
                scope.spawn(move || {
                    for _ in 0..64 {
                        let _ = hub.publish(pane, &DaemonMsg::Exited { code: None });
                    }
                });
            }
        });
        for sequence in 1..=256 {
            let first = a.receiver.try_recv().unwrap();
            assert_eq!(first, b.receiver.try_recv().unwrap());
            assert!(matches!(first, ControlEvent::PaneExited { metadata, .. }
                if metadata.sequence == sequence));
        }
    }

    #[test]
    fn slow_receivers_are_closed_without_blocking_other_receivers() {
        let hub = PaneEventHub::default();
        let slow = hub.subscribe();
        let fast = hub.subscribe();
        for _ in 0..=EVENT_QUEUE {
            let _ = hub.publish(3, &DaemonMsg::Exited { code: None });
            fast.receiver.try_recv().unwrap();
        }
        assert!(slow.receiver.is_closed());
        assert!(!fast.receiver.is_closed());
        assert_eq!(hub.0.lock().unwrap().subscribers.len(), 1);
        drop(fast);
        assert!(hub.0.lock().unwrap().subscribers.is_empty());
    }

    #[test]
    fn dropping_subscription_wakes_its_forwarder() {
        let hub = PaneEventHub::default();
        let subscription = hub.subscribe();
        let rx = subscription.receiver.clone();
        drop(subscription);
        assert!(rx.recv_blocking().is_err());
        assert!(hub.0.lock().unwrap().subscribers.is_empty());
    }
}
