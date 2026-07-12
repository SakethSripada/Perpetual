use am_proto::{
    now, GithubAuthStatus, GithubDeviceFlow, GithubDevicePoll, GithubDevicePollState,
    GithubPullRequest, GithubRepository, NewGithubRepo, Repo, RepoKind,
};
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use chrono::Duration;
use keyring::Entry;
use reqwest::header::{ACCEPT, AUTHORIZATION};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::path::{Path, PathBuf};

use crate::{AppCore, CoreError};

const GITHUB_CLIENT_ID_ENV: &str = "PERPETUAL_GITHUB_CLIENT_ID";
const GITHUB_SCOPES: &str = "repo read:user";
const GITHUB_API_VERSION: &str = "2026-03-10";
const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
const GITHUB_ACCESS_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
const GITHUB_API_BASE: &str = "https://api.github.com";
const KEYRING_SERVICE: &str = "com.perpetual.app";
const KEYRING_GITHUB_TOKEN: &str = "github-oauth-token";
const USER_AGENT_VALUE: &str = "Perpetual/0.1";

impl AppCore {
    pub async fn github_auth_status(&self) -> Result<GithubAuthStatus, CoreError> {
        let configured = github_client_id().is_some();
        let Some(token) = load_github_token()? else {
            return Ok(GithubAuthStatus {
                configured,
                authenticated: false,
                login: None,
                avatar_url: None,
                error: None,
            });
        };

        match self.github_user(&token).await {
            Ok(user) => Ok(GithubAuthStatus {
                configured,
                authenticated: true,
                login: Some(user.login),
                avatar_url: user.avatar_url,
                error: None,
            }),
            Err(err) => Ok(GithubAuthStatus {
                configured,
                authenticated: false,
                login: None,
                avatar_url: None,
                error: Some(err.to_string()),
            }),
        }
    }

    pub async fn github_start_device_flow(&self) -> Result<GithubDeviceFlow, CoreError> {
        let client_id = github_client_id().ok_or_else(missing_client_id_error)?;
        let http = github_http()?;

        let response = http
            .post(GITHUB_DEVICE_CODE_URL)
            .header(ACCEPT, "application/json")
            .form(&[("client_id", client_id.as_str()), ("scope", GITHUB_SCOPES)])
            .send()
            .await
            .map_err(http_error)?;

        ensure_success(response.status(), "start GitHub device flow")?;
        let body: DeviceCodeResponse = response.json().await.map_err(http_error)?;

        Ok(GithubDeviceFlow {
            device_code: body.device_code,
            user_code: body.user_code,
            verification_uri: body.verification_uri,
            expires_at: now() + Duration::seconds(body.expires_in as i64),
            interval_seconds: body.interval,
        })
    }

