//! Dialog layout. Trusted fields and buttons sit outside every viewport and keep their natural
//! size; caller-controlled fields live in bounded scrolling viewports and are what shrinks.
//!
//! The layout is deliberately spare: a heading, a trusted subtitle, and a two-column grid of
//! short gutter labels against their values. Anything the reader can infer from the shape of the
//! dialog — that Enter answers it, that the big box is the thing being asked about — is not
//! written out.

use gtk::prelude::*;

use crate::text;
use crate::untrusted::Escaped;

/// Tallest a caller-controlled viewport gets before it scrolls, and the taller allowance for the
/// one field marked [`Field::expand`]. These are natural heights, not minimums: every viewport
/// still shrinks to nothing when the output cannot fit the dialog.
const MAX_VIEWPORT_HEIGHT: i32 = 120;
const MAX_EXPANDED_HEIGHT: i32 = 360;

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
    /// Trusted prose that changes what the request means. Same guarantees, louder.
    Warning,
    /// Caller data. Bounded scrolling viewport, monospace, overflow marked.
    Untrusted,
}

pub struct Field {
    /// Short gutter label, lowercase. Empty for a field whose meaning is obvious from its place.
    pub label: &'static str,
    pub lines: Vec<Escaped>,
    pub kind: FieldKind,
    /// Extra trusted notes rendered under the field.
    pub notes: Vec<Escaped>,
    /// This field may grow much taller than the others before it scrolls. Mark the field the
    /// reader most needs room for.
    pub expand: bool,
}

impl Field {
    pub fn trusted(label: &'static str, lines: Vec<Escaped>) -> Self {
        Field { label, lines, kind: FieldKind::Trusted, notes: Vec::new(), expand: false }
    }

