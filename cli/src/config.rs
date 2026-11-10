/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::error::CliError;
use etcetera::base_strategy::{BaseStrategy, choose_base_strategy, choose_native_strategy};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::{fmt, fs};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

pub const MIGRATED_SINGLE_LOGIN_NAME: &str = "default";

static INVOCATION_OVERRIDES: OnceLock<InvocationOverrides> = OnceLock::new();

#[derive(Debug, Default)]
pub struct InvocationOverrides {
    pub account: Option<String>,
    pub server: Option<String>,
    pub token: Option<String>,
    pub project: Option<String>,
    pub task: Option<String>,
}

#[derive(Clone, Copy, Debug, EnumIter, Hash, Eq, PartialEq)]
pub enum ConfigKey {
    AuthToken,
    Server,
    SelectedProject,
    SelectedTask,
}

impl fmt::Display for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", format!("{:?}", self).to_lowercase())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SavedLogin {
    #[serde(alias = "Server", skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
    #[serde(alias = "AuthToken", skip_serializing_if = "Option::is_none")]
    pub auth_token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(alias = "SelectedProject", skip_serializing_if = "Option::is_none")]
    pub selected_project: Option<String>,
    #[serde(alias = "SelectedTask", skip_serializing_if = "Option::is_none")]
    pub selected_task: Option<String>,
}

impl SavedLogin {
    fn slot(&mut self, key: ConfigKey) -> &mut Option<String> {
        match key {
            ConfigKey::AuthToken => &mut self.auth_token,
            ConfigKey::Server => &mut self.server,
            ConfigKey::SelectedProject => &mut self.selected_project,
            ConfigKey::SelectedTask => &mut self.selected_task,
        }
    }

    pub fn get(&self, key: ConfigKey) -> Option<String> {
        self.clone().slot(key).clone().filter(|v| !v.is_empty())
    }

    pub fn token(&self) -> Option<String> {
        self.get(ConfigKey::AuthToken)
    }

    pub fn server(&self) -> Option<String> {
        self.get(ConfigKey::Server)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct LoginConfigFile {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_account: Option<String>,
    #[serde(default)]
    pub accounts: BTreeMap<String, SavedLogin>,
}

impl LoginConfigFile {
    fn parse(contents: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(contents)
    }

    fn from_legacy_config(contents: &str) -> Result<Self, toml::de::Error> {
        let legacy: SavedLogin = toml::from_str(contents)?;
        if legacy == SavedLogin::default() {
            return Ok(Self::default());
        }
        Ok(Self {
            default_account: Some(MIGRATED_SINGLE_LOGIN_NAME.to_owned()),
            accounts: BTreeMap::from([(MIGRATED_SINGLE_LOGIN_NAME.to_owned(), legacy)]),
        })
    }
}

fn config_file_in(base: &Path) -> PathBuf {
    base.join("gradient").join("config.toml")
}

/// The XDG path is winning, except when only a pre-XDG native config exists. macOS is ignoring
/// `XDG_CONFIG_HOME` for `~/Library/Application Support`. Installs made before #536 then stay
/// logged in.
fn resolve_config_file(xdg: PathBuf, native: PathBuf) -> PathBuf {
    if !xdg.exists() && native.exists() {
        native
    } else {
        xdg
    }
}

fn get_config_file() -> PathBuf {
    let xdg = config_file_in(
        &choose_base_strategy()
            .expect("Could not find configuration directory")
            .config_dir(),
    );

    match choose_native_strategy() {
        Ok(native) => resolve_config_file(xdg, config_file_in(&native.config_dir())),
        Err(_) => xdg,
    }
}

/// Older CLI releases panic on unknown keys in config.toml, so named accounts live in their own file and config.toml is only read once to migrate a single login.
fn saved_accounts_file_path() -> PathBuf {
    get_config_file().with_file_name("accounts.toml")
}

fn lock_saved_accounts_file_exclusively() -> std::io::Result<fs::File> {
    let path = saved_accounts_file_path().with_file_name("saved_accounts.lock");
    fs::create_dir_all(
        path.parent()
            .expect("config file path always has a parent directory"),
    )?;
    let lock = fs::File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)?;
    lock.lock()?;
    Ok(lock)
}

pub fn set_invocation_overrides(overrides: InvocationOverrides) {
    let _ = INVOCATION_OVERRIDES.set(overrides);
}

fn invocation_overrides() -> &'static InvocationOverrides {
    INVOCATION_OVERRIDES.get_or_init(InvocationOverrides::default)
}

