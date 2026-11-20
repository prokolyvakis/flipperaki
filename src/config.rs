//! Validated local configuration: which device is enrolled and how to reach it.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Where the Buddy FAP is installed on the SD card.
pub const DEFAULT_BUDDY_PATH: &str = "/ext/apps/Tools/flipperaki.fap";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Ble,
    Usb,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub transport: TransportKind,
    /// CoreBluetooth peripheral identifier chosen at enrollment (not a MAC).
    #[serde(default)]
    pub ble_peripheral_id: Option<String>,
    #[serde(default = "default_buddy_path")]
    pub buddy_path: String,
}

fn default_buddy_path() -> String {
    DEFAULT_BUDDY_PATH.to_owned()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            transport: TransportKind::Ble,
            ble_peripheral_id: None,
            buddy_path: default_buddy_path(),
        }
    }
}

impl Config {
    pub fn path(runtime_dir: &Path) -> PathBuf {
        runtime_dir.join("config.json")
    }

    /// Loads the config, or the default when none was saved yet.
    pub fn load(runtime_dir: &Path) -> anyhow::Result<Self> {
        let path = Self::path(runtime_dir);
        let config: Config = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| anyhow::anyhow!("{} is invalid: {e}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e.into()),
        };
        config.validate()?;
        Ok(config)
    }

    pub fn save(&self, runtime_dir: &Path) -> anyhow::Result<()> {
        self.validate()?;
        let path = Self::path(runtime_dir);
        std::fs::write(&path, serde_json::to_string_pretty(self)? + "\n")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        Ok(())
    }

    fn validate(&self) -> anyhow::Result<()> {
        let fap = self.buddy_path.starts_with("/ext/")
            && self.buddy_path.ends_with(".fap")
            && !self.buddy_path.contains("..")
            && self.buddy_path.len() <= 96
            && self.buddy_path.chars().all(|c| c.is_ascii_graphic());
        anyhow::ensure!(fap, "buddy_path must be an /ext/... .fap path");
        if let Some(id) = &self.ble_peripheral_id {
            anyhow::ensure!(
                !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_graphic()),
                "ble_peripheral_id is not a valid identifier"
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_paths_outside_the_sd_card() {
        let bad = Config {
            buddy_path: "/ext/../int/x.fap".into(),
            ..Config::default()
        };
        assert!(bad.validate().is_err());
        let spaces = Config {
            buddy_path: "/ext/apps/a b.fap".into(),
            ..Config::default()
        };
        assert!(spaces.validate().is_err());
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn round_trips_with_owner_only_permissions() {
        let dir = std::env::temp_dir().join(format!("flipperaki-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = Config {
            transport: TransportKind::Usb,
            ..Config::default()
        };
        config.save(&dir).unwrap();
        assert_eq!(Config::load(&dir).unwrap(), config);
        let mode = std::fs::metadata(Config::path(&dir))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
        std::fs::remove_dir_all(&dir).ok();
    }
}
