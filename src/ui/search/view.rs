//! The modal: a row of tabs over one searchable list.

use std::rc::Rc;

use gpui::{
    App, ClickEvent, Context, Entity, EventEmitter, FontWeight, MouseButton, MouseDownEvent,
    ScrollStrategy, Subscription, Task, Window, div, prelude::*, px, rems,
};
use gpui_component::{
    ActiveTheme as _, IndexPath, Selectable, h_flex,
    list::{List, ListDelegate, ListEvent, ListState},
    v_flex,
};

use super::SearchTab;
use super::command::{CommandKind, Item};
use super::sources::{Catalog, Row, Section, plain};
use crate::core::actions::{SearchNextTab, SearchPrevTab};
use crate::ui::dialog::{CARD_RADIUS, FOOTER_H, KEYCAP, keycap};
use crate::ui::i18n::{L10nKey, t, t_fmt};

/// What the list is showing: one of the tabs, or the theme picker one of the
/// Actions rows opens. The picker has no tabs — Escape goes back to the one it
/// came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Scope {
    Tab(SearchTab),
    Themes,
}

pub(crate) struct SearchDelegate {
    catalog: Rc<Catalog>,
    scope: Scope,
    /// The theme picker's rows, built when it opens.
    themes: Vec<Item>,
    query: String,
    sections: Vec<Section>,
    selected: Option<IndexPath>,
}

impl SearchDelegate {
    fn new(catalog: Rc<Catalog>, scope: Scope, themes: Vec<Item>, cx: &App) -> Self {
        let mut this = Self {
            catalog,
            scope,
            themes,
            query: String::new(),
            sections: Vec::new(),
            selected: None,
        };
        this.refresh(cx);
        this
    }

    /// Re-runs the current query against the current scope.
    fn refresh(&mut self, cx: &App) {
        self.sections = match self.scope {
            Scope::Tab(tab) => self.catalog.sections(tab, &self.query, cx),
            Scope::Themes => plain(&self.themes, &self.query),
        };
    }

    fn row_at(&self, ix: IndexPath) -> Option<&Row> {
        self.sections.get(ix.section)?.rows.get(ix.row)
    }

    fn selected_item(&self) -> Option<&Item> {
        self.selected.and_then(|ix| self.row_at(ix)?.item())
    }

    fn position_of(&self, kind: &CommandKind) -> Option<IndexPath> {
        self.sections.iter().enumerate().find_map(|(s, section)| {
            let row = section
                .rows
                .iter()
                .position(|r| r.item().is_some_and(|i| &i.kind == kind))?;
            Some(IndexPath::new(row).section(s))
        })
    }

    fn first_row(&self) -> Option<IndexPath> {
        let section = self.sections.iter().position(|s| !s.rows.is_empty())?;
        Some(IndexPath::new(0).section(section))
    }

    fn render_row(&self, ix: IndexPath, item: &Item, cx: &App) -> gpui::AnyElement {
        let picked = Some(ix) == self.selected;
        let (fg, muted) = {
            let t = cx.theme();
            (t.foreground, t.muted_foreground)
        };

        // The title holds its width longest; the subtitle beside it is what
        // truncates first.
        let mut left = h_flex().flex_1().min_w_0().items_center().gap(px(8.));
        if let Some(avatar) = item.avatar {
            left = left.child(crate::ui::tab_strip::avatar(
                ("search-avatar", ix.section * 1000 + ix.row),
                avatar,
                AVATAR,
                cx,
            ));
        }
        left = left.child(
            div()
                .flex_shrink_0()
                .max_w_full()
                .truncate()
                .text_color(fg)
                .when(picked, |d| d.font_weight(FontWeight::MEDIUM))
                .child(item.title.clone()),
        );
        if let Some(subtitle) = item.subtitle.clone() {
            left = left.child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_size(rems(ROW_META))
                    .text_color(muted)
                    .child(subtitle),
            );
        }

