//! One process per session, and the one thing a second launch can ask it
//! to do.
//!
//! Linux needs an entry point that does not depend on the tray: the host
//! may not implement StatusNotifier at all, and a release build destroys
//! the panel on focus loss, so without one there is no way back to it
//! (`docs/omarchy-port.md` 阶段 2). A compositor keybinding is that entry
//! point — but Hyprland binds run a *command*, not an IPC call, so
//! `zstats --toggle` has to reach the process already running instead of
//! starting a second one. Two processes would mean two resident
//! collectors sampling the same machine: the same double-collection
//! CLAUDE.md warns about for `zstats serve`, except self-inflicted and
//! on every keypress.
//!
//! The rendezvous is a unix socket in `$XDG_RUNTIME_DIR` — a directory
//! systemd creates 0700 per user and wipes at logout, so the socket is
//! private without a `chmod` and never outlives the session that made
//! it. **There is deliberately no fallback when that variable is unset.**
//! `/tmp` is a path another user can bind first, and a single-instance
//! lock a stranger can hold is worse than no lock: the app would hand its
//! keypresses to whoever got there first. Without the variable this
//! degrades to "no single instance" — one log line, and `--toggle` starts
//! its own panel.
//!
//! Linux-only on purpose. macOS gets single-instance from LaunchServices
//! and always has a menu bar item to click, so there is nothing here it
//! needs and nothing here that could change how it behaves.

use gpui::App;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;
use std::{env, fs, thread};

/// Named for the binary rather than the crate: what a user finds in
/// `$XDG_RUNTIME_DIR` should be recognisable as the thing they launched.
const SOCKET_NAME: &str = "zstats-app.sock";

/// How long the listener waits for a client that has connected but not
/// spoken. Accepts are served one at a time on a single thread, so a
/// client that connects and stalls would otherwise wedge every later
/// keypress. Two seconds is far beyond a local write of one word and far
/// under the patience of somebody pressing a key.
const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// How this process was started, which decides two different things: what
/// to say to an instance that is already running, and whether to put the
/// panel on screen if there is none.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Invocation {
    /// No arguments — a plain launch, which is also what autostart does.
    ///
    /// A *second* plain launch is still routed to the first (two
    /// collectors is the thing this module exists to prevent) and asks it
    /// to show itself, because somebody who launched the app twice is
    /// asking to see it. But a *first* plain launch must not open the
    /// panel: that is the login path, and a panel nobody asked for on
    /// every boot is exactly what the release build's tray-resident start
    /// avoids.
    Launch,
    /// `--toggle`, the keybinding.
    Toggle,
}

/// Something to print and stop on, rather than a way to start.
pub struct Message {
    pub text: String,
    /// 0 when the user asked for this, 2 when they got it wrong — so a
    /// mistyped keybinding fails its shell, instead of looking like a
    /// successful launch that happened to show nothing.
    pub code: i32,
}

impl Invocation {
    /// Unknown flags are refused rather than ignored: a typo in a
    /// `hyprland.conf` bind would otherwise silently launch a second
    /// panel every press instead of toggling the one already up, and the
    /// symptom (the key "stops working" after the first press) points
    /// nowhere near the cause.
    pub fn from_args() -> Result<Self, Message> {
        let mut args = env::args().skip(1);
        let Some(arg) = args.next() else {
            return Ok(Invocation::Launch);
        };
        let refuse = |text: String| {
            Err(Message {
                text: format!("{}\n\n{}", text, usage()),
                code: 2,
            })
        };
        if args.next().is_some() {
            return refuse(format!("{}: one argument at most", crate::APP_NAME));
        }
        match arg.as_str() {
            "--toggle" => Ok(Invocation::Toggle),
            "--help" | "-h" => Err(Message {
                text: usage(),
                code: 0,
            }),
            "--version" | "-V" => Err(Message {
                text: format!("{} {}", crate::APP_NAME, env!("CARGO_PKG_VERSION")),
                code: 0,
            }),
            other => refuse(format!("{}: unknown argument {other}", crate::APP_NAME)),
        }
    }

