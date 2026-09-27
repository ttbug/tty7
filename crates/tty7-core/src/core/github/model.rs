//! What the panel shows, decoded out of GitHub's REST JSON.
//!
//! The wire structs (`Raw*`) mirror only the fields read; everything else in
//! GitHub's (large) payloads is ignored, so a field GitHub adds or drops later
//! cannot break decoding. The public types are what the UI draws from, already
//! reduced to the decisions it needs — one [`ItemState`] rather than the three
//! fields (`state`, `state_reason`, `merged_at`/`draft`) GitHub spreads it over.

use serde::Deserialize;

use crate::core::git::diff::{DiffBudget, DiffParser, FileDiff, FileStatus};

/// Issues or pull requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Kind {
    #[default]
    Issues,
    Pulls,
}

/// The list's open/closed switch. "Closed" includes merged pull requests, as
/// it does on GitHub.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum StateFilter {
    #[default]
    Open,
    Closed,
}

impl StateFilter {
    pub fn as_query(self) -> &'static str {
        match self {
            StateFilter::Open => "open",
            StateFilter::Closed => "closed",
        }
    }
}

/// Where an issue or a pull request stands. Each one gets its own glyph
/// *shape*, not only its own colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemState {
    Open,
    /// A pull request marked as a draft (and still open).
    Draft,
    /// An issue closed as completed, or a pull request closed unmerged.
    Closed,
    /// An issue closed as "not planned" / duplicate.
    NotPlanned,
    Merged,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Label {
    pub name: String,
    /// `0xRRGGBB`, when GitHub sent a usable colour.
    pub color: Option<u32>,
}

/// One row of the list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    pub number: u64,
    pub title: String,
    pub state: ItemState,
    pub is_pr: bool,
    pub author: String,
    pub labels: Vec<Label>,
    pub comments: u32,
    /// Unix seconds; zero when GitHub sent something unparseable.
    pub created_at: i64,
    pub updated_at: i64,
    pub html_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: u64,
    pub author: String,
    pub body: String,
    pub created_at: i64,
    pub html_url: String,
}

/// The pull-request-only half of a detail view.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PullInfo {
    pub head_ref: String,
    pub base_ref: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub commits: u32,
}

/// One file a pull request touches, from `/pulls/{n}/files`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrFile {
    pub path: String,
    pub old_path: Option<String>,
    pub status: FileStatus,
    pub additions: u32,
    pub deletions: u32,
    /// The unified hunks, without a `diff --git` header. GitHub leaves it out
    /// for binary files and for patches too large to inline.
    pub patch: Option<String>,
}

impl PrFile {
    /// This file as the diff overlay's own model, so the overlay can show a
    /// pull request's patch exactly the way it shows a local one.
    ///
    /// The counts come from GitHub rather than from the parsed hunks: a file
    /// GitHub did not inline a patch for still has real `+N −M`.
    pub fn to_file_diff(&self, budget: DiffBudget) -> FileDiff {
        let mut parser = DiffParser::with_budget(budget);
        // A placeholder header opens the file; the real paths are set below,
        // so a path with spaces never has to survive header splitting.
        parser.push_line("diff --git a/f b/f");
        if let Some(patch) = &self.patch {
            for line in patch.lines() {
                parser.push_line(line);
            }
        }
        let mut file = parser.finish().into_iter().next().unwrap_or(FileDiff {
            path: String::new(),
            old_path: None,
            status: FileStatus::Modified,
            added: 0,
            removed: 0,
            binary: false,
            truncated: None,
            hunks: Vec::new(),
        });
        file.path = self.path.clone();
        file.old_path = self.old_path.clone();
        file.status = self.status;
        file.added = self.additions;
        file.removed = self.deletions;
        // No patch but real changes is GitHub declining to inline one — a
        // binary file or an oversized text one. The overlay's binary card
        // ("no preview") is the honest thing to show for both.
        file.binary = self.patch.is_none() && (self.additions > 0 || self.deletions > 0);
        file
    }
}

/// Everything the detail view needs about one issue or pull request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    pub item: Item,
    pub body: String,
    pub comments: Vec<Comment>,
    /// More comments exist than were fetched.
    pub comments_truncated: bool,
    pub pull: Option<PullInfo>,
    /// `Some` for a pull request.
    pub files: Option<Vec<PrFile>>,
    pub files_truncated: bool,
}

// ---- wire ------------------------------------------------------------------

#[derive(Deserialize)]
pub(crate) struct RawUser {
    #[serde(default)]
    login: String,
}

