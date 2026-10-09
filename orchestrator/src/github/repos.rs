//! Repo browsing and contents authoring: list repos (PAT), repo detail with
//! compose/Dockerfile detection, branches, and workflow-file CRUD.

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
}

#[derive(Debug, Deserialize)]
struct GhBranch {
    name: String,
    #[allow(dead_code)]
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

impl GitHubClient {
    /// PAT mode: repos the token can deploy (owner + collaborator), most recently pushed first.
    /// `/user/repos` returns a bare array (the {total_count, repositories} envelope is
    /// installation/search mode — M2).
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

    /// App mode: repos the (installation-scoped) client can see. Unlike
    /// `/user/repos`, this endpoint returns the {total_count, repositories} envelope.
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

    /// Repo metadata + root-level detection of compose files and Dockerfiles.
    pub async fn repo_detail(&self, owner: &str, repo: &str) -> Result<RepoDetail, GitHubApiError> {
        let r: GhRepo = self.json(self.get(&format!("/repos/{owner}/{repo}"))).await?;
        let entries: Vec<GhContentEntry> =
            self.json(self.get(&format!("/repos/{owner}/{repo}/contents/?ref={}", r.default_branch))).await?;
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        let compose_file = COMPOSE_CANDIDATES.iter().copied().find(|c| names.contains(c)).map(|c| c.to_string());
        let dockerfile_present = names.iter().any(|n| n.starts_with("Dockerfile"));
        Ok(RepoDetail {
            id: r.id,
            full_name: r.full_name,
            private: r.private,
            default_branch: r.default_branch,
            html_url: r.html_url,
            kind: if compose_file.is_some() { "compose" } else { "single" }.to_string(),
            compose_file,
            dockerfile_present,
        })
    }

    pub async fn list_branches(&self, owner: &str, repo: &str) -> Result<Vec<BranchSummary>, GitHubApiError> {
        let branches: Vec<GhBranch> =
            self.json(self.get(&format!("/repos/{owner}/{repo}/branches?per_page=100"))).await?;
        Ok(branches.into_iter().map(|b| BranchSummary { name: b.name, protected: b.protected }).collect())
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

    /// Create or update a file on a branch (contents API). `sha` updates, None creates.
    /// Writing under `.github/workflows/` requires the PAT `workflow` scope / App `workflows:write`.
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
