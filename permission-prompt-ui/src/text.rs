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

/// A label that renders its text literally.
pub fn label(text: &Escaped, classes: &[&str]) -> gtk::Label {
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

/// A monospace, non-wrapping label for one rendered token or `NAME=value` line.
pub fn mono_line(text: &Escaped) -> gtk::Label {
    let l = label(text, &["pp-mono"]);
    l.set_wrap(false);
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
    b.set_child(Some(&label(&Escaped::literal(text), &["pp-button-label"])));
    b.update_property(&[gtk::accessible::Property::Label(text)]);
    for c in classes {
        b.add_css_class(c);
    }
    b
}
