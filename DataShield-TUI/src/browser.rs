//! The user's browser: which one is the default, and how a URL is handed to it.
//!
//! Two jobs, both best-effort and both portable across Linux, macOS and
//! Windows:
//!
//!   * [`open_url`] — hand a URL (or a file) to the desktop's opener, detached,
//!     with its output thrown away. A TUI must never block on a browser
//!     starting up, and a chatty opener must never scribble over the frame.
//!   * [`default_browser`] — which browser the desktop would open it in, and
//!     which FAMILY it belongs to. The family is what decides which extension
//!     store the "install extension" settings row points at: the Chromium
//!     family shares the Chrome Web Store, the Gecko family shares Mozilla's
//!     add-ons site.
//!
//! The detection is three small parsers over what each platform stores
//! (an XDG `.desktop` name, a LaunchServices plist, a registry ProgId) — pure
//! functions, unit-tested with fixtures, never with the machine the suite runs
//! on. A platform that answers nothing is [`BrowserFamily::Unknown`]; nothing
//! here ever panics or blocks on a tool that is missing.

use std::process::Command;

/// The lineage a browser belongs to — what its extension store is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum BrowserFamily {
    /// Firefox and its forks (Zen, LibreWolf, Waterfox, Floorp…): Mozilla add-ons.
    Gecko,
    /// Chrome, Chromium, Brave, Edge, Vivaldi, Opera, Arc…: the Chrome Web Store.
    Chromium,
    /// Safari: a different packaging entirely (out of scope for the extension).
    Safari,
    Unknown,
}

impl BrowserFamily {
    /// The family's name for a settings row, untranslated on purpose: it is a
    /// product name, the same word in every language.
    pub fn label(self) -> &'static str {
        match self {
            BrowserFamily::Gecko => "Firefox",
            BrowserFamily::Chromium => "Chrome",
            BrowserFamily::Safari => "Safari",
            BrowserFamily::Unknown => "?",
        }
    }
}

/// What the desktop said its default browser is.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct DefaultBrowser {
    /// A readable name — `zen`, `firefox`, `brave-browser`, `com.google.Chrome`,
    /// `BraveHTML` — whatever the platform spelled it as, trimmed of `.desktop`.
    pub name: String,
    pub family: BrowserFamily,
}

/// Classifies a browser from any of the identifiers the three platforms hand
/// out: a `.desktop` file name, a macOS bundle id, a Windows ProgId or a plain
/// executable name. Case-insensitive substring tests — the identifiers are
/// free-form, and every fork keeps its parent's name somewhere in them.
pub fn classify(ident: &str) -> BrowserFamily {
    let id = ident.to_ascii_lowercase();
    const GECKO: [&str; 8] =
        ["firefox", "zen", "librewolf", "waterfox", "floorp", "mozilla", "tor browser", "mullvad"];
    const CHROMIUM: [&str; 9] =
        ["chrom", "brave", "edge", "vivaldi", "opera", "arc", "thorium", "ungoogled", "yandex"];
    if id.contains("safari") {
        return BrowserFamily::Safari;
    }
    if GECKO.iter().any(|k| id.contains(k)) {
        return BrowserFamily::Gecko;
    }
    if CHROMIUM.iter().any(|k| id.contains(k)) {
        return BrowserFamily::Chromium;
    }
    BrowserFamily::Unknown
}

/// Linux: the `.desktop` entry `xdg-settings get default-web-browser` prints
/// (`zen.desktop`, `firefox.desktop`, `brave-browser.desktop`…).
pub fn parse_xdg(output: &str) -> Option<DefaultBrowser> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    let name = line.strip_suffix(".desktop").unwrap_or(line).to_string();
    Some(DefaultBrowser { family: classify(&name), name })
}

