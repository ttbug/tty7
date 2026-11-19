use std::time::{Duration, Instant};

use tty7_core::core::agent_history::SessionKey;
use tty7_core::core::cli_agent::CLIAgent;
use tty7_core::host::HostId;

const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct TitleKey {
    pub host: HostId,
    pub session: SessionKey,
}

pub(crate) fn supports_title(agent: CLIAgent) -> bool {
    matches!(
        agent,
        CLIAgent::Codex | CLIAgent::Claude | CLIAgent::OpenCode
    )
}

/// A pane's cached name. In-flight reads belong to an exact session and
/// generation, so switching sessions cannot land a previous chat's title.
#[derive(Default)]
pub(super) struct AgentTitle {
    key: Option<TitleKey>,
    title: Option<String>,
    generation: u64,
    pending: bool,
    last_attempt: Option<Instant>,
}

impl AgentTitle {
    pub fn title(&self, key: &TitleKey) -> Option<&str> {
        (self.key.as_ref() == Some(key))
            .then_some(self.title.as_deref())
            .flatten()
    }

    pub fn set_key(&mut self, key: Option<TitleKey>) -> bool {
        if self.key == key {
            return false;
        }
        self.key = key;
        self.pending = false;
        self.last_attempt = None;
        self.generation = self.generation.wrapping_add(1);
        self.title.take().is_some()
    }

    pub fn begin(&mut self, now: Instant) -> Option<u64> {
        if self.key.is_none()
            || self.pending
            || self
                .last_attempt
                .is_some_and(|at| now.duration_since(at) < REFRESH_INTERVAL)
        {
            return None;
        }
        self.last_attempt = Some(now);
        self.pending = true;
        self.generation = self.generation.wrapping_add(1);
        Some(self.generation)
    }

    pub fn finish(
        &mut self,
        key: &TitleKey,
        generation: u64,
        result: std::io::Result<Option<String>>,
    ) -> bool {
        if self.key.as_ref() != Some(key) || generation != self.generation {
            return false;
        }
        self.pending = false;
        // A transport failure says nothing new about a previously read name.
        let Ok(title) = result else { return false };
        let title = title.and_then(|title| {
            let title = title.trim();
            (!title.is_empty()).then(|| title.to_string())
        });
        if self.title == title {
            return false;
        }
        self.title = title;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(host: HostId, agent: CLIAgent, id: &str) -> TitleKey {
        TitleKey {
            host,
            session: SessionKey {
                agent,
                id: id.into(),
            },
        }
    }

    #[test]
    fn delayed_titles_and_renames_are_retried_without_overlapping_reads() {
        let mut cache = AgentTitle::default();
        let key = key(HostId::LOCAL, CLIAgent::Codex, "session");
        let now = Instant::now();
        cache.set_key(Some(key.clone()));
        let first = cache.begin(now).unwrap();
        assert!(cache.begin(now + REFRESH_INTERVAL).is_none());
        assert!(!cache.finish(&key, first, Ok(None)));
        assert!(cache.begin(now + Duration::from_secs(1)).is_none());
        let second = cache.begin(now + REFRESH_INTERVAL).unwrap();
        assert!(cache.finish(&key, second, Ok(Some("First title".into()))));
        let third = cache.begin(now + REFRESH_INTERVAL * 2).unwrap();
        assert!(cache.finish(&key, third, Ok(Some("Renamed".into()))));
        assert_eq!(cache.title(&key), Some("Renamed"));
    }

    #[test]
    fn host_agent_and_full_id_isolate_names_and_reject_stale_results() {
        let mut cache = AgentTitle::default();
        let local = key(HostId::LOCAL, CLIAgent::Codex, "same-id");
        let remote = key(
            HostId::from_connection_key("ssh:remote"),
            CLIAgent::Codex,
            "same-id",
        );
        let claude = key(HostId::LOCAL, CLIAgent::Claude, "same-id");
        let other = key(HostId::LOCAL, CLIAgent::Codex, "same-id-2");
        let now = Instant::now();
        cache.set_key(Some(local.clone()));
        let first = cache.begin(now).unwrap();
        cache.finish(&local, first, Ok(Some("Local".into())));
        for next in [remote, claude, other] {
            assert_eq!(cache.title(&next), None);
            cache.set_key(Some(next.clone()));
            let generation = cache.begin(now).unwrap();
            assert!(!cache.finish(&local, first, Ok(Some("Wrong".into()))));
            assert!(cache.finish(&next, generation, Ok(Some("Correct".into()))));
            assert_eq!(cache.title(&next), Some("Correct"));
        }
        cache.set_key(Some(local.clone()));
        let newest = cache.begin(now).unwrap();
        assert!(!cache.finish(&local, first, Ok(Some("Stale".into()))));
        assert!(cache.finish(&local, newest, Ok(Some("Current".into()))));
        cache.set_key(None);
        assert_eq!(cache.title(&local), None);
    }

    #[test]
    fn failures_keep_a_known_title_and_empty_results_use_the_fallback() {
        let mut cache = AgentTitle::default();
        let key = key(HostId::LOCAL, CLIAgent::OpenCode, "s");
        cache.set_key(Some(key.clone()));
        let now = Instant::now();
        let generation = cache.begin(now).unwrap();
        cache.finish(&key, generation, Ok(Some(" Title ".into())));
        let generation = cache.begin(now + REFRESH_INTERVAL).unwrap();
        assert!(!cache.finish(&key, generation, Err(std::io::Error::other("offline"))));
        assert_eq!(cache.title(&key), Some("Title"));
        let generation = cache.begin(now + REFRESH_INTERVAL * 2).unwrap();
        assert!(cache.finish(&key, generation, Ok(Some("  ".into()))));
        assert_eq!(cache.title(&key), None);
    }
}