    pub async fn github_poll_device_flow(
        &self,
        device_code: &str,
    ) -> Result<GithubDevicePoll, CoreError> {
        let client_id = github_client_id().ok_or_else(missing_client_id_error)?;
        let http = github_http()?;

        let response = http
            .post(GITHUB_ACCESS_TOKEN_URL)
            .header(ACCEPT, "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("device_code", device_code),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .map_err(http_error)?;

        ensure_success(response.status(), "poll GitHub device flow")?;
        let body: AccessTokenResponse = response.json().await.map_err(http_error)?;

        if let Some(token) = body.access_token {
            store_github_token(&token)?;
            let user = self.github_user(&token).await?;
            let status = GithubAuthStatus {
                configured: true,
                authenticated: true,
                login: Some(user.login.clone()),
                avatar_url: user.avatar_url.clone(),
                error: None,
            };
            let _ = self
                .activity(
                    None,
                    None,
                    "github.authenticated",
                    json!({ "login": user.login }),
                )
                .await;
            return Ok(GithubDevicePoll {
                state: GithubDevicePollState::Authorized,
                status: Some(status),
                interval_seconds: None,
                error: None,
            });
        }

        Ok(poll_from_error(
            body.error.as_deref(),
            body.error_description,
            body.interval,
        ))
    }

    pub async fn github_disconnect(&self) -> Result<GithubAuthStatus, CoreError> {
        delete_github_token()?;
        let _ = self
            .activity(None, None, "github.disconnected", json!({}))
            .await;
        self.github_auth_status().await
    }

    pub async fn github_list_repositories(&self) -> Result<Vec<GithubRepository>, CoreError> {
        let token = load_github_token()?
            .ok_or_else(|| CoreError::Other("GitHub is not authenticated".into()))?;
        self.github_list_repositories_with_token(&token).await
    }

    pub async fn github_auth_status_for_token(
        &self,
        token: &str,
    ) -> Result<GithubAuthStatus, CoreError> {
        match self.github_user(token).await {
            Ok(user) => Ok(GithubAuthStatus {
                configured: true,
                authenticated: true,
                login: Some(user.login),
                avatar_url: user.avatar_url,
                error: None,
            }),
            Err(err) => Ok(GithubAuthStatus {
                configured: true,
                authenticated: false,
                login: None,
                avatar_url: None,
                error: Some(err.to_string()),
            }),
        }
    }

    pub async fn github_list_repositories_with_token(
        &self,
        token: &str,
    ) -> Result<Vec<GithubRepository>, CoreError> {
        let http = github_http()?;
        let mut repos = Vec::new();
        let mut page = 1_u32;

        loop {
            let page_string = page.to_string();
            let response = http
                .get(format!("{GITHUB_API_BASE}/user/repos"))
                .header(ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .query(&[
                    ("visibility", "all"),
                    ("affiliation", "owner,collaborator,organization_member"),
                    ("sort", "updated"),
                    ("direction", "desc"),
                    ("per_page", "100"),
                    ("page", page_string.as_str()),
                ])
                .send()
                .await
                .map_err(http_error)?;

            ensure_success(response.status(), "list GitHub repositories")?;
            let batch: Vec<GithubRepository> = response.json().await.map_err(http_error)?;
            let done = batch.len() < 100;
            repos.extend(batch);
            if done {
                break;
            }
            page += 1;
        }

        Ok(repos)
    }

    pub async fn connect_github_repo(&self, input: NewGithubRepo) -> Result<Repo, CoreError> {
        let token = load_github_token()?
            .ok_or_else(|| CoreError::Other("GitHub is not authenticated".into()))?;
        self.connect_github_repo_with_token(input, &token).await
    }

    pub async fn connect_github_repo_with_token(
        &self,
        input: NewGithubRepo,
        token: &str,
    ) -> Result<Repo, CoreError> {
        let remote_url = input.clone_url.trim().to_string();
        if remote_url.is_empty() {
            return Err(CoreError::Other("GitHub clone URL is empty".into()));
        }

        let clone_path = self.managed_github_repo_path(&input.full_name);

        if let Some(repo) =
            am_db::repos::repo::get_by_project_remote(&self.db.pool, &input.project_id, &remote_url)
                .await?
        {
            let existing_path = repo
                .local_path
                .as_deref()
                .map(PathBuf::from)
                .unwrap_or_else(|| clone_path.clone());
            self.ensure_github_clone(&remote_url, &existing_path, token)
                .await?;
            return Ok(repo);
        }

        let info = self
            .ensure_github_clone(&remote_url, &clone_path, token)
            .await?;
        let display_name = non_empty(&input.full_name)
            .or_else(|| non_empty(&input.name))
            .unwrap_or_else(|| info.name.clone());
        let default_branch = non_empty(&input.default_branch).unwrap_or(info.default_branch);

        let repo = am_db::repos::repo::create_github(
            &self.db.pool,
            &input.project_id,
            &display_name,
            &info.toplevel.to_string_lossy(),
            &remote_url,
            &default_branch,
        )
        .await?;

        self.events
            .publish(am_proto::AppEvent::RepoConnected(repo.clone()));
        self.activity(
            Some(repo.project_id.clone()),
            None,
            "repo.github_connected",
            json!({
                "name": repo.name,
                "remote_url": repo.remote_url,
                "path": repo.local_path,
            }),
        )
        .await?;
        Ok(repo)
    }

    async fn ensure_github_clone(
        &self,
        remote_url: &str,
        clone_path: &Path,
        token: &str,
    ) -> Result<am_vcs::RepoInfo, CoreError> {
        let remote_url = remote_url.to_string();
        let clone_path = clone_path.to_path_buf();
        let auth_header = github_basic_auth_header(token);
        tokio::task::spawn_blocking(move || {
            am_vcs::clone_repo(&remote_url, &clone_path, Some(&auth_header))
        })
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
        .map_err(|e| CoreError::Other(e.to_string()))
    }

    fn managed_github_repo_path(&self, full_name: &str) -> PathBuf {
        let mut path = self.data_dir.join("repos").join("github");
        let mut pushed = false;
        for segment in full_name.split('/').filter(|segment| !segment.is_empty()) {
            path.push(sanitize_path_segment(segment));
            pushed = true;
        }
        if !pushed {
            path.push(sanitize_path_segment(full_name));
        }
        path
    }

    pub async fn open_github_pull_request(
        &self,
        task_id: &str,
    ) -> Result<GithubPullRequest, CoreError> {
        let token = load_github_token()?
            .ok_or_else(|| CoreError::Other("GitHub is not authenticated".into()))?;
        let task = am_db::repos::task::get(&self.db.pool, task_id)
            .await?
            .ok_or(CoreError::NotFound)?;
        let link = am_db::repos::task_repo::get_for_task(&self.db.pool, task_id)
            .await?
            .ok_or_else(|| CoreError::Other("task has no repository worktree yet".into()))?;
        let repo = am_db::repos::repo::get(&self.db.pool, &link.repo_id)
            .await?
            .ok_or(CoreError::NotFound)?;

        if repo.kind != RepoKind::GitHub {
            return Err(CoreError::Other(
                "pull requests are only available for GitHub repositories".into(),
            ));
        }

        let worktree = link
            .worktree_path
            .as_deref()
            .map(PathBuf::from)
            .ok_or_else(|| CoreError::Other("task has no repository worktree yet".into()))?;
        let branch = link
            .branch
            .clone()
            .ok_or_else(|| CoreError::Other("task has no branch yet".into()))?;
        let base_ref = link
            .base_ref
            .clone()
            .ok_or_else(|| CoreError::Other("task has no base commit yet".into()))?;
        let (owner, repo_name) = github_repo_slug(&repo)?;
        let auth_header = github_basic_auth_header(&token);
        let commit_title = format!("Perpetual: {}", task.title);
        let branch_for_git = branch.clone();
        let head_sha = tokio::task::spawn_blocking(move || {
            am_vcs::commit_all_with_excludes(
                &worktree,
                &commit_title,
                &["TASK_CONTEXT.md", "CLAUDE.md", "AGENTS.md"],
            )?;
            let head = am_vcs::head_sha(&worktree)?;
            if head == base_ref {
                return Err(am_vcs::VcsError::Git(
                    "no changes to open a pull request".into(),
                ));
            }
            am_vcs::push_branch(&worktree, &branch_for_git, Some(&auth_header))?;
            Ok::<_, am_vcs::VcsError>(head)
        })
        .await
        .map_err(|e| CoreError::Other(e.to_string()))?
        .map_err(|e| CoreError::Other(e.to_string()))?;

        if let Some(existing) = self
            .find_open_pull_request(&token, &owner, &repo_name, &branch)
            .await?
        {
            return Ok(existing);
        }

        let pr = self
            .create_pull_request(&token, &owner, &repo_name, &repo, &task, &branch)
            .await?;
        self.activity(
            Some(task.project_id),
            Some(task.id),
            "github.pull_request_opened",
            json!({
                "repo": repo.name,
                "branch": branch,
                "number": pr.number,
                "url": pr.html_url,
                "head_sha": head_sha,
            }),
        )
        .await?;
        Ok(pr)
    }

    async fn github_user(&self, token: &str) -> Result<GithubUser, CoreError> {
        let http = github_http()?;
        let response = http
            .get(format!("{GITHUB_API_BASE}/user"))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(http_error)?;

        ensure_success(response.status(), "fetch GitHub user")?;
        response.json().await.map_err(http_error)
    }

    async fn find_open_pull_request(
        &self,
        token: &str,
        owner: &str,
        repo: &str,
        branch: &str,
    ) -> Result<Option<GithubPullRequest>, CoreError> {
        let http = github_http()?;
        let head = format!("{owner}:{branch}");
        let response = http
            .get(format!("{GITHUB_API_BASE}/repos/{owner}/{repo}/pulls"))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .query(&[
                ("state", "open"),
                ("head", head.as_str()),
                ("per_page", "1"),
            ])
            .send()
            .await
            .map_err(http_error)?;

        ensure_success(response.status(), "find GitHub pull request")?;
        let prs: Vec<PullRequestResponse> = response.json().await.map_err(http_error)?;
        Ok(prs.into_iter().next().map(GithubPullRequest::from))
    }

    async fn create_pull_request(
        &self,
        token: &str,
        owner: &str,
        repo_name: &str,
        repo: &Repo,
        task: &am_proto::Task,
        branch: &str,
    ) -> Result<GithubPullRequest, CoreError> {
        let http = github_http()?;
        let head = format!("{owner}:{branch}");
        let body = format!(
            "Opened by Perpetual for task `{}`.\n\n{}",
            task.id,
            task.description.clone().unwrap_or_default()
        );
        let response = http
            .post(format!("{GITHUB_API_BASE}/repos/{owner}/{repo_name}/pulls"))
            .header(ACCEPT, "application/vnd.github+json")
            .header("X-GitHub-Api-Version", GITHUB_API_VERSION)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .json(&CreatePullRequestRequest {
                title: task.title.clone(),
                head,
                base: repo.default_branch.clone(),
                body,
            })
            .send()
            .await
            .map_err(http_error)?;

        ensure_success(response.status(), "open GitHub pull request")?;
        let pr: PullRequestResponse = response.json().await.map_err(http_error)?;
        Ok(pr.into())
    }
}

#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Debug, Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
    interval: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct GithubUser {
    login: String,
    avatar_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct CreatePullRequestRequest {
    title: String,
    head: String,
    base: String,
    body: String,
}

#[derive(Debug, Deserialize)]
struct PullRequestResponse {
    number: u64,
    title: String,
    html_url: String,
    head: PullRequestRef,
    base: PullRequestRef,
}

#[derive(Debug, Deserialize)]
struct PullRequestRef {
    #[serde(rename = "ref")]
    ref_name: String,
    sha: String,
}

impl From<PullRequestResponse> for GithubPullRequest {
    fn from(value: PullRequestResponse) -> Self {
        GithubPullRequest {
            number: value.number,
            title: value.title,
            html_url: value.html_url,
            head: value.head.ref_name,
            base: value.base.ref_name,
            head_sha: value.head.sha,
        }
    }
}

fn github_http() -> Result<reqwest::Client, CoreError> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT_VALUE)
        .build()
        .map_err(http_error)
}

fn github_client_id() -> Option<String> {
    std::env::var(GITHUB_CLIENT_ID_ENV)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn missing_client_id_error() -> CoreError {
    CoreError::Other(format!(
        "GitHub OAuth is not configured; set {GITHUB_CLIENT_ID_ENV} to this app's OAuth client ID"
    ))
}

fn token_entry() -> Result<Entry, CoreError> {
    Entry::new(KEYRING_SERVICE, KEYRING_GITHUB_TOKEN).map_err(keyring_error)
}

fn load_github_token() -> Result<Option<String>, CoreError> {
    match token_entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(keyring_error(err)),
    }
}

/// The `git push` auth header for the stored GitHub token, or `None` if there is
/// no token. Used by auto-push for GitHub repositories.
pub(crate) fn github_push_header() -> Option<String> {
    load_github_token()
        .ok()
        .flatten()
        .map(|token| github_basic_auth_header(&token))
}

fn store_github_token(token: &str) -> Result<(), CoreError> {
    token_entry()?.set_password(token).map_err(keyring_error)
}

fn delete_github_token() -> Result<(), CoreError> {
    match token_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(keyring_error(err)),
    }
}

