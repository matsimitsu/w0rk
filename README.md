# W0rk - Task Management CLI

[![CI](https://github.com/matsimitsu/w0rk/actions/workflows/ci.yml/badge.svg)](https://github.com/matsimitsu/w0rk/actions/workflows/ci.yml)

W0rk is a minimalist task management CLI tool written in Rust that helps you organize daily tasks and recurring activities. It features Slack integration for syncing your tasks and a simple, file-based storage system.

## Features

- Daily task management with Markdown files
- Support for recurring tasks
- Task states: Incomplete, In Progress, Completed, and Blocked
- Slack integration for task synchronization
- Nested subtasks support
- File-based storage using simple Markdown files

## Installation

1. Ensure you have Rust installed ([rustup](https://rustup.rs/))
2. Clone the repository
3. Build the project:

```bash
cargo build --release
```

## Usage

### Basic Commands

Create a new file for today:
```bash
w0rk new
```

Sync tasks with Slack:
```bash
w0rk sync
```

### Config

Create a config file in your config directory:

```
// Linux:   /home/alice/.config/w0rk
// Windows: C:\Users\Alice\AppData\Roaming\matsimitsu\w0rk
// macOS:   /Users/Alice/Library/Application Support/com.matsimitsu.w0rk
```

And add the working directory and optional Slack config:

```json
{
  "work_dir": "/Users/Alice/Documents/Work",
  "slack": {
    "token": "slack-token",
    "channel": "slack-channel"
  }
}
```

### Recurring Tasks

Create recurring tasks in `.recurring.md` in your work directory. These tasks will be automatically added to your daily task list.

```markdown
* [ ] @weekday Post in standup channel
* [ ] @weekday Deploy production with latest changes
* [ ] @monday Weekly product call
* [ ] @friday Write weekly report in Basecamp
```

## File Structure

- Daily tasks are stored as Markdown files named `YYYY-MM-DD.md`
- Recurring tasks are stored in `.recurring.md`
- Slack sync state is maintained in a JSON file in the working directory

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

This project is licensed under the MIT License - see the [LICENSE](LICENSE) file for details.
