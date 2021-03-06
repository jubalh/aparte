use std::cell::RefCell;
use std::cmp;
use std::collections::{HashMap, hash_map::Entry, HashSet};
use std::fmt;
use std::hash::Hash;
use std::io::{Write, Stdout};
use std::rc::Rc;
use termion::raw::RawTerminal;
use termion::screen::AlternateScreen;

type Screen = AlternateScreen<RawTerminal<Stdout>>;

fn term_string_visible_len(string: &str) -> usize {
    let mut len = 0;
    let mut iter = string.chars();

    while let Some(c) = iter.next() {
        match c {
            '\x1B' => {
                if let Some(c) = iter.next() {
                    if c == '[' {
                        while let Some(c) = iter.next() {
                            match c {
                                '\x30'..='\x3f' => {}, // parameter bytes
                                '\x20'..='\x2f' => {}, // intermediate bytes
                                '\x40'..='\x7E' => break, // final byte
                                _ => break,
                            }
                        }
                    }
                }
            },
            _ => { len += 1; },
        }
    }

    len
}

#[derive(Clone)]
pub enum Dimension {
    MatchParent,
    #[allow(dead_code)]
    WrapContent,
    Absolute(u16),
}

pub trait ViewTrait<E> {
    fn measure(&mut self, width_spec: Option<u16>, height_spec: Option<u16>);
    fn layout(&mut self, top: u16, left: u16);
    fn is_dirty(&self) -> bool;
    fn get_measured_width(&self) -> Option<u16>;
    fn get_measured_height(&self) -> Option<u16>;
    fn redraw(&mut self);
    fn event(&mut self, event: &mut E);
}

pub struct View<'a, T, E> {
    pub screen: Rc<RefCell<Screen>>,
    pub width: Dimension,
    pub height: Dimension,
    pub x: u16,
    pub y: u16,
    pub w: Option<u16>,
    pub h: Option<u16>,
    pub dirty: bool,
    pub content: T,
    pub event_handler: Option<Rc<RefCell<Box<dyn FnMut(&mut Self, &mut E) + 'a>>>>,
    #[cfg(feature = "no-cursor-save")]
    pub cursor_x: Option<u16>,
    #[cfg(feature = "no-cursor-save")]
    pub cursor_y: Option<u16>,
}

macro_rules! vprint {
    ($view:expr, $fmt:expr) => {
        {
            let mut screen = $view.screen.borrow_mut();
            write!(screen, $fmt).unwrap();
        }
    };
    ($view:expr, $fmt:expr, $($arg:tt)*) => {
        {
            let mut screen = $view.screen.borrow_mut();
            write!(screen, $fmt, $($arg)*).unwrap();
        }
    };
}

macro_rules! goto {
    ($view:expr, $x:expr, $y:expr) => {
        vprint!($view, "{}", termion::cursor::Goto($x, $y));
    }
}

macro_rules! flush {
    ($view:expr) => {
        $view.screen.borrow_mut().flush().unwrap();
    }
}

impl<'a, T, E> View<'a, T, E> {
    #[cfg(not(feature = "no-cursor-save"))]
    pub fn save_cursor(&mut self) {
        vprint!(self, "{}", termion::cursor::Save);
    }

    #[cfg(feature = "no-cursor-save")]
    pub fn save_cursor(&mut self) {
        let mut screen = self.screen.borrow_mut();
        let (x, y) = screen.cursor_pos().unwrap();
        self.cursor_x = Some(x);
        self.cursor_y = Some(y);
    }

    #[cfg(not(feature = "no-cursor-save"))]
    pub fn restore_cursor(&mut self) {
        vprint!(self, "{}", termion::cursor::Restore);
    }

    #[cfg(feature = "no-cursor-save")]
    pub fn restore_cursor(&mut self) {
        goto!(self, self.cursor_x.unwrap(), self.cursor_y.unwrap());
    }

}

