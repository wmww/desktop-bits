//! Surface presentation, the input state machine, and the run loop.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::chip::{self, Chip};
use crate::dialog::{self, Dialog, DialogSpec};
use crate::settle::{Settle, SettleState};
use crate::text;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SurfaceMode {
    /// Session lock, then layer shell, then an xdg toplevel.
    Auto,
    SessionLock,
    Layer,
    Toplevel,
}

impl SurfaceMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "auto" => Some(SurfaceMode::Auto),
            "session-lock" => Some(SurfaceMode::SessionLock),
            "layer" => Some(SurfaceMode::Layer),
            "toplevel" => Some(SurfaceMode::Toplevel),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum Verdict {
    Approved,
    Denied,
    /// Input kept arriving past the settle cap. Fails closed.
    DeniedSettleCap,
    DeniedSignal(i32),
    /// Something the prompt depends on was not true. The message names the failed check.
    Error(String),
}

/// What the run came back with: the verdict, and whatever the human typed in the response box.
#[derive(Debug)]
pub struct Answer {
    pub verdict: Verdict,
    /// Only ever set for [`Verdict::Approved`] and [`Verdict::Denied`]: a settle cap, a signal or
    /// an error was not a human answering, and a half-typed sentence printed under one of those
    /// would be attributed to a decision nobody made.
    pub response: Option<String>,
}

impl Answer {
    /// An outcome with nothing typed, or one the response does not belong to.
    fn bare(verdict: Verdict) -> Self {
        Answer { verdict, response: None }
    }
}

pub struct PromptConfig {
    pub spec: DialogSpec,
    pub mode: SurfaceMode,
    pub settle: Duration,
    pub cap: Duration,
    /// True for the gate: a session lock that cannot be taken is a denial, never a downgrade to a
    /// weaker surface.
    pub lock_required: bool,
}

/// Everything that outlives one set of surfaces — and, with the chip, one whole phase. The
/// response buffer lives here rather than in [`Inner`] so every output's entry shares it, and so
/// it survives both a rebuild of the windows and a trip through the chip.
///
/// The config is an `Rc` of its own because the chip phase takes it alone, without the buffer.
struct Ui {
    cfg: Rc<PromptConfig>,
    response: Option<text::ResponseBuffer>,
}

thread_local! {
    /// The live lock, reachable from the panic hook. Only ever set on the main thread.
    static ACTIVE_LOCK: RefCell<Option<gtk4_session_lock::Instance>> = const { RefCell::new(None) };
}

/// Unlock the session and wait for the compositor to process it. Safe to call when not locked.
///
/// The roundtrip matters: dropping the connection while locked is exactly how the compositor
/// learns a lock client died, and it would leave the session locked with nothing on it.
pub fn unlock_and_sync() {
    let unlocked = ACTIVE_LOCK.with(|l| match l.try_borrow() {
        Ok(l) => match l.as_ref() {
            Some(inst) if inst.is_locked() => {
                inst.unlock();
                true
            }
            _ => false,
        },
        Err(_) => false,
    });
    if unlocked {
        if let Some(d) = gdk::Display::default() {
            d.sync();
        }
    }
}

/// A panic in a GTK callback would otherwise strand the session locked with no client.
pub fn install_panic_hook() {
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        unlock_and_sync();
        prev(info);
    }));
}

pub fn init() -> Result<(), String> {
    gtk::init().map_err(|e| format!("GTK initialization failed: {e}"))?;
    if let Some(settings) = gtk::Settings::default() {
        // The fields are selectable, and GTK's default is that a label selects all of itself when
        // the toolkit gives it focus — which it does to the first focusable widget the moment the
        // window has keyboard focus. The prompt would then come up showing a selection the reader
        // never made.
        settings.set_gtk_label_select_on_focus(false);
        // Which theme GTK settled on, so "why is the prompt not following my theme?" is one debug
        // line rather than a guess. The gate runs with a scrubbed environment, so its answer comes
        // from root's own GTK config and nothing the caller said.
        log::debug!(
            "gtk theme: {} (prefer-dark {})",
            settings.gtk_theme_name().unwrap_or_default(),
            settings.is_gtk_application_prefer_dark_theme(),
        );
    }
    Ok(())
}

