//! All TUI state and interaction logic, free of terminal and I/O so it can be tested directly.
//! Keys go in, [`Effect`]s come out; the runtime performs the effects.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, Utc};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::config::SectionConfig;
use crate::model::{
    BlockId, Fingerprint, Item, ItemId, Namespace, ProfileName, Run, RunId, RunState, SourceId,
};
use crate::rules::Rule;

const TOAST_SECONDS: i64 = 5;
const FRESH_SECONDS: i64 = 15;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowRef {
    Item(ItemId),
    Run(RunId),
}

/// How the most recently hidden item was hidden, so undo can reverse the right thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hidden {
    Dismissed,
    Ignored,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Good,
    Attention,
    Bad,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    until: DateTime<Utc>,
}

/// A profile the user can start by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileChoice {
    pub name: ProfileName,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    Spawn {
        item: ItemId,
        choices: Vec<ProfileChoice>,
        index: usize,
    },
    Prompt {
        item: ItemId,
        base: ProfileName,
        text: String,
    },
    Filter {
        index: usize,
    },
    /// `dots` selects the page: the key list, or the legend for the dots.
    Help {
        dots: bool,
    },
}

/// Something the runtime must do on the app's behalf.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Quit,
    Sync,
    Open(String),
    Spawn {
        item: ItemId,
        profile: ProfileName,
        prompt: Option<String>,
    },
    Focus(BlockId),
    Reply {
        block: BlockId,
        text: &'static str,
    },
    Dismiss(ItemId, Fingerprint),
    Undismiss(ItemId),
    Ignore(ItemId),
    Unignore(ItemId),
    StopRun {
        run: RunId,
        block: Option<BlockId>,
    },
    ClearRun(RunId),
    SetNamespaceHidden(Namespace, bool),
}

/// Where the last frame drew something clickable.
#[derive(Debug, Clone, PartialEq)]
pub enum HitTarget {
    Row(RowRef),
    Key(KeyCode),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub area: Rect,
    pub target: HitTarget,
}

pub struct App {
    pub sections: Vec<SectionConfig>,
    rules: Vec<Rule>,
    profiles: Vec<ProfileChoice>,
    items: BTreeMap<SourceId, Vec<Item>>,
    runs: Vec<Run>,
    dismissed: HashMap<ItemId, Fingerprint>,
    hidden: BTreeSet<Namespace>,
    fresh: HashMap<ItemId, DateTime<Utc>>,
    selected: Option<RowRef>,
    last_index: usize,
    pub modal: Option<Modal>,
    toast: Option<Toast>,
    pub health: BTreeMap<SourceId, String>,
    pub last_sync: Option<DateTime<Utc>>,
    pub tick: usize,
    ignored: HashSet<ItemId>,
    show_ignored: bool,
    last_hidden: Option<(ItemId, Hidden)>,
    captures: HashMap<BlockId, String>,
    /// Filled by the renderer each frame.
    pub hits: Vec<Hit>,
    last_click: Option<(RowRef, DateTime<Utc>)>,
}

impl App {
    pub fn new(
        sections: Vec<SectionConfig>,
        rules: Vec<Rule>,
        profiles: Vec<ProfileChoice>,
    ) -> Self {
        Self {
            sections: if sections.is_empty() {
                SectionConfig::defaults()
            } else {
                sections
            },
            rules,
            profiles,
            items: BTreeMap::new(),
            runs: Vec::new(),
            dismissed: HashMap::new(),
            hidden: BTreeSet::new(),
            fresh: HashMap::new(),
            selected: None,
            last_index: 0,
            modal: None,
            toast: None,
            health: BTreeMap::new(),
            last_sync: None,
            tick: 0,
            ignored: HashSet::new(),
            show_ignored: false,
            last_hidden: None,
            captures: HashMap::new(),
            hits: Vec::new(),
            last_click: None,
        }
    }

    // ---- data in -------------------------------------------------------------------------

    pub fn set_dismissed(&mut self, dismissed: HashMap<ItemId, Fingerprint>) {
        self.dismissed = dismissed;
        self.ensure_selection();
    }

    pub fn set_ignored(&mut self, ignored: HashSet<ItemId>) {
        self.ignored = ignored;
        self.ensure_selection();
    }

    pub fn set_hidden(&mut self, hidden: BTreeSet<Namespace>) {
        self.hidden = hidden;
        self.ensure_selection();
    }

    pub fn set_items(&mut self, source: SourceId, items: Vec<Item>, now: DateTime<Utc>) {
        self.health.remove(&source);
        self.items.insert(source, items);
        self.last_sync = Some(now);
        self.ensure_selection();
    }

    pub fn set_source_error(&mut self, source: SourceId, error: String) {
        self.health.insert(source, error);
    }