default impl<'a, T, E> ViewTrait<E> for View<'a, T, E> {
    fn measure(&mut self, width_spec: Option<u16>, height_spec: Option<u16>) {
        self.w = match self.width {
            Dimension::MatchParent => width_spec,
            Dimension::WrapContent => unreachable!(),
            Dimension::Absolute(width) => {
                match width_spec {
                    Some(width_spec) => Some(cmp::min(width, width_spec)),
                    None => Some(width),
                }
            }
        };

        self.h = match self.height {
            Dimension::MatchParent => height_spec,
            Dimension::WrapContent => unreachable!(),
            Dimension::Absolute(height) => {
                match height_spec {
                    Some(height_spec) => Some(cmp::min(height, height_spec)),
                    None => Some(height),
                }
            },
        };
    }

    fn layout(&mut self, top: u16, left: u16) {
        self.x = left;
        self.y = top;
        self.dirty = false;
    }

    fn get_measured_width(&self) -> Option<u16> {
        self.w
    }

    fn get_measured_height(&self) -> Option<u16> {
        self.h
    }

    fn is_dirty(&self) -> bool {
        self.dirty
    }

    fn event(&mut self, event: &mut E) {
        if let Some(handler) = &self.event_handler {
            let handler = Rc::clone(handler);
            let handler = &mut *handler.borrow_mut();
            handler(self, event);
        }
    }
}

pub struct FrameLayout<'a, K, E>
    where K: Hash + Eq
{
    pub children: HashMap<K, Box<dyn ViewTrait<E> + 'a>>,
    pub current: Option<K>,
}

impl<'a, K, E> View<'a, FrameLayout<'a, K, E>, E>
    where K: Hash + Eq
{
    pub fn new(screen: Rc<RefCell<Screen>>) -> Self {
        Self {
            screen: screen,
            width: Dimension::MatchParent,
            height: Dimension::MatchParent,
            x: 1,
            y: 1,
            w: None,
            h: None,
            dirty: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: FrameLayout {
                children: HashMap::new(),
                current: None,
            },
            event_handler: None,
        }
    }

    pub fn with_event<F>(mut self, event_handler: F) -> Self
        where F: FnMut(&mut Self, &mut E), F: 'a
    {
        self.event_handler = Some(Rc::new(RefCell::new(Box::new(event_handler))));
        self
    }

    pub fn current(&mut self, key: K) {
        self.content.current = Some(key);
        self.redraw();
    }

    pub fn insert(&mut self, key: K, mut widget: Box<dyn ViewTrait<E> + 'a>)
    {
        widget.measure(self.w, self.h);
        widget.layout(self.y, self.x);
        self.content.children.insert(key, widget);
    }
}

impl<K, E> ViewTrait<E> for View<'_, FrameLayout<'_, K, E>, E>
    where K: Hash + Eq
{
    fn measure(&mut self, width_spec: Option<u16>, height_spec: Option<u16>) {
        self.w = width_spec;
        self.h = height_spec;

        for (_, child) in self.content.children.iter_mut() {
            child.measure(self.w, self.h);
        }
    }

    fn layout(&mut self, top: u16, left: u16) {
        self.x = left;
        self.y = top;
        self.dirty = false;

        for (_, child) in self.content.children.iter_mut() {
            child.layout(top, left);
        }
    }

    fn redraw(&mut self) {
        if let Some(current) = &self.content.current {
            let child = self.content.children.get_mut(current).unwrap();
            child.redraw();
        }
    }

    fn is_dirty(&self) -> bool {
        let mut dirty = false;
        for (_, child) in self.content.children.iter() {
            dirty |= child.is_dirty()
        }
        dirty
    }
}

#[derive(Clone, PartialEq)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

pub struct LinearLayout<'a, E> {
    pub orientation: Orientation,
    pub children: Vec<Box<dyn ViewTrait<E> + 'a>>,
}

impl<'a, E> View<'a, LinearLayout<'a, E>, E> {
    pub fn new(screen: Rc<RefCell<Screen>>, orientation: Orientation, width: Dimension, height: Dimension) -> Self {
        Self {
            screen: screen,
            width: width,
            height: height,
            x: 0,
            y: 0,
            w: None,
            h: None,
            dirty: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: LinearLayout {
                orientation: orientation,
                children: Vec::new(),
            },
            event_handler: None,
        }
    }

    pub fn push<T>(&mut self, widget: T)
        where T: ViewTrait<E>, T: 'a
    {
        self.content.children.push(Box::new(widget));
    }

    pub fn with_event<F>(mut self, event_handler: F) -> Self
        where F: FnMut(&mut Self, &mut E), F: 'a
    {
        self.event_handler = Some(Rc::new(RefCell::new(Box::new(event_handler))));
        self
    }
}

