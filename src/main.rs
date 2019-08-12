// Refs:
//   Build Your Own Text Editor: https://viewsourcecode.org/snaptoken/kilo/index.html
//   VT100 User Guide: https://vt100.net/docs/vt100-ug/chapter3.html

use std::io::{self, Read, Write};
use std::ops::{Deref, DerefMut};
use std::os::unix::io::AsRawFd;
use std::str;

const VERSION: &'static str = env!("CARGO_PKG_VERSION");

struct StdinRawMode {
    stdin: io::Stdin,
    orig: termios::Termios,
}

// TODO: Separate editor into frontend and backend. In frontend, it handles actual screen and user input.
// It interacts with backend by responding to request from frontend. Frontend focues on core editor
// logic. This is useful when adding a new frontend (e.g. wasm).

impl StdinRawMode {
    fn new() -> io::Result<StdinRawMode> {
        use termios::*;

        let stdin = io::stdin();
        let fd = stdin.as_raw_fd();
        let mut termios = Termios::from_fd(fd)?;
        let orig = termios.clone();

        // Set terminal raw mode. Disable echo back, canonical mode, signals (SIGINT, SIGTSTP) and Ctrl+V.
        termios.c_lflag &= !(ECHO | ICANON | ISIG | IEXTEN);
        // Disable control flow mode (Ctrl+Q/Ctrl+S) and CR-to-NL translation
        termios.c_iflag &= !(IXON | ICRNL | BRKINT | INPCK | ISTRIP);
        // Disable output processing such as \n to \r\n translation
        termios.c_oflag &= !OPOST;
        // Ensure character size is 8bits
        termios.c_cflag |= CS8;
        // Do not wait for next byte with blocking since reading 0 byte is permitted
        termios.c_cc[VMIN] = 0;
        // Set read timeout to 1/10 second it enables 100ms timeout on read()
        termios.c_cc[VTIME] = 1;
        // Apply terminal configurations
        tcsetattr(fd, TCSAFLUSH, &mut termios)?;

        Ok(StdinRawMode { stdin, orig })
    }

    fn input_keys(self) -> InputSequences {
        InputSequences { stdin: self }
    }
}

impl Drop for StdinRawMode {
    fn drop(&mut self) {
        // Restore original terminal mode
        termios::tcsetattr(self.stdin.as_raw_fd(), termios::TCSAFLUSH, &mut self.orig).unwrap();
    }
}

impl Deref for StdinRawMode {
    type Target = io::Stdin;

    fn deref(&self) -> &Self::Target {
        &self.stdin
    }
}

impl DerefMut for StdinRawMode {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.stdin
    }
}

#[derive(PartialEq, Debug)]
enum SpecialKey {
    Left,
    Right,
    Up,
    Down,
}

#[derive(PartialEq, Debug)]
enum InputSeq {
    Unidentified,
    SpecialKey(SpecialKey),
    // TODO: Add Utf8Key(char),
    Key(u8, bool), // Char code and ctrl mod
    Cursor(usize, usize),
}

struct InputSequences {
    stdin: StdinRawMode,
}

impl InputSequences {
    fn read(&mut self) -> io::Result<u8> {
        let mut one_byte: [u8; 1] = [0];
        self.stdin.read(&mut one_byte)?;
        Ok(one_byte[0])
    }

    fn read_blocking(&mut self) -> io::Result<u8> {
        let mut one_byte: [u8; 1] = [0];
        loop {
            if self.stdin.read(&mut one_byte)? > 0 {
                return Ok(one_byte[0]);
            }
        }
    }

    fn decode(&mut self, b: u8) -> io::Result<InputSeq> {
        match b {
            // (Maybe) Escape sequence
            0x1b => {
                let b = self.read_blocking()?;
                // TODO: Escape key input by user does not work properly at this moment.
                if b != b'[' {
                    return self.decode(b);
                }

                let mut buf = vec![];
                let cmd = loop {
                    let b = self.read_blocking()?;
                    match b {
                        b'R' | b'A' | b'B' | b'C' | b'D' => break b,
                        _ => buf.push(b),
                    }
                };

                let args = buf.split(|b| *b == b';');
                match cmd {
                    b'R' => {
                        // https://vt100.net/docs/vt100-ug/chapter3.html#CPR e.g. \x1b[24;80R
                        let mut i = args
                            .map(|b| str::from_utf8(b).ok().and_then(|s| s.parse::<usize>().ok()));
                        match (i.next(), i.next()) {
                            (Some(Some(r)), Some(Some(c))) => Ok(InputSeq::Cursor(r, c)),
                            _ => Ok(InputSeq::Unidentified),
                        }
                    }
                    b'A' => Ok(InputSeq::SpecialKey(SpecialKey::Up)),
                    b'B' => Ok(InputSeq::SpecialKey(SpecialKey::Down)),
                    b'C' => Ok(InputSeq::SpecialKey(SpecialKey::Right)),
                    b'D' => Ok(InputSeq::SpecialKey(SpecialKey::Left)),
                    _ => Ok(InputSeq::Unidentified),
                }
            }
            // Ascii key inputs
            0x20..=0x7f => Ok(InputSeq::Key(b, false)),
            // 0x01~0x1f keys are ascii keys with ctrl. Ctrl mod masks key with 0b11111.
            // Here unmask it with 0b1100000. It only works with 0x61~0x7f.
            0x01..=0x1f => Ok(InputSeq::Key(b | 0b1100000, true)),
            _ => Ok(InputSeq::Unidentified), // TODO: 0x80..=0xff => { ... } Handle UTF-8
        }
    }