    /// What to send to the instance that is already running.
    fn wire(self) -> &'static str {
        match self {
            Invocation::Launch => "show",
            Invocation::Toggle => "toggle",
        }
    }

    /// Whether *this* process, having found no instance to hand the
    /// command to, should open the panel once it is up. A keybinding's
    /// first press has to put something on screen — a key that silently
    /// starts a background process reads as a key that did nothing.
    pub fn opens_the_panel(self) -> bool {
        self == Invocation::Toggle
    }
}

pub fn usage() -> String {
    format!(
        "{name} — menu-bar metrics panel\n\n\
         Usage: {bin} [--toggle]\n\n\
         With no arguments, starts the panel (or shows the one already\n\
         running). --toggle opens or closes it — bind that to a key:\n\n\
         \x20   bind = SUPER, M, exec, {bin} --toggle\n",
        name = crate::APP_NAME,
        bin = env!("CARGO_BIN_NAME"),
    )
}

/// What this process turned out to be.
pub enum Role {
    /// The instance. Serve this listener with [`serve`].
    Instance(UnixListener),
    /// An instance was already running and took the command; this process
    /// has nothing left to do and should exit.
    Delivered,
    /// No rendezvous available. Run anyway, without single-instance.
    Alone,
}

fn socket_path() -> Option<PathBuf> {
    let dir = env::var_os("XDG_RUNTIME_DIR")?;
    Some(PathBuf::from(dir).join(SOCKET_NAME))
}

/// Become the instance, or hand `invocation` to the one already running.
///
/// Binds *first* and only connects if that fails, which is what keeps a
/// live socket from being unlinked out from under its owner: the path is
/// removed only after a connect has proved nobody answers on it. The
/// reverse order (connect, then unlink-and-bind) has a window where two
/// launches both decide the socket is stale.
pub fn claim(invocation: Invocation) -> Role {
    let Some(path) = socket_path() else {
        tracing::warn!(
            "XDG_RUNTIME_DIR is unset: running without a single-instance lock, \
             so --toggle will start its own panel"
        );
        return Role::Alone;
    };
    claim_at(&path, invocation)
}

/// [`claim`] with the rendezvous named, so the whole handshake — take,
/// hand over, recover a socket nobody answers on — can be tested against
/// a temporary path instead of the session's real one. Reaching it
/// through `$XDG_RUNTIME_DIR` would mean a test that moves an environment
/// variable out from under every other test in the process.
fn claim_at(path: &Path, invocation: Invocation) -> Role {
    match UnixListener::bind(path) {
        Ok(listener) => return Role::Instance(listener),
        // Anything else — a read-only runtime dir, a name too long — is
        // not a second instance, and guessing that it is would exit a
        // launch that should have run.
        Err(e) if e.kind() != std::io::ErrorKind::AddrInUse => {
            tracing::warn!(?path, "could not take the single-instance socket: {e}");
            return Role::Alone;
        }
        Err(_) => {}
    }
    // Something holds the path. Either a live instance, or a socket left
    // behind by one that died without unlinking it.
    match UnixStream::connect(path) {
        Ok(stream) => {
            deliver(stream, invocation);
            Role::Delivered
        }
        Err(_) => {
            tracing::info!(?path, "clearing a socket left by a previous run");
            if let Err(e) = fs::remove_file(path) {
                tracing::warn!(?path, "could not clear it: {e}");
                return Role::Alone;
            }
            match UnixListener::bind(path) {
                Ok(listener) => Role::Instance(listener),
                Err(e) => {
                    tracing::warn!(?path, "could not take the socket after clearing it: {e}");
                    Role::Alone
                }
            }
        }
    }
}

/// Hand the command over and say so in the log.
///
/// A failed write still counts as delivered. We are connected, so an
/// instance is there; falling through to "start a second one" would add a
/// resident collector to fix a keypress, which is the trade this module
/// exists to refuse. The next press either reaches it or finds the socket
/// dead and starts cleanly.
fn deliver(mut stream: UnixStream, invocation: Invocation) {
    match writeln!(stream, "{}", invocation.wire()) {
        Ok(()) => tracing::info!(?invocation, "handed to the running instance"),
        Err(e) => tracing::warn!(
            ?invocation,
            "connected to the running instance but could not deliver: {e}"
        ),
    }
}