fn github_repo_slug(repo: &Repo) -> Result<(String, String), CoreError> {
    if let Some((owner, name)) = split_slug(&repo.name) {
        return Ok((owner, name));
    }
    if let Some(remote_url) = &repo.remote_url {
        if let Some((owner, name)) = parse_github_remote(remote_url) {
            return Ok((owner, name));
        }
    }
    Err(CoreError::Other(
        "could not determine GitHub owner and repository name".into(),
    ))
}

fn split_slug(value: &str) -> Option<(String, String)> {
    let (owner, repo) = value.split_once('/')?;
    let owner = owner.trim();
    let repo = repo.trim().trim_end_matches(".git");
    if owner.is_empty() || repo.is_empty() {
        None
    } else {
        Some((owner.to_string(), repo.to_string()))
    }
}

fn parse_github_remote(remote_url: &str) -> Option<(String, String)> {
    let trimmed = remote_url.trim();
    if let Some(rest) = trimmed.strip_prefix("https://github.com/") {
        return split_slug(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("git@github.com:") {
        return split_slug(rest);
    }
    None
}

fn github_basic_auth_header(token: &str) -> String {
    let encoded = BASE64_STANDARD.encode(format!("x-access-token:{token}"));
    format!("AUTHORIZATION: basic {encoded}")
}

fn non_empty(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn sanitize_path_segment(segment: &str) -> String {
    let cleaned: String = segment
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.');
    if cleaned.is_empty() || cleaned == ".." {
        "repo".to_string()
    } else {
        cleaned.to_string()
    }
}

fn poll_from_error(
    error: Option<&str>,
    description: Option<String>,
    interval: Option<u64>,
) -> GithubDevicePoll {
    let state = match error {
        Some("authorization_pending") | None => GithubDevicePollState::Pending,
        Some("slow_down") => GithubDevicePollState::SlowDown,
        Some("expired_token") | Some("token_expired") => GithubDevicePollState::Expired,
        Some("access_denied") => GithubDevicePollState::Denied,
        Some(_) => GithubDevicePollState::Error,
    };

    GithubDevicePoll {
        state,
        status: None,
        interval_seconds: interval,
        error: match state {
            GithubDevicePollState::Pending | GithubDevicePollState::SlowDown => None,
            _ => description.or_else(|| error.map(str::to_string)),
        },
    }
}

fn ensure_success(status: reqwest::StatusCode, action: &str) -> Result<(), CoreError> {
    if status.is_success() {
        Ok(())
    } else {
        Err(CoreError::Other(format!(
            "GitHub request failed while trying to {action}: HTTP {status}"
        )))
    }
}

fn http_error(err: reqwest::Error) -> CoreError {
    CoreError::Other(format!("GitHub request failed: {err}"))
}

fn keyring_error(err: keyring::Error) -> CoreError {
    CoreError::Other(format!("GitHub keychain access failed: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn poll_error_maps_pending_and_slow_down_without_surface_error() {
        let pending = poll_from_error(Some("authorization_pending"), None, None);
        let slow = poll_from_error(Some("slow_down"), None, Some(10));

        assert_eq!(pending.state, GithubDevicePollState::Pending);
        assert!(pending.error.is_none());
        assert_eq!(slow.state, GithubDevicePollState::SlowDown);
        assert_eq!(slow.interval_seconds, Some(10));
        assert!(slow.error.is_none());
    }

    #[test]
    fn poll_error_maps_terminal_errors() {
        let expired = poll_from_error(Some("expired_token"), Some("expired".into()), None);
        let denied = poll_from_error(Some("access_denied"), None, None);
        let unknown = poll_from_error(Some("bad_thing"), Some("bad".into()), None);

        assert_eq!(expired.state, GithubDevicePollState::Expired);
        assert_eq!(expired.error.as_deref(), Some("expired"));
        assert_eq!(denied.state, GithubDevicePollState::Denied);
        assert_eq!(unknown.state, GithubDevicePollState::Error);
        assert_eq!(unknown.error.as_deref(), Some("bad"));
    }

    #[test]
    fn sanitizes_github_path_segments() {
        assert_eq!(sanitize_path_segment("owner"), "owner");
        assert_eq!(sanitize_path_segment("bad/name"), "bad_name");
        assert_eq!(sanitize_path_segment(".."), "repo");
        assert_eq!(sanitize_path_segment(""), "repo");
    }

    #[test]
    fn parses_github_remote_urls() {
        assert_eq!(
            parse_github_remote("https://github.com/owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(
            parse_github_remote("git@github.com:owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
        assert_eq!(parse_github_remote("https://example.com/owner/repo"), None);
    }
}
