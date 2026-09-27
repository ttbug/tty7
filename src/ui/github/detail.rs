//! One issue or pull request, pushed into the GitHub tab over its list.
//!
//! Title, state, author, labels, the body and the conversation as Markdown —
//! through gpui-component's `TextView`, after `core::github::markdown` has
//! turned images into links and disarmed non-web link targets — and, for a
//! pull request, its branches and changed files. A file opens in the diff
//! overlay, the same surface a commit's files open in, fed the patch GitHub
//! sent rather than one git read.

use std::sync::Arc;

use gpui::{AnyElement, Context, SharedString, Window, div, prelude::*, px, rems};
use gpui_component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};

use tty7_core::core::git::diff::{CommitLabel, DiffBudget, DiffSnapshot, DiffSource};
use tty7_core::core::github::{Comment, Detail, PrFile, RepoSlug};

use crate::ui::app::{CONTENT_INSET, Tty7App};
use crate::ui::github::now_unix;
use crate::ui::i18n::{L10nKey, t, t_fmt, t_plural};
use crate::ui::panel_github::{describe_error, github_tile, label_chip, state_glyph, state_label};
use crate::ui::right_panel::{
    HEADING, META, META_MONO, ROW_FILL_RADIUS, ROW_INSET, TEXT, TEXT_INSET, git_badge,
};
use crate::ui::scm::path::{relative_time, split_display_path};
use crate::ui::scm::state::RepoKey;
use crate::ui::scm::status::{status_color, status_glyph};

const ROW_H: f32 = 26.;
const SECTION_GAP: f32 = 16.;

impl Tty7App {
    /// The rows pinned over the detail, and the detail itself.
    pub(crate) fn render_github_detail(
        &mut self,
        repo: &RepoKey,
        slug: &RepoSlug,
        number: u64,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Vec<AnyElement>, AnyElement) {
        self.github_ensure_detail(slug, number, cx);
        let key = (slug.clone(), number);
        let (detail, error) = match self.github.details.get(&key) {
            Some(c) => (c.detail.clone(), c.error.clone()),
            None => (None, None),
        };
        let is_pr = detail.as_ref().map_or(
            self.github.kind == tty7_core::core::github::Kind::Pulls,
            |d| d.item.is_pr,
        );
        let url = detail.as_ref().map_or_else(
            || {
                format!(
                    "{}/{}/{number}",
                    slug.web_url(),
                    if is_pr { "pull" } else { "issues" }
                )
            },
            |d| d.item.html_url.clone(),
        );
        let pinned = vec![self.github_back_row(repo, number, is_pr, url, cx)];

        let Some(detail) = detail else {
            let body = match error {
                Some(err) => {
                    let (text, hint) = describe_error(&err, self.github_authenticated());
                    self.panel_empty(&text, hint.as_deref(), cx)
                }
                None => self.panel_empty(t(L10nKey::PanelLoading), None, cx),
            };
            return (pinned, body);
        };

        let mut body = v_flex()
            .pb(px(16.))
            .child(self.github_detail_head(&detail, cx))
            .child(markdown_block(
                format!("gh-body-{}-{number}", slug.full()),
                &detail.body,
                t(L10nKey::GitHubNoDescription),
                cx,
            ));
        if let Some(err) = error {
            // A refresh that failed over a detail already on screen: keep the
            // detail, and say why it may be out of date.
            let (text, hint) = describe_error(&err, self.github_authenticated());
            body = body.child(self.panel_empty(&text, hint.as_deref(), cx));
        }
        if let Some(files) = &detail.files {
            body = body.child(self.github_detail_files(repo, slug, &detail, files, cx));
        }
        body = body.child(self.github_detail_comments(slug, number, &detail, cx));
        (pinned, body.into_any_element())
    }