    pub fn mark_fresh(&mut self, id: ItemId, now: DateTime<Utc>) {
        self.fresh
            .insert(id, now + Duration::seconds(FRESH_SECONDS));
    }

    pub fn set_capture(&mut self, block: BlockId, text: String) {
        self.captures.insert(block, text);
    }

    pub fn capture(&self, block: &BlockId) -> Option<&str> {
        self.captures.get(block).map(String::as_str)
    }

    /// Replace the runs list, toasting when an agent finishes or starts waiting on the user.
    pub fn set_runs(&mut self, runs: Vec<Run>, now: DateTime<Utc>) {
        let before: HashMap<RunId, RunState> = self.runs.iter().map(|r| (r.id, r.state)).collect();
        for run in &runs {
            let Some(&old) = before.get(&run.id) else {
                continue;
            };
            if old == run.state {
                continue;
            }
            let what = self.item_label(&run.item_id);
            match run.state {
                RunState::Done if old.is_active() => {
                    self.set_toast(
                        format!("✓ {} finished on {what}", run.profile),
                        ToastKind::Good,
                        now,
                    );
                }
                RunState::Waiting => {
                    self.set_toast(
                        format!("? {} needs you on {what}", run.profile),
                        ToastKind::Attention,
                        now,
                    );
                }
                _ => {}
            }
        }
        self.runs = runs;
        self.ensure_selection();
    }

    pub fn set_toast(&mut self, text: String, kind: ToastKind, now: DateTime<Utc>) {
        self.toast = Some(Toast {
            text,
            kind,
            until: now + Duration::seconds(TOAST_SECONDS),
        });
    }

    pub fn toast(&self, now: DateTime<Utc>) -> Option<&Toast> {
        self.toast.as_ref().filter(|t| t.until > now)
    }

    // ---- queries -------------------------------------------------------------------------

    pub fn item(&self, id: &ItemId) -> Option<&Item> {
        self.items.values().flatten().find(|i| &i.id == id)
    }

    fn item_label(&self, id: &ItemId) -> String {
        self.item(id)
            .map_or_else(|| id.to_string(), |i| i.title.clone())
    }

    pub fn run(&self, id: RunId) -> Option<&Run> {
        self.runs.iter().find(|r| r.id == id)
    }

    /// The newest run on an item.
    pub fn run_for_item(&self, id: &ItemId) -> Option<&Run> {
        self.runs.iter().find(|r| &r.item_id == id)
    }

    pub fn is_fresh(&self, id: &ItemId, now: DateTime<Utc>) -> bool {
        self.fresh.get(id).is_some_and(|until| *until > now)
    }

    fn namespace_of(item: &Item) -> Option<Namespace> {
        item.attrs
            .pointer("/namespace")
            .and_then(|v| v.as_str())
            .map(Namespace::from)
    }

    fn visible(&self, item: &Item) -> bool {
        let dismissed = self
            .dismissed
            .get(&item.id)
            .is_some_and(|fp| *fp == item.fingerprint);
        let hidden = Self::namespace_of(item).is_some_and(|ns| self.hidden.contains(&ns));
        let ignored = !self.show_ignored && self.ignored.contains(&item.id);
        !dismissed && !hidden && !ignored
    }

    pub fn is_ignored(&self, id: &ItemId) -> bool {
        self.ignored.contains(id)
    }

    /// Ignored items among those currently known, whether or not they are shown.
    pub fn ignored_count(&self) -> usize {
        self.items
            .values()
            .flatten()
            .filter(|i| self.ignored.contains(&i.id))
            .count()
    }

    pub fn showing_ignored(&self) -> bool {
        self.show_ignored
    }

    /// Items per configured section; an item lands in the first section that matches it.
    pub fn section_items(&self) -> Vec<Vec<&Item>> {
        let mut out: Vec<Vec<&Item>> = vec![Vec::new(); self.sections.len()];
        for item in self.items.values().flatten().filter(|i| self.visible(i)) {
            if let Some(slot) = self.sections.iter().position(|s| s.when.matches(item)) {
                out[slot].push(item);
            }
        }
        for list in &mut out {
            list.sort_by_key(|i| std::cmp::Reverse(i.updated_at));
        }
        out
    }

    /// Waiting first, then running, queued, done and stopped; newest first within a state.
    pub fn visible_runs(&self) -> Vec<&Run> {
        let rank = |s: RunState| match s {
            RunState::Waiting => 0,
            RunState::Running => 1,
            RunState::Queued => 2,
            RunState::Done => 3,
            RunState::Stopped => 4,
        };
        let mut runs: Vec<&Run> = self.runs.iter().collect();
        runs.sort_by_key(|r| (rank(r.state), std::cmp::Reverse(r.id)));
        runs
    }