/// How a full-prompt phase ended: with a decision, or with the human shrinking it to the chip.
enum Full {
    Decided(Verdict),
    Minimize,
}

struct Inner {
    settle: Settle,
    /// Last settle state pushed to the dialogs, so we only touch widgets on a change.
    shown_settled: Option<bool>,
    windows: Vec<(gtk::Window, Rc<Dialog>)>,
    /// Hardware keycodes currently down, so a synthesized autorepeat is never a fresh press.
    held: HashSet<u32>,
    outcome: Option<Full>,
    /// Set with the outcome and never cleared, so a late callback cannot restart a phase whose
    /// outcome has already been taken out (teardown destroys windows, which fires `destroy`).
    over: bool,
    /// The compositor has confirmed the lock. A lock surface exists only inside the lock, so
    /// nothing is presented before this.
    locked: bool,
    /// Lock windows created before `locked` arrived.
    pending: Vec<gtk::Window>,
    main_loop: glib::MainLoop,
    /// Set once the run has committed to a surface mode, so an async lock failure cannot
    /// start a second set of surfaces.
    fallback_done: bool,
}

impl Inner {
    /// This phase has not ended yet.
    fn live(&self) -> bool {
        !self.over
    }

    /// Records the decision and stops the loop. Deliberately does *not* unlock: the unlock needs a
    /// Wayland roundtrip, during which GTK emits signals whose handlers borrow this same state.
    /// [`run`] unlocks once, outside every callback.
    fn finish(&mut self, verdict: Verdict) {
        if !self.live() {
            return;
        }
        log::debug!("verdict: {verdict:?}");
        self.outcome = Some(Full::Decided(verdict));
        self.over = true;
        self.main_loop.quit();
    }

    /// The human asked for the prompt to get out of the way. Same path as a verdict: the loop
    /// quits and the unlock happens outside every callback.
    fn minimize(&mut self) {
        if !self.live() {
            return;
        }
        // The tests key on this line.
        log::debug!("minimized");
        self.outcome = Some(Full::Minimize);
        self.over = true;
        self.main_loop.quit();
    }
}

type State = Rc<RefCell<Inner>>;

/// The prompt, from first surface to answer.
///
/// Lock transitions only ever happen here, in straight-line code between main loops: unlocking
/// inside a GTK callback deadlocks on the state `RefCell`, because the roundtrip makes GTK emit
/// signals whose handlers borrow the same state. Each lock epoch is therefore its own *phase* with
/// its own fresh state and its own main loop, and everything per-phase — the quiet period and its
/// cap, held keys, the surfaces, the zero-output timer — resets for free. What outlives a phase
/// lives in [`Ui`], so a response typed before minimizing is still there afterwards.
pub fn run(cfg: PromptConfig) -> Answer {
    install_css();
    install_signal_handlers();
    let ui = Rc::new(Ui {
        response: cfg.spec.response.then(text::ResponseBuffer::new),
        cfg: Rc::new(cfg),
    });

    loop {
        let state = run_full_phase(&ui);
        let outcome = state.borrow_mut().outcome.take();

        // Every survivable exit path lands here: approval (before the exec), denial, the settle
        // cap, the signal handlers, every operational error after lock() succeeded, and the
        // minimize that hands the desktop back.
        unlock_and_sync();
        // Tear the lock reference down after the loop, so nothing can re-enter it.
        ACTIVE_LOCK.with(|l| {
            if let Ok(mut l) = l.try_borrow_mut() {
                *l = None;
            }
        });
        teardown(&state);

        match outcome {
            Some(Full::Decided(v)) => return answer(&ui, v),
            None => {
                return Answer::bare(Verdict::Error("prompt exited without a decision".to_string()))
            }
            // The desktop is the human's again while they check whatever made them hesitate. The
            // gate's flock is still held, so other requests stay behind this one.
            Some(Full::Minimize) => {}
        }

        // The chip gets the config and nothing else: it has no response box, and no way to reach
        // the one the full prompt keeps.
        match chip::run_phase(&ui.cfg) {
            Chip::Decided(v) => return answer(&ui, v),
            // Round again from scratch: a fresh lock epoch, fresh surfaces and a fresh quiet
            // period. That the expanding click is itself input, and so restarts the quiet period,
            // is what makes baiting a double-click useless.
            Chip::Expand => {}
        }
    }
}

