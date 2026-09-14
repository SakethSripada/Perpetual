use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use am_proto::{
    now, AgentKind, AvailabilityState, ProviderAccount, ProviderAccountAuthLaunch,
    ProviderAccountAuthMode, ProviderAccountStatus,
};
use keyring::Entry;

use crate::{AppCore, CoreError};

const KEYRING_SERVICE: &str = "Perpetual Provider Accounts";

#[derive(Clone)]
pub(crate) struct AccountRuntime {
    pub id: String,
    pub agent: AgentKind,
    pub env: Vec<(String, String)>,
}

impl AppCore {
    pub async fn provider_account_statuses(&self) -> Result<Vec<ProviderAccountStatus>, CoreError> {
        let mut out = Vec::new();
        for account in self.get_limit_policy().await?.accounts {
            let state = am_db::repos::provider_account::get(&self.db.pool, &account.id).await?;
            let (authenticated, detail) = self.probe_provider_account(&account);
            let mut availability = state
                .as_ref()
                .map(|s| s.availability)
                .unwrap_or(AvailabilityState::Unknown);
            let mut reset_at = state.as_ref().and_then(|s| s.reset_at);
            if availability == AvailabilityState::Limited && reset_at.is_some_and(|at| at <= now())
            {
                am_db::repos::provider_account::mark_available(&self.db.pool, &account.id).await?;
                availability = AvailabilityState::Available;
                reset_at = None;
            } else if authenticated && availability == AvailabilityState::Unknown {
                availability = AvailabilityState::Available;
            }
            out.push(ProviderAccountStatus {
                account,
                authenticated,
                availability,
                reset_at,
                detail,
            });
        }
        Ok(out)
    }

    pub async fn provider_account_auth_launch(
        &self,
        id: &str,
    ) -> Result<ProviderAccountAuthLaunch, CoreError> {
        let account = self.account_by_id(id).await?;
        if !matches!(account.agent, AgentKind::Codex | AgentKind::ClaudeCode) {
            return Err(CoreError::Other(
                "Provider account pools currently support Codex and Claude only".into(),
            ));
        }
        let binary = am_agents::find_binary(if account.agent == AgentKind::Codex {
            "codex"
        } else {
            "claude"
        })
        .ok_or_else(|| {
            CoreError::Other(format!("{} CLI is not installed", account.agent.label()))
        })?;
        let home = self.ensure_account_home(&account)?;
        let (args, instructions) = match (account.agent, account.auth_mode) {
            (AgentKind::Codex, _) => (vec!["login".into()], "Complete the provider-owned browser sign-in. This login is isolated to this account slot.".into()),
            (AgentKind::ClaudeCode, ProviderAccountAuthMode::OauthToken) => (vec!["setup-token".into()], "Complete OAuth, then paste the printed token into Perpetual. It is stored only in your OS credential vault.".into()),
            (AgentKind::ClaudeCode, ProviderAccountAuthMode::IsolatedCli) => (Vec::new(), "Run /login in this terminal. This configuration is isolated from other account slots.".into()),
            _ => unreachable!("provider kind validated above"),
        };
        let env = selector_env(&account, &home, None);
        Ok(ProviderAccountAuthLaunch {
            account_id: account.id,
            label: account.label,
            agent: account.agent,
            binary: binary.to_string_lossy().into_owned(),
            args,
            env,
            instructions,
        })
    }

    pub async fn set_provider_account_token(&self, id: &str, token: &str) -> Result<(), CoreError> {
        let account = self.account_by_id(id).await?;
        if account.agent != AgentKind::ClaudeCode
            || account.auth_mode != ProviderAccountAuthMode::OauthToken
        {
            return Err(CoreError::Other(
                "OAuth tokens are only accepted for Claude token accounts".into(),
            ));
        }
        let token = token.trim();
        if !token.starts_with("sk-ant-oat")
            || token.len() < 40
            || token.chars().any(char::is_whitespace)
        {
            return Err(CoreError::Other(
                "That does not look like a Claude setup-token OAuth token".into(),
            ));
        }
        token_entry(id)?
            .set_password(token)
            .map_err(keyring_error)?;
        am_db::repos::provider_account::mark_available(&self.db.pool, id).await?;
        Ok(())
    }

