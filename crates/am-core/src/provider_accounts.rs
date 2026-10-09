use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use am_proto::{
    now, AgentKind, AvailabilityState, ProviderAccount, ProviderAccountAuthLaunch,
    ProviderAccountAuthMode, ProviderAccountStatus,
};
use keyring::Entry;

use crate::{AppCore, CoreError};

const KEYRING_SERVICE: &str = "Perpetual Provider Accounts";
/// Sign-in probes spawn the provider CLI, so results are reused briefly.
/// `detect_agents` and sign-in launches invalidate the cache.
const PROBE_TTL: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub(crate) struct AccountRuntime {
    pub id: String,
    pub agent: AgentKind,
    pub env: Vec<(String, String)>,
    /// Identifies the provider profile a session lives in. Sessions can only
    /// be resumed inside the profile that created them. `None` is the CLI's
    /// default profile, shared with runs made before account pools existed.
    pub profile: Option<String>,
}

/// Outcome of choosing an account for a provider run.
pub(crate) enum AccountSelection {
    /// No accounts are configured for this provider; use the CLI as-is.
    Unmanaged,
    Ready(AccountRuntime),
    /// Every enabled, signed-in account of this provider is usage-limited.
    Limited {
        reset_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// No enabled account of this provider is signed in.
    SignedOut,
}

#[derive(Clone)]
struct Probe {
    installed: bool,
    authenticated: bool,
    detail: Option<String>,
    email: Option<String>,
    plan: Option<String>,
}

fn probe_cache() -> &'static Mutex<HashMap<String, (Instant, Probe)>> {
    static CACHE: OnceLock<Mutex<HashMap<String, (Instant, Probe)>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sync_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

pub(crate) fn invalidate_account_probes() {
    if let Ok(mut cache) = probe_cache().lock() {
        cache.clear();
    }
}

fn system_account_id(agent: AgentKind) -> String {
    format!("system-{}", agent.as_str())
}

fn cli_name(agent: AgentKind) -> &'static str {
    if agent == AgentKind::Codex {
        "codex"
    } else {
        "claude"
    }
}

impl AppCore {
    pub async fn provider_account_statuses(&self) -> Result<Vec<ProviderAccountStatus>, CoreError> {
        self.provider_account_statuses_for(None).await
    }

