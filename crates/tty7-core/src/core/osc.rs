const MAX_PAYLOAD: usize = 8192;

pub struct OscTokenizer {
    ids: &'static [&'static [u8]],
    buf: Vec<u8>,
    state: State,
}

#[derive(Default, Clone, Copy)]
enum State {
    #[default]
    Ground,
    Esc,
    Osc,
    OscEsc,
    Ignore,
    IgnoreEsc,
}

impl OscTokenizer {
    pub fn new(ids: &'static [&'static [u8]]) -> Self {
        Self {
            ids,
            buf: Vec::new(),
            state: State::Ground,
        }
    }

    pub fn feed(&mut self, bytes: &[u8], mut on_payload: impl FnMut(&[u8])) {
        self.feed_at(bytes, |_, payload| on_payload(payload));
    }

    /// [`feed`](Self::feed), but also reporting where each payload ended: an
    /// offset one past its terminator, in ascending order, so a reader can
    /// advance an emulator to exactly there and read the state the sequence
    /// left behind. A payload split across two feeds is reported against the
    /// batch its terminator landed in.
    pub fn feed_at(&mut self, bytes: &[u8], mut on_payload: impl FnMut(usize, &[u8])) {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => {
                    let Some(off) = memchr::memchr(0x1b, &bytes[i..]) else {
                        return;
                    };
                    self.state = State::Esc;
                    i += off + 1;
                    continue;
                }
                State::Ignore => {
                    let Some(off) = memchr::memchr2(0x07, 0x1b, &bytes[i..]) else {
                        return;
                    };
                    self.state = if bytes[i + off] == 0x07 {
                        State::Ground
                    } else {
                        State::IgnoreEsc
                    };
                    i += off + 1;
                    continue;
                }
                _ => {}
            }
            let b = bytes[i];
            match self.state {
                State::Ground | State::Ignore => unreachable!(),
                State::Esc => match b {
                    b']' => {
                        self.buf.clear();
                        self.state = State::Osc;
                    }
                    0x1b => {}
                    _ => self.state = State::Ground,
                },
                State::Osc => match b {
                    0x07 => self.finish(i + 1, &mut on_payload),
                    0x1b => self.state = State::OscEsc,
                    _ => {
                        self.buf.push(b);
                        if self.buf.len() > MAX_PAYLOAD || !self.identifier_could_match() {
                            self.buf.clear();
                            self.state = State::Ignore;
                        }
                    }
                },
                State::OscEsc => match b {
                    b'\\' => self.finish(i + 1, &mut on_payload),
                    0x1b => {}
                    b']' => {
                        self.buf.clear();
                        self.state = State::Osc;
                    }
                    _ => {
                        self.buf.clear();
                        self.state = State::Ground;
                    }
                },
                State::IgnoreEsc => match b {
                    b'\\' => self.state = State::Ground,
                    0x1b => {}
                    b']' => {
                        self.buf.clear();
                        self.state = State::Osc;
                    }
                    _ => self.state = State::Ground,
                },
            }
            i += 1;
        }
    }

    fn identifier_could_match(&self) -> bool {
        match self.buf.iter().position(|&b| b == b';') {
            Some(pos) => self.ids.iter().any(|&id| id == &self.buf[..pos]),
            None => self.ids.iter().any(|id| id.starts_with(&self.buf)),
        }
    }

    fn finish(&mut self, at: usize, on_payload: &mut impl FnMut(usize, &[u8])) {
        on_payload(at, &self.buf);
        self.buf.clear();
        self.state = State::Ground;
    }
}

pub fn parse_notification(payload: &[u8]) -> Option<(Option<String>, String)> {
    if let Some(rest) = payload.strip_prefix(b"9;") {
        let first = rest.split(|&b| b == b';').next().unwrap_or(rest);
        if first.len() == 1 && first[0].is_ascii_digit() {
            return None;
        }
        let body = String::from_utf8_lossy(rest).into_owned();
        return (!body.is_empty()).then_some((None, body));
    }
    if let Some(rest) = payload.strip_prefix(b"777;notify;") {
        let mut parts = rest.splitn(2, |&b| b == b';');
        let first = String::from_utf8_lossy(parts.next().unwrap_or(b"")).into_owned();
        let second = parts
            .next()
            .map(|b| String::from_utf8_lossy(b).into_owned());
        let (title, body) = match second {
            Some(body) if !body.is_empty() => (Some(first), body),
            _ => (None, first),
        };
        return (!body.is_empty()).then_some((title, body));
    }
    None
}

