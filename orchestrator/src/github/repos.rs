//! Repo browsing, detection, and workflow-file CRUD.

use serde::{Deserialize, Serialize};

use super::{GitHubApiError, GitHubClient};

/// Compose filenames probed at the repo root, in preference order.
pub const COMPOSE_CANDIDATES: [&str; 3] = ["docker-compose.yml", "compose.yaml", "compose.yml"];

#[derive(Debug, Deserialize)]
struct GhRepo {
    id: i64,
    full_name: String,
    private: bool,
    default_branch: String,
    pushed_at: Option<String>,
    updated_at: Option<String>,
    html_url: String,
    description: Option<String>,
    owner: GhOwner,
}

#[derive(Debug, Deserialize)]
struct GhOwner {
    login: String,
    avatar_url: Option<String>,
}

/// Dashboard-facing repo list item.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RepoSummary {
    pub id: i64,
    pub full_name: String,
    pub description: Option<String>,
    pub private: bool,
    pub default_branch: String,
    pub pushed_at: Option<String>,
    pub html_url: String,
    pub owner_login: String,
    pub owner_avatar: Option<String>,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RepoPage {
    pub repos: Vec<RepoSummary>,
    pub total_count: Option<i64>,
    pub page: u32,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct RepoDetail {
    pub id: i64,
    pub full_name: String,
    pub private: bool,
    pub default_branch: String,
    pub html_url: String,
    /// What the deploy form should default to.
    pub kind: String, // "compose" | "single"
    pub compose_file: Option<String>,
    pub dockerfile_present: bool,
    /// First `EXPOSE` port of the detected Dockerfile — link-form prefill.
    pub detected_port: Option<u16>,
}

#[derive(Debug, Deserialize)]
struct GhBranch {
    name: String,
    protected: bool,
}

#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct BranchSummary {
    pub name: String,
    pub protected: bool,
}

#[derive(Debug, Deserialize)]
struct GhContentEntry {
    name: String,
}

/// `folder/file`, or bare `file` at the repo root.
fn join_path(folder: &str, file: &str) -> String {
    if folder.is_empty() { file.to_string() } else { format!("{folder}/{file}") }
}

impl GitHubClient {
    /// PAT mode: bare-array endpoint, most recently pushed first.
    pub async fn list_repos(&self, page: u32) -> Result<RepoPage, GitHubApiError> {
        let list: Vec<GhRepo> = self
            .json(self.get(&format!("/user/repos?per_page=100&page={page}&sort=pushed&affiliation=owner,collaborator")))
            .await?;
        let repos = list
            .into_iter()
            .map(|r| RepoSummary {
                id: r.id,
                full_name: r.full_name,
                description: r.description,
                private: r.private,
                default_branch: r.default_branch,
                pushed_at: r.pushed_at.or(r.updated_at),
                html_url: r.html_url,
                owner_login: r.owner.login,
                owner_avatar: r.owner.avatar_url,
            })
            .collect();
        Ok(RepoPage { repos, total_count: None, page })
    }

    /// App mode: envelope endpoint ({total_count, repositories}).
    pub async fn list_installation_repos(&self, page: u32) -> Result<RepoPage, GitHubApiError> {
        #[derive(Deserialize)]
        struct Envelope {
            total_count: Option<i64>,
            repositories: Option<Vec<GhRepo>>,
        }
        let list: Envelope =
            self.json(self.get(&format!("/installation/repositories?per_page=100&page={page}&sort=pushed"))).await?;
        let total_count = list.total_count;
        let repos = list
            .repositories
            .unwrap_or_default()
            .into_iter()
            .map(|r| RepoSummary {
                id: r.id,
                full_name: r.full_name,
                description: r.description,
                private: r.private,
                default_branch: r.default_branch,
                pushed_at: r.pushed_at.or(r.updated_at),
                html_url: r.html_url,
                owner_login: r.owner.login,
                owner_avatar: r.owner.avatar_url,
            })
            .collect();
        Ok(RepoPage { repos, total_count, page })
    }

