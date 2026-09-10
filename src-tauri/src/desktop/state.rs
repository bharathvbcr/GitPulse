//! Native presentation only. Repository authorization remains in the guarded Git commands.
use super::actions::{self, NativeAction};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct MenuLabel {
    pub id: String,
    pub text: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MenuRepository {
    pub path: String,
    pub label: String,
    pub active: bool,
    pub changed: Option<u32>,
    pub conflicts: Option<u32>,
    pub busy: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StatusCard {
    pub repository: String,
    pub branch: String,
    pub changed: Option<u32>,
    pub staged: Option<u32>,
    pub conflicts: Option<u32>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub upstream: Option<String>,
    pub headline: String,
    pub tone: String,
    pub primary_label: String,
    pub watch_status: String,
    pub reduce_motion: bool,
    pub stashes: Option<u32>,
    pub operation: Option<String>,
    pub activity: Option<String>,
    pub elsewhere: u32,
    /// Epoch milliseconds of `FETCH_HEAD` mtime; `None` when never fetched.
    pub fetched_at: Option<i64>,
}

impl Default for StatusCard {
    fn default() -> Self {
        Self {
            repository: "GitPulse".into(),
            branch: String::new(),
            changed: None,
            staged: None,
            conflicts: None,
            ahead: None,
            behind: None,
            upstream: None,
            headline: "Your work, at a glance".into(),
            tone: "neutral".into(),
            primary_label: "Open repository".into(),
            watch_status: "unknown".into(),
            reduce_motion: false,
            stashes: None,
            operation: None,
            activity: None,
            elsewhere: 0,
            fetched_at: None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MenuState {
    pub enabled: Vec<String>,
    pub checked: Vec<String>,
    pub labels: Vec<MenuLabel>,
    pub repositories: Vec<MenuRepository>,
    pub active_path: Option<String>,
    pub show_status_icon: bool,
    /// Hide the Dock icon while the main window is closed and the status icon is on.
    pub hide_dock_when_closed: bool,
    /// Optional counts rendered beside the menu-bar glyph; `None` when the pref is off.
    pub tray_title: Option<String>,
    pub tray_details: Vec<String>,
    pub tray_summary: MenuLabel,
    pub tray_detail: String,
    pub status: StatusCard,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            enabled: [
                actions::OPEN,
                actions::CLONE,
                actions::SETTINGS,
                actions::SHORTCUTS,
                actions::DIAGNOSTICS,
                actions::DOCUMENTATION,
                actions::RELEASE_NOTES,
                actions::REPORT_ISSUE,
                actions::SETUP_TOOLS,
                actions::ZOOM_IN,
                actions::ZOOM_OUT,
                actions::TOGGLE_THEME,
                actions::THEME_SYSTEM,
                actions::THEME_LIGHT,
                actions::THEME_DARK,
                actions::FLEET,
                actions::PALETTE,
                actions::CHECK_UPDATES,
            ]
            .into_iter()
            .map(String::from)
            .collect(),
            checked: vec![actions::THEME_SYSTEM.into()],
            labels: vec![],
            repositories: vec![],
            active_path: None,
            show_status_icon: false,
            hide_dock_when_closed: true,
            tray_title: None,
            tray_details: vec![],
            tray_summary: MenuLabel {
                id: actions::OPEN.into(),
                text: "Open a repository…".into(),
            },
            tray_detail: String::new(),
            status: StatusCard::default(),
        }
    }
}

/// Identity for the native application-menu repository switcher (counts churn separately).
pub fn repository_switcher_key(repo: &MenuRepository) -> (&str, &str, bool) {
    (repo.path.as_str(), repo.label.as_str(), repo.active)
}

pub fn checkable(id: &str) -> bool {
    id.starts_with("tab-")
        || id.starts_with("section:")
        || id.starts_with(actions::REPOSITORY_PREFIX)
        || matches!(
            id,
            actions::THEME_SYSTEM
                | actions::THEME_LIGHT
                | actions::THEME_DARK
                | actions::FLEET
                | actions::TERMINAL_DOCK
        )
}

pub fn menu_text(text: &str) -> String {
    // Muda uses ampersands for mnemonics off macOS. Escaping works on every platform.
    let cleaned: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(240)
        .collect();
    cleaned.replace('&', "&&")
}

impl MenuState {
    pub fn enabled(&self, id: &str) -> bool {
        if let Some(path) = id.strip_prefix(actions::REPOSITORY_PREFIX) {
            return self.repositories.iter().any(|repo| repo.path == path);
        }
        self.enabled.iter().any(|entry| entry == id)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !matches!(
            self.status.tone.as_str(),
            "neutral" | "warning" | "busy" | "changed" | "clean"
        ) || !matches!(
            self.status.watch_status.as_str(),
            "unknown" | "watching" | "degraded"
        ) || [
            &self.status.repository,
            &self.status.branch,
            &self.status.headline,
            &self.status.primary_label,
        ]
        .iter()
        .any(|text| text.len() > 2048)
            || self
                .status
                .upstream
                .as_ref()
                .is_some_and(|text| text.len() > 2048)
            || self
                .status
                .operation
                .as_ref()
                .is_some_and(|text| text.len() > 2048)
            || self
                .status
                .activity
                .as_ref()
                .is_some_and(|text| text.len() > 2048)
        {
            return Err("Invalid status card presentation".into());
        }
        if self.enabled.len() > 128
            || self.checked.len() > 64
            || self.labels.len() > 64
            || self.repositories.len() > 64
        {
            return Err("Native menu state exceeds its entry limit".into());
        }
        let known =
            |id: &str| NativeAction::parse(id).is_some_and(|action| action.path().is_none());
        if self.enabled.iter().any(|id| !known(id))
            || self.checked.iter().any(|id| !known(id) || !checkable(id))
            || !matches!(
                self.tray_summary.id.as_str(),
                actions::OPEN
                    | actions::REFRESH
                    | "section:work:resolve"
                    | "section:work:overview"
                    | "section:history:graph"
            )
        {
            return Err("Native menu state contains an unknown action".into());
        }
        let mut labels = HashSet::new();
        for label in &self.labels {
            if !known(&label.id) || label.text.len() > 2048 || !labels.insert(&label.id) {
                return Err("Native menu state contains an invalid label".into());
            }
        }
        let mut paths = HashSet::new();
        for repo in &self.repositories {
            if repo.path.is_empty()
                || repo.path.len() > 16384
                || repo.label.len() > 2048
                || !paths.insert(&repo.path)
            {
                return Err("Native menu state contains an invalid repository".into());
            }
            if repo.active != (self.active_path.as_ref() == Some(&repo.path)) {
                return Err("Native menu active repository does not match its path".into());
            }
            if repo.changed.is_some_and(|n| n > 1_000_000)
                || repo.conflicts.is_some_and(|n| n > 1_000_000)
            {
                return Err("Native menu repository counts exceed their limit".into());
            }
        }
        if self
            .active_path
            .as_ref()
            .is_some_and(|path| !paths.contains(path))
        {
            return Err("Native menu active repository is missing from the switcher".into());
        }
        if self
            .active_path
            .as_ref()
            .is_some_and(|path| path.len() > 16384)
            || self.tray_details.len() > 16
            || self.tray_details.iter().any(|text| text.len() > 2048)
            || self.tray_summary.text.len() > 2048
            || self.tray_detail.len() > 2048
            || self
                .tray_title
                .as_ref()
                .is_some_and(|title| title.chars().count() > 24)
        {
            return Err("Native menu text exceeds its limit".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_is_useful_without_enabling_repository_mutations() {
        let state = MenuState::default();
        state.validate().unwrap();
        assert!(state.enabled(actions::OPEN));
        assert!(!state.enabled(actions::FETCH));
        assert!(!state.enabled(actions::STASH_POP));
        assert!(!state.enabled(actions::OPERATION_ABORT));
    }

    #[test]
    fn rejects_unknown_oversized_and_duplicate_payloads() {
        let mut state = MenuState::default();
        state.enabled.push("unknown".into());
        assert!(state.validate().is_err());
        state = MenuState::default();
        state.checked.push(actions::FETCH.into());
        assert!(state.validate().is_err());
        state = MenuState::default();
        state.repositories = vec![
            MenuRepository {
                path: "/r/a".into(),
                ..Default::default()
            };
            2
        ];
        assert!(state.validate().is_err());
        state.repositories = vec![MenuRepository::default(); 65];
        assert!(state.validate().is_err());
        state = MenuState::default();
        state.active_path = Some("/missing".into());
        assert!(state.validate().is_err());
        state.repositories.push(MenuRepository {
            path: "/missing".into(),
            ..Default::default()
        });
        assert!(state.validate().is_err());
        state.repositories[0].active = true;
        assert!(state.validate().is_ok());
        state.tray_details = vec!["line".into(); 17];
        assert!(state.validate().is_err());
        assert_eq!(menu_text("A\tB\nC & D"), "A B C && D");
        state = MenuState::default();
        state.status.operation = Some("x".repeat(2049));
        assert!(state.validate().is_err());
        state.status.operation = Some("Merge in progress".into());
        state.status.stashes = Some(2);
        state.status.elsewhere = 1;
        state.validate().unwrap();
    }
}
