//! The minimized prompt: a small layer surface in the bottom-right corner of every output.
//!
//! Deliberately powerless. It does exactly two things — deny, and hand the screen back to the full
//! prompt — so a chip that is covered, spoofed or misread costs nothing the adversary did not
//! already have (`issues/overlay-layer-spoofing.md`). Approval never happens here: it happens on
//! the session-lock surface, behind a fresh quiet period, every time.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::app::{take_signal, PromptConfig, Verdict};
use crate::untrusted::Escaped;
use crate::{icon, text};

/// What the chip shows: the command on one line, and who asked for it.
pub struct ChipSpec {
    /// The same shell-quoted line the full prompt leads with, ellipsized to fit.
    pub command: Escaped,
    pub user: Escaped,
}

/// Gap between the chip and the output's bottom-right corner.
const MARGIN: i32 = 16;

pub(crate) enum Chip {
    /// Back to the full prompt: a new lock, new surfaces and a new quiet period.
    Expand,
    Decided(Verdict),
}

struct Inner {
    windows: Vec<gtk::Window>,
    outcome: Option<Chip>,
    /// Set with the outcome and never cleared, so a late callback cannot restart a phase whose
    /// outcome has already been taken out.
    over: bool,
    main_loop: glib::MainLoop,
}

impl Inner {
    fn live(&self) -> bool {
        !self.over
    }

    fn finish(&mut self, outcome: Chip) {
        if self.over {
            return;
        }
        match &outcome {
            // The tests key on this line.
            Chip::Expand => log::debug!("expanded"),
            Chip::Decided(v) => log::debug!("verdict: {v:?}"),
        }
        self.outcome = Some(outcome);
        self.over = true;
        self.main_loop.quit();
    }
}

type State = Rc<RefCell<Inner>>;

/// One chip period: no session lock, no settling, one main loop. The desktop belongs to the human
/// throughout; the gate's flock does not, so other requests stay behind this one.
pub(crate) fn run_phase(cfg: &Rc<PromptConfig>) -> Chip {
    let state: State = Rc::new(RefCell::new(Inner {
        windows: Vec::new(),
        outcome: None,
        over: false,
        main_loop: glib::MainLoop::new(None, false),
    }));

    install_signal_timer(&state);
    if let Err(e) = start(cfg, &state) {
        state.borrow_mut().finish(Chip::Decided(Verdict::Error(e)));
    }

    let main_loop = state.borrow().main_loop.clone();
    if state.borrow().live() {
        main_loop.run();
    }

    let outcome = state.borrow_mut().outcome.take();
    let windows: Vec<gtk::Window> = state.borrow_mut().windows.drain(..).collect();
    for window in windows {
        window.destroy();
    }
    outcome.unwrap_or(Chip::Decided(Verdict::Error("the chip exited without a decision".into())))
}

/// A signal is a denial here exactly as it is under the full prompt. The chip phase has no settle
/// timer to hang the poll off, so it gets its own.
fn install_signal_timer(state: &State) {
    let state = state.clone();
    glib::timeout_add_local(std::time::Duration::from_millis(50), move || {
        let mut st = state.borrow_mut();
        if !st.live() {
            return glib::ControlFlow::Break;
        }
        match take_signal() {
            Some(sig) => {
                st.finish(Chip::Decided(Verdict::DeniedSignal(sig)));
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Continue,
        }
    });
}

fn start(cfg: &Rc<PromptConfig>, state: &State) -> Result<(), String> {
    if !gtk4_layer_shell::is_supported() {
        return Err("compositor does not support zwlr_layer_shell_v1".to_string());
    }
    let display = gdk::Display::default().ok_or("no GDK display")?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        if let Some(monitor) = monitors.item(i).and_downcast::<gdk::Monitor>() {
            add_chip(cfg, state, &monitor);
        }
    }
    if state.borrow().windows.is_empty() {
        return Err("no outputs to present the prompt on".to_string());
    }
    monitors.connect_items_changed({
        let cfg = cfg.clone();
        let state = state.clone();
        move |model, pos, _removed, added| {
            if !state.borrow().live() {
                return;
            }
            for i in pos..pos + added {
                if let Some(monitor) = model.item(i).and_downcast::<gdk::Monitor>() {
                    add_chip(&cfg, &state, &monitor);
                }
            }
        }
    });
    Ok(())
}

