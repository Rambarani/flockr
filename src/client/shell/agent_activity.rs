//! How long each agent has been in its current state, as observed by this
//! client. The endpoint snapshot carries a state change counter but no
//! timestamps, so the age starts when this client first sees a state. An agent
//! that was already in a state when the client (or its machine) was first seen
//! has an unwitnessed age, reported as a lower bound.

use std::time::{Duration, Instant};

use super::*;

#[derive(Debug, Clone)]
struct ClockEntry {
    status: crate::api::schema::AgentStatus,
    seq: u64,
    since: Instant,
    witnessed: bool,
}

#[derive(Debug, Default)]
pub(super) struct AgentClock {
    entries: HashMap<(String, String), ClockEntry>,
    /// Endpoints whose first snapshot has been taken as the baseline.
    baselined: HashSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StateAge {
    pub(super) elapsed: Duration,
    /// False when the state was already running when it was first seen, so
    /// `elapsed` is a lower bound.
    pub(super) witnessed: bool,
}

impl AgentClock {
    /// Fold the current snapshots into the clock.
    pub(super) fn observe(&mut self, endpoints: &[ClientShellEndpoint], now: Instant) {
        for endpoint in endpoints {
            let Some(snapshot) = endpoint.snapshot.as_deref() else {
                continue;
            };
            let key = endpoint.endpoint_id.storage_key();
            let baseline = self.baselined.insert(key.clone());
            for agent in &snapshot.agents {
                let entry_key = (key.clone(), agent.pane_id.clone());
                let unchanged = self.entries.get(&entry_key).is_some_and(|entry| {
                    entry.status == agent.agent_status && entry.seq == agent.state_change_seq
                });
                if unchanged {
                    continue;
                }
                let seen_before = self.entries.contains_key(&entry_key);
                self.entries.insert(
                    entry_key,
                    ClockEntry {
                        status: agent.agent_status,
                        seq: agent.state_change_seq,
                        since: now,
                        witnessed: seen_before || !baseline,
                    },
                );
            }
            self.entries.retain(|(entry_endpoint, pane_id), _| {
                *entry_endpoint != key
                    || snapshot
                        .agents
                        .iter()
                        .any(|agent| agent.pane_id == *pane_id)
            });
        }
    }

    pub(super) fn age(
        &self,
        endpoint_id: &ClientEndpointId,
        pane_id: &str,
        now: Instant,
    ) -> Option<StateAge> {
        let entry = self
            .entries
            .get(&(endpoint_id.storage_key(), pane_id.to_owned()))?;
        Some(StateAge {
            elapsed: now.saturating_duration_since(entry.since),
            witnessed: entry.witnessed,
        })
    }
}

/// `45s`, `2m 05s` or `1h 03m`, with a leading `≥` for an unwitnessed age.
pub(super) fn format_age(age: Option<StateAge>) -> String {
    let Some(age) = age else {
        return "—".to_owned();
    };
    let seconds = age.elapsed.as_secs();
    let text = match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m {:02}s", seconds / 60, seconds % 60),
        _ => format!("{}h {:02}m", seconds / 3600, seconds % 3600 / 60),
    };
    if age.witnessed {
        text
    } else {
        format!("≥{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::snapshot;
    use super::*;
    use crate::api::schema::AgentStatus;
    use crate::protocol::ClientShellAgent;

    fn agent(pane_id: &str, status: AgentStatus, seq: u64) -> ClientShellAgent {
        ClientShellAgent {
            pane_id: pane_id.into(),
            workspace_id: "ws_1".into(),
            tab_id: "tab_1".into(),
            name: None,
            display_agent: None,
            agent: Some("pi".into()),
            title: None,
            terminal_title: None,
            terminal_title_stripped: None,
            agent_status: status,
            state_change_seq: seq,
            state_labels: Vec::new(),
            tokens: Vec::new(),
            focused: false,
        }
    }

    fn endpoint(agents: Vec<ClientShellAgent>) -> Vec<ClientShellEndpoint> {
        let mut projection = snapshot();
        projection.agents = agents;
        let mut endpoint = local_endpoint();
        endpoint.snapshot = Some(Box::new(projection));
        vec![endpoint]
    }

    fn secs(seconds: u64) -> Duration {
        Duration::from_secs(seconds)
    }

    #[test]
    fn first_sighting_is_a_lower_bound_and_later_changes_are_witnessed() {
        let start = Instant::now();
        let mut clock = AgentClock::default();
        clock.observe(&endpoint(vec![agent("p1", AgentStatus::Working, 1)]), start);
        let age = clock
            .age(&ClientEndpointId::Local, "p1", start + secs(30))
            .unwrap();
        assert_eq!(age.elapsed, secs(30));
        assert!(!age.witnessed);
        assert_eq!(format_age(Some(age)), "≥30s");

        // Same state again: the clock keeps running.
        clock.observe(
            &endpoint(vec![agent("p1", AgentStatus::Working, 1)]),
            start + secs(40),
        );
        assert_eq!(
            clock
                .age(&ClientEndpointId::Local, "p1", start + secs(45))
                .unwrap()
                .elapsed,
            secs(45)
        );

        // A state change restarts it, and this one was seen happening.
        clock.observe(
            &endpoint(vec![agent("p1", AgentStatus::Idle, 2)]),
            start + secs(50),
        );
        let age = clock
            .age(&ClientEndpointId::Local, "p1", start + secs(52))
            .unwrap();
        assert_eq!(age.elapsed, secs(2));
        assert!(age.witnessed);

        // So does a new transition into the same status.
        clock.observe(
            &endpoint(vec![agent("p1", AgentStatus::Idle, 3)]),
            start + secs(60),
        );
        assert_eq!(
            clock
                .age(&ClientEndpointId::Local, "p1", start + secs(61))
                .unwrap()
                .elapsed,
            secs(1)
        );
    }

    #[test]
    fn agents_that_appear_later_are_witnessed_and_departed_ones_are_forgotten() {
        let start = Instant::now();
        let mut clock = AgentClock::default();
        clock.observe(&endpoint(vec![agent("p1", AgentStatus::Idle, 1)]), start);
        clock.observe(
            &endpoint(vec![
                agent("p1", AgentStatus::Idle, 1),
                agent("p2", AgentStatus::Working, 1),
            ]),
            start + secs(10),
        );
        assert!(
            clock
                .age(&ClientEndpointId::Local, "p2", start + secs(11))
                .unwrap()
                .witnessed
        );
        assert!(
            !clock
                .age(&ClientEndpointId::Local, "p1", start + secs(11))
                .unwrap()
                .witnessed
        );

        clock.observe(
            &endpoint(vec![agent("p2", AgentStatus::Working, 1)]),
            start + secs(20),
        );
        assert!(clock
            .age(&ClientEndpointId::Local, "p1", start + secs(21))
            .is_none());
        assert!(clock
            .age(&ClientEndpointId::Local, "p2", start + secs(21))
            .is_some());
    }

    #[test]
    fn ages_format_compactly() {
        let age = |seconds, witnessed| {
            Some(StateAge {
                elapsed: secs(seconds),
                witnessed,
            })
        };
        assert_eq!(format_age(age(0, true)), "0s");
        assert_eq!(format_age(age(59, true)), "59s");
        assert_eq!(format_age(age(60, true)), "1m 00s");
        assert_eq!(format_age(age(125, true)), "2m 05s");
        assert_eq!(format_age(age(3599, true)), "59m 59s");
        assert_eq!(format_age(age(3600, true)), "1h 00m");
        assert_eq!(format_age(age(3723, false)), "≥1h 02m");
        assert_eq!(format_age(None), "—");
    }
}