impl<E> ViewTrait<E> for View<'_, LinearLayout<'_, E>, E> {
    fn measure(&mut self, width_spec: Option<u16>, height_spec: Option<u16>) {
        /* Measure dimension of this layout with the following stpes:
         *
         *  - Compute max dimension from parent
         *  - Compute min dimension from children
         *  - Split remaining space for each child that don't have strong size requirement
         *    (answered 0 to first measure pass)
         *  - Set dimension for each children
         */
        let max_width = match self.width {
            Dimension::MatchParent => width_spec,
            Dimension::WrapContent => width_spec,
            Dimension::Absolute(width) => {
                match width_spec {
                    Some(width_spec) => Some(cmp::min(width, width_spec)),
                    None => Some(width),
                }
            },
        };

        let max_height = match self.height {
            Dimension::MatchParent => height_spec,
            Dimension::WrapContent => height_spec,
            Dimension::Absolute(height) => {
                match height_spec {
                    Some(height_spec) => Some(cmp::min(height, height_spec)),
                    None => Some(height),
                }
            },
        };

        let mut min_width = 0;
        let mut min_height = 0;
        for child in self.content.children.iter_mut() {
            child.measure(None, None);
            match self.content.orientation {
                Orientation::Vertical => {
                    min_width = cmp::max(min_width, child.get_measured_width().unwrap_or(0));
                    min_height += child.get_measured_height().unwrap_or(0);
                },
                Orientation::Horizontal => {
                    min_width += child.get_measured_width().unwrap_or(0);
                    min_height = cmp::max(min_height, child.get_measured_height().unwrap_or(0));
                },
            }
        }

        let remaining_width = match max_width {
            Some(max_width) => max_width - min_width,
            None => 0,
        };

        let remaining_height = match max_height {
            Some(max_height) => max_height - min_height,
            None => 0,
        };

        // Split remaining space to children that don't know their size
        let splitted_width = match self.content.orientation {
            Orientation::Vertical => max_width,
            Orientation::Horizontal => {
                let unsized_children = self.content.children.iter().filter(|child| child.get_measured_width().is_none());
                Some(match unsized_children.collect::<Vec<_>>().len() {
                    0 => 0,
                    count => remaining_width / count as u16,
                })
            },
        };
        let splitted_height = match self.content.orientation {
            Orientation::Vertical => {
                let unsized_children = self.content.children.iter().filter(|child| child.get_measured_height().is_none());
                Some(match unsized_children.collect::<Vec<_>>().len() {
                    0 => 0,
                    count => remaining_height / count as u16,
                })
            },
            Orientation::Horizontal => max_height,
        };

        self.w = Some(0);
        self.h = Some(0);

        for child in self.content.children.iter_mut() {
            let mut width_spec = match child.get_measured_width() {
                Some(w) => Some(w),
                None => splitted_width,
            };

            let mut height_spec = match child.get_measured_height() {
                Some(h) => Some(h),
                None => splitted_height,
            };

            if self.content.orientation == Orientation::Horizontal && max_width.is_some() {
               width_spec = Some(cmp::min(width_spec.unwrap(), max_width.unwrap() - self.w.unwrap()));
            }

            if self.content.orientation == Orientation::Vertical && max_height.is_some() {
                height_spec = Some(cmp::min(height_spec.unwrap(), max_height.unwrap() - self.h.unwrap()));
            }

            child.measure(width_spec, height_spec);

            match self.content.orientation {
                Orientation::Vertical => {
                    self.w = Some(cmp::max(self.w.unwrap(), child.get_measured_width().unwrap_or(0)));
                    self.h = Some(self.h.unwrap() + child.get_measured_height().unwrap_or(0));
                },
                Orientation::Horizontal => {
                    self.w = Some(self.w.unwrap() + child.get_measured_width().unwrap_or(0));
                    self.h = Some(cmp::max(self.h.unwrap(), child.get_measured_height().unwrap_or(0)));
                },
            }
        }
    }

