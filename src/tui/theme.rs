use ratatui::style::{Color, Modifier, Style};

/// Amber retro theme tokens.
pub struct Theme {
    pub bg: Color,
    pub fg: Color,
    pub fg_dim: Color,
    pub accent: Color,
    pub accent_bright: Color,
    pub warning: Color,
    pub danger: Color,
    pub success: Color,
    pub border: Color,
    pub border_active: Color,
    pub selection_bg: Color,
    pub selection_fg: Color,
    pub status_running: Color,
    pub status_pass: Color,
    pub status_fail: Color,
}

impl Theme {
    /// True-color amber palette.
    pub fn amber() -> Self {
        Self {
            bg: Color::Rgb(10, 10, 8),
            fg: Color::Rgb(204, 153, 51),
            fg_dim: Color::Rgb(120, 90, 40),
            accent: Color::Rgb(230, 170, 50),
            accent_bright: Color::Rgb(255, 200, 60),
            warning: Color::Rgb(200, 140, 40),
            danger: Color::Rgb(180, 70, 50),
            success: Color::Rgb(180, 200, 80),
            border: Color::Rgb(100, 75, 30),
            border_active: Color::Rgb(230, 170, 50),
            selection_bg: Color::Rgb(60, 45, 15),
            selection_fg: Color::Rgb(255, 200, 60),
            status_running: Color::Rgb(230, 170, 50),
            status_pass: Color::Rgb(180, 200, 80),
            status_fail: Color::Rgb(180, 70, 50),
        }
    }
}

/// Cached style helpers derived from theme tokens.
pub struct Styles {
    pub theme: Theme,
}

impl Styles {
    pub fn new() -> Self {
        Self {
            theme: Theme::amber(),
        }
    }

    pub fn base(&self) -> Style {
        Style::default().fg(self.theme.fg).bg(self.theme.bg)
    }

    pub fn dim(&self) -> Style {
        Style::default().fg(self.theme.fg_dim).bg(self.theme.bg)
    }

    pub fn accent(&self) -> Style {
        Style::default().fg(self.theme.accent).bg(self.theme.bg)
    }

    pub fn accent_bold(&self) -> Style {
        Style::default()
            .fg(self.theme.accent_bright)
            .bg(self.theme.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn header(&self) -> Style {
        Style::default()
            .fg(self.theme.accent_bright)
            .bg(Color::Rgb(30, 22, 8))
            .add_modifier(Modifier::BOLD)
    }

    pub fn status_bar(&self) -> Style {
        Style::default()
            .fg(self.theme.fg)
            .bg(Color::Rgb(25, 18, 6))
    }

    pub fn border(&self) -> Style {
        Style::default().fg(self.theme.border).bg(self.theme.bg)
    }

    pub fn border_active(&self) -> Style {
        Style::default()
            .fg(self.theme.border_active)
            .bg(self.theme.bg)
    }

    pub fn selected(&self) -> Style {
        Style::default()
            .fg(self.theme.selection_fg)
            .bg(self.theme.selection_bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn label(&self) -> Style {
        Style::default()
            .fg(self.theme.fg_dim)
            .bg(self.theme.bg)
            .add_modifier(Modifier::BOLD)
    }

    pub fn danger(&self) -> Style {
        Style::default().fg(self.theme.danger).bg(self.theme.bg)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.theme.success).bg(self.theme.bg)
    }

    pub fn warning(&self) -> Style {
        Style::default().fg(self.theme.warning).bg(self.theme.bg)
    }

    pub fn nav_item(&self, active: bool) -> Style {
        if active {
            Style::default()
                .fg(self.theme.accent_bright)
                .bg(Color::Rgb(40, 30, 10))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(self.theme.fg_dim).bg(self.theme.bg)
        }
    }

    pub fn status_style(&self, status: &str) -> Style {
        match status {
            "Running" => Style::default()
                .fg(self.theme.status_running)
                .add_modifier(Modifier::BOLD),
            "Completed" | "Pass" => Style::default().fg(self.theme.status_pass),
            "Crashed" | "Fail" => Style::default()
                .fg(self.theme.status_fail)
                .add_modifier(Modifier::BOLD),
            "Stopped" | "NeedsWork" => Style::default().fg(self.theme.warning),
            _ => Style::default().fg(self.theme.fg_dim),
        }
    }

    pub fn severity_style(&self, severity: &str) -> Style {
        match severity {
            "critical" => Style::default()
                .fg(self.theme.danger)
                .add_modifier(Modifier::BOLD),
            "high" => Style::default().fg(self.theme.danger),
            "medium" => Style::default().fg(self.theme.warning),
            "low" => Style::default().fg(self.theme.fg),
            _ => Style::default().fg(self.theme.fg_dim),
        }
    }
}