    fn github_back_row(
        &self,
        repo: &RepoKey,
        number: u64,
        is_pr: bool,
        url: String,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sf = cx.global::<crate::ui::presets::Surfaces>().sidebar;
        let muted = cx.theme().muted_foreground;
        h_flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(28.))
            .px(px(CONTENT_INSET))
            .child(
                h_flex()
                    .id("panel-github-back")
                    .items_center()
                    .gap(px(2.))
                    .h(px(ROW_H - 4.))
                    .pl(px(ROW_INSET - 2.))
                    .pr(px(ROW_INSET))
                    .rounded(px(6.))
                    .cursor_pointer()
                    .hover(|s| s.bg(gpui::rgb(sf.hover)))
                    .on_click(cx.listener(|this, _, _window, cx| this.github_close_detail(cx)))
                    .child(Icon::new(IconName::ChevronLeft).small().text_color(muted))
                    .child(
                        div()
                            .text_size(rems(TEXT))
                            .text_color(muted)
                            .child(t(if is_pr {
                                L10nKey::GitHubPulls
                            } else {
                                L10nKey::GitHubIssues
                            })),
                    ),
            )
            .child(div().flex_1().min_w_0())
            .child(
                div()
                    .text_size(rems(META_MONO))
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(muted)
                    .child(format!("#{number}")),
            )
            .child(self.github_refresh_tile(Some(repo.clone()), cx))
            .child(
                github_tile(
                    "panel-github-open-item",
                    Icon::new(IconName::ExternalLink),
                    t(L10nKey::GitHubOpenOnGitHub),
                    cx,
                )
                .on_click(move |_, _window, cx| cx.open_url(&url)),
            )
            .into_any_element()
    }

    /// Title, state, byline, branches and size, labels.
    fn github_detail_head(&self, detail: &Detail, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (muted, fg) = (theme.muted_foreground, theme.foreground);
        let (added_ink, removed_ink) = (theme.success, theme.danger);
        let mono = theme.mono_font_family.clone();
        let item = &detail.item;
        let now = now_unix();
        // Both times, each named: the list row shows the last update, and a
        // bare "4d" here beside a "13h" there read as the two disagreeing.
        let when = |key, at| t_fmt(key, &[("when", &relative_time(now, at))]);
        let mut byline: Vec<String> = Vec::new();
        if !item.author.is_empty() {
            byline.push(item.author.clone());
        }
        if item.created_at > 0 {
            byline.push(when(L10nKey::GitHubOpenedAt, item.created_at));
        }
        // An update within a minute of opening is the opening itself.
        if item.updated_at > item.created_at + 60 {
            byline.push(when(L10nKey::GitHubUpdatedAt, item.updated_at));
        }
        let byline = byline.join(" · ");
        let mut head = v_flex()
            .px(px(TEXT_INSET))
            .pt(px(4.))
            .pb(px(10.))
            .gap(px(6.))
            .child(
                div()
                    .text_size(rems(TEXT + 1. / 16.))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(fg)
                    .child(item.title.clone()),
            )
            .child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .flex_wrap()
                    .child(
                        h_flex()
                            .flex_none()
                            .items_center()
                            .gap(px(4.))
                            .px(px(6.))
                            .py(px(1.))
                            .rounded(px(10.))
                            .bg(theme.foreground.opacity(0.06))
                            .child(state_glyph(item.state, item.is_pr, cx))
                            .child(
                                div()
                                    .text_size(rems(META))
                                    .text_color(fg)
                                    .child(state_label(item.state)),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_size(rems(META))
                            .text_color(muted)
                            .child(byline),
                    ),
            );
        if let Some(pull) = &detail.pull {
            head = head.child(
                h_flex()
                    .items_center()
                    .flex_wrap()
                    .gap(px(6.))
                    .text_size(rems(META_MONO))
                    .font_family(mono.clone())
                    .text_color(muted)
                    .child(format!("{} → {}", pull.head_ref, pull.base_ref))
                    .child(
                        div()
                            .text_color(added_ink)
                            .child(format!("+{}", pull.additions)),
                    )
                    .child(
                        div()
                            .text_color(removed_ink)
                            .child(format!("−{}", pull.deletions)),
                    )
                    .child(
                        div()
                            .font_family(theme.font_family.clone())
                            .text_size(rems(META))
                            .child(t_plural(L10nKey::GitHubCommits, pull.commits as usize, &[])),
                    ),
            );
        }
        if !item.labels.is_empty() {
            head = head.child(
                h_flex()
                    .flex_wrap()
                    .gap(px(4.))
                    .children(item.labels.iter().map(|l| label_chip(l, cx))),
            );
        }
        head.into_any_element()
    }

    fn github_detail_comments(
        &self,
        slug: &RepoSlug,
        number: u64,
        detail: &Detail,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let muted = cx.theme().muted_foreground;
        let fg = cx.theme().foreground;
        let now = now_unix();
        let count = detail.comments.len().max(detail.item.comments as usize);
        let mut section = v_flex().mt(px(SECTION_GAP)).child(section_heading(
            t_plural(L10nKey::GitHubComments, count, &[]),
            cx,
        ));
        for c in &detail.comments {
            section = section.child(comment_block(slug, number, c, now, fg, muted, cx));
        }
        if detail.comments_truncated {
            section = section.child(more_on_github(
                "panel-github-more-comments",
                detail.item.html_url.clone(),
                cx,
            ));
        }
        section.into_any_element()
    }

    fn github_detail_files(
        &self,
        repo: &RepoKey,
        slug: &RepoSlug,
        detail: &Detail,
        files: &[PrFile],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let mono = theme.mono_font_family.clone();
        let (added_ink, removed_ink, muted) = (theme.success, theme.danger, theme.muted_foreground);
        let (added, removed) = files
            .iter()
            .fold((0, 0), |(a, r), f| (a + f.additions, r + f.deletions));
        let source = DiffSource::Patch {
            id: format!("{}#{}", slug.full(), detail.item.number),
            label: Some(CommitLabel {
                subject: detail.item.title.clone(),
                author: detail.item.author.clone(),
                at: detail.item.created_at,
            }),
        };
        let head_ref = detail
            .pull
            .as_ref()
            .map(|p| p.head_ref.clone())
            .unwrap_or_default();
        let files: Arc<Vec<PrFile>> = Arc::new(files.to_vec());
        let focused = self
            .diff_overlay_focus(repo.host, &repo.root, &source)
            .map(str::to_string);

        let heading = h_flex()
            .items_center()
            .gap(px(6.))
            .min_h(px(22.))
            .px(px(TEXT_INSET))
            .child(
                div()
                    .text_size(rems(HEADING))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(muted)
                    .child(t_plural(L10nKey::ScmFilesChanged, files.len(), &[])),
            )
            .child(
                h_flex()
                    .gap(px(5.))
                    .text_size(rems(META_MONO))
                    .font_family(mono.clone())
                    .child(div().text_color(added_ink).child(format!("+{added}")))
                    .child(div().text_color(removed_ink).child(format!("−{removed}"))),
            );
        let mut rows = v_flex().px(px(CONTENT_INSET));
        for (i, file) in files.iter().enumerate() {
            let selected = focused.as_deref() == Some(file.path.as_str());
            rows = rows.child(
                self.github_file_row(i, file, selected, repo, &source, &head_ref, &files, cx),
            );
        }
        let mut section = v_flex().mt(px(SECTION_GAP)).child(heading).child(rows);
        if detail.files_truncated {
            section = section.child(more_on_github(
                "panel-github-more-files",
                format!("{}/files", detail.item.html_url),
                cx,
            ));
        }
        section.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn github_file_row(
        &self,
        i: usize,
        file: &PrFile,
        selected: bool,
        repo: &RepoKey,
        source: &DiffSource,
        head_ref: &str,
        files: &Arc<Vec<PrFile>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sf = cx.global::<crate::ui::presets::Surfaces>().sidebar;
        let mono = cx.theme().mono_font_family.clone();
        let deco = crate::ui::diff_overlay::deco_status(file.status);
        let (name, dir) = split_display_path(&file.path);
        let (added_ink, removed_ink) = (cx.theme().success, cx.theme().danger);
        let repo = repo.clone();
        let source = source.clone();
        let path = file.path.clone();
        let branch = head_ref.to_string();
        let files = files.clone();
        h_flex()
            .id(("panel-github-file", i))
            .items_center()
            .gap(px(8.))
            .min_h(rems(ROW_H / 16.))
            .w_full()
            .min_w_0()
            .px(px(ROW_INSET))
            .py(px(3.))
            .rounded(ROW_FILL_RADIUS)
            .cursor_pointer()
            .hover(|s| s.bg(gpui::rgb(sf.hover)))
            .when(selected, |s| s.bg(gpui::rgb(sf.selected)))
            .on_click(cx.listener(move |this, _, window, cx| {
                // Built on click, not per frame: a hundred parsed patches is
                // real work, and only the one opened is looked at.
                let snapshot = Arc::new(DiffSnapshot {
                    root: repo.root.clone(),
                    source: source.clone(),
                    branch: branch.clone(),
                    files: files
                        .iter()
                        .map(|f| f.to_file_diff(DiffBudget::default()))
                        .collect(),
                    untracked: Vec::new(),
                    untracked_total: 0,
                    read_failed: false,
                });
                this.open_supplied_diff(repo.host, snapshot, Some(path.clone()), window, cx);
            }))
            .child(git_badge(status_glyph(deco), status_color(deco, cx), &mono))
            .child(
                div()
                    .flex_shrink(1.)
                    .min_w(px(40.))
                    .truncate()
                    .text_size(rems(TEXT))
                    .text_color(if selected {
                        gpui::rgb(sf.text_selected)
                    } else {
                        gpui::rgb(sf.text_resting)
                    })
                    .child(name.to_string()),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .text_ellipsis_start()
                    .text_right()
                    .text_size(rems(META))
                    .text_color(cx.theme().muted_foreground)
                    .child(dir.to_string()),
            )
            .child(
                h_flex()
                    .flex_none()
                    .gap(px(4.))
                    .text_size(rems(META_MONO))
                    .font_family(mono.clone())
                    .when(file.additions > 0, |d| {
                        d.child(
                            div()
                                .text_color(added_ink)
                                .child(format!("+{}", file.additions)),
                        )
                    })
                    .when(file.deletions > 0, |d| {
                        d.child(
                            div()
                                .text_color(removed_ink)
                                .child(format!("−{}", file.deletions)),
                        )
                    }),
            )
            .into_any_element()
    }
}

