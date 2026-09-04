//! Menu / Finder action IDs shared between the native menu and the frontend.

pub const OPEN: &str = "open";
pub const CLONE: &str = "clone";
pub const SETTINGS: &str = "settings";
pub const REFRESH: &str = "refresh";
pub const TOGGLE_THEME: &str = "toggle-theme";
pub const THEME_SYSTEM: &str = "theme-system";
pub const THEME_LIGHT: &str = "theme-light";
pub const THEME_DARK: &str = "theme-dark";
pub const TAB_WORK: &str = "tab-work";
pub const TAB_HISTORY: &str = "tab-history";
pub const TAB_CODE: &str = "tab-code";
pub const TAB_INSIGHTS: &str = "tab-insights";
/// Fleet is workspace-scoped, not a repository view, so it is deliberately
/// NOT a `tab-*` id: those are parsed back into a `ViewTab` and stored on the
/// active repository's session, which is exactly what Fleet must not be.
pub const FLEET: &str = "fleet";
/// The terminal dock, for the same reason it is not a `tab-*` id: the terminal
/// is no longer a view. It renders beneath whichever view is on screen, so
/// parsing it into a `ViewTab` would store a destination that does not exist.
pub const TERMINAL_DOCK: &str = "terminal-dock";
pub const FETCH: &str = "fetch";
pub const PULL: &str = "pull";
pub const PUSH: &str = "push";
pub const STASH: &str = "stash";
pub const STASH_POP: &str = "stash-pop";
pub const QUICK_COMMIT: &str = "quick-commit";
pub const REBASE: &str = "rebase";
pub const PALETTE: &str = "palette";
pub const FOCUS_FILTER: &str = "focus-filter";
pub const OPEN_RECENT: &str = "open-recent";
pub const RECENT_PREFIX: &str = "open-recent:";
pub const RECENT_EMPTY: &str = "recent-empty";
pub const CLOSE_TAB: &str = "close-tab";
pub const NEXT_REPO_TAB: &str = "next-repo-tab";
pub const PREV_REPO_TAB: &str = "prev-repo-tab";
pub const REOPEN_REPO_TAB: &str = "reopen-repo-tab";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeAction {
    Open,
    Clone,
    Settings,
    Refresh,
    ToggleTheme,
    ThemeSystem,
    ThemeLight,
    ThemeDark,
    TabWork,
    TabHistory,
    TabCode,
    TabInsights,
    Fleet,
    TerminalDock,
    Fetch,
    Pull,
    Push,
    Stash,
    StashPop,
    QuickCommit,
    Rebase,
    Palette,
    FocusFilter,
    OpenRecent(String),
    CloseTab,
    NextRepoTab,
    PrevRepoTab,
    ReopenRepoTab,
}

impl NativeAction {
    pub fn parse(id: &str) -> Option<Self> {
        if let Some(path) = id.strip_prefix(RECENT_PREFIX) {
            if path.is_empty() {
                return None;
            }
            return Some(Self::OpenRecent(path.to_string()));
        }
        Some(match id {
            OPEN => Self::Open,
            CLONE => Self::Clone,
            SETTINGS => Self::Settings,
            REFRESH => Self::Refresh,
            TOGGLE_THEME => Self::ToggleTheme,
            THEME_SYSTEM => Self::ThemeSystem,
            THEME_LIGHT => Self::ThemeLight,
            THEME_DARK => Self::ThemeDark,
            TAB_WORK => Self::TabWork,
            TAB_HISTORY => Self::TabHistory,
            TAB_CODE => Self::TabCode,
            TAB_INSIGHTS => Self::TabInsights,
            FLEET => Self::Fleet,
            TERMINAL_DOCK => Self::TerminalDock,
            FETCH => Self::Fetch,
            PULL => Self::Pull,
            PUSH => Self::Push,
            STASH => Self::Stash,
            STASH_POP => Self::StashPop,
            QUICK_COMMIT => Self::QuickCommit,
            REBASE => Self::Rebase,
            PALETTE => Self::Palette,
            FOCUS_FILTER => Self::FocusFilter,
            CLOSE_TAB => Self::CloseTab,
            NEXT_REPO_TAB => Self::NextRepoTab,
            PREV_REPO_TAB => Self::PrevRepoTab,
            REOPEN_REPO_TAB => Self::ReopenRepoTab,
            RECENT_EMPTY => return None,
            _ => return None,
        })
    }