/// macOS: the LaunchServices handlers plist (converted to JSON with
/// `plutil -convert json -o - <file>`): the entry whose `LSHandlerURLScheme`
/// is `http` names the bundle id under `LSHandlerRoleAll`.
pub fn parse_launchservices_json(json: &str) -> Option<DefaultBrowser> {
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let handlers = value.get("LSHandlers")?.as_array()?;
    let bundle = handlers
        .iter()
        .find(|h| h.get("LSHandlerURLScheme").and_then(|s| s.as_str()) == Some("http"))
        .and_then(|h| {
            h.get("LSHandlerRoleAll")
                .or_else(|| h.get("LSHandlerRoleViewer"))
                .and_then(|s| s.as_str())
        })?;
    Some(DefaultBrowser { family: classify(bundle), name: bundle.to_string() })
}

/// Windows: the `ProgId` value of
/// `HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice`
/// as `reg query` prints it (`    ProgId    REG_SZ    BraveHTML`).
pub fn parse_reg_query(output: &str) -> Option<DefaultBrowser> {
    let line = output.lines().find(|l| l.trim_start().starts_with("ProgId"))?;
    let progid = line.split_whitespace().nth(2)?;
    Some(DefaultBrowser { family: classify(progid), name: progid.to_string() })
}

/// The desktop's default browser, as far as this platform will say. `None`
/// when the question cannot be asked (tool missing, nothing configured).
pub fn default_browser() -> Option<DefaultBrowser> {
    #[cfg(target_os = "linux")]
    {
        let out = Command::new("xdg-settings").args(["get", "default-web-browser"]).output().ok()?;
        return parse_xdg(&String::from_utf8_lossy(&out.stdout));
    }
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")?;
        let plist = std::path::PathBuf::from(home)
            .join("Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist");
        let out = Command::new("plutil")
            .args(["-convert", "json", "-o", "-"])
            .arg(&plist)
            .output()
            .ok()?;
        return parse_launchservices_json(&String::from_utf8_lossy(&out.stdout)).or(Some(
            DefaultBrowser { name: "Safari".to_string(), family: BrowserFamily::Safari },
        ));
    }
    #[cfg(target_os = "windows")]
    {
        let out = Command::new("reg")
            .args([
                "query",
                r"HKCU\Software\Microsoft\Windows\Shell\Associations\UrlAssociations\http\UserChoice",
                "/v",
                "ProgId",
            ])
            .output()
            .ok()?;
        return parse_reg_query(&String::from_utf8_lossy(&out.stdout));
    }
    #[allow(unreachable_code)]
    None
}

