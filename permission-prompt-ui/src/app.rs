//! Surface presentation, the input state machine, and the run loop.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;

use crate::dialog::{self, Dialog, DialogSpec};
use crate::settle::{Settle, SettleState};

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

pub struct PromptConfig {
    pub spec: DialogSpec,
    pub mode: SurfaceMode,
    pub settle: Duration,
    pub cap: Duration,
    /// True for the gate: a session lock that cannot be taken is a denial, never a downgrade to a
    /// weaker surface.
    pub lock_required: bool,
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
    Ok(())
}

struct Inner {
    settle: Settle,
    /// Last settle state pushed to the dialogs, so we only touch widgets on a change.
    shown_settled: Option<bool>,
    windows: Vec<(gtk::Window, Rc<Dialog>)>,
    /// Hardware keycodes currently down, so a synthesized autorepeat is never a fresh press.
    held: HashSet<u32>,
    verdict: Option<Verdict>,
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
    /// Records the decision and stops the loop. Deliberately does *not* unlock: the unlock needs a
    /// Wayland roundtrip, during which GTK emits signals whose handlers borrow this same state.
    /// [`run`] unlocks once, outside every callback.
    fn finish(&mut self, verdict: Verdict) {
        if self.verdict.is_some() {
            return;
        }
        log::debug!("verdict: {verdict:?}");
        self.verdict = Some(verdict);
        self.main_loop.quit();
    }
}

type State = Rc<RefCell<Inner>>;

pub fn run(cfg: PromptConfig) -> Verdict {
    install_css();
    let cfg = Rc::new(cfg);
    let state: State = Rc::new(RefCell::new(Inner {
        settle: Settle::new(cfg.settle, cfg.cap),
        shown_settled: None,
        windows: Vec::new(),
        held: HashSet::new(),
        verdict: None,
        locked: false,
        pending: Vec::new(),
        main_loop: glib::MainLoop::new(None, false),
        fallback_done: false,
    }));

    install_signal_handlers();
    install_settle_timer(&state);

    match cfg.mode {
        SurfaceMode::SessionLock => {
            if let Err(e) = start_session_lock(&cfg, &state) {
                // `failed` can arrive before `lock()` returns, and its message is the more
                // specific one.
                let recorded = state.borrow_mut().verdict.take();
                unlock_and_sync();
                return recorded.unwrap_or(Verdict::Error(e));
            }
        }
        SurfaceMode::Layer => {
            if let Err(e) = start_layer(&cfg, &state) {
                return Verdict::Error(e);
            }
        }
        SurfaceMode::Toplevel => start_toplevel(&cfg, &state),
        SurfaceMode::Auto => {
            if start_session_lock(&cfg, &state).is_err() {
                fall_back(&cfg, &state);
            }
        }
    }

    let main_loop = state.borrow().main_loop.clone();
    if state.borrow().verdict.is_none() {
        main_loop.run();
    }

    // Every survivable exit path lands here: approval (before the exec), denial, the settle cap,
    // the signal handlers, and every operational error after lock() succeeded.
    unlock_and_sync();

    // Tear the lock reference down after the loop, so nothing can re-enter it.
    ACTIVE_LOCK.with(|l| {
        if let Ok(mut l) = l.try_borrow_mut() {
            *l = None;
        }
    });

    let verdict = state.borrow_mut().verdict.take();
    verdict.unwrap_or_else(|| Verdict::Error("prompt exited without a decision".to_string()))
}

fn install_css() {
    let provider = gtk::CssProvider::new();
    provider.load_from_string(dialog::CSS);
    if let Some(display) = gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

/// Set by the signal handler, read by the settle timer. glib no longer wraps
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

fn install_settle_timer(state: &State) {
    let state = state.clone();
    glib::timeout_add_local(Duration::from_millis(50), move || {
        let mut st = state.borrow_mut();
        if st.verdict.is_some() {
            return glib::ControlFlow::Break;
        }
        let sig = PENDING_SIGNAL.swap(0, Ordering::Relaxed);
        if sig != 0 {
            st.finish(Verdict::DeniedSignal(sig));
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
                    for (_, d) in &st.windows {
                        d.set_settled(settled);
                    }
                }
                glib::ControlFlow::Continue
            }
        }
    });
}

