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

/// How wide a monospace field asks to be before it wraps, in characters.
const MONO_WRAP_CHARS: i32 = 56;

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
/// gets the chance to wrap. Nothing here ever ellipsizes — dropping caller bytes silently is the
/// one thing this field must not do.
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

/// A button whose child is a literal label and which can never be focused or activated by the
/// toolkit's own key handling. Activation goes through the prompt's own state machine.
pub fn button(text: &'static str, classes: &[&str]) -> gtk::Button {
    let b = gtk::Button::new();
    b.set_use_underline(false);
    b.set_can_focus(false);
    b.set_focus_on_click(false);
    b.set_child(Some(&plain(&Escaped::literal(text), &["pp-button-label"])));
    b.update_property(&[gtk::accessible::Property::Label(text)]);
    for c in classes {
        b.add_css_class(c);
    }
    b
}