        let mut right = h_flex().flex_shrink_0().items_center().gap(px(8.));
        if let Some(note) = item.note.clone() {
            right = right.child(
                div()
                    .text_size(rems(ROW_META))
                    .text_color(muted)
                    .child(note),
            );
        }
        if item.kind.edit_variant().is_some() {
            right = right.child(
                h_flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(rems(ROW_META))
                    .text_color(muted)
                    .child(t(L10nKey::EditHint))
                    .child(keycap(
                        crate::ui::keymap::key_tokens(EDIT_GESTURE).join(""),
                        cx,
                    )),
            );
        }
        if let Some(spec) = item.kind.key_spec(cx) {
            let tokens = crate::ui::keymap::key_tokens(&spec);
            right = right.child(
                h_flex()
                    .gap(px(3.))
                    .children(tokens.into_iter().map(|t| keycap(t, cx))),
            );
        }

        h_flex()
            .w_full()
            .gap(px(12.))
            .items_center()
            .justify_between()
            .child(left)
            .child(right)
            .into_any_element()
    }
}

impl ListDelegate for SearchDelegate {
    type Item = SearchRow;

    fn sections_count(&self, _cx: &App) -> usize {
        self.sections.len().max(1)
    }

    fn items_count(&self, section: usize, _cx: &App) -> usize {
        self.sections
            .get(section)
            .map(|s| s.rows.len())
            .unwrap_or(0)
    }

