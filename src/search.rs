use crate::editor::PromptAction;
use crate::highlight::Highlighting;
use crate::input::{InputSeq, KeySeq};
use crate::screen::Screen;
use crate::text_buffer::TextBuffer;
use std::io::{self, Write};

#[derive(Clone, Copy)]
enum FindDir {
    Back,
    Forward,
}
struct FindState {
    last_match: Option<usize>, // last match line
    dir: FindDir,
}

impl Default for FindState {
    fn default() -> FindState {
        FindState {
            last_match: None,
            dir: FindDir::Forward,
        }
    }
}

pub struct TextSearch<'a, W: Write> {
    screen: &'a mut Screen<W>,
    buf: &'a mut TextBuffer,
    hl: &'a mut Highlighting,
    state: FindState,
    saved_cx: usize,
    saved_cy: usize,
    saved_coloff: usize,
    saved_rowoff: usize,
}

impl<'a, W: Write> PromptAction for TextSearch<'a, W> {
    fn on_key(&mut self, input: &str, seq: InputSeq, end: bool) -> io::Result<()> {
        use KeySeq::*;

        if self.state.last_match.is_some() {
            if let Some(matched_line) = self.hl.clear_previous_match() {
                self.hl.needs_update = true;
                self.screen.set_dirty_start(matched_line);
            }
        }

        if end {
            self.on_end(input.as_ref().map(String::is_empty).unwrap_or(true));
            return Ok(());
        }

        match (seq.key, seq.ctrl) {
            (RightKey, ..) | (DownKey, ..) | (Key(b'f'), true) | (Key(b'n'), true) => {
                self.state.dir = FindDir::Forward
            }
            (LeftKey, ..) | (UpKey, ..) | (Key(b'b'), true) | (Key(b'p'), true) => {
                self.state.dir = FindDir::Back
            }
            _ => self.state = FindState::default(),
        }

        fn next_line(y: usize, dir: FindDir, len: usize) -> usize {
            // Wrapping text search at top/bottom of text buffer
            match dir {
                FindDir::Forward if y == len - 1 => 0,
                FindDir::Forward => y + 1,
                FindDir::Back if y == 0 => len - 1,
                FindDir::Back => y - 1,
            }
        }

        let row_len = self.buf.rows().len();
        let dir = self.state.dir;
        let mut y = self
            .state
            .last_match
            .map(|y| next_line(y, dir, row_len)) // Start from next line on moving to next match
            .unwrap_or_else(|| self.buf.cy());

        // TODO: Use more efficient string search algorithm such as Aho-Corasick
        for _ in 0..row_len {
            let row = &self.buf.rows()[y];
            if let Some(byte_idx) = row.buffer().find(input) {
                let idx = row.char_idx_of(byte_idx);
                self.buf.set_cursor(idx, y);

                let row = &self.buf.rows()[y]; // Immutable borrow again since self.buf.set_cursor() yields mutable borrow
                let rx = row.rx_from_cx(self.buf.cx());
                // Cause do_scroll() to scroll upwards to the matching line at next screen redraw
                self.screen.rowoff = row_len;
                self.state.last_match = Some(y);
                // Set match highlight on the found line
                self.hl.set_match(y, rx, rx + input.chars().count());
                // XXX: It updates entire highlights
                self.hl.needs_update = true;
                self.screen.set_dirty_start(y);
                break;
            }
            y = next_line(y, dir, row_len);
        }

        Ok(())
    }
}

impl<'a, W: Write> TextSearch<'a, W> {
    pub fn new<'s: 'a, 't: 'a, 'h: 'a>(
        screen: &'s mut Screen<W>,
        buf: &'t mut TextBuffer,
        hl: &'h mut Highlighting,
    ) -> Self {
        Self {
            saved_cx: buf.cx(),
            saved_cy: buf.cy(),
            saved_coloff: screen.coloff,
            saved_rowoff: screen.rowoff,
            screen,
            buf,
            hl,
            state: FindState::default(),
        }
    }

    fn on_end(&mut self, canceled: bool) -> io::Result<()> {
        if canceled {
            // Canceled. Restore cursor position
            self.buf.set_cursor(self.saved_cx, self.saved_cy);
            self.screen.coloff = self.saved_coloff;
            self.screen.rowoff = self.saved_rowoff;
            self.screen.set_dirty_start(self.screen.rowoff); // Redraw all lines
        } else if self.state.last_match.is_some() {
            self.screen.set_info_message("Found");
        } else {
            self.screen.set_error_message("Not Found");
        }

        Ok(())
    }
}