/// Pair a verdict with whatever the human typed. Read here rather than at the verdict: the phase's
/// loop has quit by now, so nothing can be typed in between, and the buffer stays out of the state
/// every callback borrows.
fn answer(ui: &Rc<Ui>, verdict: Verdict) -> Answer {
    let response = match verdict {
        Verdict::Approved | Verdict::Denied => ui.response.as_ref().and_then(|b| b.text()),
        _ => None,
    };
    Answer { verdict, response }
}

/// One lock epoch: fresh state, fresh surfaces, fresh quiet period, one main loop. Returns the
/// phase's state with its outcome recorded, for the caller to unlock and tear down.
fn run_full_phase(ui: &Rc<Ui>) -> State {
    let state: State = Rc::new(RefCell::new(Inner {
        settle: Settle::new(ui.cfg.settle, ui.cfg.cap),
        shown_settled: None,
        windows: Vec::new(),
        held: HashSet::new(),
        outcome: None,
        over: false,
        locked: false,
        pending: Vec::new(),
        main_loop: glib::MainLoop::new(None, false),
        fallback_done: false,
    }));

    install_settle_timer(&state);
    install_signal_timer(&state);

    match ui.cfg.mode {
        SurfaceMode::SessionLock => {
            if let Err(e) = start_session_lock(ui, &state) {
                // `failed` can arrive before `lock()` returns, and its message is the more
                // specific one, so it wins by already being recorded.
                state.borrow_mut().finish(Verdict::Error(e));
                return state;
            }
        }
        SurfaceMode::Layer => {
            if let Err(e) = start_layer(ui, &state) {
                state.borrow_mut().finish(Verdict::Error(e));
                return state;
            }
        }
        SurfaceMode::Toplevel => start_toplevel(ui, &state),
        SurfaceMode::Auto => {
            if start_session_lock(ui, &state).is_err() {
                fall_back(ui, &state);
            }
        }
    }

    let main_loop = state.borrow().main_loop.clone();
    if state.borrow().live() {
        main_loop.run();
    }
    state
}

/// Drop a finished phase's surfaces. After the unlock, so nothing here runs against a window the
/// compositor still believes is a lock surface.
fn teardown(state: &State) {
    let windows: Vec<gtk::Window> = state.borrow_mut().windows.drain(..).map(|(w, _)| w).collect();
    state.borrow_mut().pending.clear();
    for window in windows {
        window.destroy();
    }
}

/// Above `GTK_STYLE_PROVIDER_PRIORITY_USER`, which is where GTK loads
/// `~/.config/gtk-4.0/gtk.css` — and forcing a GTK3-era theme on GTK4 by `@import`ing its
/// stylesheet from that file is how people actually install one (libadwaita apps ignore
/// `gtk-theme-name`, so it is the only thing that works). That puts a whole theme *above* the
/// application priority a prompt would normally use, and themes open with a reset: Sweet's is
/// `* { padding: 0; }`, which collapsed every padding here — the dialog's own, the viewports',
/// the buttons' — while leaving the colours it does not mention. The result had the text against
/// the border on all four sides.
///
/// So the prompt's layout is not the theme's to override. Colour still is: `@theme_*` resolve
/// through the whole cascade whatever priority we sit at, and this stylesheet only names
/// properties it means to fix.
const CSS_PRIORITY: u32 = gtk::STYLE_PROVIDER_PRIORITY_USER + 1;

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(dialog::CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(&display, &provider, CSS_PRIORITY);
    }
}

/// Set by the signal handler, read by whichever phase is running. glib no longer wraps
/// `g_unix_signal_add`, and an atomic store is the only thing worth doing in a handler anyway.
static PENDING_SIGNAL: AtomicI32 = AtomicI32::new(0);

extern "C" fn note_signal(sig: libc::c_int) {
    PENDING_SIGNAL.store(sig, Ordering::Relaxed);
}