    // Starting Codex must not wait for Claude's login probe (or vice versa).
    async fn provider_account_statuses_for(
        &self,
        agent: Option<AgentKind>,
    ) -> Result<Vec<ProviderAccountStatus>, CoreError> {
        self.sync_system_provider_accounts_for(agent).await?;
        let accounts: Vec<_> = self
            .get_limit_policy()
            .await?
            .accounts
            .into_iter()
            .filter(|account| agent.is_none_or(|agent| account.agent == agent))
            .collect();
        let probes = self.probe_accounts(&accounts).await;
        let mut out = Vec::with_capacity(accounts.len());
        for (account, probe) in accounts.into_iter().zip(probes) {
            let state = am_db::repos::provider_account::get(&self.db.pool, &account.id).await?;
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
            } else if probe.authenticated && availability == AvailabilityState::Unknown {
                availability = AvailabilityState::Available;
            }
            out.push(ProviderAccountStatus {
                account,
                authenticated: probe.authenticated,
                email: probe.email,
                plan: probe.plan,
                availability,
                reset_at,
                detail: probe.detail,
                installed: probe.installed,
                active: false,
            });
        }
        mark_active(&mut out);
        Ok(out)
    }

    /// Registers the provider CLI's own sign-in as an account the first time it
    /// is found signed in, so an installed, signed-in CLI is never shown as a
    /// provider with no accounts. Removing it from the pool is remembered.
    async fn sync_system_provider_accounts_for(
        &self,
        only: Option<AgentKind>,
    ) -> Result<(), CoreError> {
        let policy = self.get_limit_policy().await?;
        let candidates: Vec<_> = [AgentKind::Codex, AgentKind::ClaudeCode]
            .into_iter()
            .filter(|agent| only.is_none_or(|selected| selected == *agent))
            .filter(|agent| !policy.dismissed_system_accounts.contains(agent))
            .filter(|agent| {
                !policy
                    .accounts
                    .iter()
                    .any(|a| a.agent == *agent && a.auth_mode == ProviderAccountAuthMode::System)
            })
            .map(system_account)
            .collect();
        if candidates.is_empty() {
            return Ok(());
        }
        // Never hold the policy lock across external CLI probes: a background
        // all-provider refresh must not block a selected-provider launch.
        let probes = self.probe_accounts(&candidates).await;
        let _guard = sync_lock().lock().await;
        let mut policy = self.get_limit_policy().await?;
        let mut added = false;
        for (account, probe) in candidates.into_iter().zip(probes) {
            if probe.authenticated
                && !policy.dismissed_system_accounts.contains(&account.agent)
                && !policy.accounts.iter().any(|a| {
                    a.agent == account.agent && a.auth_mode == ProviderAccountAuthMode::System
                })
            {
                policy.accounts.push(account);
                added = true;
            }
        }
        if added {
            self.set_limit_policy(policy).await?;
        }
        Ok(())
    }

    /// Adds the provider CLI's default sign-in to the pool (again) and returns
    /// its account id. Used by "Sign in" for a provider with no accounts.
    pub async fn add_system_provider_account(&self, agent: AgentKind) -> Result<String, CoreError> {
        if !matches!(agent, AgentKind::Codex | AgentKind::ClaudeCode) {
            return Err(CoreError::Other(
                "Only Codex and Claude accounts are supported".into(),
            ));
        }
        let _guard = sync_lock().lock().await;
        let mut policy = self.get_limit_policy().await?;
        policy.dismissed_system_accounts.retain(|a| *a != agent);
        let id = system_account_id(agent);
        if !policy.accounts.iter().any(|a| a.id == id) {
            policy.accounts.push(system_account(agent));
        }
        self.set_limit_policy(policy).await?;
        Ok(id)
    }

    /// Makes an account the one used for its provider's next run by moving it
    /// ahead of every other account in the pool.
    pub async fn activate_provider_account(&self, id: &str) -> Result<(), CoreError> {
        validate_id(id)?;
        let mut policy = self.get_limit_policy().await?;
        let index = policy
            .accounts
            .iter()
            .position(|a| a.id == id)
            .ok_or_else(|| CoreError::Other("Provider account was not found".into()))?;
        let mut account = policy.accounts.remove(index);
        account.enabled = true;
        policy.accounts.insert(0, account);
        self.set_limit_policy(policy).await?;
        Ok(())
    }

    pub async fn provider_account_auth_launch(
        &self,
        id: &str,
    ) -> Result<ProviderAccountAuthLaunch, CoreError> {
        let account = self.account_by_id(id).await?;
        let binary = provider_binary(account.agent)?;
        let home = self.ensure_account_home(&account)?;
        let (args, instructions): (Vec<String>, String) = match (account.agent, account.auth_mode) {
            (AgentKind::Codex, _) => (
                vec!["login".into()],
                "Finish signing in in your browser, then return to Perpetual.".into(),
            ),
            (AgentKind::ClaudeCode, ProviderAccountAuthMode::OauthToken) => (
                vec!["setup-token".into()],
                "Finish signing in, then paste the printed token into Perpetual.".into(),
            ),
            (AgentKind::ClaudeCode, _) => (
                vec!["auth".into(), "login".into()],
                "Finish signing in in your browser, then return to Perpetual.".into(),
            ),
            _ => {
                return Err(CoreError::Other(
                    "Only Codex and Claude accounts are supported".into(),
                ))
            }
        };
        invalidate_account_probes();
        let env = selector_env(&account, home.as_deref(), None);
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

    /// Opens the provider-owned interactive CLI for one account profile.
    /// The CLI remains responsible for plugin and MCP discovery, authentication,
    /// permissions, and persistence; Perpetual only selects the account profile.
    pub async fn provider_account_tooling_launch(
        &self,
        id: &str,
    ) -> Result<ProviderAccountAuthLaunch, CoreError> {
        let account = self.account_by_id(id).await?;
        let binary = provider_binary(account.agent)?;
        let home = self.ensure_account_home(&account)?;
        let token = account_token(&account)?;
        if account.auth_mode == ProviderAccountAuthMode::OauthToken && token.is_none() {
            return Err(CoreError::Other(
                "Add this account's setup token before opening its CLI".into(),
            ));
        }
        let label = account.label.clone();
        let env = selector_env(&account, home.as_deref(), token.as_deref());
        Ok(ProviderAccountAuthLaunch {
            account_id: account.id,
            label: label.clone(),
            agent: account.agent,
            binary: binary.to_string_lossy().into_owned(),
            args: Vec::new(),
            env,
            instructions: format!("Using {label}. Changes apply to new Perpetual sessions."),
        })
    }

    pub async fn set_provider_account_token(&self, id: &str, token: &str) -> Result<(), CoreError> {
        let account = self.account_by_id(id).await?;
        if account.agent != AgentKind::ClaudeCode
            || account.auth_mode != ProviderAccountAuthMode::OauthToken
        {
            return Err(CoreError::Other(
                "Setup tokens are only used by Claude token accounts".into(),
            ));
        }
        let token = token.trim();
        if !token.starts_with("sk-ant-oat")
            || token.len() < 40
            || token.chars().any(char::is_whitespace)
        {
            return Err(CoreError::Other(
                "That doesn't look like a token from `claude setup-token`".into(),
            ));
        }
        token_entry(id)?
            .set_password(token)
            .map_err(keyring_error)?;
        am_db::repos::provider_account::mark_available(&self.db.pool, id).await?;
        invalidate_account_probes();
        Ok(())
    }

    pub async fn delete_provider_account(&self, id: &str) -> Result<(), CoreError> {
        validate_id(id)?;
        let mut policy = self.get_limit_policy().await?;
        let removed = policy.accounts.iter().find(|a| a.id == id).cloned();
        policy.accounts.retain(|account| account.id != id);
        if let Some(account) = &removed {
            if account.auth_mode == ProviderAccountAuthMode::System
                && !policy.dismissed_system_accounts.contains(&account.agent)
            {
                policy.dismissed_system_accounts.push(account.agent);
            }
        }
        self.set_limit_policy(policy).await?;
        am_db::repos::provider_account::delete(&self.db.pool, id).await?;
        invalidate_account_probes();
        // The default CLI sign-in belongs to the user's CLI; never sign it out.
        if removed.is_some_and(|a| a.auth_mode == ProviderAccountAuthMode::System) {
            return Ok(());
        }
        let _ = token_entry(id)?.delete_credential();
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
    ) -> Result<AccountSelection, CoreError> {
        self.select_provider_account_for(agent, None).await
    }

    pub(crate) async fn select_provider_account_for(
        &self,
        agent: AgentKind,
        required_id: Option<&str>,
    ) -> Result<AccountSelection, CoreError> {
        let statuses: Vec<_> = self
            .provider_account_statuses_for(Some(agent))
            .await?
            .into_iter()
            .filter(|s| required_id.is_none_or(|id| s.account.id == id))
            .collect();
        if statuses.is_empty() {
            return Ok(if required_id.is_some() {
                AccountSelection::SignedOut
            } else {
                AccountSelection::Unmanaged
            });
        }
        let limited_resets: Vec<_> = statuses
            .iter()
            .filter(|s| {
                s.account.enabled && s.authenticated && s.availability == AvailabilityState::Limited
            })
            .map(|s| s.reset_at)
            .collect();
        if let Some(runtime) = ready_runtime(self, statuses.into_iter())? {
            return Ok(AccountSelection::Ready(runtime));
        }
        if limited_resets.is_empty() {
            return Ok(AccountSelection::SignedOut);
        }
        Ok(AccountSelection::Limited {
            reset_at: limited_resets.into_iter().flatten().min(),
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
        let env = selector_env(&account, home.as_deref(), None);
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

    /// The isolated profile directory for an account, or `None` for the CLI's
    /// default profile.
    fn account_home(&self, account: &ProviderAccount) -> Result<Option<PathBuf>, CoreError> {
        validate_id(&account.id)?;
        if account.auth_mode == ProviderAccountAuthMode::System {
            return Ok(None);
        }
        Ok(Some(
            self.data_dir
                .join("provider-accounts")
                .join(&account.id)
                .join(if account.agent == AgentKind::Codex {
                    "codex-home"
                } else {
                    "claude-home"
                }),
        ))
    }

    fn ensure_account_home(&self, account: &ProviderAccount) -> Result<Option<PathBuf>, CoreError> {
        let path = self.account_home(account)?;
        if let Some(path) = &path {
            std::fs::create_dir_all(path)
                .map_err(|err| CoreError::Other(format!("could not create account home: {err}")))?;
        }
        Ok(path)
    }

    /// Probes sign-in state for every account concurrently, reusing recent
    /// results. Each CLI probe has a deadline so a hung provider cannot stall
    /// account refreshes or prevent a task from starting indefinitely.
    async fn probe_accounts(&self, accounts: &[ProviderAccount]) -> Vec<Probe> {
        let mut pending = Vec::with_capacity(accounts.len());
        for account in accounts {
            let key = format!(
                "{:?}:{}:{:?}:{:?}",
                self.data_dir, account.id, account.agent, account.auth_mode
            );
            let cached = probe_cache().lock().ok().and_then(|cache| {
                cache
                    .get(&key)
                    .filter(|(at, _)| at.elapsed() < PROBE_TTL)
                    .map(|(_, probe)| probe.clone())
            });
            if let Some(probe) = cached {
                pending.push(Err(probe));
                continue;
            }
            let account = account.clone();
            let home = self.ensure_account_home(&account);
            pending.push(Ok((
                key,
                tokio::spawn(async move {
                    match home {
                        Ok(home) => probe_provider_account(&account, home.as_deref()).await,
                        Err(_) => failed_probe("Account storage is unavailable"),
                    }
                }),
            )));
        }
        let mut out = Vec::with_capacity(pending.len());
        for item in pending {
            match item {
                Err(cached) => out.push(cached),
                Ok((key, task)) => {
                    let probe = task
                        .await
                        .unwrap_or_else(|_| failed_probe("Sign-in check failed"));
                    if let Ok(mut cache) = probe_cache().lock() {
                        cache.insert(key, (Instant::now(), probe.clone()));
                    }
                    out.push(probe);
                }
            }
        }
        out
    }
}

fn failed_probe(detail: &str) -> Probe {
    Probe {
        installed: true,
        authenticated: false,
        detail: Some(detail.into()),
        email: None,
        plan: None,
    }
}

fn system_account(agent: AgentKind) -> ProviderAccount {
    ProviderAccount {
        id: system_account_id(agent),
        label: "Default".into(),
        agent,
        enabled: true,
        use_credits: false,
        auth_mode: ProviderAccountAuthMode::System,
    }
}

fn provider_binary(agent: AgentKind) -> Result<PathBuf, CoreError> {
    if !matches!(agent, AgentKind::Codex | AgentKind::ClaudeCode) {
        return Err(CoreError::Other(
            "Only Codex and Claude accounts are supported".into(),
        ));
    }
    am_agents::find_binary(cli_name(agent))
        .ok_or_else(|| CoreError::Other(format!("{} CLI is not installed", agent.label())))
}

/// Flags the first enabled, signed-in, unlimited account of each provider:
/// the account that provider's next run will use.
fn mark_active(statuses: &mut [ProviderAccountStatus]) {
    let mut seen: Vec<AgentKind> = Vec::new();
    for status in statuses.iter_mut() {
        if seen.contains(&status.account.agent) {
            continue;
        }
        if status.installed
            && status.account.enabled
            && status.authenticated
            && status.availability != AvailabilityState::Limited
        {
            status.active = true;
            seen.push(status.account.agent);
        }
    }
}

async fn probe_provider_account(account: &ProviderAccount, home: Option<&Path>) -> Probe {
    if account.auth_mode == ProviderAccountAuthMode::OauthToken {
        let authenticated = account_token(account).ok().flatten().is_some();
        return Probe {
            installed: am_agents::find_binary(cli_name(account.agent)).is_some(),
            authenticated,
            detail: (!authenticated).then(|| "Add a setup token to finish signing in".into()),
            email: None,
            plan: None,
        };
    }
    let Some(binary) = am_agents::find_binary(cli_name(account.agent)) else {
        return Probe {
            installed: false,
            authenticated: false,
            detail: Some(format!("{} CLI is not installed", account.agent.label())),
            email: None,
            plan: None,
        };
    };
    probe_cli_account(account, home, &binary).await
}

async fn probe_cli_account(account: &ProviderAccount, home: Option<&Path>, binary: &Path) -> Probe {
    probe_cli_account_with_timeout(account, home, binary, Duration::from_secs(10)).await
}

async fn probe_cli_account_with_timeout(
    account: &ProviderAccount,
    home: Option<&Path>,
    binary: &Path,
    timeout: Duration,
) -> Probe {
    let args: &[&str] = if account.agent == AgentKind::Codex {
        &["login", "status"]
    } else {
        &["auth", "status", "--json"]
    };
    let mut command = tokio::process::Command::new(binary);
    am_proto::hide_console(command.as_std_mut());
    command
        .kill_on_drop(true)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in selector_env(account, home, None) {
        command.env(key, value);
    }
    let codex_home = || {
        home.map(Path::to_path_buf)
            .or_else(|| std::env::var_os("CODEX_HOME").map(PathBuf::from))
            .or_else(|| user_home().map(|h| h.join(".codex")))
    };
    if let Ok(Ok(output)) = tokio::time::timeout(timeout, command.output()).await {
        if output.status.success() {
            let ok = login_status_authenticated(account.agent, &output.stdout, &output.stderr);
            let (email, plan) = if !ok {
                (None, None)
            } else if account.agent == AgentKind::Codex {
                codex_home()
                    .and_then(|home| codex_account_identity(&home))
                    .unwrap_or_default()
            } else {
                claude_account_identity(&output.stdout)
            };
            return Probe {
                installed: true,
                authenticated: ok,
                detail: (!ok).then(|| "Not signed in".into()),
                email,
                plan,
            };
        }
    }
    // A credentials file can survive expiration, revocation, or a failed
    // refresh. Only the provider's successful status check establishes login.
    Probe {
        installed: true,
        authenticated: false,
        detail: Some("Could not verify sign-in. Sign in again or retry the account check.".into()),
        email: None,
        plan: None,
    }
}

fn login_status_authenticated(agent: AgentKind, stdout: &[u8], stderr: &[u8]) -> bool {
    if agent == AgentKind::Codex {
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(stdout),
            String::from_utf8_lossy(stderr)
        )
        .to_lowercase();
        text.contains("logged in") && !text.contains("not logged in")
    } else {
        serde_json::from_slice::<serde_json::Value>(stdout)
            .ok()
            .and_then(|status| {
                status
                    .get("loggedIn")
                    .or_else(|| status.get("logged_in"))
                    .and_then(|v| v.as_bool())
            })
            == Some(true)
    }
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

fn display_email(value: Option<&str>) -> Option<String> {
    let email = value?.trim();
    let (local, domain) = email.split_once('@')?;
    (!local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && email.len() <= 320
        && !email.chars().any(|c| c.is_whitespace() || c.is_control()))
    .then(|| email.to_owned())
}

/// A short, display-only plan name such as `pro` or `plus`.
fn display_plan(value: Option<&str>) -> Option<String> {
    let plan = value?.trim();
    (!plan.is_empty()
        && plan.len() <= 32
        && plan
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
    .then(|| plan.to_ascii_lowercase())
}

fn claude_account_identity(output: &[u8]) -> (Option<String>, Option<String>) {
    let Ok(status) = serde_json::from_slice::<serde_json::Value>(output) else {
        return (None, None);
    };
    let signed_in = status
        .get("loggedIn")
        .or_else(|| status.get("logged_in"))
        .and_then(|v| v.as_bool())
        == Some(true);
    if !signed_in {
        return (None, None);
    }
    (
        display_email(status.get("email").and_then(|v| v.as_str())),
        display_plan(status.get("subscriptionType").and_then(|v| v.as_str())),
    )
}

fn codex_account_identity(home: &Path) -> Option<(Option<String>, Option<String>)> {
    let path = home.join("auth.json");
    if std::fs::metadata(&path).ok()?.len() > 262_144 {
        return None;
    }
    let auth: serde_json::Value = serde_json::from_slice(&std::fs::read(path).ok()?).ok()?;
    Some(codex_id_token_identity(
        auth.pointer("/tokens/id_token")?.as_str()?,
    ))
}

// Decode only for display labels after the CLI confirms sign-in. These claims
// never establish authentication or authorization, and tokens never leave core.
fn codex_id_token_identity(token: &str) -> (Option<String>, Option<String>) {
    use base64::Engine;
    let claims = (|| {
        if token.len() > 65_536 {
            return None;
        }
        let mut parts = token.split('.');
        parts.next()?;
        let payload = parts.next()?;
        parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload.trim_end_matches('='))
            .ok()?;
        serde_json::from_slice::<serde_json::Value>(&bytes).ok()
    })();
    let Some(claims) = claims else {
        return (None, None);
    };
    (
        display_email(
            claims
                .get("email")
                .or_else(|| claims.pointer("/https:~1~1api.openai.com~1profile/email"))
                .and_then(|v| v.as_str()),
        ),
        display_plan(
            claims
                .pointer("/https:~1~1api.openai.com~1auth/chatgpt_plan_type")
                .and_then(|v| v.as_str()),
        ),
    )
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
            env: selector_env(&status.account, home.as_deref(), token.as_deref()),
            profile: (status.account.auth_mode != ProviderAccountAuthMode::System)
                .then(|| status.account.id.clone()),
        }));
    }
    Ok(None)
}