    fn perform_search(
        &mut self,
        query: &str,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Task<()> {
        self.query = query.to_string();
        self.refresh(cx);
        // Through `set_selected_index`, not by hand: the row index may not have
        // moved, but the row under it has, and the theme picker previews the
        // row — not the index.
        self.selected = None;
        let first = self.first_row();
        self.set_selected_index(first, window, cx);
        Task::ready(())
    }

    fn render_section_header(
        &mut self,
        section: usize,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<impl IntoElement> {
        let title = self.sections.get(section)?.title.clone()?;
        // The sidebar's group heading: medium, a half step under the rows, in
        // caption ink — never capitals, never a rule under it.
        Some(
            h_flex()
                .h(px(HEADER_H))
                .px(px(LABEL_INSET))
                .items_center()
                .text_size(rems(crate::ui::right_panel::HEADING))
                .font_weight(FontWeight::MEDIUM)
                .text_color(cx.theme().muted_foreground)
                .child(title),
        )
    }

    fn render_empty(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> impl IntoElement {
        // The SSH hint only where typing user@host would actually connect. A
        // theme picker with no matches teaching SSH is a crossed wire (#602).
        let hint = match self.scope {
            Scope::Tab(SearchTab::All | SearchTab::Hosts) => t(L10nKey::ConnectSshHint),
            Scope::Tab(SearchTab::Sessions) if self.query.trim().is_empty() => {
                t(L10nKey::SearchSessionsEmptyHint)
            }
            _ => t(L10nKey::PaletteTryDifferentSearch),
        };
        // The headline in body ink and the way out under it in caption ink:
        // two greys of the same size read as one sentence cut in half.
        let theme = cx.theme();
        v_flex()
            .py(px(32.))
            .px(px(LABEL_INSET))
            .gap(px(4.))
            .items_center()
            .text_size(rems(ROW_TEXT))
            .text_color(theme.foreground)
            .child(t(L10nKey::SearchNoResults))
            .child(
                div()
                    .text_size(rems(ROW_META))
                    .text_color(theme.muted_foreground)
                    .child(hint),
            )
    }

    fn render_item(
        &mut self,
        ix: IndexPath,
        _window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) -> Option<Self::Item> {
        let content = match self.row_at(ix)? {
            Row::Item(item) => self.render_row(ix, item, cx),
            Row::More { tab, hidden } => h_flex()
                .w_full()
                .items_center()
                .justify_between()
                .text_color(cx.theme().muted_foreground)
                .child(t_fmt(
                    L10nKey::SearchMoreIn,
                    &[("count", &hidden.to_string()), ("tab", tab.title())],
                ))
                .child(keycap(crate::ui::keymap::key_tokens("tab").join(""), cx))
                .into_any_element(),
        };
        Some(SearchRow {
            id: ("search-row", ix.section * 1000 + ix.row).into(),
            selected: Some(ix) == self.selected,
            child: content,
        })
    }

    fn set_selected_index(
        &mut self,
        ix: Option<IndexPath>,
        window: &mut Window,
        cx: &mut Context<ListState<Self>>,
    ) {
        // After every search the list re-picks its row from the rows it drew
        // last frame, and when that frame drew none — the search had only just
        // opened, or the previous query found nothing — it picks none. Return
        // then did nothing, on a list with rows in it. Put the first row back
        // once the list is done.
        if ix.is_none()
            && let Some(first) = self.first_row()
        {
            cx.defer_in(window, move |state, window, cx| {
                if state.selected_index().is_none() {
                    state.set_selected_index(Some(first), window, cx);
                }
            });
        }
        let moved = self.selected != ix;
        self.selected = ix;
        // The list only emits `Select` for the arrow keys; a query that re-arms
        // the first row moves the highlight silently. The theme picker previews
        // whatever is highlighted, so it has to hear about both.
        if moved && let Some(ix) = ix {
            cx.emit(ListEvent::Select(ix));
        }
        cx.notify();
    }
}

pub enum SearchEvent {
    Confirm(CommandKind),
    Dismiss,
    /// Show the theme at this preset index without persisting it: the theme
    /// picker previews the highlighted row while it stays open.
    PreviewTheme(usize),
    /// Put back the theme that was live before the preview started.
    CancelThemePreview,
}

pub struct SearchView {
    list: Entity<ListState<SearchDelegate>>,
    catalog: Rc<Catalog>,
    /// The tab showing — or, in the theme picker, the one Escape returns to.
    tab: SearchTab,
    /// What was typed when the theme picker opened, put back when it closes.
    parked_query: Option<String>,
    /// Preset index the theme picker is currently previewing, so the same
    /// theme is not re-applied on every redundant selection event.
    previewing: Option<usize>,
    _sub: Subscription,
}

impl SearchView {
    /// The search, on `tab`, with `query` already typed.
    ///
    /// The seed goes through the search field rather than around it, so what
    /// the reader sees is a search in the state they would have typed it into:
    /// the text is there, rows are ranked against it, and the next keystroke
    /// goes on refining instead of starting over.
    pub fn new(
        catalog: Catalog,
        tab: SearchTab,
        query: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let catalog = Rc::new(catalog);
        let delegate = SearchDelegate::new(catalog.clone(), Scope::Tab(tab), Vec::new(), cx);
        let list = Self::build_list(delegate, None, window, cx);
        if !query.is_empty() {
            list.update(cx, |state, cx| state.set_query(query, window, cx));
        }
        let _sub = cx.subscribe_in(&list, window, Self::on_list_event);
        Self {
            list,
            catalog,
            tab,
            parked_query: None,
            previewing: None,
            _sub,
        }
    }

    fn in_themes(&self) -> bool {
        self.parked_query.is_some()
    }

    fn build_list(
        delegate: SearchDelegate,
        selected: Option<IndexPath>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ListState<SearchDelegate>> {
        let first = selected.or_else(|| delegate.first_row());
        let list = cx.new(|cx| ListState::new(delegate, window, cx).searchable(true));
        list.update(cx, |state, cx| {
            // `ListState::new` starts with nothing selected, and it only picks a
            // row once a query changes. Opening the search and pressing Return
            // therefore did nothing at all, and until then no row showed what
            // Return was aimed at.
            state.set_selected_index(first, window, cx);
            if selected.is_some() {
                state.scroll_to_selected_item(window, cx);
            }
            state.focus(window, cx);
        });
        list
    }

    fn replace_list(
        &mut self,
        list: Entity<ListState<SearchDelegate>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self._sub = cx.subscribe_in(&list, window, Self::on_list_event);
        self.list = list;
        cx.notify();
    }

    /// Shows `tab`, keeping what is typed — or replacing it with `query`.
    ///
    /// In place, on the same list: the field keeps its text and focus, and
    /// only the rows under it change.
    pub(crate) fn set_tab(
        &mut self,
        tab: SearchTab,
        query: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.in_themes() {
            return;
        }
        self.tab = tab;
        self.list.update(cx, |state, cx| {
            state.delegate_mut().scope = Scope::Tab(tab);
            if let Some(query) = query {
                state.set_query(query, window, cx);
            }
            // `set_query` searches only when the text changed; the tab did.
            state.delegate_mut().refresh(cx);
            let first = state.delegate().first_row();
            state.set_selected_index(first, window, cx);
            state.scroll_to_item(IndexPath::default(), ScrollStrategy::Top, window, cx);
        });
        cx.notify();
    }

    /// The Sessions tab's rows, arrived from a scan that finished after the
    /// search opened. The highlight stays on the row it was on when that row
    /// is still there, so a list that fills in under the cursor does not
    /// move what Return runs.
    pub(crate) fn set_sessions(
        &mut self,
        sessions: Vec<Item>,
        here: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut catalog = (*self.catalog).clone();
        catalog.sessions = sessions;
        catalog.sessions_here = here;
        self.catalog = Rc::new(catalog);
        if self.in_themes() {
            return;
        }
        let catalog = self.catalog.clone();
        self.list.update(cx, |state, cx| {
            let before = state.delegate().selected_item().map(|i| i.kind.clone());
            let delegate = state.delegate_mut();
            delegate.catalog = catalog;
            delegate.refresh(cx);
            let keep = before.and_then(|kind| delegate.position_of(&kind));
            let target = keep.or_else(|| state.delegate().first_row());
            state.set_selected_index(target, window, cx);
        });
        cx.notify();
    }

    fn step_tab(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.set_tab(self.tab.step(forward), None, window, cx);
    }

    fn open_themes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.parked_query = Some(self.list.read(cx).delegate().query.clone());
        // Open on the theme already in use: the picker previews the
        // highlighted row, and merely opening it must not change what the
        // window looks like.
        self.previewing = Item::active_theme_index(cx);
        let delegate =
            SearchDelegate::new(self.catalog.clone(), Scope::Themes, Item::themes(cx), cx);
        let list = Self::build_list(delegate, self.previewing.map(IndexPath::new), window, cx);
        self.replace_list(list, window, cx);
    }

    fn close_themes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Backing out of the picker is not a choice: whatever was previewed
        // goes back to what it was.
        self.previewing = None;
        cx.emit(SearchEvent::CancelThemePreview);
        let query = self.parked_query.take().unwrap_or_default();
        let delegate =
            SearchDelegate::new(self.catalog.clone(), Scope::Tab(self.tab), Vec::new(), cx);
        let list = Self::build_list(delegate, None, window, cx);
        if !query.is_empty() {
            list.update(cx, |state, cx| state.set_query(&query, window, cx));
        }
        self.replace_list(list, window, cx);
    }

    fn selected_edit_command(&self, cx: &App) -> Option<CommandKind> {
        self.list
            .read(cx)
            .delegate()
            .selected_item()
            .and_then(|item| item.kind.edit_variant())
    }

    fn on_list_event(
        &mut self,
        list: &Entity<ListState<SearchDelegate>>,
        ev: &ListEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ev {
            ListEvent::Confirm(ix) => {
                let row = list.read(cx).delegate().row_at(*ix).cloned();
                match row {
                    Some(Row::More { tab, .. }) => self.set_tab(tab, None, window, cx),
                    Some(Row::Item(item)) => match item.kind {
                        CommandKind::OpenThemePicker => self.open_themes(window, cx),
                        // What was typed found this row; it is not an address.
                        CommandKind::SearchHosts => {
                            self.set_tab(SearchTab::Hosts, Some(""), window, cx)
                        }
                        kind => cx.emit(SearchEvent::Confirm(kind)),
                    },
                    None => cx.emit(SearchEvent::Dismiss),
                }
            }
            ListEvent::Cancel => match self.in_themes() {
                true => self.close_themes(window, cx),
                false => cx.emit(SearchEvent::Dismiss),
            },
            ListEvent::Select(ix) => {
                if self.in_themes()
                    && let Some(Row::Item(item)) = list.read(cx).delegate().row_at(*ix)
                    && let CommandKind::SetTheme(i) = item.kind
                    && self.previewing != Some(i)
                {
                    self.previewing = Some(i);
                    cx.emit(SearchEvent::PreviewTheme(i));
                }
            }
        }
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        // The tabs take the rows' neutral steps: the one showing is where the
        // list is, not an action, so it is the selected step and not an accent.
        let sf = cx.global::<crate::ui::presets::Surfaces>().popover;
        let (active_bg, hover_bg) = (gpui::rgb(sf.selected), gpui::rgb(sf.hover));
        let theme = cx.theme();
        let (fg, muted) = (theme.foreground, theme.muted_foreground);
        h_flex()
            .px(px(LIST_PAD))
            .pt(px(8.))
            .gap(px(2.))
            .items_center()
            .children(SearchTab::ORDER.into_iter().enumerate().map(|(i, tab)| {
                let active = tab == self.tab;
                div()
                    .id(("search-tab", i))
                    .h(px(24.))
                    .px(px(10.))
                    .flex()
                    .items_center()
                    .rounded(px(6.))
                    .text_size(rems(ROW_META))
                    .cursor_pointer()
                    .map(|d| match active {
                        true => d
                            .bg(active_bg)
                            .text_color(fg)
                            .font_weight(FontWeight::MEDIUM),
                        false => d.text_color(muted).hover(move |d| d.bg(hover_bg)),
                    })
                    .child(tab.title())
                    .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                        this.set_tab(tab, None, window, cx);
                        this.list.update(cx, |state, cx| state.focus(window, cx));
                    }))
            }))
            .child(div().flex_1())
            .child(keycap(crate::ui::keymap::key_tokens("tab").join(""), cx))
    }

    /// The switcher's footer, minus its New workspace button: the keys that
    /// move and run, as caps with a word after each.
    fn render_footer(&self, cx: &App) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let hint = |keys: Vec<gpui::AnyElement>, label: &'static str| {
            h_flex()
                .items_center()
                .gap(px(6.))
                .children(keys)
                .child(label)
        };
        h_flex()
            .flex_none()
            .items_center()
            .justify_end()
            .gap(px(16.))
            .h(px(FOOTER_H))
            .px(px(14.))
            .border_t_1()
            .border_color(theme.border)
            .overflow_hidden()
            .text_size(rems(ROW_META))
            .text_color(theme.muted_foreground)
            .child(hint(
                vec![keycap("↑", cx), keycap("↓", cx)],
                t(L10nKey::SwitcherHintNavigate),
            ))
            .child(hint(vec![keycap("↵", cx)], t(L10nKey::SwitcherHintOpen)))
    }
}

