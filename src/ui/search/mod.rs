//! Search Everywhere: one modal over everything the app can find — terminals,
//! sessions, hosts, actions — each in a tab of its own, and all of them at
//! once in the All tab. Files are Go to File's alone (see [`SearchTab::ORDER`]).
//!
//! - [`command`]: what a row runs ([`CommandKind`]) and the rows themselves.
//! - [`sources`]: what each tab holds and how it ranks against a query.
//! - [`files`]: the Files tab — the project's file index and quick open.
//! - [`score`]: the one fuzzy scorer every tab shares.
//! - [`view`]: the modal — the tab row, the list, the theme picker.

mod command;
mod files;
mod score;
mod sources;
mod view;

pub(crate) use command::{Avatar, ChromeState, CommandGroup, CommandKind, Item};
pub(crate) use files::{FileIndexStore, FileList};
pub(crate) use score::fuzzy_score;
pub(crate) use sources::{Catalog, host_items};
pub(crate) use view::{KEY_CONTEXT, SearchEvent, SearchView};

use crate::ui::i18n::{L10nKey, t};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SearchTab {
    All,
    Files,
    Actions,
    Terminals,
    Sessions,
    Hosts,
}

impl SearchTab {
    /// The tab row, left to right, and the order Tab walks it.
    ///
    /// Files is not in it. Go to File (its own chord) still opens the search
    /// on it, but the row, Tab and the All tab leave it out: the right
    /// panel's Files tab is where the project is searched, names and text
    /// together, and a sample of paths among tabs and hosts answered nothing
    /// anyone had typed. Commands last, the way the rest of the row goes
    /// from the things you have to the things you can do.
    pub(crate) const ORDER: [SearchTab; 5] = [
        SearchTab::All,
        SearchTab::Terminals,
        SearchTab::Sessions,
        SearchTab::Hosts,
        SearchTab::Actions,
    ];

    pub(crate) fn title(self) -> &'static str {
        t(match self {
            SearchTab::All => L10nKey::SearchTabAll,
            SearchTab::Files => L10nKey::SearchTabFiles,
            SearchTab::Actions => L10nKey::SearchTabActions,
            SearchTab::Terminals => L10nKey::SearchTabTerminals,
            SearchTab::Sessions => L10nKey::SearchTabSessions,
            SearchTab::Hosts => L10nKey::SearchTabHosts,
        })
    }

    pub(crate) fn placeholder(self) -> &'static str {
        t(match self {
            SearchTab::All => L10nKey::SearchPlaceholderAll,
            SearchTab::Files => L10nKey::SearchPlaceholderFiles,
            SearchTab::Actions => L10nKey::SearchPlaceholderActions,
            SearchTab::Terminals => L10nKey::SearchPlaceholderTerminals,
            SearchTab::Sessions => L10nKey::SearchPlaceholderSessions,
            SearchTab::Hosts => L10nKey::SearchPlaceholderHosts,
        })
    }

    /// The neighbouring tab, wrapping at the ends.
    pub(crate) fn step(self, forward: bool) -> SearchTab {
        let n = Self::ORDER.len();
        let i = Self::ORDER.iter().position(|t| *t == self).unwrap_or(0);
        Self::ORDER[if forward {
            (i + 1) % n
        } else {
            (i + n - 1) % n
        }]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_steps_wrap_both_ways() {
        assert_eq!(SearchTab::All.step(true), SearchTab::Terminals);
        assert_eq!(SearchTab::Hosts.step(true), SearchTab::Actions);
        assert_eq!(SearchTab::Actions.step(true), SearchTab::All);
        assert_eq!(SearchTab::All.step(false), SearchTab::Actions);
        assert_eq!(SearchTab::Terminals.step(false), SearchTab::All);
        assert_eq!(SearchTab::Terminals.step(true), SearchTab::Sessions);
    }

    #[test]
    fn files_is_reached_by_its_chord_not_by_the_row() {
        assert!(!SearchTab::ORDER.contains(&SearchTab::Files));
        // Tab out of Go to File lands on the row again.
        assert_eq!(SearchTab::Files.step(true), SearchTab::Terminals);
    }
}