/// SIGINT, SIGTERM and SIGHUP are denials, never ignored or blocked. SIGHUP is the normal outcome
/// of the requesting terminal going away while the prompt is up.
fn install_signal_handlers() {
    for sig in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
        // SAFETY: the handler only does a relaxed atomic store.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = note_signal as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(sig, &action, std::ptr::null_mut());
        }
    }
}

/// The signal the handler recorded, if any. Both phases poll for it; the chip phase has no settle
/// timer to hang it off, and a signal is a denial in either.
pub(crate) fn take_signal() -> Option<i32> {
    match PENDING_SIGNAL.swap(0, Ordering::Relaxed) {
        0 => None,
        sig => Some(sig),
    }
}

/// Consumes the flag only while this phase is live, so a signal cannot be swallowed by an outgoing
/// phase's timer during a phase change.
fn install_signal_timer(state: &State) {
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        let mut st = state.borrow_mut();
        if !st.live() {
            return glib::ControlFlow::Break;
        }
        match take_signal() {
            Some(sig) => {
                st.finish(Verdict::DeniedSignal(sig));
                glib::ControlFlow::Break
            }
            None => glib::ControlFlow::Continue,
        }
    });
}

fn install_settle_timer(state: &State) {
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        let mut st = state.borrow_mut();
        if !st.live() {
            return glib::ControlFlow::Break;
        }
        match st.settle.poll(Instant::now()) {
            SettleState::CapExceeded => {
                // Enabling the controls here would hand approval to whatever is generating the
                // input. Fail closed instead; a genuine fast typist loses nothing but a retry.
                st.finish(Verdict::DeniedSettleCap);
                glib::ControlFlow::Break
            }
            SettleState::Settled | SettleState::Waiting => {
                let settled = st.settle.is_settled();
                if st.shown_settled != Some(settled) {
                    st.shown_settled = Some(settled);
                    // The tests key on this line to know when the prompt is answerable.
                    log::debug!("controls {}", if settled { "live" } else { "settling" });
                    for (_, d) in &st.windows {
                        d.set_settled(settled);
                    }
                }
                glib::ControlFlow::Continue
            }
        }
    });
}

/// Minimizing needs both something for the chip to show and a surface worth getting out of the way
/// of. The other surface modes do not seize the screen, so there is nothing there to minimize —
/// and the gate, which is the only caller that fills in a [`ChipSpec`](crate::chip::ChipSpec),
/// only ever asks for the session lock.
fn minimizable(cfg: &PromptConfig) -> bool {
    cfg.spec.chip.is_some() && cfg.mode == SurfaceMode::SessionLock
}

fn new_window(ui: &Rc<Ui>, state: &State) -> (gtk::Window, Rc<Dialog>) {
    let window = gtk::Window::new();
    window.add_css_class("pp-window");
    window.set_title(Some(ui.cfg.spec.title));

    let d = Rc::new(dialog::build(&ui.cfg.spec, ui.response.as_ref(), minimizable(&ui.cfg)));
    if let Some(shown) = state.borrow().shown_settled {
        d.set_settled(shown);
    }

    d.root.set_margin_top(12);
    d.root.set_margin_bottom(12);
    d.root.set_margin_start(12);
    d.root.set_margin_end(12);

    // The whole dialog is constrained to the output: a session-lock surface must commit exactly the
    // size the compositor configured, so a dialog whose *minimum* exceeds a small output would be a
    // protocol error — that is, a crash that strands the session locked. The caller-controlled
    // viewports inside shrink first and are normally the only thing that gives; this outer scroller
    // is the last resort that keeps the surface legal on an output too small even for the trusted
    // fields and buttons. Keys still answer the prompt in that state.
    let fit = gtk::ScrolledWindow::new();
    fit.add_css_class("pp-backdrop");
    fit.set_child(Some(&d.root));
    fit.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Automatic);
    fit.set_overlay_scrolling(false);
    // A scrolled window allocates its child the viewport size as long as that is at least the
    // child's minimum, so the dialog is centred at its natural height on a roomy output, squeezed
    // towards its minimum on a cramped one, and only scrolled — the last resort, where keys still
    // answer the prompt — when even the trusted fields and buttons do not fit.
    fit.set_propagate_natural_width(true);
    fit.set_propagate_natural_height(true);
    fit.set_min_content_width(0);
    fit.set_min_content_height(0);
    window.set_child(Some(&fit));

    // Selectable labels are focusable, so the toolkit hands focus to one as soon as the window has
    // keyboard focus — and a label focused by the toolkit rather than by a click selects all of
    // itself, showing the reader a selection they never made. Nothing here should hold focus until
    // they click something.
    window.connect_map(|w| drop_focus(w));

    wire_input(&window, &d, state);
    wire_presentation(&window, &d, state);
    wire_destroy(&window, state);
    (window, d)
}