    pub fn warning(lines: Vec<Escaped>) -> Self {
        Field { label: "", lines, kind: FieldKind::Warning, notes: Vec::new(), expand: false }
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
    /// Trusted lines under the heading: who is asking, or what this prompt is not.
    pub subtitle: Vec<Escaped>,
    /// Icon name to show beside the heading. A name, not text: it cannot render caller prose.
    pub icon: Option<String>,
    pub fields: Vec<Field>,
    pub approve: &'static str,
    pub deny: &'static str,
}

/// One built dialog instance. Every output gets its own.
pub struct Dialog {
    pub root: gtk::Widget,
    pub approve: gtk::Button,
    pub deny: gtk::Button,
    /// Says why the controls are dead. Faded out rather than hidden once they are not: a widget
    /// that stops taking space would move the buttons at the instant they become live, and
    /// whatever the pointer was over could become the other button.
    settling: gtk::Label,
}

const SETTLING: &str = "controls unlock in a moment";

impl Dialog {
    /// Visibly disable the controls while the prompt is settling.
    pub fn set_settled(&self, settled: bool) {
        self.approve.set_sensitive(settled);
        self.deny.set_sensitive(settled);
        self.settling.set_opacity(if settled { 0.0 } else { 1.0 });
    }
}

pub fn build(spec: &DialogSpec) -> Dialog {
    let dialog = gtk::Box::new(gtk::Orientation::Vertical, 12);
    dialog.add_css_class("pp-dialog");
    dialog.add_css_class(match spec.style {
        Style::Gate => "pp-gate",
        Style::Generic => "pp-generic",
    });
    dialog.set_halign(gtk::Align::Center);
    // Centred at its natural height rather than stretched over the output: nothing here wants to be
    // bigger than its content. Under pressure the enclosing viewport allocates less than that, and
    // the caller-controlled viewports — minimum height zero — are what gives.
    dialog.set_valign(gtk::Align::Center);

    dialog.append(&build_header(spec));

    let grid = gtk::Grid::new();
    grid.add_css_class("pp-fields");
    grid.set_row_spacing(6);
    grid.set_column_spacing(10);
    let mut row = 0;
    for field in &spec.fields {
        row = attach_field(&grid, field, row);
    }
    dialog.append(&grid);

    // The settling message shares the buttons' row: on a cramped output every line of fixed
    // chrome is a line the buttons might not get.
    let settling = text::status(&Escaped::literal(SETTLING), &["pp-settling"]);
    settling.set_valign(gtk::Align::Center);
    let deny = text::button(spec.deny, &["pp-deny"]);
    let approve = text::button(spec.approve, &["pp-approve"]);

    let bottom = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    bottom.add_css_class("pp-buttons");
    bottom.append(&settling);
    // Holds the buttons against the right edge whether or not the settling message is there.
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    bottom.append(&spacer);
    bottom.append(&deny);
    bottom.append(&approve);
    dialog.append(&bottom);

    let d = Dialog { root: dialog.upcast(), approve, deny, settling };
    d.set_settled(false);
    d
}

fn build_header(spec: &DialogSpec) -> gtk::Widget {
    let header = gtk::Box::new(gtk::Orientation::Horizontal, 12);
    header.add_css_class("pp-header");
    if let Some(name) = &spec.icon {
        let image = gtk::Image::from_icon_name(name);
        image.set_icon_size(gtk::IconSize::Large);
        image.set_valign(gtk::Align::Start);
        header.append(&image);
    }
    let titles = gtk::Box::new(gtk::Orientation::Vertical, 2);
    titles.append(&text::wrapped(&Escaped::literal(spec.heading), &["pp-heading"]));
    for line in &spec.subtitle {
        titles.append(&text::wrapped(line, &["pp-subtitle"]));
    }
    header.append(&titles);
    header.upcast()
}

/// Attach one field's rows to the grid and return the next free row.
///
/// A labelled field puts its short label in the gutter and its value beside it; an unlabelled one
/// spans both columns, so a dialog with no labels at all wastes no width on an empty gutter.
fn attach_field(grid: &gtk::Grid, field: &Field, mut row: i32) -> i32 {
    let (value, notes) = build_value(field);
    value.set_hexpand(true);
    if field.label.is_empty() {
        grid.attach(&value, 0, row, 2, 1);
    } else {
        let label = text::label(&Escaped::literal(field.label), &["pp-field-label"]);
        label.set_valign(gtk::Align::Start);
        grid.attach(&label, 0, row, 1, 1);
        grid.attach(&value, 1, row, 1, 1);
    }
    row += 1;
    for note in notes {
        if field.label.is_empty() {
            grid.attach(&note, 0, row, 2, 1);
        } else {
            grid.attach(&note, 1, row, 1, 1);
        }
        row += 1;
    }
    row
}

/// The field's value widget, plus any note rows that belong under it.
fn build_value(field: &Field) -> (gtk::Widget, Vec<gtk::Widget>) {
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
            FieldKind::Warning => content.append(&text::wrapped(line, &["pp-warning-value"])),
            FieldKind::Untrusted => content.append(&text::mono_line(line)),
        }
    }
    if lines.is_empty() {
        content.append(&text::label(&Escaped::literal("(none)"), &["pp-empty"]));
    }

    let mut notes: Vec<gtk::Widget> = Vec::new();
    if escaped_any {
        notes.push(
            text::wrapped(
                &Escaped::literal("unsafe characters shown as \\xNN or \\u{NNNN}"),
                &["pp-note", "pp-warn"],
            )
            .upcast(),
        );
    }
    if truncated_any {
        notes.push(
            text::wrapped(
                &Escaped::literal("TRUNCATED — not everything is shown here; see the log"),
                &["pp-note", "pp-warn"],
            )
            .upcast(),
        );
    }
    for note in &field.notes {
        notes.push(text::wrapped(note, &["pp-note"]).upcast());
    }

    let value: gtk::Widget = match field.kind {
        FieldKind::Trusted | FieldKind::Warning => content.upcast(),
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
            scroller.set_propagate_natural_height(true);
            scroller.set_max_content_height(if field.expand {
                MAX_EXPANDED_HEIGHT
            } else {
                MAX_VIEWPORT_HEIGHT
            });
            reserve_hscrollbar_room(&scroller);

            // The overflow count floats over the bottom of the viewport rather than taking a row
            // of its own, so it costs nothing when it is not shown and no layout jump when it is.
            let marker = text::label(&Escaped::literal(""), &["pp-overflow"]);
            marker.set_halign(gtk::Align::End);
            marker.set_valign(gtk::Align::End);
            marker.set_visible(false);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&scroller));
            overlay.add_overlay(&marker);
            wire_overflow_marker(&scroller, &marker, lines.len());
            overlay.upcast()
        }
    };
    (value, notes)
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