/// Serve the socket for the process lifetime.
///
/// One blocking thread on `accept`, funnelled onto the main-thread
/// executor — the same shape `tray.rs` uses for its two event receivers,
/// and for the same reason: these are blocking receivers that must not
/// sit on gpui's loop.
pub fn serve(listener: UnixListener, cx: &mut App) {
    let (tx, rx) = smol::channel::unbounded::<Invocation>();
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let Some(invocation) = read_one(stream) else {
                continue;
            };
            if tx.send_blocking(invocation).is_err() {
                return;
            }
        }
    });

    cx.spawn(async move |cx| {
        // Ends on its own: once the app shuts down this task stops being
        // polled and the sender thread's channel drops.
        while let Ok(invocation) = rx.recv().await {
            tracing::info!(?invocation, "received from a second launch");
            cx.update(|cx| match invocation {
                Invocation::Toggle => crate::toggle_main_window(cx, None),
                Invocation::Launch => crate::show_main_window(cx),
            });
        }
    })
    .detach();
}

/// One line, one command. Anything else is ignored rather than answered:
/// this socket speaks to our own binary, and a stranger on it is not
/// somebody to hold a conversation with.
fn read_one(stream: UnixStream) -> Option<Invocation> {
    let _ = stream.set_read_timeout(Some(READ_TIMEOUT));
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).ok()?;
    match line.trim() {
        "toggle" => Some(Invocation::Toggle),
        "show" => Some(Invocation::Launch),
        other => {
            tracing::warn!("ignoring an unknown command on the socket: {other:?}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    /// The wire word and the reader have to agree, and they are written
    /// in two different places — a rename on one side would otherwise
    /// turn every keypress into an ignored line in the log.
    #[test]
    fn every_invocation_survives_the_round_trip() {
        for invocation in [Invocation::Launch, Invocation::Toggle] {
            let line = format!("{}\n", invocation.wire());
            let (client, server) = UnixStream::pair().expect("socketpair");
            let mut client = client;
            client.write_all(line.as_bytes()).expect("write");
            drop(client);
            assert_eq!(read_one(server), Some(invocation));
        }
    }

    /// The whole handshake, against a temporary rendezvous: the first
    /// launch takes the socket, the second hands its command over instead
    /// of binding a second time, and the instance reads back exactly what
    /// was sent. Then the case that decides whether a crash locks the
    /// user out — a socket file whose owner is gone answers nothing, and
    /// has to be cleared rather than mistaken for a live instance.
    #[test]
    fn a_second_launch_hands_over_and_a_dead_socket_is_reclaimed() {
        let dir = env::temp_dir().join(format!("zstats-ipc-{}", process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(SOCKET_NAME);

        let Role::Instance(listener) = claim_at(&path, Invocation::Launch) else {
            panic!("the first launch is the instance");
        };
        assert!(
            matches!(claim_at(&path, Invocation::Toggle), Role::Delivered),
            "a second launch must not bind a second socket"
        );
        let (stream, _) = listener.accept().expect("the delivered connection");
        assert_eq!(read_one(stream), Some(Invocation::Toggle));

        // The owner goes away without unlinking — the file survives, and
        // connecting to it is refused.
        drop(listener);
        assert!(
            matches!(claim_at(&path, Invocation::Launch), Role::Instance(_)),
            "a socket nobody answers on is this launch's to take"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    /// Only `--toggle` toggles, and only a bare launch is a launch.
    /// Everything else has to be refused loudly: a typo in a keybinding
    /// that silently launched a second panel is the failure this guards.
    #[test]
    fn only_a_bare_launch_and_toggle_are_accepted() {
        assert!(!Invocation::Launch.opens_the_panel());
        assert!(Invocation::Toggle.opens_the_panel());
        assert_ne!(Invocation::Launch.wire(), Invocation::Toggle.wire());
        assert!(usage().contains("--toggle"));
    }
}