    /// Repo metadata + compose/Dockerfile detection at the given folder.
    pub async fn repo_detail(&self, owner: &str, repo: &str, path: Option<&str>) -> Result<RepoDetail, GitHubApiError> {
        let r: GhRepo = self.json(self.get(&format!("/repos/{owner}/{repo}"))).await?;
        let folder = path.filter(|p| !p.is_empty() && *p != ".").unwrap_or("");
        let contents_path = if folder.is_empty() {
            format!("/repos/{owner}/{repo}/contents/?ref={}", r.default_branch)
        } else {
            format!("/repos/{owner}/{repo}/contents/{folder}?ref={}", r.default_branch)
        };
        let entries: Vec<GhContentEntry> = self.json(self.get(&contents_path)).await?;
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let compose_file = COMPOSE_CANDIDATES.iter().copied().find(|c| names.contains(c)).map(|c| c.to_string());
        let dockerfile_name = names.iter().find(|n| n.starts_with("Dockerfile")).copied();
        let detected_port = match dockerfile_name {
            Some(name) if compose_file.is_none() => self
                .file_content(owner, repo, &join_path(folder, name), &r.default_branch)
                .await
                .ok()
                .flatten()
                .and_then(|content| litebin_common::types::detect_exposed_ports(&content).first().copied()),
            _ => None,
        };
        Ok(RepoDetail {
            id: r.id,
            full_name: r.full_name,
            private: r.private,
            default_branch: r.default_branch,
            html_url: r.html_url,
            kind: if compose_file.is_some() { "compose" } else { "single" }.to_string(),
            compose_file,
            dockerfile_present: dockerfile_name.is_some(),
            detected_port,
        })
    }

    pub async fn list_branches(&self, owner: &str, repo: &str) -> Result<Vec<BranchSummary>, GitHubApiError> {
        let branches: Vec<GhBranch> =
            self.json(self.get(&format!("/repos/{owner}/{repo}/branches?per_page=100"))).await?;
        Ok(branches.into_iter().map(|b| BranchSummary { name: b.name, protected: b.protected }).collect())
    }

    /// Decoded content of a file on a branch, or None when it does not exist.
    /// Used to skip no-op commits (GitHub's contents API commits identical
    /// content otherwise).
    pub async fn file_content(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        branch: &str,
    ) -> Result<Option<String>, GitHubApiError> {
        #[derive(Deserialize)]
        struct Content {
            content: Option<String>,
            encoding: Option<String>,
        }
        let c: Content = match self
            .json::<Content>(self.get(&format!("/repos/{owner}/{repo}/contents/{path}?ref={branch}")))
            .await
        {
            Ok(c) => c,
            Err(GitHubApiError::NotFound) => return Ok(None),
            Err(e) => return Err(e),
        };
        use base64::Engine as _;
        match (c.content, c.encoding.as_deref()) {
            (Some(b64), Some("base64")) => {
                let cleaned: String = b64.chars().filter(|ch| !ch.is_whitespace()).collect();
                match base64::engine::general_purpose::STANDARD.decode(cleaned) {
                    Ok(bytes) => Ok(String::from_utf8(bytes).ok()),
                    Err(e) => Err(GitHubApiError::Api { status: 502, message: format!("invalid file content: {e}") }),
                }
            }
            _ => Ok(None),
        }
    }

    /// SHA of a file on a branch, or None when it does not exist (404).
    pub async fn file_sha(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        branch: &str,
    ) -> Result<Option<String>, GitHubApiError> {
        let resp = self.get(&format!("/repos/{owner}/{repo}/contents/{path}?ref={branch}")).send().await?;
        match resp.status().as_u16() {
            200..=299 => {
                #[derive(Deserialize)]
                struct Content {
                    sha: String,
                }
                let c: Content = resp.json().await?;
                Ok(Some(c.sha))
            }
            404 => Ok(None),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(GitHubApiError::from_response(reqwest::StatusCode::from_u16(status).unwrap(), &body))
            }
        }
    }

    /// Create (`sha` None) or update a file; workflows/ needs the workflow scope.
    #[allow(clippy::too_many_arguments)] // contents-API call shape; a struct would obscure it
    pub async fn put_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        branch: &str,
        message: &str,
        content: &str,
        sha: Option<String>,
    ) -> Result<(), GitHubApiError> {
        use base64::Engine as _;
        let body = serde_json::json!({
            "message": message,
            "content": base64::engine::general_purpose::STANDARD.encode(content),
            "branch": branch,
            "sha": sha,
        });
        self.empty(self.send(reqwest::Method::PUT, &format!("/repos/{owner}/{repo}/contents/{path}")).json(&body)).await
    }

    /// Best-effort file delete (unlink cleanup). Missing file is success.
    pub async fn delete_file(&self, owner: &str, repo: &str, path: &str, branch: &str) -> Result<(), GitHubApiError> {
        let Some(sha) = self.file_sha(owner, repo, path, branch).await? else {
            return Ok(());
        };
        let body = serde_json::json!({
            "message": "LiteBin: remove deploy workflow",
            "branch": branch,
            "sha": sha,
        });
        self.empty(self.send(reqwest::Method::DELETE, &format!("/repos/{owner}/{repo}/contents/{path}")).json(&body))
            .await
    }
}
