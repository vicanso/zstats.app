//! Debug builds only: drives the panel from a command file named by
//! `ZSTATS_PROBE`, one command per line, appended while the app runs. The
//! in-process hook the gpui front end's interface checks are made with,
//! rebuilt on AppKit: events are synthesised as `NSEvent`s and handed to
//! the panel's `sendEvent:`, so the user's own pointer never moves and no
//! other app loses focus.
//!
//! Coordinates are points from the panel's top-left corner, as a
//! screenshot of the window reads at 1×.
//!
//! ```text
//! show | show key | hide          on screen (optionally as the key window)
//! click X Y | move X Y            a left click / a pointer move
//! type TEXT                       key events for each character
//! info                            window number, frame, key and first responder
//! quit
//! ```

use crate::nsh::host::{self, Host};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_foundation::{NSPoint, NSProcessInfo, NSString};
use std::env;
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

/// How often the command file is read for new lines.
const POLL: Duration = Duration::from_millis(200);

/// Starts the reader when `ZSTATS_PROBE` names a file; does nothing
/// otherwise.
pub fn start() {
    let Some(path) = env::var_os("ZSTATS_PROBE").map(PathBuf::from) else {
        return;
    };
    tracing::info!(path = %path.display(), "probe reading commands");
    let spawned = thread::Builder::new().name("probe".into()).spawn(move || {
        let mut done = 0;
        loop {
            thread::sleep(POLL);
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            for line in text.lines().skip(done) {
                let line = line.to_owned();
                dispatch2::DispatchQueue::main().exec_async(move || run(&line));
            }
            done = text.lines().count();
        }
    });
    if let Err(e) = spawned {
        tracing::warn!("probe thread: {e}");
    }
}

fn run(line: &str) {
    let mut words = line.split_whitespace();
    let Some(cmd) = words.next() else {
        return;
    };
    let rest: Vec<&str> = words.collect();
    let num = |i: usize| {
        rest.get(i)
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(0.0)
    };
    tracing::info!("probe: {line}");
    host::with_host(|h| match cmd {
        "show" => host::show(h, None, rest.first() == Some(&"key")),
        "hide" => host::hide(h),
        "click" => {
            mouse(h, NSEventType::LeftMouseDown, num(0), num(1));
            mouse(h, NSEventType::LeftMouseUp, num(0), num(1));
        }
        "move" => {
            // A real pointer reaches NSH through its tracking areas; a
            // synthesised move is dispatched by the window, which only
            // forwards moves when asked to.
            h.panel.setAcceptsMouseMovedEvents(true);
            mouse(h, NSEventType::MouseMoved, num(0), num(1));
        }
        "type" => {
            let text = line.split_once(' ').map_or("", |(_, text)| text);
            for ch in text.chars() {
                key(h, NSEventType::KeyDown, ch);
                key(h, NSEventType::KeyUp, ch);
            }
        }
        "info" => info(h),
        "quit" => {
            let mtm = MainThreadMarker::from(&*h.panel);
            NSApplication::sharedApplication(mtm).terminate(None);
        }
        other => tracing::warn!("probe: unknown command {other:?}"),
    });
}

fn info(h: &Host) {
    let frame = h.panel.frame();
    let responder = h
        .panel
        .firstResponder()
        .map(|r| r.class().name().to_string_lossy().into_owned());
    tracing::info!(
        "probe info: window {} · frame ({:.0}, {:.0}) {:.0}×{:.0} · visible {} · key {} · first responder {:?}",
        h.panel.windowNumber(),
        frame.origin.x,
        frame.origin.y,
        frame.size.width,
        frame.size.height,
        h.panel.isVisible(),
        h.panel.isKeyWindow(),
        responder,
    );
}

fn location(h: &Host, x: f64, y: f64) -> NSPoint {
    let height = h
        .panel
        .contentView()
        .map_or(host::PANEL_SIZE.1, |v| v.frame().size.height);
    // The window's base coordinates grow upwards from its bottom edge.
    NSPoint::new(x, height - y)
}

fn uptime() -> f64 {
    NSProcessInfo::processInfo().systemUptime()
}

fn mouse(h: &Host, kind: NSEventType, x: f64, y: f64) {
    let event = NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure(
        kind,
        location(h, x, y),
        NSEventModifierFlags::empty(),
        uptime(),
        h.panel.windowNumber(),
        None,
        0,
        1,
        1.0,
    );
    if let Some(event) = event {
        h.panel.sendEvent(&event);
    }
}

fn key(h: &Host, kind: NSEventType, ch: char) {
    let chars = NSString::from_str(&ch.to_string());
    let event = NSEvent::keyEventWithType_location_modifierFlags_timestamp_windowNumber_context_characters_charactersIgnoringModifiers_isARepeat_keyCode(
        kind,
        NSPoint::new(0.0, 0.0),
        NSEventModifierFlags::empty(),
        uptime(),
        h.panel.windowNumber(),
        None,
        &chars,
        &chars,
        false,
        0,
    );
    if let Some(event) = event {
        h.panel.sendEvent(&event);
    }
}