fn add_chip(cfg: &Rc<PromptConfig>, state: &State, monitor: &gdk::Monitor) {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    let Some(spec) = &cfg.spec.chip else { return };

    let text_column = gtk::Box::new(gtk::Orientation::Vertical, 1);
    text_column.set_hexpand(true);
    text_column.set_valign(gtk::Align::Center);
    text_column.append(&text::ellipsized(&spec.command, &["pp-chip-command"]));
    text_column.append(&text::ellipsized(&spec.user, &["pp-chip-user"]));

    // The one place a denial can be reached from, and the safe direction, so it needs no quiet
    // period of its own.
    let cancel = text::icon_button(cfg.spec.deny, &["pp-icon", "pp-chip-cancel"], icon::cross);
    cancel.set_valign(gtk::Align::Center);
    cancel.connect_clicked({
        let state = state.clone();
        move |_| state.borrow_mut().finish(Chip::Decided(Verdict::Denied))
    });

    let body = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    body.add_css_class("pp-chip");
    body.append(&text_column);
    body.append(&cancel);

    let window = gtk::Window::new();
    window.add_css_class("pp-chip-window");
    window.set_title(Some(cfg.spec.title));
    window.set_child(Some(&body));

    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("sudo-prompt-chip"));
    window.set_monitor(Some(monitor));
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Bottom, MARGIN);
    window.set_margin(Edge::Right, MARGIN);
    // No keyboard at all, so the chip's entire input surface is those two click targets.
    window.set_keyboard_mode(KeyboardMode::None);
    // Floats over the desktop rather than reserving space in it: the point of minimizing is that
    // the desktop is usable, and a chip that pushed the layout around would not be out of the way.
    window.set_exclusive_zone(0);

    wire_expand(&window, state);
    wire_presentation(&window, monitor, &cancel, text_column.upcast_ref());

    window.present();
    state.borrow_mut().windows.push(window.clone());
    watch_monitor(state, monitor, &window);
}

/// A click anywhere the X did not take expands the prompt again.
///
/// Bubble phase, so the button's own gesture sees the press first and claims the sequence, which
/// cancels this one. That ordering is what keeps the X a denial rather than an expand, so the GUI
/// test checks it rather than trusting it.
fn wire_expand(window: &gtk::Window, state: &State) {
    let click = gtk::GestureClick::new();
    click.set_button(gdk::BUTTON_PRIMARY);
    click.set_propagation_phase(gtk::PropagationPhase::Bubble);
    click.connect_released({
        let state = state.clone();
        move |_, _, _, _| state.borrow_mut().finish(Chip::Expand)
    });
    window.add_controller(click);
}

/// Same second-tick rule as the full prompt: the frame clock only advances once the compositor is
/// really presenting the surface. Nothing here waits on it — the chip has no quiet period — but the
/// tests need to know the chip is up and where it landed.
fn wire_presentation(
    window: &gtk::Window,
    monitor: &gdk::Monitor,
    cancel: &gtk::Button,
    text_column: &gtk::Widget,
) {
    let ticks = Cell::new(0u32);
    let monitor = monitor.clone();
    let cancel = cancel.clone();
    let text_column = text_column.clone();
    window.add_tick_callback(move |window, _| {
        ticks.set(ticks.get() + 1);
        if ticks.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        // The tests key on this line.
        log::debug!("chip presented");
        log_geometry(window, &monitor, &cancel, &text_column);
        glib::ControlFlow::Break
    });
}

/// Where the chip landed, in *output* coordinates — unlike the full prompt's window-relative lines,
/// because a lock surface fills its output while a chip is a small window in one corner. Anchored
/// bottom-right at a fixed margin, that corner is arithmetic rather than a query.
fn log_geometry(
    window: &gtk::Window,
    monitor: &gdk::Monitor,
    cancel: &gtk::Button,
    text_column: &gtk::Widget,
) {
    let output = monitor.geometry();
    let (w, h) = (window.width(), window.height());
    let x = output.x() + output.width() - MARGIN - w;
    let y = output.y() + output.height() - MARGIN - h;
    log::debug!("geometry: chip {x} {y} {w} {h}");
    let one = |name: &str, widget: &gtk::Widget| {
        if let Some(b) = widget.compute_bounds(window) {
            log::debug!(
                "geometry: {name} {} {} {} {}",
                x + b.x().round() as i32,
                y + b.y().round() as i32,
                b.width().round() as i32,
                b.height().round() as i32
            );
        }
    };
    one("chip-cancel", cancel.upcast_ref());
    // The text, which is everything on the chip that is not the X: the tests need a click target
    // for "anywhere else".
    one("chip-body", text_column);
}

/// Losing the last output is an error here for the same reason it is under the lock: a request
/// nobody can answer must not sit there holding the gate's flock.
fn watch_monitor(state: &State, monitor: &gdk::Monitor, window: &gtk::Window) {
    let state = state.clone();
    let window = window.clone();
    monitor.connect_invalidate(move |_| {
        let mut st = state.borrow_mut();
        if !st.live() {
            return;
        }
        log::debug!("monitor removed");
        st.windows.retain(|w| *w != window);
        if st.windows.is_empty() {
            st.finish(Chip::Decided(Verdict::Error(
                "no outputs left to present the prompt on".to_string(),
            )));
        }
        drop(st);
        window.destroy();
    });
}