pub fn login_name_from_flag_env_or_config(file: &LoginConfigFile) -> Option<String> {
    invocation_overrides()
        .account
        .clone()
        .or_else(|| file.default_account.clone())
        .or_else(|| {
            file.accounts
                .is_empty()
                .then(|| MIGRATED_SINGLE_LOGIN_NAME.to_owned())
        })
}

pub fn account_name_for_command() -> Result<String, CliError> {
    let file = load_and_migrate_login_config_file();
    let name =
        login_name_from_flag_env_or_config(&file).ok_or(CliError::NoDefaultAccountSelected)?;
    if account_name_from_flag_or_env().is_some() && !file.accounts.contains_key(&name) {
        return Err(CliError::AccountNotFound(name));
    }
    Ok(name)
}

pub fn account_name_from_flag_or_env() -> Option<&'static str> {
    invocation_overrides().account.as_deref()
}

pub fn invocation_token_is_from_env_var() -> bool {
    invocation_overrides().token.is_some()
}

fn apply_invocation_settings_to_saved_login(mut login: SavedLogin) -> SavedLogin {
    let overrides = invocation_overrides();
    if let Some(server) = &overrides.server {
        login.server = Some(server.clone());
    }
    if let Some(token) = &overrides.token {
        login.auth_token = Some(token.clone());
    }
    if let Some(project) = &overrides.project {
        let task_in_other_project = login
            .selected_task
            .as_deref()
            .and_then(|task| task.split_once('/'))
            .is_some_and(|(task_project, _)| task_project != project);
        if task_in_other_project {
            login.selected_task = None;
        }
        login.selected_project = Some(project.clone());
    }
    if let Some(task) = &overrides.task {
        login.selected_task = match (task.contains('/'), &login.selected_project) {
            (false, Some(project)) => Some(format!("{project}/{task}")),
            _ => Some(task.clone()),
        };
    }
    login
}