    pub async fn delete_provider_account(&self, id: &str) -> Result<(), CoreError> {
        validate_id(id)?;
        let mut policy = self.get_limit_policy().await?;
        policy.accounts.retain(|account| account.id != id);
        self.set_limit_policy(policy).await?;
        let _ = token_entry(id)?.delete_credential();
        am_db::repos::provider_account::delete(&self.db.pool, id).await?;
        let root = self.data_dir.join("provider-accounts");
        let target = root.join(id);
        if target.starts_with(&root) {
            match std::fs::remove_dir_all(target) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    return Err(CoreError::Other(format!(
                        "could not remove account data: {err}"
                    )))
                }
            }
        }
        Ok(())
    }

    pub(crate) async fn select_provider_account(
        &self,
        agent: AgentKind,
    ) -> Result<Option<AccountRuntime>, CoreError> {
        let statuses = self.provider_account_statuses().await?;
        if !statuses.iter().any(|s| s.account.agent == agent) {
            return Ok(None);
        }
        ready_runtime(
            self,
            statuses.into_iter().filter(|s| s.account.agent == agent),
        )?
        .map(Some)
        .ok_or_else(|| {
            CoreError::Other(format!(
                "Every enabled {} account is unauthenticated or usage-limited",
                agent.label()
            ))
        })
    }

    pub(crate) async fn next_ready_provider_account(
        &self,
        current_id: Option<&str>,
    ) -> Result<Option<AccountRuntime>, CoreError> {
        ready_runtime(
            self,
            self.provider_account_statuses()
                .await?
                .into_iter()
                .filter(|s| current_id != Some(s.account.id.as_str())),
        )
    }

    pub(crate) async fn mark_provider_account_limited(
        &self,
        id: &str,
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Result<(), CoreError> {
        let strikes = am_db::repos::provider_account::get(&self.db.pool, id)
            .await?
            .map(|s| s.limit_strikes + 1)
            .unwrap_or(1);
        let reset_at = match reset_at {
            Some(value) => Some(value),
            None => {
                let seconds = self.get_limit_policy().await?.unknown_reset_retry_secs;
                (seconds > 0).then(|| now() + chrono::Duration::seconds(seconds as i64))
            }
        };
        am_db::repos::provider_account::mark_limited(&self.db.pool, id, reset_at, strikes).await?;
        if let Some(deadline) = reset_at {
            self.note_limit_reset_deadline(deadline);
        }
        Ok(())
    }

    pub(crate) async fn earliest_provider_account_reset(
        &self,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, CoreError> {
        Ok(am_db::repos::provider_account::list(&self.db.pool)
            .await?
            .into_iter()
            .filter_map(|s| s.reset_at)
            .min())
    }

    /// Redeem one earned Codex rate-limit reset only when the user explicitly
    /// opted this account in. Purchased flexible-usage balances are never
    /// toggled or bought by Perpetual.
    pub(crate) async fn try_consume_provider_credit(&self, id: &str) -> Result<bool, CoreError> {
        let account = self.account_by_id(id).await?;
        if !account.use_credits || account.agent != AgentKind::Codex {
            return Ok(false);
        }
        let home = self.ensure_account_home(&account)?;
        let env = selector_env(&account, &home, None);
        tokio::task::spawn_blocking(move || consume_codex_reset_credit(&env))
            .await
            .map_err(|err| CoreError::Other(err.to_string()))?
    }

    pub(crate) async fn provider_accounts_configured(&self) -> bool {
        self.get_limit_policy()
            .await
            .map(|p| !p.accounts.is_empty())
            .unwrap_or(false)
    }

    async fn account_by_id(&self, id: &str) -> Result<ProviderAccount, CoreError> {
        validate_id(id)?;
        self.get_limit_policy()
            .await?
            .accounts
            .into_iter()
            .find(|a| a.id == id)
            .ok_or_else(|| CoreError::Other("Provider account was not found".into()))
    }

    fn account_home(&self, account: &ProviderAccount) -> Result<PathBuf, CoreError> {
        validate_id(&account.id)?;
        Ok(self
            .data_dir
            .join("provider-accounts")
            .join(&account.id)
            .join(if account.agent == AgentKind::Codex {
                "codex-home"
            } else {
                "claude-home"
            }))
    }

    fn ensure_account_home(&self, account: &ProviderAccount) -> Result<PathBuf, CoreError> {
        let path = self.account_home(account)?;
        std::fs::create_dir_all(&path)
            .map_err(|err| CoreError::Other(format!("could not create account home: {err}")))?;
        Ok(path)
    }

    fn probe_provider_account(&self, account: &ProviderAccount) -> (bool, Option<String>) {
        let Ok(home) = self.ensure_account_home(account) else {
            return (false, Some("Account storage is unavailable".into()));
        };
        if account.auth_mode == ProviderAccountAuthMode::OauthToken {
            let authenticated = account_token(account).ok().flatten().is_some();
            return (
                authenticated,
                (!authenticated).then(|| "Paste a token generated by `claude setup-token`".into()),
            );
        }
        let name = if account.agent == AgentKind::Codex {
            "codex"
        } else {
            "claude"
        };
        let Some(binary) = am_agents::find_binary(name) else {
            return (
                false,
                Some(format!("{} CLI is not installed", account.agent.label())),
            );
        };
        let args: &[&str] = if account.agent == AgentKind::Codex {
            &["login", "status"]
        } else {
            &["auth", "status", "--json"]
        };
        let mut command = Command::new(binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in selector_env(account, &home, None) {
            command.env(key, value);
        }
        if let Ok(output) = command.output() {
            if output.status.success() {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                )
                .to_lowercase()
                .replace(' ', "");
                let ok = if account.agent == AgentKind::Codex {
                    text.contains("loggedin") && !text.contains("notloggedin")
                } else {
                    text.contains("\"loggedin\":true") || text.contains("\"logged_in\":true")
                };
                return (ok, (!ok).then(|| "Sign-in has not completed".into()));
            }
        }
        let file = if account.agent == AgentKind::Codex {
            home.join("auth.json")
        } else {
            home.join(".credentials.json")
        };
        (
            file.is_file(),
            (!file.is_file()).then(|| "Sign-in has not completed".into()),
        )
    }
}

fn ready_runtime(
    core: &AppCore,
    statuses: impl Iterator<Item = ProviderAccountStatus>,
) -> Result<Option<AccountRuntime>, CoreError> {
    if let Some(status) = first_ready_status(statuses) {
        let home = core.ensure_account_home(&status.account)?;
        let token = account_token(&status.account)?;
        return Ok(Some(AccountRuntime {
            id: status.account.id.clone(),
            agent: status.account.agent,
            env: selector_env(&status.account, &home, token.as_deref()),
        }));
    }
    Ok(None)
}

fn first_ready_status(
    statuses: impl Iterator<Item = ProviderAccountStatus>,
) -> Option<ProviderAccountStatus> {
    statuses.into_iter().find(|status| {
        status.account.enabled
            && status.authenticated
            && status.availability != AvailabilityState::Limited
    })
}

fn selector_env(
    account: &ProviderAccount,
    home: &Path,
    token: Option<&str>,
) -> Vec<(String, String)> {
    if account.agent == AgentKind::Codex {
        vec![
            ("CODEX_HOME".into(), home.to_string_lossy().into_owned()),
            ("OPENAI_API_KEY".into(), "".into()),
            ("CODEX_API_KEY".into(), "".into()),
            ("CODEX_ACCESS_TOKEN".into(), "".into()),
        ]
    } else {
        vec![
            (
                "CLAUDE_CONFIG_DIR".into(),
                home.to_string_lossy().into_owned(),
            ),
            ("ANTHROPIC_API_KEY".into(), "".into()),
            ("ANTHROPIC_AUTH_TOKEN".into(), "".into()),
            ("CLAUDE_CODE_OAUTH_TOKEN".into(), token.unwrap_or("").into()),
            ("CLAUDE_CODE_DISABLE_FAST_MODE".into(), "1".into()),
        ]
    }
}

fn account_token(account: &ProviderAccount) -> Result<Option<String>, CoreError> {
    if account.auth_mode != ProviderAccountAuthMode::OauthToken {
        return Ok(None);
    }
    match token_entry(&account.id)?.get_password() {
        Ok(token) if !token.trim().is_empty() => Ok(Some(token)),
        Ok(_) | Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(keyring_error(err)),
    }
}

fn token_entry(id: &str) -> Result<Entry, CoreError> {
    validate_id(id)?;
    Entry::new(KEYRING_SERVICE, id).map_err(keyring_error)
}
fn validate_id(id: &str) -> Result<(), CoreError> {
    if id.len() < 8
        || id.len() > 128
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        Err(CoreError::Other(
            "Invalid provider account identifier".into(),
        ))
    } else {
        Ok(())
    }
}
fn keyring_error(err: keyring::Error) -> CoreError {
    CoreError::Other(format!("OS credential vault error: {err}"))
}