fn new_window(cfg: &Rc<PromptConfig>, state: &State) -> (gtk::Window, Rc<Dialog>) {
    let window = gtk::Window::new();
    window.add_css_class("pp-window");
    window.set_title(Some(cfg.spec.heading));

    let d = Rc::new(dialog::build(&cfg.spec));
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

    wire_input(&window, &d, state);
    wire_presentation(&window, state);
    wire_destroy(&window, state);
    (window, d)
}

/// Approval requires a fresh physical key down, or a click that began after settling.
fn wire_input(window: &gtk::Window, d: &Rc<Dialog>, state: &State) {
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    keys.connect_key_pressed({
        let state = state.clone();
        move |_, keyval, keycode, _| {
            let mut st = state.borrow_mut();
            let now = Instant::now();
            // Insert before the settled check: a key held from before settling must still be
            // known, so its autorepeats are never mistaken for a fresh press afterwards.
            let repeat = !st.held.insert(keycode);
            st.settle.input(now);
            if st.settle.is_settled() && !repeat {
                match keyval {
                    gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::ISO_Enter => {
                        st.finish(Verdict::Approved)
                    }
                    gdk::Key::Escape => st.finish(Verdict::Denied),
                    _ => {}
                }
            }
            // Nothing else may turn a keystroke into an activation.
            glib::Propagation::Stop
        }
    });
    keys.connect_key_released({
        let state = state.clone();
        move |_, _keyval, keycode, _| {
            let mut st = state.borrow_mut();
            st.held.remove(&keycode);
            st.settle.input(Instant::now());
        }
    });
    window.add_controller(keys);

    // Pointer buttons during settling are swallowed here, so the button never sees the press and
    // therefore never activates on a release that lands after settling. Pointer *motion* is
    // deliberately not an input event: with no prompt timeout, a motion-sensitive quiet period
    // would mean a drifting mouse can stop sudo from ever working. Scrolling is likewise left
    // alone so the caller-controlled fields can be read while the prompt settles.
    let click = gtk::GestureClick::new();
    click.set_button(0);
    click.set_propagation_phase(gtk::PropagationPhase::Capture);
    click.connect_pressed({
        let state = state.clone();
        move |gesture, _, _, _| {
            let mut st = state.borrow_mut();
            st.settle.input(Instant::now());
            if !st.settle.is_settled() {
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

    // A prompt whose focus the compositor moved away and back must not be live the instant it
    // returns, and a key held across a focus change must not look like a fresh press.
    window.connect_is_active_notify({
        let state = state.clone();
        move |w| {
            let mut st = state.borrow_mut();
            if w.is_active() {
                st.settle.restart(Instant::now());
            } else {
                st.held.clear();
            }
        }
    });
}

/// Start the quiet period when a surface is actually presented, not when it was created.
///
/// The frame clock of a Wayland surface is driven by the compositor's frame callbacks after the
/// first commit, so a second tick means the compositor is really presenting this surface. A
/// DPMS-blanked output never gets there, which is the point: the keypress that wakes the display
/// must not approve an invisible prompt.
fn wire_presentation(window: &gtk::Window, state: &State) {
    let ticks = Cell::new(0u32);
    let state = state.clone();
    window.add_tick_callback(move |_, _| {
        ticks.set(ticks.get() + 1);
        if ticks.get() < 2 {
            return glib::ControlFlow::Continue;
        }
        let mut st = state.borrow_mut();
        if st.verdict.is_none() {
            log::debug!("surface presented; (re)starting the quiet period");
            st.settle.restart(Instant::now());
        }
        glib::ControlFlow::Break
    });
}

/// The session lock library destroys a window when its monitor goes away or the lock ends.
fn wire_destroy(window: &gtk::Window, state: &State) {
    let state = state.clone();
    window.connect_destroy(move |w| {
        let mut st = state.borrow_mut();
        st.windows.retain(|(other, _)| other != w);
        if st.windows.is_empty() && st.verdict.is_none() {
            // Holding the locks with nothing on screen is worse than failing.
            st.finish(Verdict::Error("no outputs left to present the prompt on".to_string()));
        }
    });
}

fn start_session_lock(cfg: &Rc<PromptConfig>, state: &State) -> Result<(), String> {
    // Costs a Wayland roundtrip on the first call, so ask once.
    if !gtk4_session_lock::is_supported() {
        return Err("compositor does not support ext-session-lock-v1".to_string());
    }

    let instance = gtk4_session_lock::Instance::new();

    instance.connect_monitor({
        let cfg = cfg.clone();
        let state = state.clone();
        move |inst, monitor| {
            log::debug!("lock surface for monitor {:?}", monitor.connector());
            let (window, d) = new_window(&cfg, &state);
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
        let cfg = cfg.clone();
        let state = state.clone();
        move |_| {
            if cfg.lock_required {
                // Normal cause: another client already holds the lock, i.e. the screen is
                // locked. Not a fallback trigger — a downgrade would hand back the spoofing
                // exposure session lock was chosen to remove.
                state
                    .borrow_mut()
                    .finish(Verdict::Error("could not take the session lock".to_string()));
            } else if !state.borrow().fallback_done {
                // Defer: `failed` can arrive before `lock()` returns.
                let cfg = cfg.clone();
                let state = state.clone();
                glib::idle_add_local_once(move || fall_back(&cfg, &state));
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
        if st.windows.is_empty() && st.verdict.is_none() {
            st.finish(Verdict::Error("no outputs to present the prompt on".to_string()));
        }
    });
    Ok(())
}

fn fall_back(cfg: &Rc<PromptConfig>, state: &State) {
    {
        let mut st = state.borrow_mut();
        if st.fallback_done || st.verdict.is_some() {
            return;
        }
        st.fallback_done = true;
    }
    ACTIVE_LOCK.with(|l| *l.borrow_mut() = None);
    log::info!("session lock unavailable; falling back");
    if start_layer(cfg, state).is_err() {
        start_toplevel(cfg, state);
    }
}

fn start_layer(cfg: &Rc<PromptConfig>, state: &State) -> Result<(), String> {
    if !gtk4_layer_shell::is_supported() {
        return Err("compositor does not support zwlr_layer_shell_v1".to_string());
    }
    let display = gdk::Display::default().ok_or("no GDK display")?;
    let monitors = display.monitors();
    for i in 0..monitors.n_items() {
        if let Some(monitor) = monitors.item(i).and_downcast::<gdk::Monitor>() {
            add_layer_window(cfg, state, &monitor);
        }
    }
    if state.borrow().windows.is_empty() {
        return Err("no outputs to present the prompt on".to_string());
    }
    monitors.connect_items_changed({
        let cfg = cfg.clone();
        let state = state.clone();
        move |model, pos, _removed, added| {
            for i in pos..pos + added {
                if let Some(monitor) = model.item(i).and_downcast::<gdk::Monitor>() {
                    add_layer_window(&cfg, &state, &monitor);
                }
            }
        }
    });
    Ok(())
}

fn add_layer_window(cfg: &Rc<PromptConfig>, state: &State, monitor: &gdk::Monitor) {
    use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
    let (window, d) = new_window(cfg, state);
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
        let last = st.windows.is_empty() && st.verdict.is_none();
        if last {
            st.finish(Verdict::Error("no outputs left to present the prompt on".to_string()));
        }
        drop(st);
        if destroy {
            window.destroy();
        }
    });
}

fn start_toplevel(cfg: &Rc<PromptConfig>, state: &State) {
    let (window, d) = new_window(cfg, state);
    window.set_default_size(720, 540);
    window.present();
    let state2 = state.clone();
    window.connect_close_request(move |_| {
        state2.borrow_mut().finish(Verdict::Denied);
        glib::Propagation::Stop
    });
    state.borrow_mut().windows.push((window, d));
}