/// Keep a visible horizontal scrollbar from eating the field's last line.
///
/// A scrolled window counts the scrollbar in the height it asks for only under `Always`, since
/// under `Automatic` it cannot know whether the bar will be there — so an automatic bar that does
/// appear silently costs the field its last line and starts it scrolling vertically too. Switch
/// the policy to `Always` for exactly as long as the content overflows sideways. This settles:
/// the extra height does not change how wide the content is.
fn reserve_hscrollbar_room(scroller: &gtk::ScrolledWindow) {
    let adj = scroller.hadjustment();
    let scroller = scroller.clone();
    let update = move |adj: &gtk::Adjustment| {
        let policy = if adj.upper() > adj.page_size() + 0.5 {
            gtk::PolicyType::Always
        } else {
            gtk::PolicyType::Automatic
        };
        if scroller.policy().0 != policy {
            scroller.set_policy(policy, gtk::PolicyType::Automatic);
        }
    };
    adj.connect_changed({
        let update = update.clone();
        move |a| update(a)
    });
    adj.connect_value_changed(move |a| update(a));
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
                Escaped::literal(" more"),
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
  padding: 16px 18px;
  border: 2px solid #2a2d38;
  color: #e6e8ef;
}
.pp-dialog.pp-gate { border-color: #b3461f; }
.pp-dialog.pp-generic { border-color: #2f5fa8; }
.pp-heading { font-size: 1.35em; font-weight: bold; }
.pp-dialog.pp-gate .pp-heading { color: #ff9b6a; }
.pp-dialog.pp-generic .pp-heading { color: #8ab4f8; }
.pp-subtitle { font-size: 0.95em; color: #98a0b3; }
.pp-field-label { font-size: 0.9em; color: #6d7488; }
.pp-trusted-value { font-size: 1.0em; }
.pp-warning-value { color: #ffb27a; }
.pp-mono { font-family: monospace; font-size: 0.95em; color: #d7dbe6; }
.pp-viewport {
  background-color: #0d0e13;
  border: 1px solid #22252e;
  border-radius: 6px;
  padding: 4px 6px;
}
.pp-field-content { padding: 1px; }
/* The theme's slider minimum is a floor under every viewport's height, which leaves a one-line
   field standing in a box three lines tall. */
.pp-viewport scrollbar slider { min-height: 14px; min-width: 14px; }
.pp-empty { color: #6d7488; font-style: italic; }
.pp-note { font-size: 0.85em; color: #6d7488; }
.pp-overflow {
  font-size: 0.8em;
  color: #ffd479;
  background-color: #0d0e13;
  border-radius: 4px;
  padding: 1px 6px;
  margin: 2px 14px;
}
.pp-warn { color: #ffb27a; }
.pp-settling { font-size: 0.9em; color: #6d7488; }
.pp-deny, .pp-approve { padding: 8px 20px; }
.pp-approve { background-image: none; background-color: #7a2f14; color: #ffe6d8; }
.pp-dialog.pp-generic .pp-approve { background-color: #234a80; color: #dce8ff; }
.pp-approve:disabled, .pp-deny:disabled { opacity: 0.45; }
";
