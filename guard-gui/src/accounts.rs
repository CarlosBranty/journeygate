//! "Cuentas y roles": the `[testability]` keys of `target.toml` the panel
//! edits (`roles`, `login_path`, `token_env`), checked against the contract
//! (docs/contracts/target-toml.md) and written with the same layout-preserving
//! editor `guard connect` uses.
//!
//! Each role's credentials live in `E2E_<ROLE>_EMAIL` and
//! `E2E_<ROLE>_PASSWORD`. Only whether a variable is set is ever read
//! ([`env_present`]); its value never reaches this module, and no secret is
//! written to `target.toml`.

use guard_core::config_edit::{Value, set_table_values};

#[cfg(test)]
mod tests;

const TABLE: &str = "testability";

/// The panel's `[testability]` fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TestabilityEdit {
    pub login_path: String,
    pub token_env: String,
    pub roles: Vec<String>,
}

impl TestabilityEdit {
    /// The fields as guard reads them (contract defaults for missing keys);
    /// `None` when `text` is not a valid target.toml.
    pub fn from_target(text: &str) -> Option<Self> {
        let t = guard_core::config::parse_target(text).ok()?.testability;
        Some(Self {
            login_path: t.login_path,
            token_env: t.token_env,
            roles: t.roles,
        })
    }
}

/// `^[a-z][a-z0-9_-]*$`.
pub fn check_role(role: &str) -> Result<(), String> {
    let valid = role.starts_with(|c: char| c.is_ascii_lowercase())
        && role
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-');
    if valid {
        Ok(())
    } else {
        Err(format!(
            "el rol «{role}» no es válido: minúsculas, dígitos, _ o -, y empieza con una letra (p. ej. admin)"
        ))
    }
}

/// `roles` with `input` (trimmed) appended, when it is valid and new.
pub fn add_role(roles: &[String], input: &str) -> Result<Vec<String>, String> {
    let role = input.trim();
    check_role(role)?;
    if roles.iter().any(|r| r == role) {
        return Err(format!("el rol «{role}» ya está en la lista"));
    }
    let mut out = roles.to_vec();
    out.push(role.to_string());
    Ok(out)
}

pub fn remove_role(roles: &[String], role: &str) -> Vec<String> {
    roles.iter().filter(|r| *r != role).cloned().collect()
}

/// `^/\S*$`.
pub fn check_login_path(path: &str) -> Result<(), String> {
    if path.starts_with('/') && !path.chars().any(char::is_whitespace) {
        Ok(())
    } else {
        Err("la ruta de login empieza con / y no lleva espacios (p. ej. /__test__/login)".into())
    }
}

/// `^[A-Z_][A-Z0-9_]*$`: the variable's name, never its value.
pub fn check_token_env(name: &str) -> Result<(), String> {
    let valid = name.starts_with(|c: char| c.is_ascii_uppercase() || c == '_')
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(())
    } else {
        Err("la variable del token es un nombre en mayúsculas, p. ej. GUARD_TEST_TOKEN".into())
    }
}

/// `E2E_<ROLE>_EMAIL` and `E2E_<ROLE>_PASSWORD`, the role uppercased with
/// `-` as `_`.
pub fn credential_env_names(role: &str) -> (String, String) {
    let role = role.to_ascii_uppercase().replace('-', "_");
    (format!("E2E_{role}_EMAIL"), format!("E2E_{role}_PASSWORD"))
}

/// Which of a role's credential variables are set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialStatus {
    pub email: bool,
    pub password: bool,
}

pub fn credential_status(role: &str, is_set: impl Fn(&str) -> bool) -> CredentialStatus {
    let (email, password) = credential_env_names(role);
    CredentialStatus {
        email: is_set(&email),
        password: is_set(&password),
    }
}

/// Whether this process has `name` set and non-empty. The value is never
/// read into a string.
pub fn env_present(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty())
}

fn check(edit: &TestabilityEdit) -> Result<(), String> {
    check_login_path(&edit.login_path)?;
    check_token_env(&edit.token_env)?;
    let mut seen: Vec<&str> = Vec::new();
    for role in &edit.roles {
        check_role(role)?;
        if seen.contains(&role.as_str()) {
            return Err(format!("el rol «{role}» está repetido"));
        }
        seen.push(role);
    }
    Ok(())
}

/// `target.toml` with the panel's `[testability]` fields. Only keys whose
/// value differs from what guard reads now are written (the table is added
/// when missing); the result must still be a valid target.toml.
pub fn set_testability(text: &str, edit: &TestabilityEdit) -> Result<String, String> {
    check(edit)?;
    let current = TestabilityEdit::from_target(text)
        .ok_or("target.toml no es válido; corrígelo antes de guardar")?;
    let mut entries = Vec::new();
    if edit.roles != current.roles {
        entries.push(("roles", Value::Strings(&edit.roles)));
    }
    if edit.login_path != current.login_path {
        entries.push(("login_path", Value::Str(&edit.login_path)));
    }
    if edit.token_env != current.token_env {
        entries.push(("token_env", Value::Str(&edit.token_env)));
    }
    if entries.is_empty() {
        return Ok(text.to_string());
    }
    let new = set_table_values(text, TABLE, &entries)?;
    guard_core::config::parse_target(&new)
        .map_err(|e| format!("target.toml no sería válido: {e}"))?;
    Ok(new)
}
