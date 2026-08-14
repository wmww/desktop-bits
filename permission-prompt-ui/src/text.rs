//! The only module in the workspace that may construct or write to a text widget.
//!
//! Everything here sets `use-markup` and `use-underline` off explicitly, so caller data can never
//! be interpreted as Pango markup or as a mnemonic. A workspace source lint (see
//! `tests/source_lint.rs`) fails if the markup/underline APIs appear anywhere else, and if any
//! crate outside this one names a GTK text widget at all. That lint is a backstop for the
//! encapsulation, not the mechanism: the mechanism is that `Escaped` is the only thing these
//! functions accept.

use gtk::prelude::*;

use crate::untrusted::Escaped;

/// How wide a monospace field asks to be before it wraps, in columns. This is the dialog's width
/// ceiling in practice: the caller-controlled viewports propagate their natural width, so the
/// dialog is as wide as its longest field wants and stops growing here. Columns rather than pixels
/// because the fields are monospace — a terminal's width is the shape this text is written for —
/// and because the horizontal scroll policy makes the viewport's own pixel ceiling a no-op.
const MONO_WRAP_CHARS: i32 = 80;

/// How wide the chip's one-line fields ask to be before they ellipsize. Narrow on purpose: the chip
/// sits in the corner of somebody's desktop, not in front of it.
const CHIP_WRAP_CHARS: i32 = 44;

/// A label that renders its text literally, and whose text the reader can select and copy.
///
/// Selection is why a field is one label rather than one label per line: a selection cannot span
/// two widgets, and a command the reader cannot copy in one go is a command they retype by hand.
/// Selectable labels are focusable, but nothing else here is — buttons set `can-focus` off — so
/// the only thing focus can reach is text.
pub fn label(text: &Escaped, classes: &[&str]) -> gtk::Label {
    let l = plain(text, classes);
    l.set_selectable(true);
    l
}

/// A label that cannot be selected: for text inside something clickable, where a drag has to stay
/// a click on the widget rather than becoming a text selection.
fn plain(text: &Escaped, classes: &[&str]) -> gtk::Label {
    let l = gtk::Label::new(None);
    l.set_use_markup(false);
    l.set_use_underline(false);
    l.set_selectable(false);
    l.set_xalign(0.0);
    for c in classes {
        l.add_css_class(c);
    }
    set(&l, text);
    l
}

/// Join rendered lines into one label's text. Safe because escaping leaves no newline in an
/// `Escaped`, so the joins are the only line breaks in the result.
pub fn join(lines: &[Escaped]) -> Escaped {
    let mut out = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            out.push(Escaped::literal("\n"));
        }
        out.push(line.clone());
    }
    Escaped::concat(out)
}

/// Replace a label's text. Same guarantees as [`label`].
pub fn set(l: &gtk::Label, text: &Escaped) {
    l.set_text(text.as_str());
    l.update_property(&[gtk::accessible::Property::Description(text.as_str())]);
}

/// A wrapping label, for prose.
pub fn wrapped(text: &Escaped, classes: &[&str]) -> gtk::Label {
    let l = label(text, classes);
    l.set_wrap(true);
    l.set_wrap_mode(gtk::pango::WrapMode::WordChar);
    l.set_max_width_chars(72);
    l
}

/// A monospace label for a shell command or `NAME=value` lines.
///
/// Wraps rather than running off to the right: a field that scrolls sideways can hide its tail,
/// and `max-width-chars` is what keeps a long one from making the whole dialog that wide before it
/// gets the chance to wrap — a short command still gets a narrow dialog. Nothing here ever
/// ellipsizes — dropping caller bytes silently is the one thing this field must not do.
pub fn mono_block(text: &Escaped, classes: &[&str]) -> gtk::Label {
    let l = label(text, &["pp-mono"]);
    for c in classes {
        l.add_css_class(c);
    }
    l.set_wrap(true);
    // Char, not WordChar: Pango counts `-` and `/` as word boundaries, so `--exclude` came out as
    // `--` at the end of one line and `exclude` at the start of the next, which reads like a
    // deliberate separator in an argv. Breaking at the column instead is what a terminal does with
    // a long command, and makes no such claim about where a token ends.
    l.set_wrap_mode(gtk::pango::WrapMode::Char);
    l.set_max_width_chars(MONO_WRAP_CHARS);
    // Pango hyphenates a mid-word break by default, which put a `-` on screen that was not in the
    // command (`--excl-` / `ude`). An attribute, not markup: it changes how the text is laid out
    // and cannot introduce any.
    let attrs = gtk::pango::AttrList::new();
    attrs.insert(gtk::pango::AttrInt::new_insert_hyphens(false));
    l.set_attributes(Some(&attrs));
    l.set_ellipsize(gtk::pango::EllipsizeMode::None);
    l
}

