use crate::config::{DAY_EXTENTION, DAY_FORMAT, RECURRING_FILE};
use crate::task::Task;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use time::Date;

pub struct DaysList(Vec<DayListing>);

pub type DayListing = (Date, PathBuf);

impl DaysList {
    pub fn from_path(path: &Path) -> Result<Self, crate::Error> {
        let mut days: Vec<DayListing> = path
            .read_dir()?
            .filter_map(Result::ok)
            .filter(|de| {
                de.path().is_file()
                    && de.path().extension() == Some(OsStr::new(DAY_EXTENTION))
                    && de.path().file_name() != Some(OsStr::new(RECURRING_FILE))
            })
            .filter_map(|de| {
                date_from_path(&de.path())
                    .map(|date| (date, de.path().to_owned()))
                    .ok()
            })
            .collect();
        days.sort_by(|(a, _), (b, _)| a.cmp(b));

        Ok(Self(days))
    }

    pub fn last(&self) -> Option<&DayListing> {
        self.0.last()
    }

    pub fn iter(&self) -> std::slice::Iter<DayListing> {
        self.0.iter()
    }
}

impl IntoIterator for DaysList {
    type Item = DayListing;
    type IntoIter = std::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

pub struct Day {
    pub path: PathBuf,
    pub date: Date,
    pub tasks: Vec<Task>,
    pub notes: String,
}

impl Day {
    pub fn new(path: &Path) -> Result<Self, crate::Error> {
        Ok(Self {
            path: path.into(),
            date: date_from_path(path)?,
            tasks: Vec::new(),
            notes: String::new(),
        })
    }

    pub fn from_path(path: &Path) -> Result<Self, crate::Error> {
        let content = std::fs::read_to_string(path)?;
        let (tasks, notes) = parse_day_content(&content);
        Ok(Self {
            path: path.into(),
            date: date_from_path(path)?,
            tasks,
            notes,
        })
    }

    pub fn write(&self) -> Result<(), crate::Error> {
        let content = self
            .tasks
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<String>>()
            .join("");
        let content = format!("{}\n{}", content, self.notes);
        std::fs::write(&self.path, content)?;
        Ok(())
    }
}

fn parse_day_content(content: &str) -> (Vec<Task>, String) {
    let mut tasks: Vec<Task> = Vec::new();
    let mut notes = String::new();

    for line in content.lines() {
        let (subtask, trimmed_line) = match line.starts_with("  ") || line.starts_with('\t') {
            true => (true, line.trim_start_matches("  ").trim_start_matches('\t')),
            false => (false, line),
        };

        // Attempt to parse the line as a task
        let task: Task = match trimmed_line.try_into() {
            Ok(task) => task,
            Err(_) => {
                notes.push_str(line);
                continue;
            }
        };

        // Check if it's a subtask, if so add it to the last task's subtasks, if present
        if subtask {
            if let Some(last_task) = tasks.last_mut() {
                last_task.subtasks.push(task);
                continue;
            }
        }

        // No subtask, or last task is not present, add the task to the tasks vector
        tasks.push(task);
    }

    (tasks, notes)
}

fn date_from_path(path: &Path) -> Result<Date, crate::Error> {
    let file_stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .ok_or_else(|| crate::Error::InvalidDayPath(path.to_string_lossy().to_string()))?;
    Date::parse(file_stem, &DAY_FORMAT).map_err(|err| err.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::helpers::test_fixtures_path;
    use time::Month;

    #[test]
    fn test_day_list_from_path() {
        let path = test_fixtures_path().join("work");
        let days_list = DaysList::from_path(&path).expect("Could not create days list");
        assert_eq!(days_list.0.len(), 1);

        assert_eq!(
            days_list.0[0].0,
            Date::from_calendar_date(2010, Month::October, 1).expect("Could not parse date")
        );
    }

    #[test]
    fn test_date_from_path() {
        let path = Path::new("2021-01-01.md");
        let date = date_from_path(path).expect("Could not parse date");
        assert_eq!(
            date,
            Date::from_calendar_date(2021, Month::January, 1).expect("Could not parse date")
        );
    }

    #[test]
    fn test_parse_day_content() {
        let content = r#"
* [ ] Logs
  * [ ] Log subtask
      "#;
        let (tasks, _) = parse_day_content(content);

        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].name, "Logs");
        assert_eq!(tasks[0].subtasks.len(), 1);
        assert_eq!(tasks[0].subtasks[0].name, "Log subtask");
    }
}
