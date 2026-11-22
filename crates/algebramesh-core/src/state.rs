use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Job states (design §7.2).
///
/// ```text
/// QUEUED -> ASSIGNED -> RUNNING -> SUCCEEDED
///    |          |          |      -> FAILED
///    |          |          |      -> TIMED_OUT
///    |          |          +------> CANCELLING -> CANCELLED
///    |          +-----------------> LOST
///    +---------------------------> CANCELLED
/// ```
///
/// `LOST` means the outcome cannot be determined (for example the worker
/// disconnected); it is not the same as `FAILED`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum JobState {
    Queued,
    Assigned,
    Running,
    Succeeded,
    Failed,
    TimedOut,
    Cancelling,
    Cancelled,
    Lost,
}

#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
#[error("invalid job state transition {from:?} -> {to:?}")]
pub struct InvalidTransition {
    pub from: JobState,
    pub to: JobState,
}

impl JobState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Succeeded | Self::Failed | Self::TimedOut | Self::Cancelled | Self::Lost
        )
    }

    pub fn can_transition_to(self, to: JobState) -> bool {
        use JobState::*;
        matches!(
            (self, to),
            (Queued, Assigned)
                | (Queued, Cancelled)
                | (Assigned, Running)
                | (Assigned, Lost)
                | (Running, Succeeded)
                | (Running, Failed)
                | (Running, TimedOut)
                | (Running, Cancelling)
                | (Running, Lost)
                | (Cancelling, Cancelled)
                | (Cancelling, Lost)
        )
    }

    pub fn transition(self, to: JobState) -> Result<JobState, InvalidTransition> {
        if self.can_transition_to(to) {
            Ok(to)
        } else {
            Err(InvalidTransition { from: self, to })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::JobState::*;
    use super::*;

    const ALL: [JobState; 9] = [
        Queued, Assigned, Running, Succeeded, Failed, TimedOut, Cancelling, Cancelled, Lost,
    ];

    #[test]
    fn terminal_states_have_no_successors() {
        for from in ALL.into_iter().filter(|s| s.is_terminal()) {
            for to in ALL {
                assert!(!from.can_transition_to(to), "{from:?} -> {to:?}");
            }
        }
    }

    #[test]
    fn happy_path_and_cancellation() {
        let s = Queued.transition(Assigned).unwrap();
        let s = s.transition(Running).unwrap();
        assert_eq!(s.transition(Succeeded), Ok(Succeeded));
        assert_eq!(
            Running
                .transition(Cancelling)
                .and_then(|s| s.transition(Cancelled)),
            Ok(Cancelled)
        );
        assert_eq!(Queued.transition(Cancelled), Ok(Cancelled));
    }

    #[test]
    fn lost_is_not_failed_and_running_cannot_be_skipped() {
        assert!(Assigned.can_transition_to(Lost));
        assert!(!Lost.can_transition_to(Failed));
        assert_eq!(
            Queued.transition(Succeeded),
            Err(InvalidTransition {
                from: Queued,
                to: Succeeded
            })
        );
        assert!(!Running.can_transition_to(Cancelled));
    }

    #[test]
    fn serializes_like_design() {
        assert_eq!(serde_json::to_string(&TimedOut).unwrap(), "\"TIMED_OUT\"");
    }
}