/// What a sequence did to the title a pane is showing — see
/// [`TitleLifetime`].
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum TitleEffect {
    /// Nothing to do with the title.
    None,
    /// An OSC 0/2: the pane just named itself.
    Set,
    /// The command that set the title showing has finished, so the title
    /// describes something that is no longer running and has to go.
    Retire,
}

/// How long a title a program set outlives the program (#889).
///
/// An OSC 0/2 has no owner and no end: whatever a pane last named itself
/// stands until something else names it. That is right while the program that
/// wrote it is still running, and wrong the instant it exits — a tab that goes
/// on reading "✳ refactoring the parser" after Claude Code has quit is naming
/// a session that no longer exists, and the pane's directory (the rung below
/// it in the label ladder) would describe it far better.
///
/// The shell integration already says when a command starts and stops: OSC
/// 133;C and 133;D. So a title set *between* them belongs to that command and
/// is retired by its `D`; a title set at a prompt — the shell's own, or one
/// the reader pinned by hand with a bare `printf '\e]0;…'` — belongs to
/// nobody in particular and is left alone. A pane with no shell integration
/// sees neither mark and so keeps every title, exactly as before.
///
/// Both the daemon (which keeps `PaneRecord::osc_title` for the switcher and
/// the CLI) and the window (which keeps its own terminal's title for its tab
/// strip) run this over the same bytes, so the two can never disagree about
/// whether a title is still current. Feeding it in stream order is what makes
/// the answer right: a shell that re-titles itself in `precmd` emits its OSC
/// 0/2 *after* the `D`, and that [`Set`](TitleEffect::Set) is simply the last
/// word.
#[derive(Debug, Default, Clone, Copy)]
pub struct TitleLifetime {
    /// A command owns the pane: a `C` has arrived and its `D` has not.
    running: bool,
    /// The title standing right now was set while a command owned the pane.
    from_command: bool,
    /// Any `133` prompt mark (`A`–`D`) has been read at all.
    marked: bool,
}

impl TitleLifetime {
    /// Reads one OSC payload — identifier included, as
    /// [`OscTokenizer`] reports it — and says what it did to the title.
    pub fn saw(&mut self, payload: &[u8]) -> TitleEffect {
        if payload.starts_with(b"0;") || payload.starts_with(b"2;") {
            self.from_command = self.running;
            return TitleEffect::Set;
        }
        let Some(rest) = payload.strip_prefix(b"133;") else {
            return TitleEffect::None;
        };
        if matches!(rest.first(), Some(b'A'..=b'D')) {
            self.marked = true;
        }
        match rest.first() {
            Some(b'C') => self.running = true,
            Some(b'D') => {
                let retire = self.running && self.from_command;
                // Whatever stands after this mark was not written by a
                // command that is still running, whoever wrote it.
                self.running = false;
                self.from_command = false;
                if retire {
                    return TitleEffect::Retire;
                }
            }
            // `A` and `B` only draw a prompt. They must not retire anything:
            // a shell's own `precmd` title lands between the `D` and the `A`.
            _ => {}
        }
        TitleEffect::None
    }

