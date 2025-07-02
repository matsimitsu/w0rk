use std::convert::TryFrom;
use std::fmt::Display;

use crate::Error;
use lazy_static::lazy_static;
use regex::Regex;

lazy_static! {
    static ref TASK_REGEX: Regex =
        Regex::new(r"^[\*|-]\s?\[(?<completed>.?)\]\s?(?<name>.+)$").unwrap();
}

#[derive(Debug, PartialEq, Clone)]
pub enum State {
    Completed,
    Incomplete,
    InProgress,
    Blocked,
}
impl TryFrom<&str> for State {
    type Error = crate::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "x" => Ok(State::Completed),
            " " => Ok(State::Incomplete),
            "~" => Ok(State::InProgress),
            "#" => Ok(State::Blocked),
            _ => Err(Error::InvalidTaskSyntax(value.to_string())),
        }
    }
}

impl Display for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = match self {
            State::Completed => "x",
            State::Incomplete => " ",
            State::InProgress => "~",
            State::Blocked => "#",
        };
        write!(f, "{}", state)
    }
}

#[derive(Debug, PartialEq, Clone)]
pub struct Task {
    pub name: String,
    pub state: State,
    pub subtasks: Vec<Task>,
}

impl TryFrom<&str> for Task {
    type Error = crate::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let captures = match TASK_REGEX.captures(value) {
            Some(captures) => captures,
            None => return Err(Error::InvalidTaskSyntax(value.to_string())),
        };

        if let (Some(state), Some(name)) = (captures.name("completed"), captures.name("name")) {
            Ok(Task {
                name: name.as_str().to_string(),
                state: state.as_str().try_into()?,
                subtasks: Vec::new(),
            })
        } else {
            Err(Error::InvalidTaskSyntax(value.to_string()))
        }
    }
}

impl Task {
    pub fn add_subtask(&mut self, subtask: Task) {
        self.subtasks.push(subtask);
        self.update_state_from_subtasks();
    }

    pub fn remove_subtask(&mut self, index: usize) -> Option<Task> {
        if index < self.subtasks.len() {
            let subtask = self.subtasks.remove(index);
            self.update_state_from_subtasks();
            Some(subtask)
        } else {
            None
        }
    }

    pub fn mark_subtask_complete(&mut self, index: usize) -> bool {
        if let Some(subtask) = self.subtasks.get_mut(index) {
            subtask.state = State::Completed;
            self.update_state_from_subtasks();
            true
        } else {
            false
        }
    }

    pub fn update_state_from_subtasks(&mut self) {
        if self.subtasks.is_empty() {
            return;
        }

        let all_complete = self.subtasks.iter().all(|t| t.state == State::Completed);
        let any_in_progress = self.subtasks.iter().any(|t| t.state == State::InProgress);

        if all_complete {
            self.state = State::Completed;
        } else if any_in_progress {
            self.state = State::InProgress;
        } else {
            self.state = State::Incomplete;
        }
    }

    pub fn has_subtasks(&self) -> bool {
        !self.subtasks.is_empty()
    }
}

impl Display for Task {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "* [{}] {}", self.state, self.name)?;
        for subtask in &self.subtasks {
            writeln!(f, "  * [{}] {}", subtask.state, subtask.name)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple() {
        let task: Task = "* [x] Water plants"
            .try_into()
            .expect("Could not parse task");
        assert_eq!(task.state, State::Completed);
        assert_eq!(task.name, "Water plants");
    }

    #[test]
    fn test_parse_alternative() {
        let task: Task = "- [ ] Water plants"
            .try_into()
            .expect("Could not parse task");
        assert_eq!(task.state, State::Incomplete);
        assert_eq!(task.name, "Water plants");
    }

    #[test]
    fn test_parse_in_progress() {
        let task: Task = "-[~]Water plants".try_into().expect("Could not parse task");
        assert_eq!(task.state, State::InProgress);
        assert_eq!(task.name, "Water plants");
    }

    #[test]
    fn test_parse_in_blocked() {
        let task: Task = "-[#]Water plants".try_into().expect("Could not parse task");
        assert_eq!(task.state, State::Blocked);
        assert_eq!(task.name, "Water plants");
    }

    #[test]
    fn test_parse_messy() {
        let task: Task = "-[ ]Water plants".try_into().expect("Could not parse task");
        assert_eq!(task.state, State::Incomplete);
        assert_eq!(task.name, "Water plants");
    }

    #[test]
    fn test_add_subtask() {
        let mut task: Task = "* [ ] Main task".try_into().unwrap();
        let subtask: Task = "* [ ] Subtask 1".try_into().unwrap();

        task.add_subtask(subtask);
        assert_eq!(task.subtasks.len(), 1);
        assert_eq!(task.subtasks[0].name, "Subtask 1");
    }

    #[test]
    fn test_auto_complete_when_all_subtasks_complete() {
        let mut task: Task = "* [ ] Main task".try_into().unwrap();
        let subtask1: Task = "* [ ] Subtask 1".try_into().unwrap();
        let subtask2: Task = "* [ ] Subtask 2".try_into().unwrap();

        task.add_subtask(subtask1);
        task.add_subtask(subtask2);

        assert_eq!(task.state, State::Incomplete);

        task.mark_subtask_complete(0);
        assert_eq!(task.state, State::Incomplete);

        task.mark_subtask_complete(1);
        assert_eq!(task.state, State::Completed);
    }

    #[test]
    fn test_auto_in_progress_when_any_subtask_in_progress() {
        let mut task: Task = "* [ ] Main task".try_into().unwrap();
        let mut subtask1: Task = "* [ ] Subtask 1".try_into().unwrap();
        let subtask2: Task = "* [ ] Subtask 2".try_into().unwrap();

        subtask1.state = State::InProgress;
        task.add_subtask(subtask1);
        task.add_subtask(subtask2);

        assert_eq!(task.state, State::InProgress);
    }

    #[test]
    fn test_remove_subtask() {
        let mut task: Task = "* [ ] Main task".try_into().unwrap();
        let subtask: Task = "* [ ] Subtask 1".try_into().unwrap();

        task.add_subtask(subtask);
        assert_eq!(task.subtasks.len(), 1);

        let removed = task.remove_subtask(0);
        assert!(removed.is_some());
        assert_eq!(removed.unwrap().name, "Subtask 1");
        assert_eq!(task.subtasks.len(), 0);
    }

    #[test]
    fn test_has_subtasks() {
        let mut task: Task = "* [ ] Main task".try_into().unwrap();
        assert!(!task.has_subtasks());

        let subtask: Task = "* [ ] Subtask 1".try_into().unwrap();
        task.add_subtask(subtask);
        assert!(task.has_subtasks());
    }

    #[test]
    fn test_display_with_subtasks() {
        let mut task: Task = "* [ ] Main task".try_into().unwrap();
        let subtask: Task = "* [x] Completed subtask".try_into().unwrap();

        task.add_subtask(subtask);
        let output = format!("{}", task);

        assert!(output.contains("* [x] Main task"));
        assert!(output.contains("  * [x] Completed subtask"));
    }
}
