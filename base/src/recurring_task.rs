use crate::task::{State as TaskState, Task};
use std::convert::TryFrom;
use std::fmt::Display;
use std::fs::File;
use std::io::{BufRead, BufReader};

use crate::Error;
use lazy_static::lazy_static;
use regex::Regex;
use time::Date;

#[derive(Default, Debug)]
pub struct RecurringTasks(Vec<RecurringTask>);

impl RecurringTasks {
    pub fn from_path(path: &std::path::Path) -> Result<Self, crate::Error> {
        let mut tasks = Vec::new();
        let file = File::open(path)?;
        let reader = BufReader::new(file);

        for line in reader.lines() {
            let line = line?;
            tasks.push(line.as_str().try_into()?);
        }

        Ok(Self(tasks))
    }

    pub fn for_date(&self, date: &Date) -> Vec<RecurringTask> {
        self.0
            .iter()
            .filter(|task| task.is_due(date))
            .cloned()
            .collect()
    }
}

impl From<&RecurringTask> for Task {
    fn from(val: &RecurringTask) -> Self {
        Task {
            name: val.name.to_string(),
            state: TaskState::Incomplete,
            subtasks: Vec::new(),
        }
    }
}

lazy_static! {
    static ref RECURRING_TASK_REGEX: Regex =
        Regex::new(r"^[\*|-]\s?\[\s?\]\s?@(?<interval>\w+)\s(?<name>.+)$").unwrap();
}

#[derive(Debug, PartialEq, Clone)]
pub struct RecurringTask {
    pub name: String,
    pub interval: Interval,
}

impl RecurringTask {
    pub fn is_due(&self, date: &Date) -> bool {
        match self.interval {
            Interval::Daily => true,
            Interval::Weekly => date.weekday().number_from_monday() == 1,
            Interval::Monthly => date.day() == 1,
            Interval::Weekday => date.weekday().number_from_monday() <= 5,
            Interval::Weekend => date.weekday().number_from_monday() > 5,
            Interval::Monday => date.weekday().number_from_monday() == 1,
            Interval::Tuesday => date.weekday().number_from_monday() == 2,
            Interval::Wednesday => date.weekday().number_from_monday() == 3,
            Interval::Thursday => date.weekday().number_from_monday() == 4,
            Interval::Friday => date.weekday().number_from_monday() == 5,
            Interval::Saturday => date.weekday().number_from_monday() == 6,
            Interval::Sunday => date.weekday().number_from_monday() == 7,
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum Interval {
    Daily,
    Weekly,
    Monthly,
    Weekday,
    Weekend,
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

impl Display for Interval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Interval::Daily => write!(f, "daily"),
            Interval::Weekly => write!(f, "weekly"),
            Interval::Monthly => write!(f, "monthly"),
            Interval::Weekday => write!(f, "weekday"),
            Interval::Weekend => write!(f, "weekend"),
            Interval::Monday => write!(f, "monday"),
            Interval::Tuesday => write!(f, "tuesday"),
            Interval::Wednesday => write!(f, "wednesday"),
            Interval::Thursday => write!(f, "thursday"),
            Interval::Friday => write!(f, "friday"),
            Interval::Saturday => write!(f, "saturday"),
            Interval::Sunday => write!(f, "sunday"),
        }
    }
}

impl TryFrom<&str> for Interval {
    type Error = crate::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value.to_ascii_lowercase().as_str() {
            "daily" => Ok(Interval::Daily),
            "weekly" => Ok(Interval::Weekly),
            "monthly" => Ok(Interval::Monthly),
            "weekday" => Ok(Interval::Weekday),
            "weekend" => Ok(Interval::Weekend),
            "monday" => Ok(Interval::Monday),
            "tuesday" => Ok(Interval::Tuesday),
            "wednesday" => Ok(Interval::Wednesday),
            "thursday" => Ok(Interval::Thursday),
            "friday" => Ok(Interval::Friday),
            "saturday" => Ok(Interval::Saturday),
            "sunday" => Ok(Interval::Sunday),
            _ => Err(Error::InvalidIntervalSyntax(value.to_string())),
        }
    }
}

impl Display for RecurringTask {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "* [] @{} {}", self.interval, self.name)
    }
}

impl TryFrom<&str> for RecurringTask {
    type Error = crate::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let captures = match RECURRING_TASK_REGEX.captures(value) {
            Some(captures) => captures,
            None => return Err(Error::InvalidRecurringTaskSyntax(value.to_string())),
        };

        if let (Some(interval), Some(name)) = (captures.name("interval"), captures.name("name")) {
            Ok(RecurringTask {
                name: name.as_str().to_string(),
                interval: interval.as_str().try_into()?,
            })
        } else {
            Err(Error::InvalidRecurringTaskSyntax(value.to_string()))
        }
    }
}

#[cfg(test)]
mod tests {
    use time::Month;

    use super::*;
    use crate::tests::helpers::test_fixtures_path;

    #[test]
    fn test_recurring_tasks_from_path() {
        let path = test_fixtures_path().join("work");
        let recurring_tasks = RecurringTasks::from_path(&path.join(".recurring.md"))
            .expect("Could not load recurring tasks");

        assert_eq!(recurring_tasks.0.len(), 4);
    }

    #[test]
    fn test_recurring_task_to_string() {
        let recurring_task = RecurringTask {
            name: "test".to_string(),
            interval: Interval::Daily,
        };
        assert_eq!(&recurring_task.to_string(), "* [] @daily test");
    }

    #[test]
    fn test_try_from_recurring_task() {
        let recurring_task = RecurringTask::try_from("* [] @daily test").unwrap();
        assert_eq!(recurring_task.name, "test");
        assert_eq!(recurring_task.interval, Interval::Daily);

        let recurring_task = RecurringTask::try_from("-[]@weekly test").unwrap();
        assert_eq!(recurring_task.name, "test");
        assert_eq!(recurring_task.interval, Interval::Weekly);
    }

    #[test]
    fn test_for_date_daily() {
        // July 1st, a Monady
        assert_eq!(helpers::for_date("* [ ] @daily feed the cat", 7).len(), 1,);
    }

    #[test]
    fn test_for_date_weekday() {
        // July 1st, a Monady
        assert_eq!(helpers::for_date("* [ ] @weekday feed the cat", 1).len(), 1);

        // July 7th, a Sunday
        assert_eq!(helpers::for_date("* [ ] @weekday feed the cat", 7).len(), 0);
    }

    #[test]
    fn test_for_date_weekend() {
        // July 1st, a Monady
        assert_eq!(helpers::for_date("* [ ] @weekend feed the cat", 1).len(), 0);

        // July 7th, a Sunday
        assert_eq!(helpers::for_date("* [ ] @weekend feed the cat", 7).len(), 1);
    }

    #[test]
    fn test_for_date_monday() {
        // July 1st, a Monady
        assert_eq!(helpers::for_date("* [ ] @monday feed the cat", 1).len(), 1);

        // July 7th, a Sunday
        assert_eq!(helpers::for_date("* [ ] @monday feed the cat", 7).len(), 0);
    }

    mod helpers {
        use super::*;

        pub fn running_tasks(task_str: &str) -> RecurringTasks {
            RecurringTasks(vec![
                RecurringTask::try_from(task_str).expect("Could not parse running task")
            ])
        }

        pub fn for_date(task_str: &str, day: u8) -> Vec<RecurringTask> {
            running_tasks(task_str).for_date(
                &Date::from_calendar_date(2024, Month::July, day).expect("Could not parse date"),
            )
        }
    }
}