    /// Agents holding a concurrency slot: started, and still running or waiting on the user.
    pub fn active_count(&self) -> usize {
        self.runs
            .iter()
            .filter(|r| {
                r.block.is_some() && matches!(r.state, RunState::Running | RunState::Waiting)
            })
            .count()
    }

    pub fn rows(&self) -> Vec<RowRef> {
        let mut rows: Vec<RowRef> = self
            .section_items()
            .into_iter()
            .flatten()
            .map(|i| RowRef::Item(i.id.clone()))
            .collect();
        rows.extend(self.visible_runs().into_iter().map(|r| RowRef::Run(r.id)));
        rows
    }

    pub fn selected(&self) -> Option<&RowRef> {
        self.selected.as_ref()
    }

    pub fn selected_item(&self) -> Option<&Item> {
        match self.selected.as_ref()? {
            RowRef::Item(id) => self.item(id),
            RowRef::Run(id) => self.item(&self.run(*id)?.item_id),
        }
    }

    /// The run the selection refers to: the selected agent, or the newest agent on the selected item.
    pub fn selected_run(&self) -> Option<&Run> {
        match self.selected.as_ref()? {
            RowRef::Run(id) => self.run(*id),
            RowRef::Item(id) => self.run_for_item(id),
        }
    }

    /// Namespaces to offer in the filter: everything seen plus anything already hidden.
    pub fn namespaces(&self) -> Vec<Namespace> {
        let mut all: BTreeSet<Namespace> = self.hidden.clone();
        all.extend(self.items.values().flatten().filter_map(Self::namespace_of));
        all.into_iter().collect()
    }

    pub fn is_hidden(&self, ns: &Namespace) -> bool {
        self.hidden.contains(ns)
    }

    pub fn hidden_count(&self) -> usize {
        self.items
            .values()
            .flatten()
            .filter(|i| Self::namespace_of(i).is_some_and(|ns| self.hidden.contains(&ns)))
            .count()
    }

    // ---- selection -----------------------------------------------------------------------

    fn ensure_selection(&mut self) {
        let rows = self.rows();
        if self.selected.as_ref().is_some_and(|s| rows.contains(s)) {
            if let Some(i) = self
                .selected
                .as_ref()
                .and_then(|s| rows.iter().position(|r| r == s))
            {
                self.last_index = i;
            }
            return;
        }
        self.selected = rows
            .get(self.last_index.min(rows.len().saturating_sub(1)))
            .cloned();
    }

    pub fn select(&mut self, row: RowRef) {
        self.selected = Some(row);
        self.ensure_selection();
    }