/// Approval requires a fresh physical key down, or a click that began after settling.
fn wire_input(window: &gtk::Window, d: &Rc<Dialog>, state: &State) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let state = state.clone();
        // Weak, so the controller the window owns does not own the window back.
        let window = window.downgrade();
        let response = d.response.clone();
        move |_, keyval, keycode, modifiers| {
            let mut st = state.borrow_mut();
            let now = Instant::now();
            let win = window.upgrade();
            let typing = win.as_ref().is_some_and(|w| focus_is_in(w, response.as_ref()));
            // Insert before the settled check: a key held from before settling must still be
            // known, so its autorepeats are never mistaken for a fresh press afterwards.
            let repeat = !st.held.insert(keycode);
            // Typing into the response box is not a disturbance. The box takes focus only from a
            // deliberate click — which is itself input, and restarts the quiet period — and it
            // cannot answer anything, so a sentence typed into it is aimed at this prompt by
            // definition. Counting it would let someone writing a reason run into the settle cap
            // and have the request denied out from under them.
            if !typing || is_answer_key(keyval) {
                st.settle.input(now);
            }
            let answered = st.settle.is_settled() && !repeat;
            // Enter and Escape keep their meaning while the human is typing: they are handled
            // here, before the entry can see them, so a response is an annotation on the answer
            // rather than a third answer.
            match keyval {
                gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                    if answered {
                        st.finish(Verdict::Approved);
                    }
                    return glib::Propagation::Stop;
                }
                gdk::Key::Escape => {
                    if answered {
                        st.finish(Verdict::Denied);
                    }
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
            if is_copy(keyval, modifiers) {
                // Allowed through to the focused widget wherever the focus is: it can copy text
                // and nothing else, since it cannot reach an activatable widget.
                return glib::Propagation::Proceed;
            }
            // Everything else reaches the response entry, and only the response entry — settled
            // or not. The entry holds focus only after a click on it, buttons cannot take focus,
            // and Enter and Escape never get this far, so no keystroke can reach anything
            // activatable however the prompt is focused.
            if typing {
                glib::Propagation::Proceed
            } else {
                // Nothing else may turn a keystroke into an activation.
                glib::Propagation::Stop
            }
        }
    });
    keys.connect_key_released({
        let state = state.clone();
        let window = window.downgrade();
        let response = d.response.clone();
        move |_, keyval, keycode, _| {
            let mut st = state.borrow_mut();
            st.held.remove(&keycode);
            // Same exemption as the press, or the releases alone would keep the quiet period
            // restarting while the human types. See `connect_key_pressed`.
            let typing = window
                .upgrade()
                .is_some_and(|w| focus_is_in(&w, response.as_ref()));
            if !typing || is_answer_key(keyval) {
                st.settle.input(Instant::now());
            }
        }
    });
    window.add_controller(keys);

    // Pointer buttons during settling are swallowed here, so the button never sees the press and
    // therefore never activates on a release that lands after settling. Pointer *motion* is
    // deliberately not an input event: with no prompt timeout, a motion-sensitive quiet period
    // would mean a drifting mouse can stop sudo from ever working. Scrolling is likewise left
    // alone so the caller-controlled fields can be read while the prompt settles.
    //
    // A press over a control that does not answer the prompt is let through instead: the quiet
    // period guards the answer, not the whole surface. It still counts as input, so it restarts
    // the quiet period like any other press.
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed({
        let state = state.clone();
        let window = window.downgrade();
        let live = live_controls(d);
        move |gesture, _, x, y| {
            let mut st = state.borrow_mut();
            st.settle.input(Instant::now());
            if st.settle.is_settled() {
                return;
            }
            let over_live = window.upgrade().is_some_and(|w| hits(&w, x, y, &live));
            if !over_live {
                gesture.set_state(gtk::EventSequenceState::Claimed);
            }
        }
    });
    window.add_controller(click);

    d.approve.connect_clicked({
        let state = state.clone();
        move |_| state.borrow_mut().finish(Verdict::Approved)
    });
    d.deny.connect_clicked({
        let state = state.clone();
        move |_| state.borrow_mut().finish(Verdict::Denied)
    });
    if let Some(m) = &d.minimize {
        m.connect_clicked({
            let state = state.clone();
            move |_| state.borrow_mut().minimize()
        });
    }

    // A prompt whose focus the compositor moved away and back must not be live the instant it
    // returns, and a key held across a focus change must not look like a fresh press.
    window.connect_is_active_notify({
        let state = state.clone();
        move |w| {
            let mut st = state.borrow_mut();
            if w.is_active() {
                st.settle.restart(Instant::now());
                drop(st);
                // The toolkit gives a focusable widget focus whenever the window gains it.
                drop_focus(w);
            } else {
                st.held.clear();
            }
        }
    });
}

