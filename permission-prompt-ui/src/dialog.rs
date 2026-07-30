//! Dialog layout. Trusted fields and buttons sit outside every viewport and keep their natural
//! size; caller-controlled fields live in bounded scrolling viewports and are what shrinks.

use gtk::prelude::*;

use crate::text;
use crate::untrusted::Escaped;

/// Tallest a non-expanding caller-controlled viewport gets before it scrolls.
const MAX_VIEWPORT_HEIGHT: i32 = 120;

/// Hard ceiling on rendered lines in one caller-controlled field.
pub const MAX_FIELD_LINES: usize = 512;
/// Hard ceiling on rendered characters in one caller-controlled field.
pub const MAX_FIELD_CHARS: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// The sudo gate: fixed security presentation.
    Gate,
    /// The generic presenter. Deliberately distinct, and not claiming to be a root prompt.
    Generic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    /// Compiled-in prose or gate-computed values. Never scrolls, never moves.
    Trusted,
    /// Caller data. Bounded scrolling viewport, monospace, overflow marked.
    Untrusted,
}

pub struct Field {
    pub label: &'static str,
    pub lines: Vec<Escaped>,
    pub kind: FieldKind,
    /// Extra trusted notes rendered under the field.
    pub notes: Vec<Escaped>,
    /// This field reports no natural height and takes whatever vertical space is left over.
    /// Mark the field the reader most needs room for. The others size to their content, so the
    /// dialog's natural height stays small enough that a cramped output shrinks the viewports
    /// rather than pushing the buttons off screen.
    pub expand: bool,
}

impl Field {
    pub fn trusted(label: &'static str, lines: Vec<Escaped>) -> Self {
        Field { label, lines, kind: FieldKind::Trusted, notes: Vec::new(), expand: false }
    }

    pub fn untrusted(label: &'static str, lines: Vec<Escaped>) -> Self {
        Field { label, lines, kind: FieldKind::Untrusted, notes: Vec::new(), expand: false }
    }

    /// Give this field the leftover vertical space. See [`Field::expand`].
    pub fn expanding(mut self) -> Self {
        self.expand = true;
        self
    }

    pub fn with_note(mut self, note: Escaped) -> Self {
        self.notes.push(note);
        self
    }
}

pub struct DialogSpec {
    pub style: Style,
    pub heading: &'static str,
    /// Icon name to show beside the heading. A name, not text: it cannot render caller prose.
    pub icon: Option<String>,
    pub fields: Vec<Field>,
    pub approve: &'static str,
    pub deny: &'static str,
    /// Prose describing how to answer. Swapped for the "wait" message while settling.
    pub footer: &'static str,
}

/// One built dialog instance. Every output gets its own.
pub struct Dialog {
    pub root: gtk::Widget,
    pub approve: gtk::Button,
    pub deny: gtk::Button,
    footer: gtk::Label,
    footer_text: &'static str,
}

const SETTLING_FOOTER: &str = "Reading… controls unlock in a moment.";

impl Dialog {
    /// Visibly disable the controls while the prompt is settling.
    pub fn set_settled(&self, settled: bool) {
        self.approve.set_sensitive(settled);
        self.deny.set_sensitive(settled);
        text::set(
            &self.footer,
            &if settled {
                Escaped::literal(self.footer_text)
            } else {
                Escaped::literal(SETTLING_FOOTER)
            },
        );
    }
}

pub fn build(spec: &DialogSpec) -> Dialog {
    let dialog = gtk::Box::new(gtk::Orientation::Vertical, 8);
    dialog.add_css_class("pp-dialog");
    dialog.add_css_class(match spec.style {
        Style::Gate => "pp-gate",
        Style::Generic => "pp-generic",
    });
    dialog.set_halign(gtk::Align::Center);
    // Fill vertically: the expanding field takes the slack, so the dialog stays as tall as its
    // output and the trusted fields and buttons keep their natural size.
    dialog.set_valign(gtk::Align::Fill);

    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.add_css_class("pp-header");
    if let Some(name) = &spec.icon {
        let image = gtk::Image::from_icon_name(name);
        image.set_icon_size(gtk::IconSize::Large);
        header.append(&image);
    }
    header.append(&text::wrapped(&Escaped::literal(spec.heading), &["pp-heading"]));
    dialog.append(&header);

    for field in &spec.fields {
        dialog.append(&build_field(field));
    }

    // Footer and buttons share a row for the same reason as the marker above.
    let footer = text::wrapped(&Escaped::literal(SETTLING_FOOTER), &["pp-footer"]);
    footer.set_hexpand(true);
    footer.set_valign(gtk::Align::Center);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    buttons.add_css_class("pp-buttons");
    buttons.set_halign(gtk::Align::End);
    let deny = text::button(spec.deny, &["pp-deny"]);
    let approve = text::button(spec.approve, &["pp-approve"]);
    buttons.append(&deny);
    buttons.append(&approve);

    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bottom.append(&footer);
    bottom.append(&buttons);
    dialog.append(&bottom);

    let d = Dialog {
        root: dialog.upcast(),
        approve,
        deny,
        footer,
        footer_text: spec.footer,
    };
    d.set_settled(false);
    d
}

