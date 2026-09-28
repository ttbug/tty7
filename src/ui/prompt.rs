//! Every confirmation the app asks — `window.prompt` — drawn as one of the
//! app's own cards.
//!
//! gpui answers `window.prompt` with the platform's dialog where there is one:
//! NSAlert on macOS, TaskDialog on Windows, and on Linux a fallback of its own
//! that clipped any sentence longer than its box (#920). The native two were
//! correct and still wrong for this app. Every other question tty7 asks — the
//! SSH sheet, the worktree prompt, New Workspace — is a card built from
//! [`crate::ui::dialog`]: the switcher's 12px corner, a title row with an
//! `esc` cap, a hairline footer, an ink-filled primary. Closing a busy tab
//! instead threw up a system alert with a centred app icon and grey stacked
//! buttons, so the same app had two ideas of what a dialog looks like, and
//! which one you got depended on which question it was.
//!
//! So all three platforms use this. It is laid out like the other cards, sits
//! where they sit, wraps its text, and answers Return and Escape the way the
//! native dialogs did — which the call sites were written against.

use gpui::{
    App, Context, EventEmitter, FocusHandle, Focusable, PromptButton, PromptHandle, PromptLevel,
    PromptResponse, RenderablePromptHandle, Window, div, prelude::*, px, rems,
};
use gpui_component::{ActiveTheme as _, h_flex};

use crate::ui::dialog::{self, Tone};
use crate::ui::right_panel::{TAB_TEXT, TEXT};

/// Route every `window.prompt` through [`TextPrompt`].
pub(crate) fn install(cx: &mut App) {
    cx.set_prompt_builder(build);
}

fn build(
    _level: PromptLevel,
    message: &str,
    detail: Option<&str>,
    answers: &[PromptButton],
    handle: PromptHandle,
    window: &mut Window,
    cx: &mut App,
) -> RenderablePromptHandle {
    let prompt = cx.new(|cx| TextPrompt::new(message, detail, answers, cx));
    handle.with_view(prompt, window, cx)
}

/// A confirmation card is a sentence and two buttons; the worktree form, with
/// three fields, is 440.
const WIDTH: f32 = 420.;

pub(crate) struct TextPrompt {
    message: String,
    detail: Option<String>,
    answers: Vec<PromptButton>,
    focus: FocusHandle,
}

impl TextPrompt {
    fn new(
        message: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            message: message.to_string(),
            detail: detail.map(str::to_string).filter(|d| !d.trim().is_empty()),
            answers: answers.to_vec(),
            focus: cx.focus_handle(),
        }
    }

    /// The answer a key picks. Return is answer 0 — `confirm_answers` puts the
    /// action there for exactly this — and Escape is [`Self::cancel_answer`];
    /// a prompt with no way out has nothing Escape can safely mean.
    fn answer_for_key(&self, key: &str) -> Option<usize> {
        match key {
            "enter" if !self.answers.is_empty() => Some(0),
            "escape" => self.cancel_answer(),
            _ => None,
        }
    }

    /// The answer that backs out: the one marked cancel, or a lone
    /// acknowledgement — an `OK` with nothing else to pick is also the way
    /// out, and a card Escape cannot close is a trap.
    fn cancel_answer(&self) -> Option<usize> {
        match self.answers.as_slice() {
            [_] => Some(0),
            answers => answers.iter().position(PromptButton::is_cancel),
        }
    }

    /// Whether answer `ix` sits apart, at the footer's far left. Answers
    /// listed after the cancel in a prompt of three or more are the ones a
    /// caller wants away from Return — Discard beside Save / Cancel — and
    /// packed in with the rest it would sit one button from the default.
    fn stands_apart(&self, ix: usize) -> bool {
        self.answers.len() >= 3
            && self
                .answers
                .iter()
                .position(PromptButton::is_cancel)
                .is_some_and(|cancel| ix > cancel)
    }

    /// How answer `ix` is painted.
    ///
    /// Answer 0 is the one Return picks, so it is the one filled in — unless
    /// it is the cancel. A caller only puts Cancel first when the action is
    /// one Return must never reach by accident (discarding changes, a hard
    /// reset); that action is the one the card warns about, so it goes red,
    /// and the safe answer keeps the quiet paint.
    fn tone(&self, ix: usize) -> Tone {
        let answer = &self.answers[ix];
        let cancel_first = self.answers.first().is_some_and(PromptButton::is_cancel);
        match (ix, answer.is_cancel()) {
            (_, true) => Tone::Secondary,
            (0, false) => Tone::Primary,
            (_, false) if cancel_first => Tone::Danger,
            _ => Tone::Secondary,
        }
    }
}

impl EventEmitter<PromptResponse> for TextPrompt {}

impl Focusable for TextPrompt {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for TextPrompt {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let rungs = dialog::popover_rungs(cx);
        let escapable = self.cancel_answer().is_some();
        let has_detail = self.detail.is_some();

        // Answer 0 goes rightmost, where the native dialogs put it and where
        // `confirm_answers` expects it to land.
        let mut apart = Vec::new();
        let mut packed = Vec::new();
        for ix in (0..self.answers.len()).rev() {
            let button = dialog::button(
                ("prompt-answer", ix),
                self.answers[ix].label().clone(),
                self.tone(ix),
                true,
                rungs,
                cx,
                cx.listener(move |_, _, _, cx| {
                    cx.emit(PromptResponse(ix));
                    cx.stop_propagation();
                }),
            );
            match self.stands_apart(ix) {
                true => apart.push(button),
                false => packed.push(button),
            }
        }

