//! Desktop notifications: a word from the system when something finishes while the
//! window isn't in front. Best effort; nothing is said if the desktop can't show one.

/// Show a notification, off the UI thread.
pub fn send(summary: String, body: String) {
    std::thread::spawn(move || {
        if let Err(e) = show(&summary, &body) {
            eprintln!("notification: {e}");
        }
    });
}

#[cfg(target_os = "linux")]
fn show(summary: &str, body: &str) -> anyhow::Result<()> {
    use std::collections::HashMap;
    use zbus::zvariant::Value;
    let conn = zbus::blocking::Connection::session()?;
    let hints: HashMap<&str, Value> = HashMap::from([("desktop-entry", Value::from("plasmid-gpui"))]);
    conn.call_method(
        Some("org.freedesktop.Notifications"),
        "/org/freedesktop/Notifications",
        Some("org.freedesktop.Notifications"),
        "Notify",
        &("Plasmid Editor", 0u32, "", summary, body, Vec::<&str>::new(), hints, -1i32),
    )?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn show(summary: &str, body: &str) -> anyhow::Result<()> {
    // AppleScript string literals: escape backslashes and quotes.
    let q = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let script = format!("display notification {} with title \"Plasmid Editor\" subtitle {}", q(body), q(summary));
    let status = std::process::Command::new("osascript").arg("-e").arg(script).status()?;
    anyhow::ensure!(status.success(), "osascript exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Puts a real notification on screen: `cargo test notify -- --ignored`.
    #[test]
    #[ignore]
    fn shows_one() {
        super::show("BLASTX finished", "Test notification from plasmid-gpui").unwrap();
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn show(_summary: &str, _body: &str) -> anyhow::Result<()> {
    Ok(())
}
