//! The browser launcher is not the window: Edge can hand off to another process.
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct WindowLifetime(Mutex<HashMap<String, (Instant, bool)>>);
impl WindowLifetime {
    pub fn update(&self, id: &str, closed: bool) {
        if let Ok(mut pages) = self.0.lock() {
            pages.insert(id.to_owned(), (Instant::now(), closed));
        }
    }
    pub fn should_exit(&self, now: Instant) -> bool {
        self.0.lock().map(|pages| !pages.is_empty() && pages.values().all(|(time,closed)| {
            now.saturating_duration_since(*time) >= if *closed { Duration::from_secs(10) } else { Duration::from_secs(300) }
        })).unwrap_or(false)
    }
    pub fn resumed(&self) {
        if let Ok(mut pages) = self.0.lock() {
            for (time,closed) in pages.values_mut() { if !*closed { *time = Instant::now(); } }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launcher_exit_is_not_window_close() {
        let life=WindowLifetime::default();
        assert!(!life.should_exit(Instant::now()+Duration::from_secs(600)));
        life.update("page",false);
        assert!(!life.should_exit(Instant::now()+Duration::from_secs(60)));
    }
    #[test]
    fn reload_and_multiple_windows_keep_service_alive() {
        let life=WindowLifetime::default();life.update("old",true);life.update("new",false);
        assert!(!life.should_exit(Instant::now()+Duration::from_secs(11)));
        life.update("new",true);
        assert!(life.should_exit(Instant::now()+Duration::from_secs(11)));
        life.update("new",false);
        assert!(!life.should_exit(Instant::now()+Duration::from_secs(11)));
    }
}