/// Enter and Escape: the keys that answer the prompt. They are handled by the key controller
/// wherever the focus is, so they are a disturbance to the quiet period wherever the focus is.
fn is_answer_key(keyval: gdk::Key) -> bool {
    matches!(
        keyval,
        gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter | gdk::Key::Escape
    )
}

/// The controls that stay usable through the quiet period: the response box and minimize. Neither
/// answers the prompt — minimize hands the desktop back with the request still pending — so a
/// press on either is let through while the prompt is settling.
fn live_controls(d: &Dialog) -> Vec<gtk::Widget> {
    let mut live = Vec::new();
    if let Some(r) = &d.response {
        live.push(r.clone());
    }
    if let Some(m) = &d.minimize {
        live.push(m.clone().upcast());
    }
    live
}

/// Is the point (window coordinates) over one of `targets`, or over something inside one?
fn hits(window: &gtk::Window, x: f64, y: f64, targets: &[gtk::Widget]) -> bool {
    let Some(picked) = window.pick(x, y, gtk::PickFlags::DEFAULT) else {
        return false;
    };
    targets.iter().any(|t| &picked == t || picked.is_ancestor(t))
}

/// Is the window's focus the response entry, or something inside it? A `GtkEntry` puts the focus
/// on its own inner text widget, so this walks up rather than comparing the two directly.
fn focus_is_in(window: &gtk::Window, target: Option<&gtk::Widget>) -> bool {
    let Some(target) = target else { return false };
    let mut focused = gtk::prelude::GtkWindowExt::focus(window);
    while let Some(w) = focused {
        if &w == target {
            return true;
        }
        focused = w.parent();
    }
    false
}

/// Leave nothing focused, once the toolkit has finished deciding what to focus. Deferred to an
/// idle because the assignment happens after both the map and the active notification.
fn drop_focus(window: &gtk::Window) {
    let window = window.clone();
    glib::idle_add_local_once(move || {
        gtk::prelude::GtkWindowExt::set_focus(&window, None::<&gtk::Widget>);
    });
}

/// Ctrl+C or Ctrl+Insert: copy the selection. Ctrl+A is deliberately not here — a select-all the
/// reader did not aim at hides which field they are about to copy.
fn is_copy(keyval: gdk::Key, modifiers: gdk::ModifierType) -> bool {
    modifiers.contains(gdk::ModifierType::CONTROL_MASK)
        && matches!(keyval, gdk::Key::c | gdk::Key::C | gdk::Key::Insert | gdk::Key::KP_Insert)
}