        // The other cards' title row, except that the title wraps: theirs are
        // names, which truncate cleanly, and this one is a question, which
        // cut short no longer asks anything. One line still sits in the
        // 48px row exactly where theirs does.
        let title = h_flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .min_h(px(dialog::HEADER_H))
            .py(px(12.))
            .pl(px(dialog::INSET))
            .pr(px(14.))
            .when(has_detail, |row| row.border_b_1().border_color(border))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(rems(TEXT))
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .child(self.message.clone()),
            )
            .when(escapable, |row| row.child(dialog::keycap("esc", cx)));

        let card = dialog::card(WIDTH, cx)
            .max_w(gpui::relative(0.9))
            .child(title)
            .children(self.detail.clone().map(|detail| {
                dialog::body().child(
                    div()
                        .text_size(rems(TAB_TEXT))
                        .line_height(rems(TAB_TEXT * 1.5))
                        .text_color(muted)
                        .child(detail),
                )
            }))
            .child(
                dialog::footer(cx)
                    .children(apart)
                    .when(self.answers.len() >= 3, |row| row.child(div().flex_1()))
                    .children(packed),
            );

        div()
            .id("text-prompt")
            .track_focus(&self.focus)
            .size_full()
            // Nothing under the scrim answers the pointer while the question
            // is up: the prompt is painted over the window, not into it, and
            // gpui hands a click to every hitbox under it that is not
            // occluded — the dimmed window would still take it.
            .occlude()
            .cursor_default()
            .bg(crate::ui::presets::scrim_fill(cx))
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if let Some(ix) = this.answer_for_key(&ev.keystroke.key) {
                    cx.emit(PromptResponse(ix));
                    cx.stop_propagation();
                }
            }))
            // Clicking the scrim backs out, as it does around the other
            // cards — when there is a safe answer to back out to.
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| {
                    if let Some(ix) = this.cancel_answer() {
                        cx.emit(PromptResponse(ix));
                    }
                }),
            )
            .flex()
            .flex_col()
            .items_center()
            .justify_start()
            .pt(px(crate::ui::switcher::CARD_TOP))
            .child(card)
    }
}

#[cfg(test)]
mod tests {
    use super::TextPrompt;
    use gpui::{AppContext as _, PromptButton, TestAppContext};

    fn prompt(cx: &mut TestAppContext, answers: &[PromptButton]) -> gpui::Entity<TextPrompt> {
        cx.update(|cx| {
            cx.new(|cx| {
                TextPrompt::new(
                    "Quit and stop tty7 server?",
                    Some("a detail long enough to need more than one line"),
                    answers,
                    cx,
                )
            })
        })
    }

    #[gpui::test]
    fn return_confirms_and_escape_takes_the_cancel_answer(cx: &mut TestAppContext) {
        let p = prompt(cx, &crate::ui::confirm_answers("Quit", "Cancel"));
        p.read_with(cx, |p, _| {
            assert_eq!(p.answer_for_key("enter"), Some(0));
            assert_eq!(p.answer_for_key("escape"), Some(1));
            assert_eq!(p.answer_for_key("a"), None);
        });
    }

    #[gpui::test]
    fn the_action_is_filled_and_the_safe_answer_stays_quiet(cx: &mut TestAppContext) {
        use crate::ui::dialog::Tone;
        let p = prompt(cx, &crate::ui::confirm_answers("Close", "Keep"));
        p.read_with(cx, |p, _| {
            assert_eq!(p.tone(0), Tone::Primary);
            assert_eq!(p.tone(1), Tone::Secondary);
        });
    }

    #[gpui::test]
    fn cancel_first_marks_the_other_answer_as_the_dangerous_one(cx: &mut TestAppContext) {
        // Discard puts Cancel at 0 so Return cannot throw work away; the card
        // has to say which button does.
        use crate::ui::dialog::Tone;
        let p = prompt(
            cx,
            &[PromptButton::cancel("Cancel"), PromptButton::new("Discard")],
        );
        p.read_with(cx, |p, _| {
            assert_eq!(p.answer_for_key("enter"), Some(0));
            assert_eq!(p.answer_for_key("escape"), Some(0));
            assert_eq!(p.tone(0), Tone::Secondary);
            assert_eq!(p.tone(1), Tone::Danger);
        });
    }

    #[gpui::test]
    fn discard_stands_apart_from_save_and_cancel(cx: &mut TestAppContext) {
        let p = prompt(
            cx,
            &[
                PromptButton::ok("Save"),
                PromptButton::cancel("Cancel"),
                PromptButton::ok("Discard"),
            ],
        );
        p.read_with(cx, |p, _| {
            assert!(!p.stands_apart(0));
            assert!(!p.stands_apart(1));
            assert!(p.stands_apart(2));
            assert_eq!(p.answer_for_key("escape"), Some(1));
        });
    }

    #[gpui::test]
    fn a_lone_ok_answers_escape_too(cx: &mut TestAppContext) {
        let p = prompt(cx, &[PromptButton::new("OK")]);
        p.read_with(cx, |p, _| {
            assert_eq!(p.answer_for_key("escape"), Some(0));
        });
    }

    #[gpui::test]
    fn escape_does_nothing_without_a_cancel_answer(cx: &mut TestAppContext) {
        let p = prompt(cx, &[PromptButton::ok("OK"), PromptButton::new("Retry")]);
        p.read_with(cx, |p, _| {
            assert_eq!(p.answer_for_key("escape"), None);
        });
    }
}