/// Headings a step or two over the body rather than the document-sized ramp
/// the text view defaults to: an issue's `## What happened?` is a label in a
/// 280px column, not a page title.
fn panel_markdown_style(cx: &gpui::App) -> gpui_component::text::TextViewStyle {
    gpui_component::text::TextViewStyle {
        heading_font_size: Some(Arc::new(|level, base| match level {
            1 => base * 1.25,
            2 => base * 1.15,
            _ => base * 1.05,
        })),
        ..crate::ui::theme::markdown_style(cx)
    }
}

fn section_heading(text: String, cx: &gpui::App) -> AnyElement {
    div()
        .flex()
        .items_center()
        .min_h(px(22.))
        .px(px(TEXT_INSET))
        .text_size(rems(HEADING))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(cx.theme().muted_foreground)
        .child(text)
        .into_any_element()
}

/// Untrusted Markdown, made safe and rendered. An empty body says so.
fn markdown_block(id: String, source: &str, empty: &str, cx: &gpui::App) -> AnyElement {
    let muted = cx.theme().muted_foreground;
    if source.trim().is_empty() {
        return div()
            .px(px(TEXT_INSET))
            .text_size(rems(META))
            .text_color(muted)
            .italic()
            .child(empty.to_string())
            .into_any_element();
    }
    let safe = tty7_core::core::github::markdown::sanitize(source, t(L10nKey::GitHubImage));
    div()
        .px(px(TEXT_INSET))
        .min_w_0()
        .text_size(rems(TEXT))
        .child(
            gpui_component::text::TextView::markdown(SharedString::from(id), safe)
                .style(panel_markdown_style(cx))
                .selectable(true)
                // The rewrite above works on lines of source; this checks what
                // the parser actually made of them. A block holding anything
                // the rewrite should have removed is drawn as its plain source
                // instead, so a construct the two read differently can never
                // load a third-party image or hand the OS a `file:` link.
                .markdown_block_parser(|node, parse| {
                    (!renders_safely(node)).then(|| {
                        let text = parse.node_source(node).unwrap_or_default().to_string();
                        gpui_component::text::MarkdownNode::new(UNSAFE_BLOCK, text.clone())
                            .text(text)
                    })
                })
                .markdown_block_renderer(UNSAFE_BLOCK, |node, _window, cx| {
                    div()
                        .text_color(cx.theme().muted_foreground)
                        .whitespace_normal()
                        .child(node.data::<String>().cloned().unwrap_or_default())
                }),
        )
        .into_any_element()
}

