use crate::collector::ClaudeCollector;
use crate::model::AgentSession;

pub struct App {
    pub sessions: Vec<AgentSession>,
    pub selected: usize,
    pub should_quit: bool,
    collector: ClaudeCollector,
}

impl App {
    pub fn new() -> Self {
        Self {
            sessions: Vec::new(),
            selected: 0,
            should_quit: false,
            collector: ClaudeCollector::new(),
        }
    }

    pub fn tick(&mut self) {
        self.sessions = self.collector.collect();
        if self.selected >= self.sessions.len() && !self.sessions.is_empty() {
            self.selected = self.sessions.len() - 1;
        }
    }

    pub fn select_next(&mut self) {
        if !self.sessions.is_empty() {
            self.selected = (self.selected + 1).min(self.sessions.len() - 1);
        }
    }

    pub fn select_prev(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub fn quit(&mut self) {
        self.should_quit = true;
    }
}