    fn layout(&mut self, top: u16, left: u16) {
        self.x = left;
        self.y = top;
        self.dirty = false;

        let mut x = self.x;
        let mut y = self.y;

        for child in self.content.children.iter_mut() {
            child.layout(y, x);
            match self.content.orientation {
                Orientation::Vertical => y += child.get_measured_height().unwrap(),
                Orientation::Horizontal => x += child.get_measured_width().unwrap(),
            }
        }
    }

    fn redraw(&mut self) {
        for child in self.content.children.iter_mut() {
            child.redraw();
        }
    }

    fn is_dirty(&self) -> bool {
        let mut dirty = false;
        for child in self.content.children.iter() {
            dirty |= child.is_dirty()
        }
        dirty
    }
}

pub struct Input {
    pub buf: String,
    pub tmp_buf: Option<String>,
    pub password: bool,
    pub history: Vec<String>,
    pub history_index: usize,
    // Used to index code points in buf (don't use it to directly index buf)
    pub cursor: usize,
}

impl Input {
    pub fn byte_index(&self, mut cursor: usize) -> usize {
        let mut byte_index = 0;
        while cursor > 0 {
            byte_index += 1;
            if self.buf.is_char_boundary(byte_index) {
                cursor -= 1;
            }
        }
        byte_index
    }
}

impl<'a, E> View<'a, Input, E> {
    pub fn new(screen: Rc<RefCell<Screen>>) -> Self {
        Self {
            screen: screen,
            width: Dimension::MatchParent,
            height: Dimension::Absolute(1),
            x: 0,
            y: 0,
            w: None,
            h: None,
            dirty: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: Input {
                buf: String::new(),
                tmp_buf: None,
                password: false,
                history: Vec::new(),
                history_index: 0,
                cursor: 0,
            },
            event_handler: None,
        }
    }

    pub fn with_event<F>(mut self, event_handler: F) -> Self
        where F: FnMut(&mut Self, &mut E), F: 'a
    {
        self.event_handler = Some(Rc::new(RefCell::new(Box::new(event_handler))));
        self
    }