/// The custom block a Markdown block that failed [`renders_safely`] becomes.
const UNSAFE_BLOCK: &str = "tty7-github-unsafe-block";

/// Whether `node` and everything under it is what
/// `core::github::markdown::sanitize` promises: images only from GitHub's own
/// hosts, links (and link definitions) only to web and mail targets, and raw
/// HTML with no `<img>` or unsafe `href`/`src`.
fn renders_safely(node: &gpui_component::text::markdown_ast::Node) -> bool {
    use gpui_component::text::markdown_ast::Node;
    use tty7_core::core::github::markdown::{html_is_safe, is_github_hosted, is_safe_target};
    let here = match node {
        Node::Image(image) => is_github_hosted(&image.url),
        // Never written by the rewrite (it makes every `![…][…]` a link).
        Node::ImageReference(_) => false,
        Node::Link(link) => is_safe_target(&link.url),
        Node::Definition(def) => is_safe_target(&def.url),
        Node::Html(html) => html_is_safe(&html.value),
        _ => true,
    };
    here && node
        .children()
        .is_none_or(|children| children.iter().all(renders_safely))
}

#[allow(clippy::too_many_arguments)]
fn comment_block(
    slug: &RepoSlug,
    number: u64,
    c: &Comment,
    now: i64,
    fg: gpui::Hsla,
    muted: gpui::Hsla,
    cx: &gpui::App,
) -> AnyElement {
    let when = (c.created_at > 0).then(|| relative_time(now, c.created_at));
    v_flex()
        .pt(px(14.))
        .gap(px(4.))
        .child(
            h_flex()
                .px(px(TEXT_INSET))
                .gap(px(6.))
                .items_baseline()
                .text_size(rems(META))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .text_color(fg)
                        .child(c.author.clone()),
                )
                .children(when.map(|w| div().text_color(muted).child(w))),
        )
        .child(markdown_block(
            format!("gh-comment-{}-{number}-{}", slug.full(), c.id),
            &c.body,
            "",
            cx,
        ))
        .into_any_element()
}

fn more_on_github(id: &'static str, url: String, cx: &gpui::App) -> AnyElement {
    h_flex()
        .id(id)
        .mt(px(6.))
        .mx(px(TEXT_INSET))
        .gap(px(4.))
        .items_center()
        .cursor_pointer()
        .text_size(rems(META))
        .text_color(cx.theme().link)
        .hover(|s| s.underline())
        .on_click(move |_, _window, cx| cx.open_url(&url))
        .child(t(L10nKey::GitHubMoreOnGitHub))
        .child(Icon::new(IconName::ExternalLink).xsmall())
        .into_any_element()
}