#[derive(Deserialize)]
pub(crate) struct RawLabel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct RawPrRef {
    #[serde(default)]
    merged_at: Option<String>,
}

/// An entry of `/issues` — which also lists pull requests, marked by the
/// presence of `pull_request`.
#[derive(Deserialize)]
pub(crate) struct RawIssue {
    pub(crate) number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    comments: u32,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    pub(crate) body: Option<String>,
    /// Only with the `full` media type.
    #[serde(default)]
    pub(crate) body_html: Option<String>,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    pub(crate) pull_request: Option<RawPrRef>,
    #[serde(default)]
    draft: Option<bool>,
}

#[derive(Deserialize)]
pub(crate) struct RawBranchRef {
    #[serde(default, rename = "ref")]
    name: String,
}

/// An entry of `/pulls`, or `/pulls/{n}`.
#[derive(Deserialize)]
pub(crate) struct RawPull {
    number: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    draft: Option<bool>,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    labels: Vec<RawLabel>,
    #[serde(default)]
    comments: Option<u32>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    updated_at: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    head: Option<RawBranchRef>,
    #[serde(default)]
    base: Option<RawBranchRef>,
    #[serde(default)]
    additions: Option<u32>,
    #[serde(default)]
    deletions: Option<u32>,
    #[serde(default)]
    changed_files: Option<u32>,
    #[serde(default)]
    commits: Option<u32>,
}

#[derive(Deserialize)]
pub(crate) struct RawComment {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    user: Option<RawUser>,
    #[serde(default)]
    body: Option<String>,
    /// Only with the `full` media type.
    #[serde(default)]
    body_html: Option<String>,
    #[serde(default)]
    created_at: String,
    #[serde(default)]
    html_url: String,
}

#[derive(Deserialize)]
pub(crate) struct RawFile {
    #[serde(default)]
    filename: String,
    #[serde(default)]
    previous_filename: Option<String>,
    #[serde(default)]
    status: String,
    #[serde(default)]
    additions: u32,
    #[serde(default)]
    deletions: u32,
    #[serde(default)]
    patch: Option<String>,
}

fn login(user: Option<RawUser>) -> String {
    user.map(|u| u.login).unwrap_or_default()
}

fn labels(raw: Vec<RawLabel>) -> Vec<Label> {
    raw.into_iter()
        .filter(|l| !l.name.is_empty())
        .map(|l| Label {
            color: l
                .color
                .as_deref()
                .filter(|c| c.len() == 6)
                .and_then(|c| u32::from_str_radix(c, 16).ok()),
            name: l.name,
        })
        .collect()
}

/// GitHub's `2024-05-01T12:34:56Z`, as unix seconds (zero if unparseable).
pub fn timestamp(text: &str) -> i64 {
    crate::core::git::log::parse_iso8601(text).map_or(0, |ts| ts.unix)
}

impl RawIssue {
    pub(crate) fn is_pr(&self) -> bool {
        self.pull_request.is_some()
    }

    pub(crate) fn into_item(self) -> Item {
        let is_pr = self.is_pr();
        let merged = self
            .pull_request
            .as_ref()
            .is_some_and(|p| p.merged_at.is_some());
        let state = item_state(
            &self.state,
            self.state_reason.as_deref(),
            merged,
            self.draft.unwrap_or(false),
        );
        Item {
            number: self.number,
            title: self.title,
            state,
            is_pr,
            author: login(self.user),
            labels: labels(self.labels),
            comments: self.comments,
            created_at: timestamp(&self.created_at),
            updated_at: timestamp(&self.updated_at),
            html_url: self.html_url,
        }
    }
}

impl RawPull {
    pub(crate) fn into_item(self) -> (Item, PullInfo) {
        let state = item_state(
            &self.state,
            None,
            self.merged_at.is_some(),
            self.draft.unwrap_or(false),
        );
        let info = PullInfo {
            head_ref: self.head.map(|h| h.name).unwrap_or_default(),
            base_ref: self.base.map(|b| b.name).unwrap_or_default(),
            additions: self.additions.unwrap_or(0),
            deletions: self.deletions.unwrap_or(0),
            changed_files: self.changed_files.unwrap_or(0),
            commits: self.commits.unwrap_or(0),
        };
        let item = Item {
            number: self.number,
            title: self.title,
            state,
            is_pr: true,
            author: login(self.user),
            labels: labels(self.labels),
            comments: self.comments.unwrap_or(0),
            created_at: timestamp(&self.created_at),
            updated_at: timestamp(&self.updated_at),
            html_url: self.html_url,
        };
        (item, info)
    }
}

