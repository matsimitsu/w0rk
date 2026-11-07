//! `w0rk hook <event>` and `w0rk report`: how a running agent tells w0rk what it's doing.
//! Both identify the run through `W0RK_RUN_ID`, which the spawner sets on the pane.

use chrono::{DateTime, Utc};

use crate::model::{RunId, RunState, Verdict};
use crate::store::{Store, StoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum HookEvent {
    SessionStart,
    Prompt,
    Notification,
    Stop,
}

pub const RUN_ID_ENV: &str = "W0RK_RUN_ID";

/// Read the run id from the environment value, if the process was started by w0rk.
pub fn run_id_from(value: Option<&str>) -> Option<RunId> {
    value?.trim().parse().ok().map(RunId::new)
}

pub fn apply(
    store: &Store,
    run: RunId,
    event: HookEvent,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    match event {
        HookEvent::SessionStart | HookEvent::Prompt => store.set_state(run, RunState::Running, now),
        HookEvent::Stop => store.set_state(run, RunState::Done, now),
        // Claude also sends an idle notification after it has finished; that must not turn a
        // finished run back into "waiting on you".
        HookEvent::Notification => store.set_state_from(
            run,
            RunState::Waiting,
            &[RunState::Queued, RunState::Running],
            now,
        ),
    }
}

pub fn report(
    store: &Store,
    run: RunId,
    verdict: Verdict,
    summary: &str,
    now: DateTime<Utc>,
) -> Result<(), StoreError> {
    store.report(run, verdict, summary, now)?;
    if verdict == Verdict::NeedsYou {
        store.set_state(run, RunState::Waiting, now)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Fingerprint, ItemId, ProfileName};

    fn setup() -> (Store, RunId) {
        let store = Store::open_in_memory().unwrap();
        let id = store
            .create_run(
                &ItemId::new("a"),
                &Fingerprint::new("1"),
                &ProfileName::new("p"),
                DateTime::UNIX_EPOCH,
            )
            .unwrap()
            .unwrap();
        (store, id)
    }

    fn state(store: &Store) -> RunState {
        store.runs().unwrap()[0].state
    }

    #[test]
    fn run_id_comes_from_the_environment_value() {
        assert_eq!(run_id_from(Some("12\n")), Some(RunId::new(12)));
        assert_eq!(run_id_from(Some("nope")), None);
        assert_eq!(run_id_from(None), None);
    }

    #[test]
    fn events_drive_the_lifecycle() {
        let (store, id) = setup();
        let t = DateTime::UNIX_EPOCH;
        apply(&store, id, HookEvent::SessionStart, t).unwrap();
        assert_eq!(state(&store), RunState::Running);
        apply(&store, id, HookEvent::Notification, t).unwrap();
        assert_eq!(state(&store), RunState::Waiting);
        apply(&store, id, HookEvent::Prompt, t).unwrap();
        assert_eq!(state(&store), RunState::Running);
        apply(&store, id, HookEvent::Stop, t).unwrap();
        assert_eq!(state(&store), RunState::Done);
    }

    #[test]
    fn idle_notification_after_stop_stays_done() {
        let (store, id) = setup();
        let t = DateTime::UNIX_EPOCH;
        apply(&store, id, HookEvent::Stop, t).unwrap();
        apply(&store, id, HookEvent::Notification, t).unwrap();
        assert_eq!(state(&store), RunState::Done);
    }

    #[test]
    fn needs_you_report_flags_the_run_as_waiting() {
        let (store, id) = setup();
        let t = DateTime::UNIX_EPOCH;
        apply(&store, id, HookEvent::Stop, t).unwrap();
        report(&store, id, Verdict::NeedsYou, "odd install script", t).unwrap();
        let run = &store.runs().unwrap()[0];
        assert_eq!(run.state, RunState::Waiting);
        assert_eq!(run.verdict, Some(Verdict::NeedsYou));

        let (store, id) = setup();
        report(&store, id, Verdict::Ok, "clean", t).unwrap();
        assert_eq!(store.runs().unwrap()[0].verdict, Some(Verdict::Ok));
    }
}