fn first_ready_status(
    statuses: impl Iterator<Item = ProviderAccountStatus>,
) -> Option<ProviderAccountStatus> {
    statuses.into_iter().find(|status| {
        status.installed
            && status.account.enabled
            && status.authenticated
            && status.availability != AvailabilityState::Limited
    })
}

/// Environment that points a provider CLI at one account profile. The default
/// CLI profile (`home == None`) keeps the user's own environment untouched.
fn selector_env(
    account: &ProviderAccount,
    home: Option<&Path>,
    token: Option<&str>,
) -> Vec<(String, String)> {
    let Some(home) = home else {
        return if account.agent == AgentKind::ClaudeCode {
            vec![("CLAUDE_CODE_DISABLE_FAST_MODE".into(), "1".into())]
        } else {
            Vec::new()
        };
    };
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
    am_proto::hide_console(&mut command);
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

    #[test]
    fn login_status_requires_structured_claude_boolean() {
        for bytes in [
            br#"{"loggedIn": true}"#.as_slice(),
            b"{\n\"logged_in\":\ttrue\n}",
        ] {
            assert!(login_status_authenticated(
                AgentKind::ClaudeCode,
                bytes,
                b""
            ));
        }
        for bytes in [
            br#"{"loggedIn": false}"#.as_slice(),
            br#"{"error":"loggedIn: true"}"#,
            b"not JSON",
        ] {
            assert!(!login_status_authenticated(
                AgentKind::ClaudeCode,
                bytes,
                b""
            ));
        }
        assert!(login_status_authenticated(
            AgentKind::Codex,
            b"",
            b"Logged in using ChatGPT\n"
        ));
        assert!(!login_status_authenticated(
            AgentKind::Codex,
            b"",
            b"Not logged in\n"
        ));
    }

    #[tokio::test]
    async fn failed_cli_check_never_trusts_stale_credentials_file() {
        let root = std::env::temp_dir().join(format!("perpetual-auth-test-{}", am_proto::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("auth.json"), "{}").unwrap();
        std::fs::write(root.join(".credentials.json"), "{}").unwrap();
        #[cfg(windows)]
        let binary = {
            let binary = root.join("signed-out.cmd");
            std::fs::write(&binary, "@echo off\r\necho Not logged in\r\nexit /b 1\r\n").unwrap();
            binary
        };
        #[cfg(not(windows))]
        let binary = {
            use std::os::unix::fs::PermissionsExt;
            let binary = root.join("signed-out");
            std::fs::write(&binary, "#!/bin/sh\necho 'Not logged in'\nexit 1\n").unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            binary
        };
        for agent in [AgentKind::Codex, AgentKind::ClaudeCode] {
            let result = probe_cli_account(&account(agent), Some(&root), &binary).await;
            assert!(result.installed);
            assert!(!result.authenticated);
            assert!(result.detail.is_some());
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn hung_sign_in_check_returns_without_blocking_account_refresh() {
        let root =
            std::env::temp_dir().join(format!("perpetual-auth-timeout-{}", am_proto::new_id()));
        std::fs::create_dir_all(&root).unwrap();
        #[cfg(windows)]
        let binary = {
            let binary = root.join("hung.cmd");
            std::fs::write(&binary, "@echo off\r\n:spin\r\ngoto spin\r\n").unwrap();
            binary
        };
        #[cfg(not(windows))]
        let binary = {
            use std::os::unix::fs::PermissionsExt;
            let binary = root.join("hung");
            std::fs::write(&binary, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
            binary
        };
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            probe_cli_account_with_timeout(
                &account(AgentKind::Codex),
                Some(&root),
                &binary,
                Duration::from_millis(100),
            ),
        )
        .await
        .expect("probe must respect its deadline");
        assert!(!result.authenticated);
        // The async child is killed on cancellation; leave time for OS cleanup.
        tokio::time::sleep(Duration::from_millis(100)).await;
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn identity_is_display_only_and_requires_a_signed_in_claude_status() {
        assert_eq!(
            claude_account_identity(
                br#"{"loggedIn":true,"email":"person@example.com","subscriptionType":"pro"}"#
            ),
            (Some("person@example.com".into()), Some("pro".into()))
        );
        assert_eq!(
            claude_account_identity(br#"{"loggedIn":false,"email":"old@example.com"}"#),
            (None, None)
        );
        assert_eq!(
            claude_account_identity(br#"{"loggedIn":true,"authMethod":"oauth_token"}"#),
            (None, None)
        );
        assert_eq!(claude_account_identity(b"not json"), (None, None));
        assert_eq!(display_email(Some("secret-token")), None);
        assert_eq!(display_email(Some("a@b\nspoof")), None);
        assert_eq!(display_plan(Some("Max 20x!")), None);
    }

    #[test]
    fn codex_identity_accepts_email_claim_variants_without_exposing_tokens() {
        use base64::Engine;
        let token = |claims: &str| {
            format!(
                "header.{}.signature",
                base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims)
            )
        };
        assert_eq!(
            codex_id_token_identity(&token(
                r#"{"email":"person@example.com","access_token":"never-return-this","https://api.openai.com/auth":{"chatgpt_plan_type":"Plus"}}"#
            )),
            (Some("person@example.com".into()), Some("plus".into()))
        );
        assert_eq!(
            codex_id_token_identity(&token(
                r#"{"https://api.openai.com/profile":{"email":"profile@example.com"}}"#
            ))
            .0,
            Some("profile@example.com".into())
        );
        assert_eq!(
            codex_id_token_identity(&token(r#"{"sub":"id-only"}"#)),
            (None, None)
        );
        assert_eq!(codex_id_token_identity("malformed"), (None, None));
        assert_eq!(
            codex_id_token_identity("header.%%%.signature"),
            (None, None)
        );
    }

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
    fn status(id: &str, agent: AgentKind) -> ProviderAccountStatus {
        ProviderAccountStatus {
            account: account_with_id(id, agent),
            authenticated: true,
            email: None,
            plan: None,
            availability: AvailabilityState::Available,
            reset_at: None,
            detail: None,
            installed: true,
            active: false,
        }
    }
    #[test]
    fn codex_selector_isolates_home_and_clears_key_override() {
        let env = selector_env(
            &account(AgentKind::Codex),
            Some(Path::new("/tmp/codex-a")),
            None,
        );
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
        let env = selector_env(&a, Some(Path::new("/tmp/claude-b")), Some("secret"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "CLAUDE_CODE_OAUTH_TOKEN" && v == "secret"));
        assert!(env
            .iter()
            .any(|(k, v)| k == "ANTHROPIC_API_KEY" && v.is_empty()));
    }
    #[test]
    fn default_profile_keeps_the_users_cli_environment() {
        let mut a = account(AgentKind::Codex);
        a.auth_mode = ProviderAccountAuthMode::System;
        assert!(selector_env(&a, None, None).is_empty());
        a.agent = AgentKind::ClaudeCode;
        assert!(!selector_env(&a, None, None)
            .iter()
            .any(|(k, _)| k == "CLAUDE_CONFIG_DIR" || k == "ANTHROPIC_API_KEY"));
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
        .map(|(id, agent)| status(id, agent))
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
        let mut disabled = status("codex-off", AgentKind::Codex);
        disabled.account.enabled = false;
        let mut signed_out = status("codex-noauth", AgentKind::Codex);
        signed_out.authenticated = false;
        let mut limited = status("codex-limited", AgentKind::Codex);
        limited.availability = AvailabilityState::Limited;
        let statuses = vec![
            disabled,
            signed_out,
            limited,
            ProviderAccountStatus {
                installed: false,
                ..status("missing-cli", AgentKind::ClaudeCode)
            },
            status("claude-ready", AgentKind::ClaudeCode),
        ];
        assert_eq!(
            first_ready_status(statuses.into_iter()).unwrap().account.id,
            "claude-ready"
        );
    }

    #[test]
    fn policy_keeps_one_default_profile_account_per_provider() {
        let mut system = account_with_id("system-codex", AgentKind::Codex);
        system.auth_mode = ProviderAccountAuthMode::System;
        let mut duplicate = account_with_id("system-other", AgentKind::Codex);
        duplicate.auth_mode = ProviderAccountAuthMode::System;
        let policy = crate::orchestrate::normalize_limit_policy(am_proto::LimitPolicy {
            accounts: vec![
                system,
                account_with_id("codex-work", AgentKind::Codex),
                duplicate,
            ],
            dismissed_system_accounts: vec![AgentKind::Codex, AgentKind::ClaudeCode],
            ..Default::default()
        });
        let ids: Vec<_> = policy.accounts.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["system-other", "codex-work"]);
        assert_eq!(
            policy.accounts[0].auth_mode,
            ProviderAccountAuthMode::System
        );
        // Re-adding a default profile account clears its dismissal.
        assert_eq!(policy.dismissed_system_accounts, [AgentKind::ClaudeCode]);
    }

    #[test]
    fn active_marks_the_first_ready_account_of_each_provider() {
        let mut limited = status("codex-limited", AgentKind::Codex);
        limited.availability = AvailabilityState::Limited;
        let mut statuses = vec![
            limited,
            status("codex-second", AgentKind::Codex),
            status("codex-third", AgentKind::Codex),
            status("claude-first", AgentKind::ClaudeCode),
        ];
        mark_active(&mut statuses);
        let active: Vec<_> = statuses
            .iter()
            .filter(|s| s.active)
            .map(|s| s.account.id.as_str())
            .collect();
        assert_eq!(active, ["codex-second", "claude-first"]);
    }
}