    fn move_by(&mut self, delta: isize) {
        let rows = self.rows();
        if rows.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|s| rows.iter().position(|r| r == s))
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(rows.len() - 1);
        self.selected = Some(rows[next].clone());
        self.last_index = next;
    }

    /// Jump to the first row of the next (or previous) non-empty section.
    fn jump_section(&mut self, forward: bool) {
        let mut groups: Vec<Vec<RowRef>> = self
            .section_items()
            .into_iter()
            .map(|items| {
                items
                    .into_iter()
                    .map(|i| RowRef::Item(i.id.clone()))
                    .collect()
            })
            .collect();
        groups.push(
            self.visible_runs()
                .into_iter()
                .map(|r| RowRef::Run(r.id))
                .collect(),
        );
        groups.retain(|g| !g.is_empty());
        let Some(current) = groups
            .iter()
            .position(|g| self.selected.as_ref().is_some_and(|s| g.contains(s)))
        else {
            return;
        };
        let n = groups.len();
        let next = if forward {
            (current + 1) % n
        } else {
            (current + n - 1) % n
        };
        self.selected = Some(groups[next][0].clone());
        self.ensure_selection();
    }

    // ---- input ---------------------------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent, now: DateTime<Utc>) -> Vec<Effect> {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return vec![Effect::Quit];
        }
        if self.modal.is_some() {
            return self.modal_key(key.code);
        }
        self.key(key.code, now)
    }

    /// Mouse click on a previously drawn target. A second click on the same row opens it.
    pub fn click(&mut self, column: u16, row: u16, now: DateTime<Utc>) -> Vec<Effect> {
        let hit = self.hits.iter().rev().find(|h| {
            column >= h.area.x
                && column < h.area.x + h.area.width
                && row >= h.area.y
                && row < h.area.y + h.area.height
        });
        match hit.map(|h| h.target.clone()) {
            Some(HitTarget::Key(code)) => {
                self.handle_key(KeyEvent::new(code, KeyModifiers::NONE), now)
            }
            Some(HitTarget::Row(target)) => {
                let double = self.last_click.as_ref().is_some_and(|(prev, at)| {
                    *prev == target && now - *at < Duration::milliseconds(500)
                });
                self.last_click = Some((target.clone(), now));
                self.select(target);
                if double {
                    self.open_selected(now)
                } else {
                    Vec::new()
                }
            }
            None => Vec::new(),
        }
    }

    pub fn scroll(&mut self, down: bool) {
        if self.modal.is_none() {
            self.move_by(if down { 1 } else { -1 });
        }
    }

    fn key(&mut self, code: KeyCode, now: DateTime<Utc>) -> Vec<Effect> {
        match code {
            KeyCode::Char('q') => return vec![Effect::Quit],
            KeyCode::Char('j') | KeyCode::Down => self.move_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_by(-1),
            KeyCode::Tab => self.jump_section(true),
            KeyCode::BackTab => self.jump_section(false),
            KeyCode::Enter => return self.open_selected(now),
            KeyCode::Char('s') => self.open_spawn(now),
            KeyCode::Char('p') => self.open_prompt(now),
            KeyCode::Char('g') => return self.focus_selected(now),
            KeyCode::Char(c @ ('y' | 'n')) => {
                return self.reply_selected(if c == 'y' { "y" } else { "n" }, now);
            }
            KeyCode::Char('x') => return self.dismiss_selected(now),
            KeyCode::Char('i') => return self.toggle_ignore(now),
            KeyCode::Char('I') => self.toggle_show_ignored(now),
            KeyCode::Char('u') => return self.undo(now),
            KeyCode::Char('f') => self.modal = Some(Modal::Filter { index: 0 }),
            KeyCode::Char('r') => return vec![Effect::Sync],
            KeyCode::Char('?') => self.modal = Some(Modal::Help { dots: false }),
            _ => {}
        }
        Vec::new()
    }

    fn open_selected(&mut self, now: DateTime<Utc>) -> Vec<Effect> {
        match self.selected.clone() {
            Some(RowRef::Run(id)) => {
                if let Some(item) = self.run(id).map(|r| r.item_id.clone()) {
                    self.select(RowRef::Item(item));
                }
                Vec::new()
            }
            Some(RowRef::Item(id)) => match self.item(&id).map(|i| i.url.clone()) {
                Some(url) if !url.is_empty() => vec![Effect::Open(url)],
                _ => {
                    self.set_toast("This item has no link".to_owned(), ToastKind::Info, now);
                    Vec::new()
                }
            },
            None => Vec::new(),
        }
    }

    /// Profiles whose rules match the item come first, then the rest.
    fn choices_for(&self, item: &Item) -> Vec<ProfileChoice> {
        let mut ordered: Vec<ProfileChoice> = Vec::new();
        let matching = self
            .rules
            .iter()
            .filter(|r| r.when.matches(item))
            .map(|r| &r.spawn);
        for name in matching.chain(self.profiles.iter().map(|p| &p.name)) {
            if let Some(choice) = self
                .profiles
                .iter()
                .find(|p| &p.name == name)
                .filter(|p| ordered.iter().all(|c| c.name != p.name))
            {
                ordered.push(choice.clone());
            }
        }
        ordered
    }

    fn open_spawn(&mut self, now: DateTime<Utc>) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        let choices = self.choices_for(&item);
        if choices.is_empty() {
            self.set_toast("No profiles configured".to_owned(), ToastKind::Info, now);
            return;
        }
        self.modal = Some(Modal::Spawn {
            item: item.id,
            choices,
            index: 0,
        });
    }

    fn open_prompt(&mut self, now: DateTime<Utc>) {
        let Some(item) = self.selected_item().cloned() else {
            return;
        };
        let Some(base) = self.choices_for(&item).into_iter().next() else {
            self.set_toast("No profiles configured".to_owned(), ToastKind::Info, now);
            return;
        };
        self.modal = Some(Modal::Prompt {
            item: item.id,
            base: base.name,
            text: String::new(),
        });
    }

    fn focus_selected(&mut self, now: DateTime<Utc>) -> Vec<Effect> {
        match self.selected_run().and_then(|r| r.block.clone()) {
            Some(block) => vec![Effect::Focus(block)],
            None => {
                self.set_toast(
                    "No agent pane on this item yet. Press s to start one.".to_owned(),
                    ToastKind::Info,
                    now,
                );
                Vec::new()
            }
        }
    }

    fn reply_selected(&mut self, text: &'static str, now: DateTime<Utc>) -> Vec<Effect> {
        match self.selected_run() {
            Some(run) if run.state == RunState::Waiting => match run.block.clone() {
                Some(block) => vec![Effect::Reply { block, text }],
                None => Vec::new(),
            },
            _ => {
                self.set_toast(
                    "No agent is waiting for an answer here".to_owned(),
                    ToastKind::Info,
                    now,
                );
                Vec::new()
            }
        }
    }

    fn dismiss_selected(&mut self, now: DateTime<Utc>) -> Vec<Effect> {
        match self.selected.clone() {
            Some(RowRef::Run(id)) => {
                let Some(run) = self.run(id).cloned() else {
                    return Vec::new();
                };
                if run.state.is_active() {
                    self.set_toast(
                        format!("Stopped {}. Press x again to clear it.", run.profile),
                        ToastKind::Info,
                        now,
                    );
                    vec![Effect::StopRun {
                        run: id,
                        block: run.block,
                    }]
                } else {
                    self.runs.retain(|r| r.id != id);
                    self.ensure_selection();
                    vec![Effect::ClearRun(id)]
                }
            }
            Some(RowRef::Item(id)) => {
                let Some(item) = self.item(&id).cloned() else {
                    return Vec::new();
                };
                self.dismissed.insert(id.clone(), item.fingerprint.clone());
                self.last_hidden = Some((id.clone(), Hidden::Dismissed));
                self.ensure_selection();
                self.set_toast(
                    format!("Dismissed {} until it changes. u to undo.", item.title),
                    ToastKind::Info,
                    now,
                );
                vec![Effect::Dismiss(id, item.fingerprint)]
            }
            None => Vec::new(),
        }
    }

    /// Ignore the selected item for good, or bring it back if it is already ignored.
    fn toggle_ignore(&mut self, now: DateTime<Utc>) -> Vec<Effect> {
        let Some(item) = self.selected_item().cloned() else {
            return Vec::new();
        };
        if self.ignored.remove(&item.id) {
            self.set_toast(
                format!("No longer ignoring {}", item.title),
                ToastKind::Info,
                now,
            );
            self.select(RowRef::Item(item.id.clone()));
            return vec![Effect::Unignore(item.id)];
        }
        self.ignored.insert(item.id.clone());
        self.last_hidden = Some((item.id.clone(), Hidden::Ignored));
        self.ensure_selection();
        let how = if self.show_ignored {
            "Ignoring"
        } else {
            "Ignored"
        };
        self.set_toast(
            format!("{how} {}. u to undo, I to show ignored.", item.title),
            ToastKind::Info,
            now,
        );
        vec![Effect::Ignore(item.id)]
    }

    fn toggle_show_ignored(&mut self, now: DateTime<Utc>) {
        self.show_ignored = !self.show_ignored;
        self.ensure_selection();
        let text = if self.show_ignored {
            "Showing ignored items"
        } else {
            "Hiding ignored items"
        };
        self.set_toast(text.to_owned(), ToastKind::Info, now);
    }

    fn undo(&mut self, now: DateTime<Utc>) -> Vec<Effect> {
        let Some((id, how)) = self.last_hidden.take() else {
            return Vec::new();
        };
        // Restore first: the item must be visible again before it can be selected.
        let effect = match how {
            Hidden::Dismissed => {
                self.dismissed.remove(&id);
                Effect::Undismiss(id.clone())
            }
            Hidden::Ignored => {
                self.ignored.remove(&id);
                Effect::Unignore(id.clone())
            }
        };
        self.select(RowRef::Item(id));
        self.set_toast("Restored".to_owned(), ToastKind::Info, now);
        vec![effect]
    }

    fn modal_key(&mut self, code: KeyCode) -> Vec<Effect> {
        let namespaces = self.namespaces();
        let Some(modal) = self.modal.as_mut() else {
            return Vec::new();
        };
        if code == KeyCode::Esc {
            self.modal = None;
            return Vec::new();
        }
        match modal {
            Modal::Help { dots } => {
                if code == KeyCode::Tab {
                    *dots = !*dots;
                } else {
                    self.modal = None;
                }
            }
            Modal::Spawn {
                item,
                choices,
                index,
            } => match code {
                KeyCode::Char('j') | KeyCode::Down => *index = (*index + 1).min(choices.len() - 1),
                KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                KeyCode::Enter => {
                    let effect = Effect::Spawn {
                        item: item.clone(),
                        profile: choices[*index].name.clone(),
                        prompt: None,
                    };
                    self.modal = None;
                    return vec![effect];
                }
                _ => {}
            },
            Modal::Prompt { item, base, text } => match code {
                KeyCode::Enter if !text.trim().is_empty() => {
                    let effect = Effect::Spawn {
                        item: item.clone(),
                        profile: base.clone(),
                        prompt: Some(text.trim().to_owned()),
                    };
                    self.modal = None;
                    return vec![effect];
                }
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                _ => {}
            },
            Modal::Filter { index } => match code {
                KeyCode::Char('j') | KeyCode::Down => {
                    *index = (*index + 1).min(namespaces.len().saturating_sub(1))
                }
                KeyCode::Char('k') | KeyCode::Up => *index = index.saturating_sub(1),
                KeyCode::Char(' ') => {
                    if let Some(ns) = namespaces.get(*index).cloned() {
                        let hide = !self.hidden.contains(&ns);
                        if hide {
                            self.hidden.insert(ns.clone());
                        } else {
                            self.hidden.remove(&ns);
                        }
                        self.ensure_selection();
                        return vec![Effect::SetNamespaceHidden(ns, hide)];
                    }
                }
                KeyCode::Enter => self.modal = None,
                _ => {}
            },
        }
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::model::{ItemKind, ProfileName};

    fn now() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH + Duration::seconds(1000)
    }

    fn item(id: &str, kind: &str, fp: &str, secs: i64, attrs: serde_json::Value) -> Item {
        Item {
            id: ItemId::new(id),
            source: SourceId::new("s"),
            kind: ItemKind::new(kind),
            title: format!("title {id}"),
            url: format!("https://x/{id}"),
            fingerprint: Fingerprint::new(fp),
            updated_at: DateTime::UNIX_EPOCH + Duration::seconds(secs),
            attrs,
        }
    }

    fn run(id: i64, item: &str, state: RunState, block: Option<&str>) -> Run {
        Run {
            id: RunId::new(id),
            item_id: ItemId::new(item),
            fingerprint: Fingerprint::new("f"),
            profile: ProfileName::new("code-review"),
            block: block.map(BlockId::from),
            state,
            verdict: None,
            summary: None,
            created_at: DateTime::UNIX_EPOCH,
            updated_at: DateTime::UNIX_EPOCH,
        }
    }

    fn app() -> App {
        let rules = crate::config::Config::parse(
            r#"
            [[rule]]
            name = "r"
            when = { kind = "incident.exception" }
            spawn = "debug"
            [profile.debug]
            prompt = "p"
            cwd = "/"
            [profile.review]
            prompt = "p"
            cwd = "/"
            "#,
        )
        .unwrap()
        .rules;
        let profiles = ["debug", "review"]
            .iter()
            .map(|n| ProfileChoice {
                name: ProfileName::new(*n),
                description: String::new(),
            })
            .collect();
        let mut app = App::new(Vec::new(), rules, profiles);
        app.set_items(
            SourceId::new("s"),
            vec![
                item("pr1", "pr.review", "a", 10, json!({})),
                item("pr2", "pr.review", "a", 20, json!({})),
                item(
                    "inc1",
                    "incident.exception",
                    "OPEN",
                    5,
                    json!({"namespace": "web"}),
                ),
                item(
                    "inc2",
                    "incident.exception",
                    "OPEN",
                    6,
                    json!({"namespace": "admin"}),
                ),
            ],
            now(),
        );
        app
    }

    fn press(app: &mut App, c: char) -> Vec<Effect> {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), now())
    }

    #[test]
    fn items_are_sectioned_newest_first_then_agents() {
        let mut app = app();
        app.set_runs(vec![run(1, "pr1", RunState::Running, Some("b"))], now());
        let ids: Vec<String> = app
            .rows()
            .iter()
            .map(|r| match r {
                RowRef::Item(id) => id.to_string(),
                RowRef::Run(id) => format!("run{id}"),
            })
            .collect();
        assert_eq!(ids, ["pr2", "pr1", "inc2", "inc1", "run1"]);
        assert_eq!(
            app.selected(),
            Some(&RowRef::Item(ItemId::new("pr2"))),
            "first row is selected"
        );
    }

    #[test]
    fn j_k_move_and_clamp_and_tab_jumps_sections() {
        let mut app = app();
        press(&mut app, 'j');
        assert_eq!(app.selected(), Some(&RowRef::Item(ItemId::new("pr1"))));
        for _ in 0..10 {
            press(&mut app, 'j');
        }
        assert_eq!(
            app.selected(),
            Some(&RowRef::Item(ItemId::new("inc1"))),
            "clamped at the end"
        );
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), now());
        assert_eq!(
            app.selected(),
            Some(&RowRef::Item(ItemId::new("pr2"))),
            "wraps to the first section"
        );
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), now());
        assert_eq!(app.selected(), Some(&RowRef::Item(ItemId::new("inc2"))));
    }

    #[test]
    fn enter_opens_the_url() {
        let mut app = app();
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now()),
            vec![Effect::Open("https://x/pr2".to_owned())]
        );
    }

    #[test]
    fn dismiss_hides_until_the_fingerprint_changes_and_undo_restores() {
        let mut app = app();
        let effects = press(&mut app, 'x');
        assert_eq!(
            effects,
            vec![Effect::Dismiss(ItemId::new("pr2"), Fingerprint::new("a"))]
        );
        assert!(!app.rows().contains(&RowRef::Item(ItemId::new("pr2"))));
        assert_eq!(
            app.selected(),
            Some(&RowRef::Item(ItemId::new("pr1"))),
            "selection moves on"
        );

        // The PR gets new commits: it comes back.
        let mut items = vec![item("pr2", "pr.review", "b", 30, json!({}))];
        items.push(item("pr1", "pr.review", "a", 10, json!({})));
        app.set_items(SourceId::new("s"), items, now());
        assert!(app.rows().contains(&RowRef::Item(ItemId::new("pr2"))));

        let mut app = self::app();
        press(&mut app, 'x');
        assert_eq!(
            press(&mut app, 'u'),
            vec![Effect::Undismiss(ItemId::new("pr2"))]
        );
        assert!(app.rows().contains(&RowRef::Item(ItemId::new("pr2"))));
    }

    #[test]
    fn ignoring_hides_for_good_even_when_the_item_changes() {
        let mut app = app();
        let effects = press(&mut app, 'i');
        assert_eq!(effects, vec![Effect::Ignore(ItemId::new("pr2"))]);
        assert!(!app.rows().contains(&RowRef::Item(ItemId::new("pr2"))));
        assert_eq!(app.ignored_count(), 1);

        // New commits change the fingerprint; a dismissal would lift, an ignore must not.
        app.set_items(
            SourceId::new("s"),
            vec![
                item("pr2", "pr.review", "NEW-SHA", 99, json!({})),
                item("pr1", "pr.review", "a", 10, json!({})),
            ],
            now(),
        );
        assert!(!app.rows().contains(&RowRef::Item(ItemId::new("pr2"))));
    }

    #[test]
    fn capital_i_shows_ignored_items_and_i_unignores_them() {
        let mut app = app();
        press(&mut app, 'i');
        press(&mut app, 'I');
        assert!(app.showing_ignored());
        assert!(
            app.rows().contains(&RowRef::Item(ItemId::new("pr2"))),
            "shown again"
        );

        app.select(RowRef::Item(ItemId::new("pr2")));
        assert_eq!(
            press(&mut app, 'i'),
            vec![Effect::Unignore(ItemId::new("pr2"))]
        );
        assert!(!app.is_ignored(&ItemId::new("pr2")));
        press(&mut app, 'I');
        assert!(
            app.rows().contains(&RowRef::Item(ItemId::new("pr2"))),
            "no longer ignored"
        );
    }

    #[test]
    fn undo_reverses_whichever_hide_came_last() {
        let mut app = app();
        press(&mut app, 'i');
        assert_eq!(
            press(&mut app, 'u'),
            vec![Effect::Unignore(ItemId::new("pr2"))]
        );
        assert!(app.rows().contains(&RowRef::Item(ItemId::new("pr2"))));

        press(&mut app, 'x');
        assert_eq!(
            press(&mut app, 'u'),
            vec![Effect::Undismiss(ItemId::new("pr2"))]
        );
    }

    #[test]
    fn namespace_filter_hides_incidents_and_reports_the_change() {
        let mut app = app();
        press(&mut app, 'f');
        assert!(matches!(app.modal, Some(Modal::Filter { .. })));
        // namespaces are sorted: admin, web
        let effects = press(&mut app, ' ');
        assert_eq!(
            effects,
            vec![Effect::SetNamespaceHidden(Namespace::new("admin"), true)]
        );
        assert!(!app.rows().contains(&RowRef::Item(ItemId::new("inc2"))));
        assert_eq!(app.hidden_count(), 1);
        press(&mut app, ' ');
        assert!(app.rows().contains(&RowRef::Item(ItemId::new("inc2"))));
    }

    #[test]
    fn spawn_picker_lists_matching_profiles_first() {
        let mut app = app();
        app.select(RowRef::Item(ItemId::new("inc1")));
        press(&mut app, 's');
        let Some(Modal::Spawn { choices, .. }) = &app.modal else {
            panic!("no spawn modal")
        };
        assert_eq!(
            choices[0].name.as_str(),
            "debug",
            "rule-matched profile leads"
        );
        assert_eq!(choices.len(), 2);

        press(&mut app, 'j');
        let effects = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now());
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                item: ItemId::new("inc1"),
                profile: ProfileName::new("review"),
                prompt: None
            }]
        );
        assert!(app.modal.is_none());
    }

    #[test]
    fn custom_prompt_collects_text_then_spawns_on_the_base_profile() {
        let mut app = app();
        app.select(RowRef::Item(ItemId::new("inc1")));
        press(&mut app, 'p');
        for c in "is this #1927?".chars() {
            press(&mut app, c);
        }
        let effects = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now());
        assert_eq!(
            effects,
            vec![Effect::Spawn {
                item: ItemId::new("inc1"),
                profile: ProfileName::new("debug"),
                prompt: Some("is this #1927?".to_owned())
            }]
        );
    }

    #[test]
    fn empty_prompt_does_not_spawn_and_esc_cancels() {
        let mut app = app();
        press(&mut app, 'p');
        assert!(
            app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), now())
                .is_empty()
        );
        assert!(app.modal.is_some());
        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), now());
        assert!(app.modal.is_none());
    }

    #[test]
    fn reply_only_works_on_a_waiting_agent() {
        let mut app = app();
        app.set_runs(
            vec![run(1, "pr2", RunState::Running, Some("block:1"))],
            now(),
        );
        assert!(press(&mut app, 'y').is_empty());
        app.set_runs(
            vec![run(1, "pr2", RunState::Waiting, Some("block:1"))],
            now(),
        );
        assert_eq!(
            press(&mut app, 'y'),
            vec![Effect::Reply {
                block: BlockId::new("block:1"),
                text: "y"
            }]
        );
        assert!(
            app.toast(now())
                .is_some_and(|t| t.kind == ToastKind::Attention),
            "the transition toasted"
        );
    }

    #[test]
    fn g_focuses_the_agent_pane_from_the_item_or_the_agent_row() {
        let mut app = app();
        app.set_runs(
            vec![run(1, "pr2", RunState::Running, Some("block:1"))],
            now(),
        );
        assert_eq!(
            press(&mut app, 'g'),
            vec![Effect::Focus(BlockId::new("block:1"))]
        );
        app.select(RowRef::Run(RunId::new(1)));
        assert_eq!(
            press(&mut app, 'g'),
            vec![Effect::Focus(BlockId::new("block:1"))]
        );
        app.select(RowRef::Item(ItemId::new("pr1")));
        assert!(press(&mut app, 'g').is_empty(), "no agent on pr1");
    }

    #[test]
    fn x_on_agents_stops_then_clears() {
        let mut app = app();
        app.set_runs(
            vec![run(1, "pr2", RunState::Running, Some("block:1"))],
            now(),
        );
        app.select(RowRef::Run(RunId::new(1)));
        assert_eq!(
            press(&mut app, 'x'),
            vec![Effect::StopRun {
                run: RunId::new(1),
                block: Some(BlockId::new("block:1"))
            }]
        );
        app.set_runs(
            vec![run(1, "pr2", RunState::Stopped, Some("block:1"))],
            now(),
        );
        app.select(RowRef::Run(RunId::new(1)));
        assert_eq!(press(&mut app, 'x'), vec![Effect::ClearRun(RunId::new(1))]);
        assert!(!app.rows().contains(&RowRef::Run(RunId::new(1))));
    }

    #[test]
    fn finishing_an_agent_toasts() {
        let mut app = app();
        app.set_runs(vec![run(1, "pr2", RunState::Running, Some("b"))], now());
        app.set_runs(vec![run(1, "pr2", RunState::Done, Some("b"))], now());
        let toast = app.toast(now()).unwrap();
        assert_eq!(toast.kind, ToastKind::Good);
        assert!(toast.text.contains("title pr2"));
        assert!(
            app.toast(now() + Duration::seconds(10)).is_none(),
            "toasts expire"
        );
    }

    #[test]
    fn clicking_selects_and_double_click_opens() {
        let mut app = app();
        app.hits = vec![Hit {
            area: Rect::new(0, 3, 40, 1),
            target: HitTarget::Row(RowRef::Item(ItemId::new("pr1"))),
        }];
        assert!(app.click(5, 3, now()).is_empty());
        assert_eq!(app.selected(), Some(&RowRef::Item(ItemId::new("pr1"))));
        let effects = app.click(5, 3, now() + Duration::milliseconds(200));
        assert_eq!(effects, vec![Effect::Open("https://x/pr1".to_owned())]);
        assert!(
            app.click(90, 20, now()).is_empty(),
            "clicks on nothing do nothing"
        );
    }

    #[test]
    fn help_has_two_pages_and_any_other_key_closes_it() {
        let mut app = app();
        press(&mut app, '?');
        assert_eq!(app.modal, Some(Modal::Help { dots: false }));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), now());
        assert_eq!(app.modal, Some(Modal::Help { dots: true }));
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), now());
        assert_eq!(app.modal, Some(Modal::Help { dots: false }));
        press(&mut app, 'x');
        assert_eq!(app.modal, None);
    }

    #[test]
    fn quit_keys_and_empty_app() {
        let mut empty = App::new(Vec::new(), Vec::new(), Vec::new());
        assert_eq!(empty.selected(), None);
        assert!(press(&mut empty, 'j').is_empty());
        assert!(press(&mut empty, 's').is_empty());
        assert_eq!(press(&mut empty, 'q'), vec![Effect::Quit]);
        assert_eq!(
            empty.handle_key(
                KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
                now()
            ),
            vec![Effect::Quit]
        );
    }
}