impl EventEmitter<SearchEvent> for SearchView {}

// The search is the workspace switcher's sibling (see `switcher.rs`): the
// same card, the same corner, the same keycaps and footer, and the same
// neutral step for the row the keyboard is on. The numbers below are the ones
// the switcher uses wherever the two overlays show the same thing, so opening
// one after the other does not move the chrome.

/// How tall a row is. One line where the switcher's rows carry two; 32 keeps
/// an 18px keycap clear of the row's edges by the same 7px the switcher's
/// footer gives its own.
const ROW_H: f32 = 32.;

/// A section heading: the right panel's 28px heading row.
const HEADER_H: f32 = 28.;

/// How far the list sits in from the card's edges, and how far a row's text
/// then sits in from its own fill — the switcher's `COLUMN_PAD` and
/// `ROW_PAD`.
const LIST_PAD: f32 = 8.;
const ROW_PAD: f32 = 10.;

/// Where a row's label starts, so a section heading lands on the same column
/// as the rows under it. A heading has neither the row's inset nor its
/// padding, so it carries the sum.
const LABEL_INSET: f32 = LIST_PAD + ROW_PAD;

/// The row fill's corner — the switcher's `LIST_RADIUS`.
const ROW_RADIUS: f32 = 8.;

/// The card's breathing room from the window's bottom edge, and its width.
/// Its corner and its footer are `ui::dialog`'s, the switcher's numbers.
const CARD_MARGIN: f32 = 24.;
const CARD_MAX_W: f32 = 600.;

