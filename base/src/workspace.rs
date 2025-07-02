use crate::config::{DAY_EXTENTION, DAY_FORMAT, RECURRING_FILE};
use crate::day::{Day, DaysList};
use crate::recurring_task::RecurringTasks;
use crate::task::State as TaskState;
use crate::Error;
use std::path::{Path, PathBuf};
use time::OffsetDateTime;

pub struct Workspace {
    pub name: String,
    pub path: PathBuf,
    pub recurring_tasks: RecurringTasks,
    pub day_list: DaysList,
}

impl Workspace {
    pub fn from_path(path: &Path) -> Result<Self, crate::Error> {
        if !path.is_dir() {
            return Err(Error::WorkspaceIsNotDirectory);
        }

        let name = match path.iter().next_back().and_then(|res| res.to_str()) {
            Some(name) => name.to_string(),
            None => {
                return Err(Error::InvalidWorkspaceName(
                    path.to_string_lossy().to_string(),
                ))
            }
        };
        let recurring_tasks = RecurringTasks::from_path(&path.join(RECURRING_FILE));
        let day_list = DaysList::from_path(path)?;

        Ok(Workspace {
            path: path.to_owned(),
            name,
            recurring_tasks: recurring_tasks.unwrap_or_default(),
            day_list,
        })
    }

    pub fn today(&self) -> Option<Day> {
        let date = OffsetDateTime::now_utc().date();
        self.day_list
            .iter()
            .find(|(day, _)| day == &date)
            .map(|(_, path)| Day::from_path(path).unwrap())
    }

    pub fn new_day(&self) -> Result<Day, crate::Error> {
        let date = OffsetDateTime::now_utc().date();
        let day_file = format!("{}.{}", date.format(&DAY_FORMAT)?, DAY_EXTENTION);
        let day_path = self.path.join(&day_file);
        if day_path.exists() {
            return Err(Error::DayAlreadyExists(day_file));
        }
        let mut new_day = Day::new(&day_path)?;

        if let Some((_, path)) = self.day_list.last() {
            let last_day = Day::from_path(path)?;
            new_day.tasks = last_day
                .tasks
                .iter()
                .filter(|task| task.state != TaskState::Completed)
                .cloned()
                .collect();
        };

        for rt in self.recurring_tasks.for_date(&date).iter() {
            if new_day.tasks.iter().any(|task| task.name == rt.name) {
                continue;
            }
            new_day.tasks.push(rt.into());
        }

        new_day.write()?;
        Ok(new_day)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::Task;
    use crate::tests::helpers::test_fixtures_path;

    #[test]
    fn test_new_day() {
        helpers::clean_fs();

        let workspace = Workspace::from_path(&test_fixtures_path().join("work"))
            .expect("Could not create workspace");
        let new_day = workspace.new_day().expect("Could not create new day");

        assert_eq!(
            new_day.tasks,
            vec![
                Task {
                    name: "Do the laundry".to_string(),
                    state: TaskState::InProgress,
                    subtasks: Vec::new(),
                },
                Task {
                    name: "Cook lunch".to_string(),
                    state: TaskState::Incomplete,
                    subtasks: Vec::new(),
                },
                Task {
                    name: "Deploy staging with latest changes".to_string(),
                    state: TaskState::Incomplete,
                    subtasks: Vec::new(),
                },
                Task {
                    name: "Deploy production with latest changes".to_string(),
                    state: TaskState::Incomplete,
                    subtasks: Vec::new(),
                },
                Task {
                    name: "Update changelog with latest production changes".to_string(),
                    state: TaskState::Incomplete,
                    subtasks: Vec::new(),
                },
            ]
        );
        assert_eq!(&new_day.notes, "");
        helpers::clean_fs();
    }

    pub mod helpers {
        use super::*;
        use std::fs::remove_file;

        pub(crate) fn clean_fs() {
            let date = OffsetDateTime::now_utc().date();
            let day_file = format!(
                "{}.{}",
                date.format(&DAY_FORMAT).expect("Could not format date"),
                DAY_EXTENTION
            );

            let _ = remove_file(test_fixtures_path().join("work").join(day_file));
        }
    }
}
