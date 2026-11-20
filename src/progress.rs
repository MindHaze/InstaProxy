use crate::model::CheckStatus;
use std::io::{self, IsTerminal};

const SPINNER: [char; 4] = ['-', '/', '|', '\\'];

#[derive(Debug, Default)]
pub struct CheckProgress {
    total: usize,
    completed: usize,
    alive: usize,
    dead: usize,
    timeouts: usize,
    auth: usize,
    frame: usize,
}

impl CheckProgress {
    pub fn new(total: usize) -> Self {
        Self {
            total,
            ..Self::default()
        }
    }

    pub fn record(&mut self, status: CheckStatus) {
        self.completed += 1;
        match status {
            CheckStatus::Working | CheckStatus::Slow => self.alive += 1,
            CheckStatus::Dead => self.dead += 1,
            CheckStatus::Timeout => self.timeouts += 1,
            CheckStatus::AuthRequired => self.auth += 1,
        }
    }

    pub fn advance(&mut self) {
        self.frame = (self.frame + 1) % SPINNER.len();
    }

    pub fn summary(&self) -> String {
        format!(
            "Alive: {} | Dead: {} | Timeout: {} | Remaining: {} | Auth: {} | Checked: {}/{}",
            self.alive,
            self.dead,
            self.timeouts,
            self.total.saturating_sub(self.completed),
            self.auth,
            self.completed,
            self.total,
        )
    }

    pub fn title(&self) -> String {
        let frame = if self.completed == self.total {
            '+'
        } else {
            SPINNER[self.frame]
        };
        format!("[{frame}] InstaProxy | {}", self.summary())
    }
}

pub fn set_console_title(title: &str) {
    if !io::stdout().is_terminal() {
        return;
    }
    #[cfg(windows)]
    {
        let title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
        // The buffer is NUL-terminated and remains valid throughout the API call.
        unsafe {
            windows_sys::Win32::System::Console::SetConsoleTitleW(title.as_ptr());
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Write;
        let mut stdout = io::stdout().lock();
        let _ = write!(stdout, "\x1b]0;{title}\x07");
        let _ = stdout.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spinner_rotates_while_checks_are_pending() {
        let mut progress = CheckProgress::new(1);
        for frame in ['-', '/', '|', '\\', '-'] {
            assert!(progress
                .title()
                .starts_with(&format!("[{frame}] InstaProxy |")));
            progress.advance();
        }
    }

    #[test]
    fn counters_partition_results_and_count_slow_as_alive() {
        let mut progress = CheckProgress::new(6);
        for status in [
            CheckStatus::Working,
            CheckStatus::Slow,
            CheckStatus::Dead,
            CheckStatus::Timeout,
            CheckStatus::AuthRequired,
        ] {
            progress.record(status);
        }
        assert_eq!(
            progress.summary(),
            "Alive: 2 | Dead: 1 | Timeout: 1 | Remaining: 1 | Auth: 1 | Checked: 5/6"
        );
        progress.record(CheckStatus::Dead);
        assert_eq!(progress.title(), "[+] InstaProxy | Alive: 2 | Dead: 2 | Timeout: 1 | Remaining: 0 | Auth: 1 | Checked: 6/6");
    }

    #[test]
    fn empty_batch_has_a_finished_title() {
        assert_eq!(CheckProgress::new(0).title(), "[+] InstaProxy | Alive: 0 | Dead: 0 | Timeout: 0 | Remaining: 0 | Auth: 0 | Checked: 0/0");
    }
}