fn build_field(field: &Field) -> gtk::Widget {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 2);
    outer.add_css_class("pp-field");

    // The overflow marker shares the label's row rather than taking one of its own: on a small
    // output every line of fixed chrome is a line the buttons might not get.
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    header.append(&text::label(&Escaped::literal(field.label), &["pp-field-label"]));
    let marker = text::label(&Escaped::literal(""), &["pp-overflow"]);
    marker.set_hexpand(true);
    marker.set_xalign(1.0);
    marker.set_visible(false);
    header.append(&marker);
    outer.append(&header);

    let (lines, capped) = cap(&field.lines);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("pp-field-content");

    let mut escaped_any = false;
    let mut truncated_any = capped;
    for line in &lines {
        escaped_any |= line.was_escaped();
        truncated_any |= line.was_truncated();
        match field.kind {
            FieldKind::Trusted => content.append(&text::wrapped(line, &["pp-trusted-value"])),
            FieldKind::Untrusted => content.append(&text::mono_line(line)),
        }
    }
    if lines.is_empty() {
        content.append(&text::label(&Escaped::literal("(none)"), &["pp-empty"]));
    }

    match field.kind {
        FieldKind::Trusted => {
            outer.append(&content);
        }
        FieldKind::Untrusted => {
            let scroller = gtk::ScrolledWindow::new();
            scroller.add_css_class("pp-viewport");
            scroller.set_child(Some(&content));
            scroller.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
            // A permanently visible scrollbar: "there is more to read" must not be something
            // the human has to discover.
            scroller.set_overlay_scrolling(false);
            // The viewports are what shrinks when the dialog will not fit its output.
            scroller.set_min_content_height(0);
            if field.expand {
                scroller.set_propagate_natural_height(false);
                scroller.set_vexpand(true);
                outer.set_vexpand(true);
            } else {
                scroller.set_propagate_natural_height(true);
                scroller.set_max_content_height(MAX_VIEWPORT_HEIGHT);
            }
            outer.append(&scroller);
            wire_overflow_marker(&scroller, &marker, lines.len());
        }
    }

    if escaped_any {
        outer.append(&text::wrapped(
            &Escaped::literal(
                "Unsafe characters were escaped for display as \\xNN or \\u{NNNN}.",
            ),
            &["pp-note", "pp-warn"],
        ));
    }
    if truncated_any {
        outer.append(&text::wrapped(
            &Escaped::literal("TRUNCATED — this field does not show everything. See the log."),
            &["pp-note", "pp-warn"],
        ));
    }
    for note in &field.notes {
        outer.append(&text::wrapped(note, &["pp-note"]));
    }

    outer.upcast()
}

/// Apply the per-field ceilings. Returns the lines to render and whether anything was dropped.
fn cap(lines: &[Escaped]) -> (Vec<Escaped>, bool) {
    let mut out = Vec::new();
    let mut chars = 0usize;
    for line in lines {
        if out.len() >= MAX_FIELD_LINES || chars + line.as_str().chars().count() > MAX_FIELD_CHARS {
            return (out, true);
        }
        chars += line.as_str().chars().count();
        out.push(line.clone());
    }
    (out, false)
}

fn wire_overflow_marker(scroller: &gtk::ScrolledWindow, marker: &gtk::Label, total_lines: usize) {
    let adj = scroller.vadjustment();
    let marker = marker.clone();
    let update = move |adj: &gtk::Adjustment| {
        let upper = adj.upper();
        let page = adj.page_size();
        if total_lines == 0 || upper <= page + 0.5 {
            marker.set_visible(false);
            return;
        }
        let line_height = upper / total_lines as f64;
        let visible = if line_height > 0.0 { (page / line_height).floor() as usize } else { 0 };
        let hidden = total_lines.saturating_sub(visible);
        if hidden == 0 {
            marker.set_visible(false);
            return;
        }
        text::set(
            &marker,
            &Escaped::concat([
                Escaped::literal("▾ "),
                Escaped::number(hidden as u64),
                Escaped::literal(" more line(s) — scroll"),
            ]),
        );
        marker.set_visible(true);
    };
    adj.connect_changed({
        let update = update.clone();
        move |a| update(a)
    });
    adj.connect_value_changed(move |a| update(a));
}

pub const CSS: &str = "
window.pp-window { background-color: #0a0b0f; }
.pp-backdrop { background-color: #0a0b0f; }
.pp-dialog {
  background-color: #16181f;
  border-radius: 12px;
  padding: 14px;
  border: 2px solid #2a2d38;
  color: #e6e8ef;
}
.pp-dialog.pp-gate { border-color: #b3461f; }
.pp-dialog.pp-generic { border-color: #2f5fa8; }
.pp-heading { font-size: 1.5em; font-weight: bold; }
.pp-dialog.pp-gate .pp-heading { color: #ff9b6a; }
.pp-dialog.pp-generic .pp-heading { color: #8ab4f8; }
.pp-field-label { font-size: 0.85em; color: #98a0b3; text-transform: uppercase; }
.pp-trusted-value { font-size: 1.05em; font-weight: bold; }
.pp-mono { font-family: monospace; font-size: 0.95em; color: #d7dbe6; }
.pp-viewport {
  background-color: #0d0e13;
  border: 1px solid #2a2d38;
  border-radius: 6px;
  padding: 4px;
}
.pp-field-content { padding: 2px; }
.pp-empty { color: #6d7488; font-style: italic; }
.pp-note { font-size: 0.85em; color: #98a0b3; }
.pp-overflow { font-size: 0.85em; color: #ffd479; }
.pp-warn { color: #ffb27a; }
.pp-footer { font-size: 0.9em; color: #98a0b3; }
.pp-buttons { margin-top: 2px; }
.pp-deny, .pp-approve { padding: 8px 20px; }
.pp-approve { background-image: none; background-color: #7a2f14; color: #ffe6d8; }
.pp-dialog.pp-generic .pp-approve { background-color: #234a80; color: #dce8ff; }
.pp-approve:disabled, .pp-deny:disabled { opacity: 0.45; }
";
