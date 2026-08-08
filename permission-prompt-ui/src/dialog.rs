//! Dialog layout. Trusted fields and buttons sit outside every viewport and keep their natural
//! size; caller-controlled fields live in bounded scrolling viewports and are what shrinks.
//!
//! The layout is deliberately spare: an optional heading and trusted subtitle, then a two-column
//! grid of short gutter labels against their values. Anything the reader can infer from the shape
//! of the dialog — that Enter answers it, that the big accented box at the top is the thing being
//! asked about — is not written out.

use gtk::prelude::*;

use crate::text;
use crate::untrusted::Escaped;

/// Tallest a caller-controlled viewport gets before it scrolls, and the taller allowance for the
/// one field marked [`Field::expand`]. These are natural heights, not minimums: every viewport
/// still shrinks to nothing when the output cannot fit the dialog.
const MAX_VIEWPORT_HEIGHT: i32 = 120;
const MAX_EXPANDED_HEIGHT: i32 = 360;

/// Width of the gutter the short field labels sit in.
const GUTTER_WIDTH: i32 = 30;

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
    /// Render this field larger and in the dialog's accent colour. At most one field per dialog:
    /// it is what the reader is being asked about.
    pub prominent: bool,
    /// Drop the viewport chrome and set this field as plain running text. Only for a field that
    /// is one short line by nature: it wraps rather than scrolls, so it stays bounded, but it has
    /// no box around it saying "caller data". See [`Field::flat`].
    pub flat: bool,
}

impl Field {
    pub fn trusted(label: &'static str, lines: Vec<Escaped>) -> Self {
        Field::new(label, lines, FieldKind::Trusted)
    }

    pub fn warning(lines: Vec<Escaped>) -> Self {
        Field::new("", lines, FieldKind::Warning)
    }

    pub fn untrusted(label: &'static str, lines: Vec<Escaped>) -> Self {
        Field::new(label, lines, FieldKind::Untrusted)
    }

    fn new(label: &'static str, lines: Vec<Escaped>, kind: FieldKind) -> Self {
        Field {
            label,
            lines,
            kind,
            notes: Vec::new(),
            expand: false,
            prominent: false,
            flat: false,
        }
    }

    /// Give this field the leftover vertical space. See [`Field::expand`].
    pub fn expanding(mut self) -> Self {
        self.expand = true;
        self
    }

    /// Make this the field the eye lands on first. See [`Field::prominent`].
    pub fn prominent(mut self) -> Self {
        self.prominent = true;
        self
    }

    /// Render as plain running text rather than in a viewport. Still escaped, still capped, and it
    /// wraps rather than growing the dialog, so what it gives up is the box, not a bound.
    pub fn flat(mut self) -> Self {
        self.flat = true;
        self
    }

    pub fn with_note(mut self, note: Escaped) -> Self {
        self.notes.push(note);
        self
    }
}

pub struct DialogSpec {
    pub style: Style,
    /// Window title, and the heading when there is one. Never caller data.
    pub title: &'static str,
    /// Heading above the fields. `None` when the first field says it better than a heading could:
    /// the gate leads with the command itself rather than a sentence about it.
    pub heading: Option<&'static str>,
    /// Trusted lines under the heading: what this prompt is not.
    pub subtitle: Vec<Escaped>,
    pub fields: Vec<Field>,
    pub approve: &'static str,
    pub deny: &'static str,
}

/// One built dialog instance. Every output gets its own.
pub struct Dialog {
    pub root: gtk::Widget,
    pub approve: gtk::Button,
    pub deny: gtk::Button,
    /// The prominent field's value label, when the spec has one. Exposed so the presented-geometry
    /// log lines (see `app::log_geometry`) can name it; the GUI tests derive drag targets from
    /// those lines instead of hardcoding layout.
    pub prominent: Option<gtk::Label>,
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

    if let Some(header) = build_header(spec) {
        dialog.append(&header);
    }

    let fields = gtk::Box::new(gtk::Orientation::Vertical, 6);
    fields.add_css_class("pp-fields");
    let mut prominent = None;
    for field in &spec.fields {
        let (widget, body) = build_field(field);
        fields.append(&widget);
        if field.prominent {
            prominent = body;
        }
    }
    dialog.append(&fields);

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

    let d = Dialog { root: dialog.upcast(), approve, deny, prominent, settling };
    d.set_settled(false);
    d
}