/// Start the quiet period when a surface is actually presented, not when it was created.
///
/// The frame clock of a Wayland surface is driven by the compositor's frame callbacks after the
/// first commit, so a second tick means the compositor is really presenting this surface. A
/// DPMS-blanked output never gets there, which is the point: the keypress that wakes the display
/// must not approve an invisible prompt.
fn wire_presentation(window: &gtk::Window, d: &Rc<Dialog>, state: &State) {
    let ticks = Cell::new(0u32);
    let d = d.clone();
    let state = state.clone();
    window.add_tick_callback(move |window, _| {
        ticks.set(ticks.get() + 1);
        if ticks.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        let mut st = state.borrow_mut();
        if st.live() {
            log::debug!("surface presented; (re)starting the quiet period");
            st.settle.restart(Instant::now());
            log_geometry(window, &d);
        }
        glib::ControlFlow::Break
    });
}

/// Where the controls landed on this surface, window-relative. The GUI tests derive their click
/// and drag targets from these lines, so layout changes never invalidate the tests.
fn log_geometry(window: &gtk::Window, d: &Dialog) {
    let one = |name: &str, widget: &gtk::Widget| {
        if let Some(b) = widget.compute_bounds(window) {
            log::debug!(
                "geometry: {name} {} {} {} {}",
                b.x().round() as i32,
                b.y().round() as i32,
                b.width().round() as i32,
                b.height().round() as i32
            );
        }
    };
    // The dialog itself, so a test can see how far its contents sit inside its border — which is
    // what a theme loaded above our stylesheet used to flatten. See `CSS_PRIORITY`.
    one("dialog", &d.root);
    one("approve", d.approve.upcast_ref());
    one("deny", d.deny.upcast_ref());
    if let Some(m) = &d.minimize {
        one("minimize", m.upcast_ref());
    }
    if let Some(p) = &d.prominent {
        one("prominent", p.upcast_ref());
    }
    if let Some(r) = &d.response {
        one("response", r);
    }
}

/// The session lock library destroys a window when its monitor goes away or the lock ends.
fn wire_destroy(window: &gtk::Window, state: &State) {
    let state = state.clone();
    window.connect_destroy(move |w| {
        let mut st = state.borrow_mut();
        st.windows.retain(|(other, _)| other != w);
        if st.windows.is_empty() && st.live() {
            // Holding the locks with nothing on screen is worse than failing.
            st.finish(Verdict::Error("no outputs left to present the prompt on".to_string()));
        }
    });
}

fn start_session_lock(ui: &Rc<Ui>, state: &State) -> Result<(), String> {
    // Costs a Wayland roundtrip on the first call, so ask once.
    if !gtk4_session_lock::is_supported() {
        return Err("compositor does not support ext-session-lock-v1".to_string());
    }

    let instance = gtk4_session_lock::Instance::new();

    instance.connect_monitor({
        let ui = ui.clone();
        let state = state.clone();
        move |inst, monitor| {
            log::debug!("lock surface for monitor {:?}", monitor.connector());
            let (window, d) = new_window(&ui, &state);
            inst.assign_window_to_monitor(&window, monitor);
            let mut st = state.borrow_mut();
            if st.locked {
                window.present();
            } else {
                // The `monitor` signal can arrive before `locked`, and can arrive for a lock that
                // then fails; presenting either way is a protocol error waiting to happen.
                st.pending.push(window.clone());
            }
            st.windows.push((window.clone(), d));
            drop(st);
            // The session lock library unmaps and unrefs the window when its monitor goes away,
            // but we hold a strong reference, so no destroy signal arrives. Watch the monitor
            // itself instead.
            watch_monitor(&state, monitor, &window, false);
        }
    });

    instance.connect_locked({
        let state = state.clone();
        move |_| {
            log::debug!("session locked");
            let pending: Vec<gtk::Window> = {
                let mut st = state.borrow_mut();
                st.locked = true;
                st.pending.drain(..).collect()
            };
            for window in pending {
                window.present();
            }
        }
    });

    instance.connect_failed({
        let ui = ui.clone();
        let state = state.clone();
        move |_| {
            if ui.cfg.lock_required {
                // Normal cause: another client already holds the lock, i.e. the screen is
                // locked. Not a fallback trigger — a downgrade would hand back the spoofing
                // exposure session lock was chosen to remove.
                state
                    .borrow_mut()
                    .finish(Verdict::Error("could not take the session lock".to_string()));
            } else if !state.borrow().fallback_done {
                // Defer: `failed` can arrive before `lock()` returns.
                let ui = ui.clone();
                let state = state.clone();
                glib::idle_add_local_once(move || fall_back(&ui, &state));
            }
        }
    });

    instance.connect_unlocked(|_| log::debug!("session unlocked"));
    let started = instance.lock();
    log::debug!("lock() -> {started}");
    if !started {
        return Err("session lock request was refused".to_string());
    }
    ACTIVE_LOCK.with(|l| *l.borrow_mut() = Some(instance));

    // The `monitor` signal fires after `locked`, so "were there any outputs at all?" cannot be
    // answered from either signal. Ask a moment later instead: holding the session lock and the
    // gate's flock with nothing on screen is worse than failing.
    let state = state.clone();
    glib::timeout_add_local_once(Duration::from_millis(2000), move || {
        let mut st = state.borrow_mut();
        if st.windows.is_empty() && st.live() {
            st.finish(Verdict::Error("no outputs to present the prompt on".to_string()));
        }
    });
    Ok(())
}