    /// The stream was picked up partway through a command whose `C` mark is
    /// not in it — a window reattaching to a pane whose replay ring rolled
    /// past the `C` while a long session (an agent, an editor) ran on.
    ///
    /// If what was read carried no prompt mark at all, every byte of it was
    /// written under that command, titles included, so the command's `D`
    /// must retire them just as it would have on a link that saw the `C`.
    /// Without this a reattached window keeps the dead program's title
    /// forever — exactly #889 — while the daemon, which saw the whole stream,
    /// has already dropped it. Any mark read settles the question on its own,
    /// so this does nothing then.
    pub fn joined_mid_command(&mut self) {
        if !self.marked {
            self.running = true;
            self.from_command = true;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(ids: &'static [&'static [u8]], chunks: &[&[u8]]) -> Vec<Vec<u8>> {
        let mut tok = OscTokenizer::new(ids);
        let mut out = Vec::new();
        for c in chunks {
            tok.feed(c, |payload| out.push(payload.to_vec()));
        }
        out
    }

    #[test]
    fn bel_and_st_terminators_both_complete_a_payload() {
        assert_eq!(
            collect(&[b"9"], &[b"\x1b]9;bel\x07"]),
            vec![b"9;bel".to_vec()]
        );
        assert_eq!(
            collect(&[b"9"], &[b"\x1b]9;st\x1b\\"]),
            vec![b"9;st".to_vec()]
        );
    }

    #[test]
    fn sequence_split_across_reads_is_reassembled() {
        assert_eq!(
            collect(&[b"7"], &[b"\x1b]7;file:", b"//h/x", b"\x07"]),
            vec![b"7;file://h/x".to_vec()]
        );
        assert_eq!(
            collect(&[b"9"], &[b"\x1b]9;ping\x1b", b"\\"]),
            vec![b"9;ping".to_vec()]
        );
    }

    #[test]
    fn uninteresting_identifiers_are_skipped_and_state_recovers() {
        assert_eq!(
            collect(
                &[b"9"],
                &[b"\x1b]0;title\x07\x1b]52;c;abc\x1b\\\x1b]9;kept\x07"]
            ),
            vec![b"9;kept".to_vec()]
        );
    }

    #[test]
    fn resyncs_on_new_osc_after_an_unterminated_one() {
        assert_eq!(
            collect(&[b"9"], &[b"\x1b]9;dropped\x1b]9;kept\x07"]),
            vec![b"9;kept".to_vec()]
        );
        assert_eq!(
            collect(&[b"9"], &[b"\x1b]0;title\x1b]9;kept\x07"]),
            vec![b"9;kept".to_vec()]
        );
    }

    #[test]
    fn identifier_prefix_matching_buffers_only_possible_ids() {
        let ids: &'static [&'static [u8]] = &[b"777"];
        assert_eq!(
            collect(ids, &[b"\x1b]78;x\x07\x1b]777;y\x07"]),
            vec![b"777;y".to_vec()]
        );
        assert_eq!(collect(ids, &[b"\x1b]77;x\x07"]), Vec::<Vec<u8>>::new());
    }

    #[test]
    fn oversized_payload_is_abandoned_not_truncated() {
        let mut big = b"\x1b]9;".to_vec();
        big.extend(std::iter::repeat_n(b'x', MAX_PAYLOAD + 1));
        big.extend_from_slice(b"\x07\x1b]9;next\x07");
        assert_eq!(collect(&[b"9"], &[&big]), vec![b"9;next".to_vec()]);
    }

    #[test]
    fn byte_at_a_time_delivery_reassembles_every_state_transition() {
        let stream = b"\x1b]0;title\x07\x1b]133;A\x1b\\plain\x1b]7;file://h/x\x07";
        let chunks: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(
            collect(&[b"7", b"133"], &chunks),
            vec![b"133;A".to_vec(), b"7;file://h/x".to_vec()]
        );
    }

    #[test]
    fn ignored_sequence_split_across_reads_still_recovers() {
        assert_eq!(
            collect(
                &[b"9"],
                &[b"\x1b]52;c;abc", b"defgh\x1b", b"\\\x1b]9;ok\x07"]
            ),
            vec![b"9;ok".to_vec()]
        );
    }

    #[test]
    fn offsets_land_one_past_the_terminator() {
        let mut tok = OscTokenizer::new(&[b"9"]);
        let mut got = Vec::new();
        let stream = b"ab\x1b]9;bel\x07cd\x1b]9;st\x1b\\";
        tok.feed_at(stream, |at, payload| got.push((at, payload.to_vec())));
        assert_eq!(
            got,
            vec![(10, b"9;bel".to_vec()), (20, b"9;st".to_vec())],
            "a cut must point just past its sequence"
        );
        assert_eq!(&stream[10..12], b"cd");
        assert_eq!(stream.len(), 20, "the ST-terminated one ends the stream");
    }

    #[test]
    fn an_offset_is_reported_against_the_batch_its_terminator_lands_in() {
        let mut tok = OscTokenizer::new(&[b"777"]);
        let mut got = Vec::new();
        tok.feed_at(b"out\x1b]777;no", |at, p| got.push((at, p.to_vec())));
        assert!(got.is_empty(), "unterminated, so nothing to report yet");
        tok.feed_at(b"tify;x\x07tail", |at, p| got.push((at, p.to_vec())));
        assert_eq!(got, vec![(7, b"777;notify;x".to_vec())]);
    }

    /// Feeds a stream through the tokenizer the way both readers do and
    /// reports what the title ended up being: `Some(t)` for a title that
    /// stands, `None` for one that was retired or never set.
    fn showing(stream: &[u8]) -> Option<String> {
        let mut tok = OscTokenizer::new(&[b"0", b"2", b"133"]);
        let mut life = TitleLifetime::default();
        let mut title = None;
        tok.feed(stream, |payload| match life.saw(payload) {
            TitleEffect::Set => {
                let body = payload.split(|&b| b == b';').nth(1).unwrap_or(b"");
                title = (!body.is_empty()).then(|| String::from_utf8_lossy(body).into_owned());
            }
            TitleEffect::Retire => title = None,
            TitleEffect::None => {}
        });
        title
    }

