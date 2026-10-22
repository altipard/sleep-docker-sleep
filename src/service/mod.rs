//! Service units so `sds run --headless` survives reboots and crashes.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Systemd,
    Launchd,
}

impl ServiceKind {
    pub fn for_host() -> Self {
        if cfg!(target_os = "macos") { Self::Launchd } else { Self::Systemd }
    }

    pub fn unit_path(self) -> PathBuf {
        match self {
            Self::Systemd => PathBuf::from("/etc/systemd/system/sds.service"),
            Self::Launchd => PathBuf::from("/Library/LaunchDaemons/com.zilicon.sds.plist"),
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Self::Systemd => {
                "sudo systemctl daemon-reload && sudo systemctl enable --now sds\n# logs: journalctl -u sds -f"
            }
            Self::Launchd => {
                "sudo launchctl bootstrap system /Library/LaunchDaemons/com.zilicon.sds.plist\n# logs: tail -f /var/log/sds.log"
            }
        }
    }

    pub fn uninstall_hint(self) -> &'static str {
        match self {
            Self::Systemd => {
                "sudo systemctl disable --now sds && sudo rm /etc/systemd/system/sds.service && sudo systemctl daemon-reload"
            }
            Self::Launchd => {
                "sudo launchctl bootout system /Library/LaunchDaemons/com.zilicon.sds.plist && sudo rm /Library/LaunchDaemons/com.zilicon.sds.plist"
            }
        }
    }
}

pub struct UnitInput<'a> {
    pub binary: &'a Path,
    pub config: Option<&'a Path>,
    /// Unix user to run as; must be allowed to use the Docker socket.
    pub user: &'a str,
}

pub fn render(kind: ServiceKind, input: &UnitInput<'_>) -> String {
    let config_arg = input.config.map(|c| format!(" --config {}", c.display())).unwrap_or_default();
    match kind {
        ServiceKind::Systemd => format!(
            r#"[Unit]
Description=sleep-docker-sleep: scale idle containers to zero
Documentation=https://github.com/altipard/sleep-docker-sleep
After=docker.service network-online.target
Requires=docker.service
Wants=network-online.target

[Service]
Type=simple
User={user}
ExecStart={binary} run --headless{config_arg}
Restart=always
RestartSec=2s
# reload the configuration without restarting
ExecReload=/bin/kill -HUP $MAINPID
# hardening (the service only needs the Docker socket and the network)
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadOnlyPaths=/etc/sds
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
"#,
            user = input.user,
            binary = input.binary.display(),
            config_arg = config_arg,
        ),
        ServiceKind::Launchd => {
            let config_args = input
                .config
                .map(|c| format!("\n    <string>--config</string>\n    <string>{}</string>", c.display()))
                .unwrap_or_default();
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>com.zilicon.sds</string>
  <key>ProgramArguments</key>
  <array>
    <string>{binary}</string>
    <string>run</string>
    <string>--headless</string>{config_args}
  </array>
  <key>UserName</key>
  <string>{user}</string>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>/var/log/sds.log</string>
  <key>StandardErrorPath</key>
  <string>/var/log/sds.log</string>
</dict>
</plist>
"#,
                binary = input.binary.display(),
                config_args = config_args,
                user = input.user,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_both_units_with_config() {
        let input = UnitInput {
            binary: Path::new("/usr/local/bin/sds"),
            config: Some(Path::new("/etc/sds/sds.toml")),
            user: "sds",
        };
        let s = render(ServiceKind::Systemd, &input);
        assert!(s.contains("ExecStart=/usr/local/bin/sds run --headless --config /etc/sds/sds.toml"));
        assert!(s.contains("Restart=always"));
        let p = render(ServiceKind::Launchd, &input);
        assert!(p.contains("<string>--config</string>"));
        assert!(p.contains("com.zilicon.sds"));
    }
}
