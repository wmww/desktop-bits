//! The artwork on the two icon buttons.
//!
//! Cairo, not a font glyph: there is no glyph for "minimize" a theme can be relied on to have, and
//! drawing both of them keeps their stroke weight and colour identical. The colour is the widget's
//! own, read at draw time, so the theme decides it and the `:disabled` rule fades the icon exactly
//! as it fades a label.

use gtk::cairo;
use gtk::prelude::*;

/// Side of an icon button's face, in logical pixels. A little smaller than the text beside it: the
/// pair is punctuation on the button row, not a second heading.
pub const SIZE: i32 = 14;

const STROKE: f64 = 1.6;

/// Half-pixel offsets, so a stroke this thin lands on a pixel boundary rather than across two.
fn snap(v: f64) -> f64 {
    v.floor() + 0.5
}

fn pen(area: &gtk::DrawingArea, cr: &cairo::Context) {
    let c = area.color();
    cr.set_source_rgba(c.red() as f64, c.green() as f64, c.blue() as f64, c.alpha() as f64);
    cr.set_line_width(STROKE);
    cr.set_line_cap(cairo::LineCap::Round);
    cr.set_line_join(cairo::LineJoin::Miter);
}

/// A small rectangle in the lower right of the box: a picture of what the button does, drawn where
/// the chip it produces will land.
pub fn minimize(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32) {
    pen(area, cr);
    let (w, h) = (w as f64, h as f64);
    let x = snap(w * 0.42);
    let y = snap(h * 0.5);
    cr.rectangle(x, y, snap(w * 0.92) - x, snap(h * 0.92) - y);
    let _ = cr.stroke();
}

/// Two strokes. The deny button, which every dialog has.
pub fn cross(area: &gtk::DrawingArea, cr: &cairo::Context, w: i32, h: i32) {
    pen(area, cr);
    let (w, h) = (w as f64, h as f64);
    let (x0, y0) = (snap(w * 0.2), snap(h * 0.2));
    let (x1, y1) = (snap(w * 0.8), snap(h * 0.8));
    cr.move_to(x0, y0);
    cr.line_to(x1, y1);
    cr.move_to(x1, y0);
    cr.line_to(x0, y1);
    let _ = cr.stroke();
}
