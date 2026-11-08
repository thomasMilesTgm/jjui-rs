use ratatui::{style::Color, text::Line};
use std::{
    process::{Command, Stdio},
    sync::{OnceLock, mpsc},
};

pub fn library_version() -> &'static str {
    static VERSION: OnceLock<String> = OnceLock::new();
    VERSION.get_or_init(|| {
        let lock: toml::Value =
            toml::from_str(include_str!("../../Cargo.lock")).expect("embedded Cargo.lock is valid");
        lock["package"]
            .as_array()
            .unwrap()
            .iter()
            .find(|package| package["name"].as_str() == Some("jj-lib"))
            .expect("jj-lib is in Cargo.lock")["version"]
            .as_str()
            .unwrap()
            .to_owned()
    })
}

#[derive(Default)]
pub enum Status {
    #[default]
    Checking,
    Installed(String),
    Unavailable,
}

fn release_series(version: &str) -> Option<(u64, u64)> {
    let mut parts = version.split('+').next()?.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    parts.next()?.parse::<u64>().ok()?;
    parts.next().is_none().then_some((major, minor))
}

impl Status {
    fn parse(output: &str) -> Self {
        let mut words = output.split_whitespace();
        match (words.next(), words.next()) {
            (Some("jj"), Some(version))
                if version.split('.').take(3).count() == 3
                    && version
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || ".+-".contains(c)) =>
            {
                Self::Installed(version.to_owned())
            }
            _ => Self::Unavailable,
        }
    }

    pub fn label(&self) -> (String, Color) {
        let lib = library_version();
        match self {
            Self::Checking => ("jj checking…".into(), Color::DarkGray),
            Self::Unavailable => ("jj unavailable".into(), Color::Red),
            Self::Installed(version) => {
                if version.split('+').next() == Some(lib) {
                    return (format!("jj {version}"), Color::Green);
                }
                let same_series = release_series(version).is_some()
                    && release_series(version) == release_series(lib);
                (
                    "Update jjui-rs".into(),
                    if same_series {
                        Color::Indexed(208)
                    } else {
                        Color::Red
                    },
                )
            }
        }
    }

    pub fn line(&self) -> Line<'static> {
        let (text, color) = self.label();
        Line::styled(text, ratatui::style::Style::default().fg(color))
    }
}

// Run once, off the UI thread. Version queries do not snapshot or read repository history.
pub fn check() -> mpsc::Receiver<Status> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let status = Command::new("jj")
            .arg("--version")
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| Status::parse(&String::from_utf8_lossy(&output.stdout)))
            .unwrap_or(Status::Unavailable);
        let _ = sender.send(status);
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions_and_handles_unavailable_cli() {
        let version = library_version();
        for output in [format!("jj {version}\n"), format!("jj {version}+abcdef\n")] {
            assert_eq!(
                Status::parse(&output).label(),
                (output.trim().to_owned(), Color::Green)
            );
        }
        let (major, minor) = release_series(version).unwrap();
        let other_patch = Status::parse(&format!("jj {major}.{minor}.999"));
        assert_eq!(
            other_patch.label(),
            ("Update jjui-rs".into(), Color::Indexed(208))
        );
        for output in [
            format!("jj {major}.{}.0", minor + 1),
            format!("jj {}.{minor}.0", major + 1),
            format!("jj {version}-rc.1"),
        ] {
            let output = output.as_str();
            assert_eq!(
                Status::parse(output).label(),
                ("Update jjui-rs".into(), Color::Red)
            );
        }
        for output in ["", "not jj", "jj unknown"] {
            assert!(matches!(Status::parse(output), Status::Unavailable));
        }
    }
}