/// The search row gpui-component's `List` draws above the rows: a 32px field
/// with 6px above and below and a 1px rule.
const SEARCH_H: f32 = 32. + 6. * 2. + 1.;

/// A row's title, and the subtitle, hints and keycaps beside it: the
/// switcher's row name and its metadata line.
const ROW_TEXT: f32 = 13. / 16.;
const ROW_META: f32 = 11.5 / 16.;

const AVATAR: f32 = 18.;

/// The tab row's height with its padding, reserved out of the list's.
const TABS_H: f32 = 32.;

/// The chord that opens the selected row for editing instead of running it.
///
/// It cannot be `→`: gpui-component's `Input` binds bare `right` to MoveRight
/// in its own key context, so with the query field focused the search never
/// sees the key — the old `→ edit` badge was advertising a gesture that could
/// not fire. `⌘↵` is no better; the app binds it to ToggleFullscreen, which
/// wins for the same reason. `secondary-e` is claimed by neither.
const EDIT_GESTURE: &str = "secondary-e";

/// Matches `EDIT_GESTURE` against a live keystroke. Keep the two in step.
fn is_edit_gesture(ks: &gpui::Keystroke) -> bool {
    if ks.key != "e" {
        return false;
    }
    let m = &ks.modifiers;
    let secondary = if cfg!(target_os = "macos") {
        m.platform
    } else {
        m.control
    };
    secondary && !m.shift && !m.alt
}