/// The heading and subtitle, or nothing at all when the dialog has neither. No icon: a picture of
/// a warning triangle tells the reader nothing the command itself does not.
fn build_header(spec: &DialogSpec) -> Option<gtk::Widget> {
    if spec.heading.is_none() && spec.subtitle.is_empty() {
        return None;
    }
    let header = gtk::Box::new(gtk::Orientation::Vertical, 2);
    header.add_css_class("pp-header");
    if let Some(heading) = spec.heading {
        header.append(&text::wrapped(&Escaped::literal(heading), &["pp-heading"]));
    }
    if !spec.subtitle.is_empty() {
        header.append(&text::wrapped(&text::join(&spec.subtitle), &["pp-subtitle"]));
    }
    Some(header.upcast())
}

/// One field: its value and notes in a column, with a short gutter label beside them if it has one.
/// Also returns the value's own label, for [`Dialog::prominent`].
///
/// Rows rather than a grid, because a grid gives its spare width to every column a spanning child
/// covers — which left the gutter label stranded a third of the dialog away from its own value.
/// A fixed gutter width keeps the labels lined up with each other instead.
fn build_field(field: &Field) -> (gtk::Widget, Option<gtk::Label>) {
    let (value, notes, body) = build_value(field);
    let column = gtk::Box::new(gtk::Orientation::Vertical, 2);
    column.set_hexpand(true);
    column.append(&value);
    for note in notes {
        column.append(&note);
    }
    if field.label.is_empty() {
        return (column.upcast(), body);
    }
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    let label = text::label(&Escaped::literal(field.label), &["pp-field-label"]);
    label.set_valign(gtk::Align::Start);
    label.set_size_request(GUTTER_WIDTH, -1);
    row.append(&label);
    row.append(&column);
    (row.upcast(), body)
}