fn fall_back(ui: &Rc<Ui>, state: &State) {
    {
        let mut st = state.borrow_mut();
        if st.fallback_done || !st.live() {
            return;
        }
        st.fallback_done = true;
    }
    ACTIVE_LOCK.with(|l| *l.borrow_mut() = None);
    log::info!("session lock unavailable; falling back");
    if start_layer(ui, state).is_err() {
        start_toplevel(ui, state);
    }
}

fn start_layer(ui: &Rc<Ui>, state: &State) -> Result<(), String> {
    if !gtk4_layer_shell::is_supported() {
        return Err("compositor does not support zwlr_layer_shell_v1".to_string());
    }
    let display = gdk::Display::default().ok_or("no GDK display")?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        if let Some(monitor) = monitors.item(i).and_downcast::<gdk::Monitor>() {
            add_layer_window(ui, state, &monitor);
        }
    }
    if state.borrow().windows.is_empty() {
        return Err("no outputs to present the prompt on".to_string());
    }
    monitors.connect_items_changed({
        let ui = ui.clone();
        let state = state.clone();
        move |model, pos, _removed, added| {
            for i in pos..pos + added {
                if let Some(monitor) = model.item(i).and_downcast::<gdk::Monitor>() {
                    add_layer_window(&ui, &state, &monitor);
                }
            }
        }
    });
    Ok(())
}

fn add_layer_window(ui: &Rc<Ui>, state: &State, monitor: &gdk::Monitor) {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    let (window, d) = new_window(ui, state);
    window.init_layer_shell();
    window.set_layer(Layer::Overlay);
    window.set_namespace(Some("permission-prompt"));
    window.set_monitor(Some(monitor));
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        window.set_anchor(edge, true);
    }
    window.set_keyboard_mode(KeyboardMode::Exclusive);
    window.present();
    state.borrow_mut().windows.push((window.clone(), d));
    watch_monitor(state, monitor, &window, true);
}

/// Drop a window when its output goes away, and fail rather than hold the locks with nothing on
/// screen once the last one has.
fn watch_monitor(state: &State, monitor: &gdk::Monitor, window: &gtk::Window, destroy: bool) {
    let state = state.clone();
    let window = window.clone();
    monitor.connect_invalidate(move |_| {
        log::debug!("monitor removed");
        let mut st = state.borrow_mut();
        st.windows.retain(|(w, _)| *w != window);
        st.pending.retain(|w| *w != window);
        let last = st.windows.is_empty() && st.live();
        if last {
            st.finish(Verdict::Error("no outputs left to present the prompt on".to_string()));
        }
        drop(st);
        if destroy {
            window.destroy();
        }
    });
}

fn start_toplevel(ui: &Rc<Ui>, state: &State) {
    let (window, d) = new_window(ui, state);
    window.set_default_size(720, 540);
    window.present();
    let state2 = state.clone();
    window.connect_close_request(move |_| {
        state2.borrow_mut().finish(Verdict::Denied);
        glib::Propagation::Stop
    });
    state.borrow_mut().windows.push((window, d));
}