impl RawComment {
    pub(crate) fn into_comment(self) -> Comment {
        Comment {
            id: self.id,
            author: login(self.user),
            body: super::markdown::sign_attachments(
                self.body.as_deref().unwrap_or_default(),
                self.body_html.as_deref().unwrap_or_default(),
            ),
            created_at: timestamp(&self.created_at),
            html_url: self.html_url,
        }
    }
}

impl RawFile {
    pub(crate) fn into_file(self) -> PrFile {
        let status = match self.status.as_str() {
            "added" => FileStatus::Added,
            "removed" => FileStatus::Deleted,
            "renamed" => FileStatus::Renamed,
            "copied" => FileStatus::Copied,
            // "modified", "changed" (mode only) and "unchanged" all draw as a
            // modification; none of them is a different kind of row.
            _ => FileStatus::Modified,
        };
        PrFile {
            path: self.filename,
            old_path: self.previous_filename,
            status,
            additions: self.additions,
            deletions: self.deletions,
            patch: self.patch,
        }
    }
}

/// The one place GitHub's three state fields become one state.
fn item_state(state: &str, reason: Option<&str>, merged: bool, draft: bool) -> ItemState {
    if merged {
        return ItemState::Merged;
    }
    match state {
        "closed" => match reason {
            Some("not_planned") | Some("duplicate") => ItemState::NotPlanned,
            _ => ItemState::Closed,
        },
        _ if draft => ItemState::Draft,
        _ => ItemState::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_folds_three_fields_into_one() {
        assert_eq!(item_state("open", None, false, false), ItemState::Open);
        assert_eq!(item_state("open", None, false, true), ItemState::Draft);
        assert_eq!(item_state("closed", None, false, false), ItemState::Closed);
        assert_eq!(
            item_state("closed", Some("completed"), false, false),
            ItemState::Closed
        );
        assert_eq!(
            item_state("closed", Some("not_planned"), false, false),
            ItemState::NotPlanned
        );
        // Merged wins over "closed", and a closed draft is closed.
        assert_eq!(item_state("closed", None, true, false), ItemState::Merged);
        assert_eq!(item_state("closed", None, false, true), ItemState::Closed);
    }

    #[test]
    fn label_colours_parse_and_bad_ones_are_dropped() {
        let got = labels(vec![
            RawLabel {
                name: "bug".into(),
                color: Some("d73a4a".into()),
            },
            RawLabel {
                name: "weird".into(),
                color: Some("zzz".into()),
            },
            RawLabel {
                name: String::new(),
                color: None,
            },
        ]);
        assert_eq!(
            got,
            vec![
                Label {
                    name: "bug".into(),
                    color: Some(0xd73a4a)
                },
                Label {
                    name: "weird".into(),
                    color: None
                },
            ]
        );
    }

    #[test]
    fn github_timestamps_are_unix_seconds() {
        assert_eq!(timestamp("1970-01-01T00:00:00Z"), 0);
        assert_eq!(timestamp("2024-02-29T12:00:00Z"), 1_709_208_000);
        assert_eq!(timestamp("garbage"), 0);
    }

    fn file(patch: Option<&str>, additions: u32, deletions: u32) -> PrFile {
        PrFile {
            path: "src/a file.rs".into(),
            old_path: None,
            status: FileStatus::Modified,
            additions,
            deletions,
            patch: patch.map(str::to_string),
        }
    }

    #[test]
    fn a_github_patch_becomes_the_overlays_file_model() {
        let f = file(
            Some(
                "@@ -1,3 +1,3 @@\n fn a() {}\n-fn b() {}\n+fn c() {}\n fn d() {}\n\\ No newline at end of file",
            ),
            1,
            1,
        );
        let diff = f.to_file_diff(DiffBudget::default());
        assert_eq!(diff.path, "src/a file.rs", "a path with a space survives");
        assert_eq!((diff.added, diff.removed), (1, 1));
        assert!(!diff.binary);
        assert_eq!(diff.hunks.len(), 1);
        let lines = &diff.hunks[0].lines;
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].old_no, Some(2));
        assert_eq!(lines[2].new_no, Some(2));
    }

    #[test]
    fn a_file_github_would_not_inline_reads_as_binary() {
        let diff = file(None, 0, 0).to_file_diff(DiffBudget::default());
        assert!(!diff.binary, "an empty change is not a binary one");
        let diff = file(None, 120, 4).to_file_diff(DiffBudget::default());
        assert!(diff.binary);
        assert_eq!((diff.added, diff.removed), (120, 4));
        assert!(diff.hunks.is_empty());
    }
}
