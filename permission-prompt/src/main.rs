//! `permission-prompt` — a generic yes/no presenter.
//!
//! It never executes a command, receives no passwordless sudo permission, and is **not** an
//! authorization boundary. Its caller owns its prose and interprets its exit status. An
//! unprivileged caller can spoof its own generic request, which grants nothing; a future privileged
//! service that needs generic confirmation must define its own authenticated request protocol and
//! must not treat arbitrary invocation of this binary as authorization.

use clap::Parser;
use permission_prompt_ui::dialog::{DialogSpec, Field, Style};
use permission_prompt_ui::{Escaped, PromptConfig, SurfaceMode, Untrusted, Verdict};
use permission_prompt_ui::{SETTLE, SETTLE_CAP_MULTIPLE};

/// Exit codes. This is not a sudo wrapper, so no exit-status collision exists.
const EXIT_APPROVED: i32 = 0;
const EXIT_DENIED: i32 = 1;
const EXIT_ERROR: i32 = 3;

const HEADING: &str = "Permission requested";
const APPROVE: &str = "Allow";
const DENY: &str = "Deny";
const FOOTER: &str = "Enter allows · Escape denies";

/// Compiled-in, so the presenter can never be dressed up as the sudo gate or as a lock screen.
const DISCLAIMER: &[&str] = &[
    "An application asked for this. It is not a sudo prompt and nothing here runs as root.",
    "Your session is not locked: denying returns you straight to the desktop.",
];

#[derive(Parser)]
#[command(
    about = "Show a yes/no permission prompt and report the answer as an exit status",
    long_about = None,
)]
struct Args {
    /// Short caller-supplied title.
    #[arg(long)]
    title: Option<String>,

    /// Caller-supplied message.
    #[arg(long)]
    body: Option<String>,

    /// Extra caller-supplied line. Repeatable.
    #[arg(long = "detail")]
    details: Vec<String>,

    /// Icon name shown beside the heading.
    #[arg(long)]
    icon: Option<String>,

    /// Surface to present on. `auto` tries session lock, then layer shell, then an xdg toplevel.
    #[arg(long, default_value = "auto")]
    surface: String,

    /// Log the surface-mode and settling decisions.
    #[arg(long, short)]
    verbose: bool,
}

fn main() {
    let args = Args::parse();
    env_logger::Builder::from_env(
        env_logger::Env::default()
            .default_filter_or(if args.verbose { "debug" } else { "info" }),
    )
    .init();

    let Some(mode) = SurfaceMode::parse(&args.surface) else {
        eprintln!("permission-prompt: --surface must be auto, session-lock, layer or toplevel");
        std::process::exit(EXIT_ERROR);
    };

    if let Err(e) = permission_prompt_ui::init() {
        eprintln!("permission-prompt: {e}");
        std::process::exit(EXIT_ERROR);
    }
    // Being unprivileged does not make stranding the session acceptable: session-lock mode gets the
    // same panic hook and signal handling as the gate. A caller that would rather have a plain
    // window than that failure mode should pass --surface=toplevel.
    permission_prompt_ui::install_panic_hook();

    let verdict = permission_prompt_ui::run(PromptConfig {
        spec: spec(&args),
        mode,
        settle: SETTLE,
        cap_multiple: SETTLE_CAP_MULTIPLE,
        // Not a security boundary, and nothing to fail closed about: a lock that cannot be taken
        // falls through to the next mode.
        lock_required: false,
    });

    log::info!("verdict: {verdict:?}");
    std::process::exit(match verdict {
        Verdict::Approved => EXIT_APPROVED,
        Verdict::Denied | Verdict::DeniedSettleCap | Verdict::DeniedSignal(_) => EXIT_DENIED,
        Verdict::Error(e) => {
            eprintln!("permission-prompt: {e}");
            EXIT_ERROR
        }
    });
}

/// Caller prose is visually quarantined: escaped through the same `Untrusted` path and shown in the
/// same bounded, overflow-marked viewports as the gate's caller-controlled fields.
fn spec(args: &Args) -> DialogSpec {
    let mut fields = vec![Field::trusted(
        "About this prompt",
        DISCLAIMER.iter().copied().map(Escaped::literal).collect(),
    )];

    if let Some(title) = &args.title {
        fields.push(Field::untrusted("Title", vec![escape(title)]));
    }
    if let Some(body) = &args.body {
        fields.push(Field::untrusted("Message", lines(body)).expanding());
    }
    if !args.details.is_empty() {
        fields.push(Field::untrusted(
            "Details",
            args.details.iter().flat_map(|d| lines(d)).collect(),
        ));
    }

    DialogSpec {
        style: Style::Generic,
        heading: HEADING,
        icon: args.icon.clone().or_else(|| Some("dialog-question-symbolic".to_string())),
        fields,
        approve: APPROVE,
        deny: DENY,
        footer: FOOTER,
    }
}

fn escape(s: &str) -> Escaped {
    Escaped::of(&Untrusted::from_bytes(s.as_bytes().to_vec()))
}

/// Newlines in caller prose become separate rendered lines rather than escapes, since here the
/// caller is writing prose rather than argv tokens.
fn lines(s: &str) -> Vec<Escaped> {
    s.split('\n').map(escape).collect()
}