/// The shell both button constructors share: never focusable, never activatable by the toolkit's
/// own key handling — activation goes through the prompt's own state machine — and always carrying
/// an accessible label. The label is a required parameter rather than something a test checks
/// after the fact, since a widget test would need a display to run.
fn bare_button(label: &'static str, classes: &[&str]) -> gtk::Button {
    let b = gtk::Button::new();
    b.set_use_underline(false);
    b.set_can_focus(false);
    b.set_focus_on_click(false);
    b.update_property(&[gtk::accessible::Property::Label(label)]);
    for c in classes {
        b.add_css_class(c);
    }
    b
}

/// A button whose child is a literal label.
pub fn button(text: &'static str, classes: &[&str]) -> gtk::Button {
    let b = bare_button(text, classes);
    b.set_child(Some(&plain(&Escaped::literal(text), &["pp-button-label"])));
    b
}

/// A button whose face is drawn (see [`crate::icon`]) rather than written. It lives here beside
/// [`button`] rather than with the artwork because the safety setup the two share is this module's
/// to own — a drawing area is not a text widget, but the button around it is the same button.
pub fn icon_button(
    label: &'static str,
    classes: &[&str],
    draw: impl Fn(&gtk::DrawingArea, &gtk::cairo::Context, i32, i32) + 'static,
) -> gtk::Button {
    let b = bare_button(label, classes);
    let face = gtk::DrawingArea::new();
    face.set_content_width(crate::icon::SIZE);
    face.set_content_height(crate::icon::SIZE);
    // Centred rather than filling the button's content box, so the icon is drawn in a square and
    // the artwork's proportions are the ones it was written for.
    face.set_halign(gtk::Align::Center);
    face.set_valign(gtk::Align::Center);
    face.set_draw_func(draw);
    b.set_child(Some(&face));
    b
}

/// One line of caller text that gives up rather than growing: it ellipsizes at the end.
///
/// Only for the chip, and deliberately unlike [`mono_block`], which must never drop a byte because
/// approval happens in front of it. The chip approves nothing, and the whole argv is one click
/// away on the surface that does.
pub fn ellipsized(text: &Escaped, classes: &[&str]) -> gtk::Label {
    // Not selectable: the whole chip is one click target, and a drag across it must stay a click
    // rather than becoming a text selection.
    let l = plain(text, classes);
    l.set_wrap(false);
    l.set_single_line_mode(true);
    l.set_ellipsize(gtk::pango::EllipsizeMode::End);
    l.set_max_width_chars(CHIP_WRAP_CHARS);
    l
}

/// Ceiling on the response, in characters. A response is a sentence; the ceiling only has to stop
/// a paste from putting something unbounded into a log record and onto the caller's stderr. Kept
/// below [`crate::untrusted::MAX_TOKEN_CHARS`], so what is printed is never clipped as well.
const MAX_RESPONSE_CHARS: u16 = 1024;

/// The backing store of the response entry, shared by every output's copy of the dialog so the
/// text stays in sync across outputs and survives a window being rebuilt.
///
/// A newtype rather than the buffer itself, so the entry APIs stay inside this module — which is
/// what the source lint checks.
#[derive(Clone)]
pub struct ResponseBuffer(gtk::EntryBuffer);

impl ResponseBuffer {
    pub fn new() -> Self {
        let b = gtk::EntryBuffer::new(None::<&str>);
        b.set_max_length(Some(MAX_RESPONSE_CHARS));
        ResponseBuffer(b)
    }

    /// What the human typed, or `None` if they typed nothing. Not trimmed and not otherwise
    /// touched: it is their sentence, and "empty" means literally empty.
    pub fn text(&self) -> Option<String> {
        let text = self.0.text().to_string();
        (!text.is_empty()).then_some(text)
    }
}

impl Default for ResponseBuffer {
    fn default() -> Self {
        ResponseBuffer::new()
    }
}

/// The response box: one line the human may type into, sitting above the buttons.
///
/// The only text widget here whose content is neither compiled in nor escaped caller data — it is
/// the human's own, typed on the trusted surface. An entry renders its text literally, so there is
/// no markup to turn off; it is built here anyway because one audited module makes text widgets.
///
/// It cannot answer the prompt: the input state machine handles Enter and Escape in the capture
/// phase before the entry sees them, and `activates-default` is off besides.
pub fn entry(buffer: &ResponseBuffer, placeholder: &'static str) -> gtk::Widget {
    let e = gtk::Entry::with_buffer(&buffer.0);
    e.set_placeholder_text(Some(placeholder));
    e.set_activates_default(false);
    e.set_hexpand(true);
    e.update_property(&[gtk::accessible::Property::Label(placeholder)]);
    e.upcast()
}
