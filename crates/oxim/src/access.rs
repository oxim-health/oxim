//! `oxim users` and `oxim tokens`: accounts for the web server, managed on
//! the machine that runs OXIM.

use std::io::{BufRead, Write};

use oxim_auth::{AuthStore, NewUser, Role, UserUpdate};
use oxim_core::{Clock, SystemClock};
use oxim_model::Timestamp;
use oxim_store::{AuditEvent, MessageStore, SqliteStore};

use crate::CliResult;
use crate::settings::Settings;

/// Where a password comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PasswordSource {
    /// Prompt twice on the terminal without echo.
    Prompt,
    /// Read the first line of standard input (for scripts).
    Stdin,
}

fn open(settings: &Settings) -> CliResult<AuthStore> {
    std::fs::create_dir_all(&settings.data_dir)
        .map_err(|e| format!("cannot create {}: {e}", settings.data_dir.display()))?;
    let path = settings.auth_database_path();
    AuthStore::open(&path).map_err(|e| format!("cannot open {}: {e}", path.display()).into())
}

fn now() -> Timestamp {
    SystemClock.now()
}

/// Who ran the command, for the audit trail.
fn actor() -> String {
    let user = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());
    format!("cli:{user}")
}

/// Records the change in the message database's audit trail, if the
/// engine has created it.
fn audit(settings: &Settings, action: &str, detail: String) {
    let path = settings.database_path();
    if !path.exists() {
        return;
    }
    let event = AuditEvent {
        at: now(),
        action: action.to_owned(),
        actor: actor(),
        message_id: None,
        channel: None,
        detail: Some(detail),
    };
    if let Err(e) = SqliteStore::open(&path).and_then(|mut store| store.record_audit(&event)) {
        eprintln!("warning: cannot write the audit event: {e}");
    }
}

fn read_password(source: PasswordSource, username: &str) -> CliResult<String> {
    let password = match source {
        PasswordSource::Stdin => {
            let mut line = String::new();
            std::io::stdin().lock().read_line(&mut line)?;
            line.trim_end_matches(['\r', '\n']).to_owned()
        }
        PasswordSource::Prompt => {
            let first = rpassword::prompt_password(format!("Password for {username}: "))?;
            let second = rpassword::prompt_password("Repeat the password: ")?;
            if first != second {
                return Err("the passwords differ".into());
            }
            first
        }
    };
    oxim_auth::check_password_policy(username, &password)?;
    Ok(password)
}

fn role(text: &str) -> CliResult<Role> {
    Ok(text.parse::<Role>()?)
}

/// Creates a user; `create-admin` is this with the admin role.
pub(crate) fn add_user(
    settings: &Settings,
    username: &str,
    display_name: Option<&str>,
    role_name: &str,
    source: PasswordSource,
    out: &mut impl Write,
) -> CliResult<()> {
    let role = role(role_name)?;
    let auth = open(settings)?;
    if auth.user(username)?.is_some() {
        return Err(format!("user {username} already exists").into());
    }
    let password = read_password(source, username)?;
    let user = auth.create_user(
        &NewUser {
            username,
            display_name: display_name.unwrap_or(username),
            password: &password,
            role,
        },
        now(),
    )?;
    audit(
        settings,
        "user.created",
        format!("{} role={}", user.username, user.role),
    );
    writeln!(out, "created {} user {}", user.role, user.username)?;
    Ok(())
}

/// Lists users.
pub(crate) fn list_users(settings: &Settings, out: &mut impl Write) -> CliResult<()> {
    let auth = open(settings)?;
    let users = auth.users()?;
    if users.is_empty() {
        writeln!(out, "no users; create one with `oxim users create-admin`")?;
    }
    for user in users {
        writeln!(
            out,
            "{:24} {:9} {:9} last login {}  {}",
            user.username,
            user.role.as_str(),
            if user.disabled { "disabled" } else { "active" },
            user.last_login_at
                .map_or_else(|| "never".to_owned(), |at| at.to_string()),
            user.display_name
        )?;
    }
    Ok(())
}