const VISIBLE_ROWS: f32 = 12.;

/// The key context the search's own bindings (Tab, Shift-Tab) live in.
pub(crate) const KEY_CONTEXT: &str = "Search";

impl Render for SearchView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let scrim = crate::ui::presets::scrim_fill(cx);
        let tabs = !self.in_themes();

        let viewport = window.viewport_size();
        // The switcher's drop from the top on a full-size window; a short one
        // still brings the card up to keep its rows.
        let top = (viewport.height.as_f32() * 0.16).clamp(16., crate::ui::switcher::CARD_TOP);
        // Reserve space for the tab row, the search row, the footer and the
        // card's margin, including when a split-screen window is shorter than
        // the full list.
        let tabs_h = if tabs { TABS_H } else { 0. };
        let chrome = tabs_h + SEARCH_H + FOOTER_H + CARD_MARGIN;
        let list_max_h = px((viewport.height.as_f32() - top - chrome)
            .max(ROW_H)
            .min(ROW_H * VISIBLE_ROWS + LIST_PAD * 2.));
        let typed = !self.list.read(cx).delegate().query.is_empty();
        let placeholder = match tabs {
            true => self.tab.placeholder(),
            false => t(L10nKey::SearchTheme),
        };
        let card = v_flex()
            .relative()
            .w(px((viewport.width.as_f32() - 32.).clamp(0., CARD_MAX_W)))
            .map(|panel| crate::ui::theme::floating_surface(panel, cx))
            .rounded(px(CARD_RADIUS))
            .overflow_hidden()
            .when(tabs, |card| card.child(self.render_tabs(cx)))
            .child(
                List::new(&self.list)
                    .search_placeholder(placeholder)
                    .py(px(LIST_PAD))
                    .max_h(list_max_h),
            )
            // The switcher's `esc` cap in the search row's trailing corner.
            // Laid over the row rather than inside it — the row is the list's
            // own — and only while the field is empty, since typing brings up
            // the field's clear button in the same spot. No handlers, so a
            // click on it still reaches the field under it.
            .when(!typed, |card| {
                card.child(
                    div()
                        .absolute()
                        .top(px(tabs_h + (SEARCH_H - 1. - KEYCAP) / 2.))
                        .right(px(14.))
                        .child(keycap("esc", cx)),
                )
            })
            .child(self.render_footer(cx));

        div()
            .absolute()
            .inset_0()
            .flex()
            .items_start()
            .justify_center()
            .pt(px(top))
            .bg(scrim)
            .key_context(KEY_CONTEXT)
            .on_action(
                cx.listener(|this, _: &SearchNextTab, window, cx| this.step_tab(true, window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SearchPrevTab, window, cx| this.step_tab(false, window, cx)),
            )
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _window, cx| {
                if is_edit_gesture(&ev.keystroke)
                    && let Some(edit) = this.selected_edit_command(cx)
                {
                    cx.stop_propagation();
                    cx.emit(SearchEvent::Confirm(edit));
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_this, _: &MouseDownEvent, _window, cx| {
                    cx.emit(SearchEvent::Dismiss);
                }),
            )
            .child(div().occlude().child(card))
    }
}