/// The field's value widget, any note rows that belong under it, and the label holding the value
/// text itself.
fn build_value(field: &Field) -> (gtk::Widget, Vec<gtk::Widget>, Option<gtk::Label>) {
    let (lines, capped) = cap(&field.lines);
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.add_css_class("pp-field-content");

    let mut escaped_any = false;
    let mut truncated_any = capped;
    for line in &lines {
        escaped_any |= line.was_escaped();
        truncated_any |= line.was_truncated();
    }
    // One label for the whole field, not one per line: a selection cannot cross widgets, and the
    // reader who wants the command wants all of it.
    let joined = text::join(&lines);
    let mut classes = vec![match field.kind {
        FieldKind::Trusted => "pp-trusted-value",
        FieldKind::Warning => "pp-warning-value",
        FieldKind::Untrusted => "pp-untrusted-value",
    }];
    if field.prominent {
        classes.push("pp-prominent");
    }
    if field.flat {
        classes.push("pp-flat");
    }
    // Kept for the overflow marker, which counts *rendered* lines and so has to ask the label.
    let mut body: Option<gtk::Label> = None;
    if lines.is_empty() {
        content.append(&text::label(&Escaped::literal("(none)"), &["pp-empty"]));
    } else if field.kind == FieldKind::Untrusted && !field.flat {
        let l = text::mono_block(&joined, &classes);
        content.append(&l);
        body = Some(l);
    } else {
        let l = text::wrapped(&joined, &classes);
        content.append(&l);
        body = Some(l);
    }

    let mut notes: Vec<gtk::Widget> = Vec::new();
    if escaped_any {
        notes.push(
            text::wrapped(
                &Escaped::literal("unsafe characters shown as \\xNN or \\uNNNN"),
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
        FieldKind::Untrusted if field.flat => content.upcast(),
        FieldKind::Untrusted => {
            let scroller = gtk::ScrolledWindow::new();
            scroller.add_css_class("pp-viewport");
            scroller.set_child(Some(&content));
            // Never sideways: the content wraps instead. A field that scrolls horizontally can
            // hide its tail off the right edge behind nothing louder than a scrollbar, and the
            // tail of a command is exactly where the interesting argument tends to be.
            scroller.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
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

            // The overflow count floats over the bottom of the viewport rather than taking a row
            // of its own, so it costs nothing when it is not shown and no layout jump when it is.
            let marker = text::label(&Escaped::literal(""), &["pp-overflow"]);
            marker.set_halign(gtk::Align::End);
            marker.set_valign(gtk::Align::End);
            marker.set_visible(false);
            let overlay = gtk::Overlay::new();
            overlay.set_child(Some(&scroller));
            overlay.add_overlay(&marker);
            if let Some(body) = &body {
                wire_overflow_marker(&scroller, &marker, body);
            }
            overlay.upcast()
        }
    };
    (value, notes, body)
}

/// Apply the per-field ceilings. Returns the lines to render and whether anything was dropped.
///
/// The character ceiling clips *within* a line rather than dropping it: the gate's command is one
/// line however many arguments it has, and dropping it would leave the field empty.
fn cap(lines: &[Escaped]) -> (Vec<Escaped>, bool) {
    let mut out = Vec::new();
    let mut chars = 0usize;
    for line in lines {
        if out.len() >= MAX_FIELD_LINES {
            return (out, true);
        }
        let len = line.as_str().chars().count();
        if chars + len > MAX_FIELD_CHARS {
            out.push(line.clone().clipped(MAX_FIELD_CHARS - chars));
            return (out, true);
        }
        chars += len;
        out.push(line.clone());
    }
    (out, false)
}

/// Count of hidden lines, floating over the bottom of a viewport that has more to show.
///
/// The count comes from the label's own Pango layout, not from how many `Escaped` lines went in:
/// the content wraps, so one logical line can be six rendered ones, and a marker counting the
/// former would under-report exactly when the command is long enough for the count to matter.
fn wire_overflow_marker(scroller: &gtk::ScrolledWindow, marker: &gtk::Label, body: &gtk::Label) {
    let adj = scroller.vadjustment();
    let marker = marker.clone();
    let body = body.clone();
    let update = move |adj: &gtk::Adjustment| {
        let upper = adj.upper();
        let page = adj.page_size();
        let total_lines = body.layout().line_count().max(0) as usize;
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

/// Everything colour here comes from the theme's named colours, so the prompt looks like the rest
/// of the desktop rather than like one particular palette. Only the two derived shades below are
/// ours, and both are mixes of theme colours. The names used are the ones every GTK theme defines.
///
/// The gate is red and the generic presenter blue: the two must never be mistakable for each
/// other, and that distinction is the one thing the theme does not get to decide.
pub const CSS: &str = "
/* The recessed background of a caller-controlled viewport, and the same shade under the overflow
   marker that floats over it. A mix rather than a fixed colour, so it stays a slight step away
   from the dialog in a light theme and a dark one alike. */
@define-color pp_inset mix(@theme_base_color, @theme_fg_color, 0.06);
window.pp-window { background-color: @theme_bg_color; }
.pp-backdrop { background-color: @theme_bg_color; }
.pp-dialog {
  background-color: @theme_base_color;
  border-radius: 12px;
  padding: 16px 18px;
  border: 2px solid @borders;
  color: @theme_text_color;
}
.pp-dialog.pp-gate { border-color: @error_color; }
.pp-dialog.pp-generic { border-color: @accent_color; }
.pp-heading { font-size: 1.35em; font-weight: bold; }
.pp-dialog.pp-gate .pp-heading { color: @error_color; }
.pp-dialog.pp-generic .pp-heading { color: @accent_color; }
.pp-subtitle { font-size: 0.95em; color: @insensitive_fg_color; }
.pp-field-label { font-size: 0.9em; color: @insensitive_fg_color; }
.pp-trusted-value { font-size: 1.0em; }
.pp-warning-value { color: @warning_color; }
.pp-mono { font-family: monospace; font-size: 0.95em; }
/* The one field the reader is being asked about. */
.pp-prominent { font-size: 1.3em; font-weight: bold; }
.pp-dialog.pp-gate .pp-prominent { color: @error_color; }
.pp-dialog.pp-generic .pp-prominent { color: @accent_color; }
/* Caller data outside a viewport: quieter than the boxed fields, never louder. */
.pp-flat { font-family: monospace; font-size: 1.05em; color: @insensitive_fg_color; }
.pp-viewport {
  background-color: @pp_inset;
  border: 1px solid @borders;
  border-radius: 6px;
  padding: 4px 6px;
}
.pp-field-content { padding: 1px; }
/* The theme's slider minimum is a floor under every viewport's height, which leaves a one-line
   field standing in a box three lines tall. */
.pp-viewport scrollbar slider { min-height: 14px; min-width: 14px; }
.pp-empty { color: @insensitive_fg_color; font-style: italic; }
.pp-note { font-size: 0.85em; color: @insensitive_fg_color; }
.pp-overflow {
  font-size: 0.8em;
  color: @warning_color;
  background-color: @pp_inset;
  border-radius: 4px;
  padding: 1px 6px;
  margin: 2px 14px;
}
.pp-warn { color: @warning_color; }
.pp-settling { font-size: 0.9em; color: @insensitive_fg_color; }
.pp-deny, .pp-approve { padding: 8px 20px; }
/* No border and no theme highlight on the accented button: the theme draws a button outline in its
   own button colour, which around a filled accent reads as a stray hairline. */
.pp-approve {
  background-image: none;
  border: none;
  box-shadow: none;
  background-color: @accent_color;
  color: @theme_selected_fg_color;
}
.pp-dialog.pp-gate .pp-approve { background-color: @error_color; }
.pp-approve:disabled, .pp-deny:disabled { opacity: 0.45; }
";