    /// The bug in #889: Claude Code names the tab, quits, and the name stays
    /// on a pane that is back at its own prompt in a real directory.
    #[test]
    fn a_title_a_command_set_is_retired_when_that_command_finishes() {
        assert_eq!(
            showing(b"\x1b]133;C;claude\x07\x1b]0;refactoring the parser\x07\x1b]133;D;0\x07"),
            None,
            "the program that named the tab has exited"
        );
    }

    /// The case the title is *supposed* to stick for: a TUI that names itself
    /// once and keeps running.
    #[test]
    fn a_title_of_a_command_still_running_stands() {
        assert_eq!(
            showing(b"\x1b]133;C;vim\x07\x1b]0;vim \xe2\x80\x94 main.rs\x07").as_deref(),
            Some("vim — main.rs"),
        );
    }

    /// A shell that re-titles itself in `precmd` writes its OSC 0/2 after the
    /// `D` and before the `A` (tty7's own zsh helper prepends the `D` emitter
    /// for exactly that reason, and the PowerShell one titles between them).
    /// Reading the stream in order is what keeps that title.
    #[test]
    fn a_title_the_shell_writes_at_its_next_prompt_is_the_last_word() {
        assert_eq!(
            showing(
                b"\x1b]133;C;claude\x07\x1b]0;claude\x07\
                  \x1b]133;D;0\x07\x1b]0;me@box:~/dev\x07\x1b]133;A\x07\x1b]133;B\x07"
            )
            .as_deref(),
            Some("me@box:~/dev"),
        );
    }

    /// A title nobody's command set — the shell's, or one pinned by hand at a
    /// prompt — is not a command's to retire.
    #[test]
    fn a_title_set_at_a_prompt_survives_the_next_command() {
        assert_eq!(
            showing(
                b"\x1b]133;A\x07\x1b]0;my tab\x07\x1b]133;B\x07\
                  \x1b]133;C;ls\x07\x1b]133;D;0\x07"
            )
            .as_deref(),
            Some("my tab"),
        );
    }

    /// Without shell integration there are no marks at all, and a title is
    /// kept the way it always was.
    #[test]
    fn a_pane_with_no_marks_keeps_every_title() {
        assert_eq!(showing(b"\x1b]2;anything\x07").as_deref(), Some("anything"));
        let mut life = TitleLifetime::default();
        assert_eq!(life.saw(b"7;file://h/x"), TitleEffect::None);
        assert_eq!(life.saw(b"133;V;1"), TitleEffect::None);
    }

    /// A `D` with no command before it reports the shell's own startup, not a
    /// command that ended; there is nothing of anyone's to retire.
    #[test]
    fn a_d_mark_with_no_command_before_it_retires_nothing() {
        let mut life = TitleLifetime::default();
        assert_eq!(life.saw(b"0;pinned"), TitleEffect::Set);
        assert_eq!(life.saw(b"133;D;0"), TitleEffect::None);
    }

    /// A reattach whose replay ring no longer holds the running command's `C`:
    /// the titles in it are that command's, and its `D` retires them.
    #[test]
    fn a_stream_joined_mid_command_retires_its_title_at_the_d() {
        let mut life = TitleLifetime::default();
        assert_eq!(
            life.saw(b"2;\xe2\x9c\xb3 fixing the switcher"),
            TitleEffect::Set
        );
        life.joined_mid_command();
        assert_eq!(life.saw(b"133;D;0"), TitleEffect::Retire);

        // A replay that carried marks already knows who owns the title: a
        // title pinned at a prompt stays pinned through the next command.
        let mut life = TitleLifetime::default();
        assert_eq!(life.saw(b"133;B"), TitleEffect::None);
        assert_eq!(life.saw(b"0;my tab"), TitleEffect::Set);
        life.joined_mid_command();
        assert_eq!(life.saw(b"133;C;ls"), TitleEffect::None);
        assert_eq!(life.saw(b"133;D;0"), TitleEffect::None);
    }

    #[test]
    fn esc_runs_and_non_osc_escapes_do_not_confuse_the_scanner() {
        assert_eq!(
            collect(&[b"9"], &[b"\x1b\x1b]9;ok\x07"]),
            vec![b"9;ok".to_vec()]
        );
        assert_eq!(
            collect(&[b"9"], &[b"\x1b]9;half\x1b[0m\x1b]9;whole\x07"]),
            vec![b"9;whole".to_vec()]
        );
    }
}
