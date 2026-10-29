//! Ordering for one pane's live status stream, anchored to an atomic snapshot.
use tty7_core::core::cli_agent::AgentSessionState;
use tty7_core::daemon::control::{
    ControlEvent, ControlHelloOk, PaneAgentState, PaneEventMetadata, feature,
};

pub enum Update {
    Agent(Option<PaneAgentState>),
    Exited,
    Resync,
    Ignore,
}

pub struct Cursor {
    pane: u64,
    agent: Option<tty7_core::core::cli_agent::CLIAgent>,
    watermark: PaneEventMetadata,
}

impl Cursor {
    pub fn from_snapshot(state: &PaneAgentState, hello: &ControlHelloOk) -> Option<Self> {
        let metadata = state.event_metadata.as_ref()?;
        if !hello.has_feature(feature::PANE_EVENTS)
            || metadata.instance.is_empty()
            || metadata.instance != hello.instance
            || metadata.sequence == 0
        {
            return None;
        }
        Some(Self {
            pane: state.pane_id,
            agent: state.agent,
            watermark: metadata.clone(),
        })
    }

    pub fn covers(&self, snapshot: &PaneAgentState) -> bool {
        snapshot.event_metadata.as_ref().is_some_and(|metadata| {
            metadata.instance == self.watermark.instance
                && metadata.sequence <= self.watermark.sequence
        })
    }

    pub fn apply(&mut self, event: ControlEvent) -> Update {
        let (pane, metadata) = match &event {
            ControlEvent::AgentStatus {
                pane_id, metadata, ..
            }
            | ControlEvent::PaneExited {
                pane_id, metadata, ..
            } => (*pane_id, metadata),
            _ => return Update::Ignore,
        };
        if pane != self.pane {
            return Update::Ignore;
        }
        if metadata.instance != self.watermark.instance || metadata.sequence == 0 {
            return Update::Resync;
        }
        if metadata.sequence <= self.watermark.sequence {
            return Update::Ignore;
        }
        self.watermark = metadata.clone();
        match event {
            ControlEvent::PaneExited { .. } => Update::Exited,
            ControlEvent::AgentStatus { json, metadata, .. } => {
                match serde_json::from_value::<Option<AgentSessionState>>(json) {
                    Ok(state) => Update::Agent(state.map(|state| PaneAgentState {
                        pane_id: pane,
                        agent: self.agent,
                        state,
                        event_metadata: Some(metadata),
                    })),
                    Err(_) => Update::Resync,
                }
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tty7_core::core::cli_agent::AgentStatus;

    fn metadata(sequence: u64) -> PaneEventMetadata {
        PaneEventMetadata {
            instance: "daemon".into(),
            sequence,
            timestamp_ms: 1,
        }
    }

    fn cursor() -> Cursor {
        Cursor {
            pane: 3,
            agent: None,
            watermark: metadata(10),
        }
    }

    fn event(pane: u64, sequence: u64, status: AgentStatus) -> ControlEvent {
        ControlEvent::AgentStatus {
            pane_id: pane,
            metadata: metadata(sequence),
            json: serde_json::to_value(AgentSessionState {
                status,
                ..Default::default()
            })
            .unwrap(),
        }
    }

    #[test]
    fn snapshot_watermark_rejects_older_and_duplicate_events() {
        let mut cursor = cursor();
        for sequence in [8, 10] {
            assert!(matches!(
                cursor.apply(event(3, sequence, AgentStatus::Done)),
                Update::Ignore
            ));
        }
        assert!(matches!(
            cursor.apply(event(3, 11, AgentStatus::Waiting)),
            Update::Agent(Some(_))
        ));
        assert!(matches!(
            cursor.apply(event(3, 11, AgentStatus::Waiting)),
            Update::Ignore
        ));
    }

    #[test]
    fn other_panes_do_not_advance_the_target_watermark() {
        let mut cursor = cursor();
        assert!(matches!(
            cursor.apply(event(4, 99, AgentStatus::Done)),
            Update::Ignore
        ));
        assert!(matches!(
            cursor.apply(event(3, 12, AgentStatus::Done)),
            Update::Agent(Some(_))
        ));
    }

    #[test]
    fn a_restart_or_invalid_status_requires_a_new_snapshot() {
        let mut cursor = cursor();
        let mut restarted = metadata(11);
        restarted.instance = "new-daemon".into();
        assert!(matches!(
            cursor.apply(ControlEvent::PaneExited {
                pane_id: 3,
                code: None,
                metadata: restarted,
            }),
            Update::Resync
        ));
        assert!(matches!(
            cursor.apply(ControlEvent::AgentStatus {
                pane_id: 3,
                json: serde_json::json!("invalid"),
                metadata: metadata(11),
            }),
            Update::Resync
        ));
    }

    #[test]
    fn cleared_sessions_and_exit_are_distinct() {
        let mut cursor = cursor();
        assert!(matches!(
            cursor.apply(ControlEvent::AgentStatus {
                pane_id: 3,
                json: serde_json::Value::Null,
                metadata: metadata(11),
            }),
            Update::Agent(None)
        ));
        assert!(matches!(
            cursor.apply(ControlEvent::PaneExited {
                pane_id: 3,
                code: Some(7),
                metadata: metadata(12),
            }),
            Update::Exited
        ));
    }
}