fn read_file_and_parse_with_parser(
    path: &Path,
    parse: fn(&str) -> Result<LoginConfigFile, toml::de::Error>,
) -> Result<Option<LoginConfigFile>, String> {
    match fs::read_to_string(path) {
        Ok(contents) => parse(&contents).map(Some).map_err(|e| {
            let path = path.display();
            format!("Invalid config file {path}: {e}")
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

pub fn read_login_config_file() -> Result<LoginConfigFile, String> {
    if let Some(file) =
        read_file_and_parse_with_parser(&saved_accounts_file_path(), LoginConfigFile::parse)?
    {
        return Ok(file);
    }
    read_file_and_parse_with_parser(&get_config_file(), LoginConfigFile::from_legacy_config)
        .map(Option::unwrap_or_default)
}

pub fn load_and_migrate_login_config_file() -> LoginConfigFile {
    read_login_config_file().unwrap_or_else(|message| {
        eprintln!("{message}");
        std::process::exit(2)
    })
}

fn saved_login_for_selected_account_with_overrides(file: LoginConfigFile) -> SavedLogin {
    let name = login_name_from_flag_env_or_config(&file);
    let saved = name
        .and_then(|name| file.accounts.get(&name).cloned())
        .unwrap_or_default();
    apply_invocation_settings_to_saved_login(saved)
}

pub fn saved_login_for_invocation() -> SavedLogin {
    saved_login_for_selected_account_with_overrides(load_and_migrate_login_config_file())
}

pub fn saved_login_for_shell_completion() -> SavedLogin {
    saved_login_for_selected_account_with_overrides(read_login_config_file().unwrap_or_default())
}

fn save_login_config_file(file: &LoginConfigFile) -> std::io::Result<()> {
    let path = saved_accounts_file_path();
    let dir = path
        .parent()
        .expect("config file path always has a parent directory");
    fs::create_dir_all(dir)?;

    let contents = toml::to_string_pretty(file).map_err(std::io::Error::other)?;
    let mut staged = tempfile::NamedTempFile::new_in(dir)?;
    staged.write_all(contents.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        staged
            .as_file()
            .set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    staged.persist(&path).map_err(|e| e.error)?;
    Ok(())
}

fn print_saved_accounts_file_write_error_and_exit(e: std::io::Error) -> ! {
    eprintln!(
        "Failed to write {}: {e}",
        saved_accounts_file_path().display()
    );
    std::process::exit(1);
}

pub fn update_login_config_file<T>(change: impl FnOnce(&mut LoginConfigFile) -> T) -> T {
    let _lock = lock_saved_accounts_file_exclusively()
        .unwrap_or_else(|e| print_saved_accounts_file_write_error_and_exit(e));
    let mut file = load_and_migrate_login_config_file();
    let result = change(&mut file);
    save_login_config_file(&file)
        .unwrap_or_else(|e| print_saved_accounts_file_write_error_and_exit(e));
    result
}

pub fn update_or_create_saved_login_in_config_file<T>(
    change: impl FnOnce(&mut SavedLogin) -> T,
) -> T {
    update_login_config_file(|file| {
        let name = login_name_from_flag_env_or_config(file).unwrap_or_else(|| {
            eprintln!("{}", CliError::NoDefaultAccountSelected);
            std::process::exit(2)
        });
        if file.default_account.is_none() {
            file.default_account = Some(name.clone());
        }
        change(file.accounts.entry(name).or_default())
    })
}

pub fn set_or_get_by_name(
    key: &str,
    value: Option<String>,
    quiet: bool,
) -> Result<Option<String>, String> {
    let Some(config_key) = ConfigKey::iter().find(|k| k.to_string() == key.to_lowercase()) else {
        let valid = ConfigKey::iter()
            .map(|k| k.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(format!("Invalid key: {key}. Valid keys: {valid}"));
    };

    Ok(match value {
        Some(value) => {
            set_value(config_key, value.clone(), quiet);
            Some(value)
        }
        None => get_value(config_key, quiet),
    })
}

pub fn set_value(key: ConfigKey, value: String, quiet: bool) {
    update_or_create_saved_login_in_config_file(|login| *login.slot(key) = Some(value.clone()));

    if !quiet {
        println!("{} set to \"{}\"", key, value);
    }
}

pub fn get_value(key: ConfigKey, quiet: bool) -> Option<String> {
    let value = saved_login_for_invocation().get(key);

    if !quiet {
        match &value {
            Some(value) => println!("{}", value),
            None => println!("[unset]"),
        }
    }

    value
}

pub fn selected_task_ids() -> Option<(String, String)> {
    get_value(ConfigKey::SelectedTask, true)
        .as_deref()
        .and_then(|selected| selected.split_once('/'))
        .map(|(project, task)| (project.to_owned(), task.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn touch(base: &Path) -> PathBuf {
        let file = config_file_in(base);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "Server = 'http://localhost'\n").unwrap();
        file
    }

    #[test]
    fn fresh_install_writes_the_xdg_path() {
        let xdg = TempDir::new().unwrap();
        let native = TempDir::new().unwrap();
        assert_eq!(
            resolve_config_file(config_file_in(xdg.path()), config_file_in(native.path())),
            config_file_in(xdg.path())
        );
    }

    #[test]
    fn xdg_config_wins_over_a_native_one() {
        let xdg = TempDir::new().unwrap();
        let native = TempDir::new().unwrap();
        touch(xdg.path());
        touch(native.path());
        assert_eq!(
            resolve_config_file(config_file_in(xdg.path()), config_file_in(native.path())),
            config_file_in(xdg.path())
        );
    }

    #[test]
    fn a_pre_xdg_native_config_keeps_being_used() {
        let xdg = TempDir::new().unwrap();
        let native = TempDir::new().unwrap();
        touch(native.path());
        assert_eq!(
            resolve_config_file(config_file_in(xdg.path()), config_file_in(native.path())),
            config_file_in(native.path())
        );
    }

    #[test]
    fn a_flat_single_login_file_becomes_the_default_account() {
        let file = LoginConfigFile::from_legacy_config(
            "AuthToken = 't'\nServer = 'http://a'\nSelectedProject = 'p'\nSelectedBuild = 'b'\n",
        )
        .unwrap();
        assert_eq!(file.default_account.as_deref(), Some("default"));
        let login = &file.accounts["default"];
        assert_eq!(login.server(), Some("http://a".into()));
        assert_eq!(login.token(), Some("t".into()));
        assert_eq!(login.selected_project.as_deref(), Some("p"));
    }

    #[test]
    fn an_empty_file_has_no_accounts() {
        assert!(
            LoginConfigFile::from_legacy_config("")
                .unwrap()
                .accounts
                .is_empty()
        );
    }

    #[test]
    fn named_accounts_round_trip() {
        let file = LoginConfigFile::parse(
            "default_account = 'work'\n[accounts.work]\nserver = 'http://w'\n[accounts.home]\nserver = 'http://h'\n",
        )
        .unwrap();
        assert_eq!(file.default_account.as_deref(), Some("work"));
        assert_eq!(file.accounts.len(), 2);
        let again = LoginConfigFile::parse(&toml::to_string_pretty(&file).unwrap()).unwrap();
        assert_eq!(again.accounts, file.accounts);
    }

    #[test]
    fn a_cleared_token_reads_as_unset() {
        let login = SavedLogin {
            auth_token: Some(String::new()),
            ..Default::default()
        };
        assert_eq!(login.token(), None);
    }
}
