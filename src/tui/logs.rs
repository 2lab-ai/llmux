//! Log console state: a bounded ring of tracing [`LogLine`]s (newest last,
//! auto-follow — the renderer shows the tail) and the `l`-key panel size
//! toggle. Pure state — rendering lives in `ui`.

use std::collections::VecDeque;

use crate::logging::LogLine;

/// Ring capacity for raw tracing lines (distinct from the structured
/// activity log's 200-entry ring).
pub(crate) const LOG_CONSOLE_CAPACITY: usize = 500;

#[derive(Debug, Default)]
pub(crate) struct LogConsole {
    capacity: usize,
    /// Back = newest (the console renders the tail at the bottom).
    lines: VecDeque<LogLine>,
}

impl LogConsole {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity,
            lines: VecDeque::new(),
        }
    }

    pub(crate) fn push(&mut self, line: LogLine) {
        if self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// The newest `n` lines, oldest→newest (render top→bottom; the newest
    /// line lands on the bottom row).
    pub(crate) fn tail(&self, n: usize) -> impl Iterator<Item = &LogLine> {
        self.lines.iter().skip(self.lines.len().saturating_sub(n))
    }
}

/// Log panel height, cycled by the `l` key: small → large → hidden → small.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LogPanelSize {
    Small,
    Large,
    Hidden,
}

impl LogPanelSize {
    pub(crate) fn cycle(self) -> Self {
        match self {
            Self::Small => Self::Large,
            Self::Large => Self::Hidden,
            Self::Hidden => Self::Small,
        }
    }

    /// Panel height in terminal rows, including the top border. 0 = hidden.
    pub(crate) fn height(self) -> u16 {
        match self {
            Self::Small => 8,
            Self::Large => 16,
            Self::Hidden => 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing::Level;

    fn line(text: &str) -> LogLine {
        LogLine {
            level: Level::INFO,
            text: text.into(),
        }
    }

    #[test]
    fn ring_trims_oldest_and_tail_is_newest_last() {
        let mut console = LogConsole::new(3);
        for i in 0..5 {
            console.push(line(&format!("l{i}")));
        }
        let all: Vec<&str> = console.tail(usize::MAX).map(|l| l.text.as_str()).collect();
        assert_eq!(all, vec!["l2", "l3", "l4"], "capacity 3, oldest evicted");
        let tail: Vec<&str> = console.tail(2).map(|l| l.text.as_str()).collect();
        assert_eq!(tail, vec!["l3", "l4"], "tail keeps the newest, in order");
    }

    #[test]
    fn panel_size_cycles_small_large_hidden() {
        let mut size = LogPanelSize::Small;
        size = size.cycle();
        assert_eq!(size, LogPanelSize::Large);
        size = size.cycle();
        assert_eq!(size, LogPanelSize::Hidden);
        assert_eq!(size.height(), 0, "hidden takes no rows");
        size = size.cycle();
        assert_eq!(size, LogPanelSize::Small, "full cycle returns to small");
    }
}