/// Disables or enables a user. Disabling ends the user's sessions.
pub(crate) fn set_disabled(
    settings: &Settings,
    username: &str,
    disabled: bool,
    out: &mut impl Write,
) -> CliResult<()> {
    let auth = open(settings)?;
    let update = UserUpdate {
        disabled: Some(disabled),
        ..UserUpdate::default()
    };
    let user = auth.update_user(username, &update, now())?;
    audit(
        settings,
        "user.updated",
        format!("{} disabled={disabled}", user.username),
    );
    let state = if disabled { "disabled" } else { "enabled" };
    writeln!(out, "{state} user {}", user.username)?;
    Ok(())
}

/// Sets a new password; the user's sessions end.
pub(crate) fn set_password(
    settings: &Settings,
    username: &str,
    source: PasswordSource,
    out: &mut impl Write,
) -> CliResult<()> {
    let auth = open(settings)?;
    let user = auth
        .user(username)?
        .ok_or_else(|| format!("no user {username}"))?;
    let password = read_password(source, &user.username)?;
    auth.set_password(&user.username, &password, now())?;
    audit(settings, "user.password_reset", user.username.clone());
    writeln!(out, "password of {} changed", user.username)?;
    Ok(())
}

/// Creates an API token and prints it once.
pub(crate) fn create_token(
    settings: &Settings,
    name: &str,
    role_name: &str,
    expires_in: Option<std::time::Duration>,
    out: &mut impl Write,
) -> CliResult<()> {
    let role = role(role_name)?;
    let auth = open(settings)?;
    let now = now();
    let expires_at = expires_in.map(|age| {
        let nanos = i64::try_from(age.as_nanos()).unwrap_or(i64::MAX);
        Timestamp::from_unix_nanos(now.unix_nanos().saturating_add(nanos))
    });
    let (secret, token) = auth.create_api_token(name, role, &actor(), expires_at, now)?;
    audit(
        settings,
        "token.created",
        format!("id={} name={} role={}", token.id, token.name, token.role),
    );
    writeln!(out, "token {} ({}, {}):", token.id, token.name, token.role)?;
    writeln!(out, "{secret}")?;
    writeln!(
        out,
        "Store it now; it is not shown again. Send it as `Authorization: Bearer <token>`."
    )?;
    Ok(())
}

/// Lists API tokens.
pub(crate) fn list_tokens(settings: &Settings, out: &mut impl Write) -> CliResult<()> {
    let auth = open(settings)?;
    for token in auth.api_tokens()? {
        let state = if token.revoked {
            "revoked".to_owned()
        } else {
            match token.expires_at {
                Some(at) if at <= now() => "expired".to_owned(),
                Some(at) => format!("expires {at}"),
                None => "no expiry".to_owned(),
            }
        };
        writeln!(
            out,
            "{:5} {:24} {:9} {}  created by {} at {}",
            token.id,
            token.name,
            token.role.as_str(),
            state,
            token.created_by,
            token.created_at
        )?;
    }
    Ok(())
}

/// Revokes an API token.
pub(crate) fn revoke_token(settings: &Settings, id: i64, out: &mut impl Write) -> CliResult<()> {
    let auth = open(settings)?;
    auth.revoke_api_token(id)?;
    audit(settings, "token.revoked", format!("id={id}"));
    writeln!(out, "token {id} revoked")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(dir: &std::path::Path) -> Settings {
        Settings {
            data_dir: dir.join("data"),
            ..Settings::default()
        }
    }

    #[test]
    fn manages_users_and_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let settings = settings(dir.path());
        let mut out = Vec::new();
        // Passwords from standard input cannot be supplied in a unit test,
        // so users are created through the store here.
        let auth = open(&settings).unwrap();
        auth.create_user(
            &NewUser {
                username: "admin",
                display_name: "Administrator",
                password: "correct horse battery",
                role: Role::Admin,
            },
            now(),
        )
        .unwrap();
        drop(auth);
        list_users(&settings, &mut out).unwrap();
        set_disabled(&settings, "admin", true, &mut out).unwrap();
        create_token(
            &settings,
            "prometheus",
            "viewer",
            Some(std::time::Duration::from_secs(3600)),
            &mut out,
        )
        .unwrap();
        list_tokens(&settings, &mut out).unwrap();
        revoke_token(&settings, 1, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("admin"), "{text}");
        assert!(text.contains("disabled user admin"), "{text}");
        assert!(text.contains("oxt_"), "{text}");
        assert!(text.contains("token 1 revoked"), "{text}");
        assert!(role("superuser").is_err());
        assert!(set_disabled(&settings, "nobody", true, &mut Vec::new()).is_err());
    }
}