fn consume_codex_reset_credit(env: &[(String, String)]) -> Result<bool, CoreError> {
    use std::io::{BufRead, BufReader, Write};
    use std::time::{Duration, Instant};
    let binary = am_agents::find_binary("codex")
        .ok_or_else(|| CoreError::Other("Codex CLI is not installed".into()))?;
    let mut command = Command::new(binary);
    command
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    for (key, value) in env {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|err| CoreError::Other(err.to_string()))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| CoreError::Other("Codex app-server stdin unavailable".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| CoreError::Other("Codex app-server stdout unavailable".into()))?;
    let messages = [
        serde_json::json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"Perpetual","version":env!("CARGO_PKG_VERSION")}}}),
        serde_json::json!({"method":"initialized","params":{}}),
        serde_json::json!({"id":2,"method":"account/rateLimitResetCredit/consume","params":{"idempotencyKey":am_proto::new_id()}}),
    ];
    for message in messages {
        writeln!(stdin, "{message}").map_err(|err| CoreError::Other(err.to_string()))?;
    }
    stdin
        .flush()
        .map_err(|err| CoreError::Other(err.to_string()))?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut redeemed = false;
    while let Ok(line) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if value.get("id").and_then(|id| id.as_u64()) != Some(2) {
            continue;
        }
        redeemed = matches!(
            value.pointer("/result/outcome").and_then(|v| v.as_str()),
            Some("reset" | "alreadyRedeemed")
        );
        break;
    }
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    Ok(redeemed)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn account_with_id(id: &str, agent: AgentKind) -> ProviderAccount {
        ProviderAccount {
            id: id.into(),
            label: "Test".into(),
            agent,
            enabled: true,
            use_credits: false,
            auth_mode: ProviderAccountAuthMode::IsolatedCli,
        }
    }
    fn account(agent: AgentKind) -> ProviderAccount {
        account_with_id("account-1234", agent)
    }
    #[test]
    fn codex_selector_isolates_home_and_clears_key_override() {
        let env = selector_env(&account(AgentKind::Codex), Path::new("/tmp/codex-a"), None);
        assert!(env
            .iter()
            .any(|(k, v)| k == "CODEX_HOME" && v.ends_with("codex-a")));
        assert!(env
            .iter()
            .any(|(k, v)| k == "OPENAI_API_KEY" && v.is_empty()));
    }
    #[test]
    fn claude_selector_uses_selected_token() {
        let mut a = account(AgentKind::ClaudeCode);
        a.auth_mode = ProviderAccountAuthMode::OauthToken;
        let env = selector_env(&a, Path::new("/tmp/claude-b"), Some("secret"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "CLAUDE_CODE_OAUTH_TOKEN" && v == "secret"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_API_KEY" && v.is_empty()));
    }

    #[test]
    fn dummy_five_account_pool_rotates_in_global_priority_order() {
        let mut statuses: Vec<_> = [
            ("codex-0001", AgentKind::Codex),
            ("codex-0002", AgentKind::Codex),
            ("codex-0003", AgentKind::Codex),
            ("claude-001", AgentKind::ClaudeCode),
            ("claude-002", AgentKind::ClaudeCode),
        ]
        .into_iter()
        .map(|(id, agent)| ProviderAccountStatus {
            account: account_with_id(id, agent),
            authenticated: true,
            availability: AvailabilityState::Available,
            reset_at: None,
            detail: None,
        })
        .collect();

        let mut selected = Vec::new();
        while let Some(next) = first_ready_status(statuses.clone().into_iter()) {
            selected.push(next.account.id.clone());
            statuses
                .iter_mut()
                .find(|item| item.account.id == next.account.id)
                .unwrap()
                .availability = AvailabilityState::Limited;
        }

        assert_eq!(
            selected,
            [
                "codex-0001",
                "codex-0002",
                "codex-0003",
                "claude-001",
                "claude-002"
            ]
        );
        assert!(first_ready_status(statuses.into_iter()).is_none());
    }

    #[test]
    fn disabled_unauthenticated_and_limited_slots_are_skipped() {
        let mut disabled = account_with_id("codex-off", AgentKind::Codex);
        disabled.enabled = false;
        let statuses = vec![
            ProviderAccountStatus {
                account: disabled,
                authenticated: true,
                availability: AvailabilityState::Available,
                reset_at: None,
                detail: None,
            },
            ProviderAccountStatus {
                account: account_with_id("codex-noauth", AgentKind::Codex),
                authenticated: false,
                availability: AvailabilityState::Available,
                reset_at: None,
                detail: None,
            },
            ProviderAccountStatus {
                account: account_with_id("codex-limited", AgentKind::Codex),
                authenticated: true,
                availability: AvailabilityState::Limited,
                reset_at: None,
                detail: None,
            },
            ProviderAccountStatus {
                account: account_with_id("claude-ready", AgentKind::ClaudeCode),
                authenticated: true,
                availability: AvailabilityState::Available,
                reset_at: None,
                detail: None,
            },
        ];
        assert_eq!(
            first_ready_status(statuses.into_iter()).unwrap().account.id,
            "claude-ready"
        );
    }
}
