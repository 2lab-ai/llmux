//! Frame-indexed single-glyph animations for the dashboard, inspired by the
//! effect families in `agents-are-thinking` (braille / block / shade / bar).
//!
//! Every function is pure — `glyph(frame)` — and the render loop advances one
//! shared `frame` counter per tick (see `RENDER_TICK`), so every animation on
//! screen steps in lockstep.
//!
//! GLYPH SET IS DELIBERATELY BRAILLE + BLOCK-ELEMENTS ONLY. Both Unicode
//! blocks (U+2800–28FF braille, U+2580–259F block elements) are East-Asian
//! *Narrow*, so they stay one terminal column wide in a CJK locale — geometric
//! shapes (◐ ● ○ …) are *Ambiguous* width and would misalign the table on a
//! Korean/Japanese/Chinese terminal.

/// Index `frames[(frame / hold) % len]`. `hold` > 1 slows the cycle by holding
/// each glyph for that many ticks (for breaths/pulses that should feel calm).
fn at(frames: &[char], frame: usize, hold: usize) -> char {
    frames[(frame / hold.max(1)) % frames.len()]
}

/// Classic 10-frame braille orbit — the "thinking/working" spinner. Used for
/// in-flight **Claude** requests and the working state of the current account.
pub fn braille_spin(frame: usize) -> char {
    at(
        &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'],
        frame,
        1,
    )
}

/// 4-frame quarter-block corner orbit — a visually distinct "thinking/working"
/// spinner for in-flight **Codex** requests (block family, not braille).
pub fn block_spin(frame: usize) -> char {
    at(&['▖', '▘', '▝', '▗'], frame, 1)
}

/// 4-frame half-block sweep (left→top→right→bottom) — a rotating "timer",
/// used for the cooldown / waiting-for-reset state.
pub fn half_block_clock(frame: usize) -> char {
    at(&['▌', '▀', '▐', '▄'], frame, 1)
}

/// Bar that swells and ebbs — a calm "heartbeat" for the active (selected,
/// idle) account.
pub fn bar_pulse(frame: usize) -> char {
    at(&['▂', '▃', '▄', '▅', '▆', '▅', '▄', '▃'], frame, 2)
}

/// Faint single braille dot drifting — "idle but ready" (eligible, not current).
pub fn idle_drift(frame: usize) -> char {
    at(&['⠁', '⠂', '⠄', '⠂'], frame, 2)
}

/// Shade ramp breathing up and down — "quota filling up", for an account over
/// its 5h/7d threshold.
pub fn shade_breathe(frame: usize) -> char {
    at(&['░', '▒', '▓', '█', '▓', '▒'], frame, 2)
}

/// Slow on/off blink for alerts (auth failure): `glyph` on, a space off,
/// toggling about every ~3 ticks so it pulses rather than strobes.
pub fn blink(frame: usize, glyph: char) -> char {
    if (frame / 3).is_multiple_of(2) {
        glyph
    } else {
        ' '
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn working_spinners_are_distinct_families_and_cycle() {
        // Claude (braille) vs Codex (block) must look different at every step.
        for f in 0..12 {
            assert_ne!(braille_spin(f), block_spin(f), "frame {f}");
        }
        assert_eq!(braille_spin(0), braille_spin(10), "braille is a 10-cycle");
        assert_eq!(block_spin(0), block_spin(4), "block is a 4-cycle");
        assert_eq!(half_block_clock(0), half_block_clock(4));
    }

    #[test]
    fn hold_slows_pulses() {
        assert_eq!(bar_pulse(0), bar_pulse(1), "held 2 ticks per glyph");
        assert_ne!(bar_pulse(0), bar_pulse(2));
    }

    #[test]
    fn blink_pulses_on_a_slow_cadence() {
        assert_eq!(blink(0, '!'), '!');
        assert_eq!(blink(3, '!'), ' ');
        assert_eq!(blink(6, '!'), '!');
    }

    #[test]
    fn all_glyphs_are_braille_or_block_elements() {
        // Guard the CJK-width invariant: every glyph stays in the braille or
        // block-elements range (both East-Asian Narrow).
        let narrow = |c: char| {
            let u = c as u32;
            c == ' ' || (0x2800..=0x28FF).contains(&u) || (0x2580..=0x259F).contains(&u)
        };
        for f in 0..24 {
            for g in [
                braille_spin(f),
                block_spin(f),
                half_block_clock(f),
                bar_pulse(f),
                idle_drift(f),
                shade_breathe(f),
                blink(f, '!'),
            ] {
                if g == '!' {
                    continue; // ASCII alert glyph, also narrow
                }
                assert!(narrow(g), "glyph {g:?} (U+{:04X}) not narrow", g as u32);
            }
        }
    }
}