/// A search row.
///
/// Its own element rather than gpui-component's `ListItem`, which paints the
/// keyboard row in the theme's `list_active` — the accent wash menus use. The
/// search's keyboard row is where the cursor is, not an action, so it takes
/// the popover's neutral selected step the switcher's rows do. The list moves
/// the selection under the pointer, so hover and selection are one bar; the
/// hover step here only shows while a row is under the pointer and the
/// selection has not caught up with it.
#[derive(IntoElement)]
pub struct SearchRow {
    id: gpui::ElementId,
    selected: bool,
    child: gpui::AnyElement,
}

impl Selectable for SearchRow {
    fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    fn is_selected(&self) -> bool {
        self.selected
    }
}

impl RenderOnce for SearchRow {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let sf = cx.global::<crate::ui::presets::Surfaces>().popover;
        let (hover, picked) = (gpui::rgb(sf.hover), gpui::rgb(sf.selected));
        h_flex()
            .id(self.id)
            .items_center()
            .h(px(ROW_H))
            .mx(px(LIST_PAD))
            .px(px(ROW_PAD))
            .rounded(px(ROW_RADIUS))
            .overflow_hidden()
            .cursor_pointer()
            .text_size(rems(ROW_TEXT))
            .text_color(cx.theme().foreground)
            .when(self.selected, |r| r.bg(picked))
            .when(!self.selected, |r| r.hover(move |s| s.bg(hover)))
            .child(self.child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::app::{Tty7App, test_window::harness_with_tabs};
    use gpui::{TestAppContext, VisualTestContext};

    fn open(app: &Entity<Tty7App>, vcx: &mut VisualTestContext) -> Entity<SearchView> {
        app.read_with(vcx, |app, _| {
            app.search.clone().expect("the search is open")
        })
    }

    fn first_kind(view: &Entity<SearchView>, vcx: &mut VisualTestContext) -> Option<CommandKind> {
        view.read_with(vcx, |view, cx| {
            let delegate = view.list.read(cx).delegate();
            Some(delegate.row_at(delegate.first_row()?)?.item()?.kind.clone())
        })
    }

    #[gpui::test]
    fn tab_walks_the_tabs_and_keeps_what_was_typed(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 2);
        crate::ui::i18n::set_locale("en");
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_search(SearchTab::All, "split right", window, cx)
        });
        vcx.run_until_parked();
        let view = open(&app, &mut vcx);

        vcx.simulate_keystrokes("tab");
        vcx.run_until_parked();
        view.read_with(&vcx, |view, cx| {
            assert_eq!(view.tab, SearchTab::Actions);
            assert_eq!(view.list.read(cx).delegate().query, "split right");
        });
        assert_eq!(first_kind(&view, &mut vcx), Some(CommandKind::SplitRight));

        // Backwards, and round the end.
        vcx.simulate_keystrokes("shift-tab shift-tab");
        vcx.run_until_parked();
        view.read_with(&vcx, |view, _| assert_eq!(view.tab, SearchTab::Hosts));
        assert!(
            app.read_with(&vcx, |app, _| app.search.is_some()),
            "Tab stays inside the search instead of walking focus out of it"
        );
    }

    /// Opened with nothing typed, Return goes back to the tab you were last
    /// in — the All tab leads with this window's other tabs.
    #[gpui::test]
    fn return_on_an_empty_search_goes_to_the_previous_tab(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 3);
        app.update_in(&mut vcx, |app, _, _| {
            app.tabs[1].last_used.set(5);
            app.tabs[2].last_used.set(9);
        });
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_search(SearchTab::All, "", window, cx)
        });
        vcx.run_until_parked();
        let view = open(&app, &mut vcx);
        assert!(
            matches!(
                first_kind(&view, &mut vcx),
                Some(CommandKind::GoToTab { .. })
            ),
            "the first row is a tab"
        );

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        app.read_with(&vcx, |app, _| {
            assert!(app.search.is_none(), "picking a row closes the search");
            assert_eq!(app.active, 2, "back to the tab used before this one");
        });
    }

    /// "SSH: Add Connection…" was a second input box. It is the Hosts tab
    /// now — and whatever found the row is not an address, so it goes.
    #[gpui::test]
    fn add_connection_moves_to_an_empty_hosts_tab(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 1);
        crate::ui::i18n::set_locale("en");
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_search(SearchTab::Actions, "ssh-add-connection", window, cx)
        });
        vcx.run_until_parked();
        let view = open(&app, &mut vcx);
        assert_eq!(first_kind(&view, &mut vcx), Some(CommandKind::SearchHosts));

        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        view.read_with(&vcx, |view, cx| {
            assert_eq!(view.tab, SearchTab::Hosts);
            assert_eq!(view.list.read(cx).delegate().query, "");
        });
    }

    /// The list re-picks its row from what it drew last frame, and a query
    /// that found nothing drew nothing — so the query typed next came up with
    /// rows and nothing selected, and Return did nothing.
    #[gpui::test]
    fn a_query_after_one_that_found_nothing_still_arms_its_first_row(cx: &mut TestAppContext) {
        let (app, mut vcx, _streams) = harness_with_tabs(cx, 1);
        crate::ui::i18n::set_locale("en");
        app.update_in(&mut vcx, |app, window, cx| {
            app.open_search(SearchTab::Actions, "", window, cx)
        });
        vcx.run_until_parked();
        let view = open(&app, &mut vcx);

        vcx.simulate_input("split rightq");
        vcx.run_until_parked();
        assert_eq!(first_kind(&view, &mut vcx), None, "nothing matches");
        // One keystroke back to a query with rows: the one search that runs
        // right after a frame that drew nothing.
        vcx.simulate_keystrokes("backspace");
        vcx.run_until_parked();

        let selected = view.read_with(&vcx, |view, cx| view.list.read(cx).selected_index());
        assert!(selected.is_some(), "Return has a row to run");
        vcx.simulate_keystrokes("enter");
        vcx.run_until_parked();
        assert!(app.read_with(&vcx, |app, _| app.search.is_none()));
    }
}