/// Hands `target` (a URL or a path) to the desktop's opener, DETACHED: no
/// waiting, no output on the frame, and the child is reaped by a thread of its
/// own (a `Child` has no reaping `Drop`, so without it every open would leave a
/// zombie until the app quits). `Err` = no opener could be spawned at all (a
/// bare TTY, no desktop session); the caller toasts.
pub fn open_url(target: &str) -> std::io::Result<()> {
    let mut cmd = opener_command(target);
    let mut child = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// The platform's opener, with `target` as its argument.
fn opener_command(target: &str) -> Command {
    #[cfg(target_os = "macos")]
    {
        let mut c = Command::new("open");
        c.arg(target);
        return c;
    }
    #[cfg(target_os = "windows")]
    {
        // `rundll32 url.dll,FileProtocolHandler` takes the URL as one argument,
        // which `cmd /c start` would not (its quoting rules eat `&` and `#`).
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(target);
        return c;
    }
    #[allow(unreachable_code)]
    {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    }
}

// ---- getting the user's attention ------------------------------------------

/// A desktop notification, best-effort and detached: `notify-send` on Linux,
/// `osascript` on macOS, nothing on Windows yet. A missing tool is not an
/// error — the status line said the same thing already.
pub fn notify(title: &str, body: &str) {
    let mut cmd = {
        #[cfg(target_os = "macos")]
        {
            let mut c = Command::new("osascript");
            c.arg("-e").arg(format!(
                "display notification \"{}\" with title \"{}\"",
                body.replace('"', "'"),
                title.replace('"', "'")
            ));
            c
        }
        #[cfg(target_os = "linux")]
        {
            let mut c = Command::new("notify-send");
            c.arg("--app-name=DataShield").arg(title).arg(body);
            c
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        {
            let _ = (title, body);
            return;
        }
    };
    if let Ok(mut child) = cmd
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
}

/// Brings DataShield's own window forward, best-effort: inside the dedicated
/// kitty window (`dist/datashield`, remote control on) `kitty @ focus-window`
/// does it; on Hyprland the compositor can be asked by window class. Anywhere
/// else this quietly does nothing — the notification above still lands.
pub fn bring_to_front() {
    let spawn = |mut c: Command| {
        if let Ok(mut child) = c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    };
    if std::env::var_os("KITTY_WINDOW_ID").is_some() {
        let mut c = Command::new("kitty");
        c.args(["@", "focus-window"]);
        spawn(c);
    }
    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some() {
        let mut c = Command::new("hyprctl");
        c.args(["dispatch", "focuswindow", "class:DataShield"]);
        spawn(c);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_browser_lands_in_its_family() {
        for (ident, family) in [
            ("zen.desktop", BrowserFamily::Gecko),
            ("firefox", BrowserFamily::Gecko),
            ("org.mozilla.firefox", BrowserFamily::Gecko),
            ("librewolf.desktop", BrowserFamily::Gecko),
            ("FirefoxURL-308046B0AF4A39CB", BrowserFamily::Gecko),
            ("brave-browser.desktop", BrowserFamily::Chromium),
            ("BraveHTML", BrowserFamily::Chromium),
            ("google-chrome.desktop", BrowserFamily::Chromium),
            ("com.google.Chrome", BrowserFamily::Chromium),
            ("ChromeHTML", BrowserFamily::Chromium),
            ("chromium.desktop", BrowserFamily::Chromium),
            ("microsoft-edge.desktop", BrowserFamily::Chromium),
            ("MSEdgeHTM", BrowserFamily::Chromium),
            ("vivaldi-stable.desktop", BrowserFamily::Chromium),
            ("com.apple.Safari", BrowserFamily::Safari),
            ("org.gnome.Epiphany", BrowserFamily::Unknown),
            ("", BrowserFamily::Unknown),
        ] {
            assert_eq!(classify(ident), family, "{ident}");
        }
    }

    #[test]
    fn the_three_platform_formats_parse() {
        let xdg = parse_xdg("zen.desktop\n").expect("xdg");
        assert_eq!(xdg.name, "zen");
        assert_eq!(xdg.family, BrowserFamily::Gecko);
        assert!(parse_xdg("\n  \n").is_none(), "nothing configured");

        let plist = r#"{"LSHandlers":[
            {"LSHandlerURLScheme":"mailto","LSHandlerRoleAll":"com.apple.mail"},
            {"LSHandlerURLScheme":"http","LSHandlerRoleAll":"com.brave.browser"}
        ]}"#;
        let mac = parse_launchservices_json(plist).expect("plist");
        assert_eq!(mac.name, "com.brave.browser");
        assert_eq!(mac.family, BrowserFamily::Chromium);
        assert!(parse_launchservices_json("{}").is_none());
        assert!(parse_launchservices_json("not json").is_none());

        let reg = "\r\nHKEY_CURRENT_USER\\...\\UserChoice\r\n    ProgId    REG_SZ    FirefoxURL-308046B0AF4A39CB\r\n\r\n";
        let win = parse_reg_query(reg).expect("reg");
        assert_eq!(win.name, "FirefoxURL-308046B0AF4A39CB");
        assert_eq!(win.family, BrowserFamily::Gecko);
        assert!(parse_reg_query("ERROR: The system was unable to find the specified registry key").is_none());
    }

    #[test]
    fn the_family_label_is_a_product_name() {
        assert_eq!(BrowserFamily::Gecko.label(), "Firefox");
        assert_eq!(BrowserFamily::Chromium.label(), "Chrome");
    }
}