    pub fn event_id(&self) -> &'static str {
        match self {
            Self::Open => OPEN,
            Self::Clone => CLONE,
            Self::Settings => SETTINGS,
            Self::Refresh => REFRESH,
            Self::ToggleTheme => TOGGLE_THEME,
            Self::ThemeSystem => THEME_SYSTEM,
            Self::ThemeLight => THEME_LIGHT,
            Self::ThemeDark => THEME_DARK,
            Self::TabWork => TAB_WORK,
            Self::TabHistory => TAB_HISTORY,
            Self::TabCode => TAB_CODE,
            Self::TabInsights => TAB_INSIGHTS,
            Self::Fleet => FLEET,
            Self::TerminalDock => TERMINAL_DOCK,
            Self::Fetch => FETCH,
            Self::Pull => PULL,
            Self::Push => PUSH,
            Self::Stash => STASH,
            Self::StashPop => STASH_POP,
            Self::QuickCommit => QUICK_COMMIT,
            Self::Rebase => REBASE,
            Self::Palette => PALETTE,
            Self::FocusFilter => FOCUS_FILTER,
            Self::OpenRecent(_) => OPEN_RECENT,
            Self::CloseTab => CLOSE_TAB,
            Self::NextRepoTab => NEXT_REPO_TAB,
            Self::PrevRepoTab => PREV_REPO_TAB,
            Self::ReopenRepoTab => REOPEN_REPO_TAB,
        }
    }

    pub fn path(&self) -> Option<&str> {
        match self {
            Self::OpenRecent(path) => Some(path),
            _ => None,
        }
    }

    pub fn recent_menu_id(path: &str) -> String {
        format!("{RECENT_PREFIX}{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_known_commands() {
        assert_eq!(NativeAction::parse(OPEN), Some(NativeAction::Open));
        assert_eq!(NativeAction::parse(CLONE), Some(NativeAction::Clone));
        assert_eq!(NativeAction::parse(SETTINGS), Some(NativeAction::Settings));
        assert_eq!(NativeAction::parse(PALETTE), Some(NativeAction::Palette));
        assert_eq!(
            NativeAction::parse(QUICK_COMMIT),
            Some(NativeAction::QuickCommit)
        );
        assert_eq!(NativeAction::parse(CLOSE_TAB), Some(NativeAction::CloseTab));
        assert_eq!(
            NativeAction::parse(NEXT_REPO_TAB),
            Some(NativeAction::NextRepoTab)
        );
        assert_eq!(
            NativeAction::parse(PREV_REPO_TAB),
            Some(NativeAction::PrevRepoTab)
        );
        assert_eq!(
            NativeAction::parse(REOPEN_REPO_TAB),
            Some(NativeAction::ReopenRepoTab)
        );
        assert_eq!(NativeAction::parse(TAB_WORK), Some(NativeAction::TabWork));
        assert_eq!(NativeAction::parse(TAB_CODE), Some(NativeAction::TabCode));
        assert_eq!(
            NativeAction::parse(TAB_INSIGHTS),
            Some(NativeAction::TabInsights)
        );
        assert_eq!(
            NativeAction::parse(TERMINAL_DOCK),
            Some(NativeAction::TerminalDock)
        );
        // Retired views must not resolve, or an old menu build would silently
        // store a `ViewTab` this app has no pane for. The terminal became a
        // dock; Diff and Reflog became sections of History.
        assert_eq!(NativeAction::parse("tab-terminal"), None);
        assert_eq!(NativeAction::parse("tab-diff"), None);
        assert_eq!(NativeAction::parse("tab-reflog"), None);
        // Pulse, Coverage, Health and Storage became sections of Insights.
        assert_eq!(NativeAction::parse("tab-pulse"), None);
        assert_eq!(NativeAction::parse("tab-coverage"), None);
        assert_eq!(NativeAction::parse("tab-health"), None);
        assert_eq!(NativeAction::parse("tab-storage"), None);
        // GitHub, MANVI, Stack and Resolve became sections of Work.
        assert_eq!(NativeAction::parse("tab-github"), None);
        assert_eq!(NativeAction::parse("tab-manvi"), None);
        assert_eq!(NativeAction::parse("tab-stack"), None);
        assert_eq!(NativeAction::parse("tab-conflict"), None);
        // Files and Blame became sections of Code.
        assert_eq!(NativeAction::parse("tab-files"), None);
        assert_eq!(NativeAction::parse("tab-blame"), None);
        assert_eq!(NativeAction::parse("nope"), None);
        assert_eq!(NativeAction::parse(RECENT_EMPTY), None);
        assert_eq!(NativeAction::parse(RECENT_PREFIX), None);
    }

    #[test]
    fn parse_recent_keeps_colons_in_path() {
        let id = NativeAction::recent_menu_id(r"C:\Users\acme\repo");
        assert_eq!(
            NativeAction::parse(&id),
            Some(NativeAction::OpenRecent(r"C:\Users\acme\repo".into()))
        );
        let unix = NativeAction::recent_menu_id("/Users/acme/my:repo");
        assert_eq!(
            NativeAction::parse(&unix),
            Some(NativeAction::OpenRecent("/Users/acme/my:repo".into()))
        );
    }

    #[test]
    fn event_payload_uses_stable_ids() {
        let recent = NativeAction::OpenRecent("/tmp/repo".into());
        assert_eq!(recent.event_id(), OPEN_RECENT);
        assert_eq!(recent.path(), Some("/tmp/repo"));
        assert_eq!(NativeAction::Fetch.event_id(), FETCH);
        assert_eq!(NativeAction::QuickCommit.event_id(), QUICK_COMMIT);
        assert_eq!(NativeAction::Fetch.path(), None);
        assert_eq!(NativeAction::Settings.event_id(), SETTINGS);
        assert_eq!(NativeAction::Settings.path(), None);
        assert_eq!(NativeAction::CloseTab.event_id(), CLOSE_TAB);
        assert_eq!(NativeAction::NextRepoTab.event_id(), NEXT_REPO_TAB);
        assert_eq!(NativeAction::PrevRepoTab.event_id(), PREV_REPO_TAB);
        assert_eq!(NativeAction::ReopenRepoTab.event_id(), REOPEN_REPO_TAB);
    }
}
