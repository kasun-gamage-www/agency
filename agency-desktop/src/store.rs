//! Persistent workspace: the repository library and the projects that group them.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Project {
    pub name: String,
    #[serde(default)]
    pub repos: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
pub struct Store {
    /// Every repository known to the app, whether or not it belongs to a project.
    #[serde(default)]
    pub repos: Vec<PathBuf>,
    #[serde(default)]
    pub projects: Vec<Project>,
}

impl Store {
    pub fn file() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("agency-desktop").join("workspace.json"))
    }

    pub fn load() -> Self {
        Self::file()
            .and_then(|f| fs::read_to_string(f).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) -> Result<(), String> {
        let file = Self::file().ok_or_else(|| "no config directory available".to_string())?;
        if let Some(dir) = file.parent() {
            fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        fs::write(&file, json).map_err(|e| e.to_string())
    }

    /// Returns true if the repo was not already in the library.
    pub fn add_repo(&mut self, path: PathBuf) -> bool {
        if self.repos.contains(&path) {
            false
        } else {
            self.repos.push(path);
            true
        }
    }

    pub fn remove_repo(&mut self, path: &Path) {
        self.repos.retain(|r| r != path);
        for p in &mut self.projects {
            p.repos.retain(|r| r != path);
        }
    }

    pub fn set_repo_in_project(&mut self, project: usize, path: &Path, member: bool) {
        let Some(p) = self.projects.get_mut(project) else { return };
        let present = p.repos.iter().any(|r| r == path);
        if member && !present {
            p.repos.push(path.to_path_buf());
        } else if !member && present {
            p.repos.retain(|r| r != path);
        }
    }
}
