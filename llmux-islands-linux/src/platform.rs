//! Runtime selection for the portable window and compositor-specific surfaces.

use std::env;

/// The native presentation path selected for the current desktop session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SurfaceMode {
    WaylandLayerShell,
    X11Positioned,
    RegularWindow,
}

impl SurfaceMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WaylandLayerShell => "wayland-layer-shell",
            Self::X11Positioned => "x11-positioned",
            Self::RegularWindow => "regular-window",
        }
    }
}

/// Detect the surface path from XDG and display-server environment variables.
#[must_use]
pub fn detect_surface_mode() -> SurfaceMode {
    detect_surface_mode_from(|key| env::var(key).ok())
}

fn detect_surface_mode_from(mut read: impl FnMut(&str) -> Option<String>) -> SurfaceMode {
    let session_type = read("XDG_SESSION_TYPE")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let has_wayland = read("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty());
    let has_x11 = read("DISPLAY").is_some_and(|value| !value.is_empty());
    let plasma_desktop = [
        read("XDG_CURRENT_DESKTOP"),
        read("XDG_SESSION_DESKTOP"),
        read("DESKTOP_SESSION"),
    ]
    .into_iter()
    .flatten()
    .any(|value| identifies_plasma(&value));

    if session_type == "wayland" {
        if has_wayland && plasma_desktop {
            SurfaceMode::WaylandLayerShell
        } else {
            // Layer-shell protocols can exist outside Plasma, but this build
            // configures and verifies LayerShellQt specifically as a KDE path.
            // Unknown/non-Plasma Wayland sessions retain a portable QWindow.
            SurfaceMode::RegularWindow
        }
    } else if session_type == "x11" || has_x11 {
        SurfaceMode::X11Positioned
    } else {
        SurfaceMode::RegularWindow
    }
}

fn identifies_plasma(value: &str) -> bool {
    value
        .split(|character: char| {
            character == ':'
                || character == ';'
                || character == ','
                || character.is_ascii_whitespace()
        })
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .any(|token| {
            matches!(
                token.to_ascii_lowercase().as_str(),
                "kde" | "plasma" | "plasmawayland" | "plasma-wayland"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::{detect_surface_mode_from, SurfaceMode};
    use std::collections::HashMap;

    fn detect(entries: &[(&str, &str)]) -> SurfaceMode {
        let env = entries.iter().copied().collect::<HashMap<_, _>>();
        detect_surface_mode_from(|key| env.get(key).map(ToString::to_string))
    }

    #[test]
    fn plasma_wayland_session_selects_layer_shell() {
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "KDE:Plasma")
            ]),
            SurfaceMode::WaylandLayerShell
        );
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("WAYLAND_DISPLAY", "wayland-1"),
                ("DESKTOP_SESSION", "plasmawayland")
            ]),
            SurfaceMode::WaylandLayerShell
        );
    }

    #[test]
    fn non_plasma_and_ambiguous_wayland_sessions_use_regular_windows() {
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "GNOME")
            ]),
            SurfaceMode::RegularWindow
        );
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("WAYLAND_DISPLAY", "wayland-0")
            ]),
            SurfaceMode::RegularWindow
        );
        assert_eq!(
            detect(&[
                ("XDG_SESSION_TYPE", "wayland"),
                ("WAYLAND_DISPLAY", "wayland-0"),
                ("XDG_CURRENT_DESKTOP", "NotKDE")
            ]),
            SurfaceMode::RegularWindow
        );
    }

    #[test]
    fn x11_and_headless_sessions_have_explicit_fallbacks() {
        assert_eq!(detect(&[("DISPLAY", ":0")]), SurfaceMode::X11Positioned);
        assert_eq!(
            detect(&[("XDG_SESSION_TYPE", "x11"), ("XDG_CURRENT_DESKTOP", "KDE")]),
            SurfaceMode::X11Positioned
        );
        assert_eq!(detect(&[]), SurfaceMode::RegularWindow);
    }
}