    fn read_seq(&mut self) -> io::Result<InputSeq> {
        let b = self.read()?;
        self.decode(b)
    }
}

impl Iterator for InputSequences {
    type Item = io::Result<InputSeq>;

    // Read next byte from stdin with timeout 100ms. If nothing was read, it returns InputSeq::Unidentified.
    // This method never returns None so for loop never ends
    fn next(&mut self) -> Option<Self::Item> {
        Some(self.read_seq())
    }
}

enum CursorDir {
    Left,
    Right,
    Up,
    Down,
}

struct Editor {
    // Editor state goes here
    // Cursor position
    cx: usize,
    cy: usize,
    // Screen size
    screen_rows: usize,
    screen_cols: usize,
}

impl Editor {
    fn new(size: Option<(usize, usize)>) -> Editor {
        let (screen_cols, screen_rows) = size.unwrap_or((0, 0));
        Editor {
            cx: 0,
            cy: 0,
            screen_cols,
            screen_rows,
        }
    }

    fn write_rows<W: Write>(&self, mut buf: W) -> io::Result<()> {
        for y in 0..self.screen_rows {
            if y == self.screen_rows / 3 {
                let msg_buf = format!("Kilo editor -- version {}", VERSION);
                let mut welcome = msg_buf.as_str();
                if welcome.len() > self.screen_cols {
                    welcome = &welcome[..self.screen_cols];
                }
                let padding = (self.screen_cols - welcome.len()) / 2;
                if padding > 0 {
                    buf.write(b"~")?;
                    for _ in 0..padding - 1 {
                        buf.write(b" ")?;
                    }
                }
                buf.write(welcome.as_bytes())?;
            } else {
                buf.write(b"~")?;
            }

            // Erases the part of the line to the right of the cursor. http://vt100.net/docs/vt100-ug/chapter3.html#EL
            buf.write(b"\x1b[K")?;

            // Avoid adding newline at the end of buffer
            if y < self.screen_rows - 1 {
                buf.write(b"\r\n")?;
            }
        }
        Ok(())
    }

    fn refresh_screen(&self) -> io::Result<()> {
        let mut buf = Vec::with_capacity((self.screen_rows + 1) * self.screen_cols);
        // \x1b[: Escape sequence header
        // Hide cursor while updating screen. 'l' is command to set mode http://vt100.net/docs/vt100-ug/chapter3.html#SM
        buf.write(b"\x1b[?25l")?;
        // H: Command to move cursor. Here \x1b[H is the same as \x1b[1;1H
        buf.write(b"\x1b[H")?;

        self.write_rows(&mut buf)?;

        // Move cursor
        write!(buf, "\x1b[{};{}H", self.cy + 1, self.cx + 1)?;

        // Reveal cursor again. 'h' is command to reset mode https://vt100.net/docs/vt100-ug/chapter3.html#RM
        buf.write(b"\x1b[?25h")?;

        let mut stdout = io::stdout();
        stdout.write(&buf)?;
        stdout.flush()
    }

    fn move_cursor(&mut self, dir: CursorDir) {
        match dir {
            CursorDir::Up => self.cy = self.cy.saturating_sub(1),
            CursorDir::Down => self.cy = self.cy.saturating_add(1),
            CursorDir::Left => self.cx = self.cx.saturating_sub(1),
            CursorDir::Right => self.cx = self.cx.saturating_add(1),
        }
    }

    fn process_sequence(&mut self, seq: InputSeq) -> io::Result<bool> {
        let mut exit = false;
        match seq {
            InputSeq::Key(b'w', false) | InputSeq::SpecialKey(SpecialKey::Up) => {
                self.move_cursor(CursorDir::Up)
            }
            InputSeq::Key(b'a', false) | InputSeq::SpecialKey(SpecialKey::Left) => {
                self.move_cursor(CursorDir::Left)
            }
            InputSeq::Key(b's', false) | InputSeq::SpecialKey(SpecialKey::Down) => {
                self.move_cursor(CursorDir::Down)
            }
            InputSeq::Key(b'd', false) | InputSeq::SpecialKey(SpecialKey::Right) => {
                self.move_cursor(CursorDir::Right)
            }
            InputSeq::Key(b'q', true) => exit = true,
            _ => {}
        }
        Ok(exit)
    }

    fn ensure_screen_size<I>(&mut self, mut input: I) -> io::Result<I>
    where
        I: Iterator<Item = io::Result<InputSeq>>,
    {
        if self.screen_cols > 0 && self.screen_rows > 0 {
            return Ok(input);
        }

        // By moving cursor at the bottom-right corner by 'B' and 'C' commands, get the size of
        // current screen. \x1b[9999;9999H is not available since it does not guarantee cursor
        // stops on the corner. Finaly command 'n' queries cursor position.
        let mut stdout = io::stdout();
        stdout.write(b"\x1b[9999C\x1b[9999B\x1b[6n")?;
        stdout.flush()?;

        // Wait for response from terminal discarding other sequences
        for seq in &mut input {
            if let InputSeq::Cursor(r, c) = seq? {
                self.screen_cols = c;
                self.screen_rows = r;
                break;
            }
        }

        Ok(input)
    }

    fn run<I>(&mut self, input: I) -> io::Result<()>
    where
        I: Iterator<Item = io::Result<InputSeq>>,
    {
        let input = self.ensure_screen_size(input)?;

        for seq in input {
            self.refresh_screen()?;
            if self.process_sequence(seq?)? {
                break;
            }
        }

        self.refresh_screen() // Finally refresh screen on exit
    }
}

fn main() -> io::Result<()> {
    Editor::new(term_size::dimensions_stdout()).run(StdinRawMode::new()?.input_keys())
}