    pub fn key(&mut self, c: char) {
        let byte_index = self.content.byte_index(self.content.cursor);
        self.content.buf.insert(byte_index, c);
        self.content.cursor += 1;
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn backspace(&mut self) {
        if self.content.cursor > 0 {
            let byte_index = self.content.byte_index(self.content.cursor - 1);
            self.content.buf.remove(byte_index);
            while !self.content.buf.is_char_boundary(byte_index) {
                self.content.buf.remove(byte_index);
            }
            self.content.cursor -= 1;
        }
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn backward_delete_word(&mut self) {
        enum WordParserState {
            Init,
            Space,
            Separator,
            Word,
        };

        use WordParserState::*;

        let mut iter = self.content.buf[..self.content.byte_index(self.content.cursor)].chars().rev();
        let mut state = Init;
        let mut word_start = self.content.cursor;

        while let Some(c) = iter.next() {
            state = match state {
                Init => {
                    match c {
                        ' ' => Space,
                        '/' | '\\' | '\'' | '"' | '&' | '(' | ')' | '*' | ',' | ';' | '<' | '='
                            | '>' | '?' | '@' | '[' | ']' | '^' | '{' | '|' | '}' => Separator,
                        _ => Word,
                    }
                },
                Space => {
                    match c {
                        ' ' => Space,
                        '/' | '\\' | '\'' | '"' | '&' | '(' | ')' | '*' | ',' | ';' | '<' | '='
                            | '>' | '?' | '@' | '[' | ']' | '^' | '{' | '|' | '}' => Separator,
                        _ => Word,
                    }
                }
                Separator => {
                    match c {
                        '/' | '\\' | '\'' | '"' | '&' | '(' | ')' | '*' | ',' | ';' | '<' | '='
                            | '>' | '?' | '@' | '[' | ']' | '^' | '{' | '|' | '}' => Separator,
                        _ => break,
                    }
                }
                Word => {
                    match c {
                        ' ' | '/' | '\\' | '\'' | '"' | '&' | '(' | ')' | '*' | ',' | ';' | '<'
                            | '=' | '>' | '?' | '@' | '[' | ']' | '^' | '{' | '|' | '}' => break,
                        _ => Word,
                    }
                }
            };

            word_start -= 1;
        }
        self.content.buf.replace_range(self.content.byte_index(word_start)..self.content.byte_index(self.content.cursor), "");
        self.content.cursor = word_start;
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn delete(&mut self) {
        if self.content.cursor < self.content.buf.len() {
            self.content.buf.remove(self.content.cursor);
        }
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn home(&mut self) {
        self.content.cursor = 0;
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn end(&mut self) {
        self.content.cursor = self.content.buf.len();
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn clear(&mut self) {
        self.content.buf.clear();
        self.content.cursor = 0;
        let _ = self.content.tmp_buf.take();
        self.content.password = false;
        goto!(self, self.x, self.y);
        for _ in 0 .. self.w.unwrap() {
            vprint!(self, " ");
        }
        goto!(self, self.x, self.y);
        flush!(self);
    }

    pub fn left(&mut self) {
        if self.content.cursor > 0 {
            self.content.cursor -= 1;
        }
        if !self.content.password {
            self.redraw();
        }
    }

    pub fn right(&mut self) {
        if self.content.cursor < self.content.buf.len() {
            self.content.cursor += 1;
        }
        if !self.content.password {
            self.redraw()
        }
    }

    pub fn password(&mut self) {
        self.clear();
        self.content.password = true;
        vprint!(self, "password: ");
        flush!(self);
    }

    pub fn validate(&mut self) -> (String, bool) {
        if !self.content.password {
            self.content.history.push(self.content.buf.clone());
            self.content.history_index = self.content.history.len();
        }
        let buf = self.content.buf.clone();
        let password = self.content.password;
        self.clear();
        (buf, password)
    }

    pub fn previous(&mut self) {
        if self.content.history_index == 0 {
            return;
        }

        if self.content.tmp_buf.is_none() {
            self.content.tmp_buf = Some(self.content.buf.clone());
        }

        self.content.history_index -= 1;
        self.content.buf = self.content.history[self.content.history_index].clone();
        self.content.cursor = self.content.buf.len();
        self.redraw();
    }

    pub fn next(&mut self) {
        if self.content.history_index == self.content.history.len() {
            return;
        }

        self.content.history_index += 1;
        if self.content.history_index == self.content.history.len() {
            self.content.buf = self.content.tmp_buf.take().unwrap();
        } else {
            self.content.buf = self.content.history[self.content.history_index].clone();
        }
        self.content.cursor = self.content.buf.len();

        self.redraw();
    }
}

impl<E> ViewTrait<E> for View<'_, Input, E> {
    fn redraw(&mut self) {
        goto!(self, self.x, self.y);
        for _ in 0 .. self.w.unwrap() {
            vprint!(self, " ");
        }

        goto!(self, self.x, self.y);
        vprint!(self, "{}", self.content.buf);
        goto!(self, self.x + self.content.cursor as u16, self.y);

        flush!(self);
    }
}

pub trait BufferedMessage = fmt::Display + Hash + std::cmp::Eq + std::clone::Clone;

pub trait Window<T: BufferedMessage, E>: ViewTrait<E> {
    fn recv_message(&mut self, message: &T, print: bool);
    fn send_message(&self);
    fn page_up(&mut self);
    fn page_down(&mut self);
}

pub struct BufferedWin<T: BufferedMessage> {
    pub next_line: u16,
    pub buf: Vec<T>,
    pub history: HashMap<T, usize>,
    pub view: usize,
}

impl<'a, T: BufferedMessage, E> View<'a, BufferedWin<T>, E> {
    pub fn new(screen: Rc<RefCell<Screen>>) -> Self {
        Self {
            screen: screen,
            width: Dimension::MatchParent,
            height: Dimension::MatchParent,
            x: 0,
            y: 0,
            w: None,
            h: None,
            dirty: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: BufferedWin {
                next_line: 0,
                buf: Vec::new(),
                history: HashMap::new(),
                view: 0,
            },
            event_handler: None,
        }
    }

    pub fn with_event<F>(mut self, event_handler: F) -> Self
        where F: FnMut(&mut Self, &mut E), F: 'a
    {
        self.event_handler = Some(Rc::new(RefCell::new(Box::new(event_handler))));
        self
    }
}

impl<T: BufferedMessage, E> Window<T, E> for View<'_, BufferedWin<T>, E> {
    fn recv_message(&mut self, message: &T, print: bool) {
        if self.content.history.contains_key(message) {
            return;
        }

        self.content.history.insert(message.clone(), self.content.buf.len());
        self.content.buf.push(message.clone());

        if print {
            self.redraw();
        }
    }

    fn page_up(&mut self) {
        let buffers = self.content.buf.iter().flat_map(|m| format!("{}", m).lines().map(str::to_owned).collect::<Vec<_>>());
        let count = buffers.collect::<Vec<_>>().len();

        if count < self.h.unwrap() as usize {
            return;
        }

        let max = count - self.h.unwrap() as usize;

        if self.content.view + (self.h.unwrap() as usize) < max {
            self.content.view += self.h.unwrap() as usize;
        } else {
            self.content.view = max;
        }

        self.redraw();
    }

    fn page_down(&mut self) {
        if self.content.view > self.h.unwrap() as usize {
            self.content.view -= self.h.unwrap() as usize;
        } else {
            self.content.view = 0;
        }
        self.redraw();
    }

    fn send_message(&self) {
    }
}

impl<T: BufferedMessage, E> ViewTrait<E> for View<'_, BufferedWin<T>, E> {
    fn redraw(&mut self) {
        self.save_cursor();

        self.content.next_line = 0;
        let buffers = self.content.buf.iter().flat_map(|m| format!("{}", m).lines().map(str::to_owned).collect::<Vec<_>>());
        let count = buffers.collect::<Vec<_>>().len();

        let mut buffers = self.content.buf.iter().flat_map(|m| format!("{}", m).lines().map(str::to_owned).collect::<Vec<_>>());

        if count > self.h.unwrap() as usize {
            for _ in 0 .. count - self.h.unwrap() as usize - self.content.view {
                if buffers.next().is_none() {
                    break;
                }
            }
        }

        for y in self.y .. self.y + self.h.unwrap() {
            goto!(self, self.x, y);
            for _ in self.x  .. self.x + self.w.unwrap() {
                vprint!(self, " ");
            }

            goto!(self, self.x, y);
            if let Some(buf) = buffers.next() {
                vprint!(self, "{}", buf);
                self.content.next_line += 1;
            }
        }

        self.restore_cursor();
        flush!(self);
    }
}

pub struct ListView<G, V>
    where G: fmt::Display + Hash + std::cmp::Eq, V: fmt::Display + Hash + std::cmp::Eq
{
    items: HashMap<Option<G>, HashSet<V>>,
}

impl<'a, G: fmt::Display + Hash + std::cmp::Eq, V: fmt::Display + Hash + std::cmp::Eq, E> View<'a, ListView<G, V>, E> {
    pub fn new(screen: Rc<RefCell<Screen>>) -> Self {
        Self {
            screen: screen,
            width: Dimension::WrapContent,
            height: Dimension::MatchParent,
            x: 0,
            y: 0,
            w: None,
            h: None,
            dirty: true,
            #[cfg(feature = "no-cursor-save")]
            cursor_x: None,
            #[cfg(feature = "no-cursor-save")]
            cursor_y: None,
            content: ListView {
                items: HashMap::new(),
            },
            event_handler: None,
        }
    }

    pub fn with_event<F>(mut self, event_handler: F) -> Self
        where F: FnMut(&mut Self, &mut E), F: 'a
    {
        self.event_handler = Some(Rc::new(RefCell::new(Box::new(event_handler))));
        self
    }

    pub fn with_none_group(mut self) -> Self {
        if let Entry::Vacant(vacant) = self.content.items.entry(None) {
            vacant.insert(HashSet::new());
        }
        self
    }

    pub fn add_group(&mut self, group: G) {
        if let Entry::Vacant(vacant) = self.content.items.entry(Some(group)) {
            vacant.insert(HashSet::new());
        }
    }

    pub fn insert(&mut self, item: V, group: Option<G>) {
        match self.content.items.entry(group) {
            Entry::Vacant(vacant) => {
                let mut items = HashSet::new();
                items.insert(item);
                vacant.insert(items);
            },
            Entry::Occupied(mut occupied) => {
                occupied.get_mut().replace(item);
            }
        }
        self.dirty = true
    }
}

impl<G: fmt::Display + Hash + std::cmp::Eq, V: fmt::Display + Hash + std::cmp::Eq, E> ViewTrait<E> for View<'_, ListView<G, V>, E> {
    fn measure(&mut self, width_spec: Option<u16>, height_spec: Option<u16>) {
        self.w = match self.width {
            Dimension::MatchParent => width_spec,
            Dimension::WrapContent => {
                let mut width: u16 = 0;
                for (group, items) in &self.content.items {
                    if let Some(group) = group {
                        width = cmp::max(width, term_string_visible_len(&format!("{}", group)) as u16);
                    }

                    let indent = match group {
                        Some(_) => "  ",
                        None => "",
                    };

                    for item in items {
                        width = cmp::max(width, term_string_visible_len(&format!("{}{}", indent, item)) as u16);
                    }
                }
                match width_spec {
                    Some(width_spec) => Some(cmp::min(width, width_spec)),
                    None => Some(width),
                }
            },
            Dimension::Absolute(width) => {
                match width_spec {
                    Some(width_spec) => Some(cmp::min(width, width_spec)),
                    None => Some(width),
                }
            }
        };

        self.h = match self.height {
            Dimension::MatchParent => height_spec,
            Dimension::WrapContent => {
                let mut height: u16 = 0;
                for (group, items) in &self.content.items {
                    if group.is_some() {
                        height += 1;
                    }

                    height += items.len() as u16;
                }
                match height_spec {
                    Some(height_spec) => Some(cmp::min(height, height_spec)),
                    None => Some(height),
                }
            },
            Dimension::Absolute(height) => {
                match height_spec {
                    Some(height_spec) => Some(cmp::min(height, height_spec)),
                    None => Some(height),
                }
            },
        };
    }

    fn redraw(&mut self) {
        self.save_cursor();

        let mut y = self.y;

        for y in self.y .. self.y + self.h.unwrap() {
            goto!(self, self.x, y);
            for _ in self.x  .. self.x + self.w.unwrap() {
                vprint!(self, " ");
            }

            goto!(self, self.x, y);
        }

        for (group, items) in &self.content.items {
            goto!(self, self.x, y);
            if group.is_some() {
                vprint!(self, "{}", group.as_ref().unwrap());
                y += 1;
            }

            for item in items {
                goto!(self, self.x, y);
                match group {
                    Some(_) => vprint!(self, "  {}", item),
                    None => vprint!(self, "{}", item),
                };

                y += 1;
            }
        }

        self.restore_cursor();
        flush!(self);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_term_string_visible_len_is_correct() {
        assert_eq!(term_string_visible_len(&format!("{}ab{}", termion::color::Bg(termion::color::Red), termion::cursor::Goto(1, 123))), 2);
        assert_eq!(term_string_visible_len(&format!("{}ab{}", termion::cursor::Goto(1, 123), termion::color::Bg(termion::color::Red))), 2);
    }

    #[test]
    fn test_input_byte_index_for_cursor() {
        let input = Input {
            buf: "aça".to_string(),
            tmp_buf: None,
            password: true,
            history: Vec::new(),
            history_index: 0,
            cursor: 1,
        };

        assert_eq!(input.buf.len(), 4);
        assert_eq!(input.byte_index(0), 0);
        assert_eq!(input.byte_index(1), 1);
        assert_eq!(input.byte_index(2), 3);
    }
}
