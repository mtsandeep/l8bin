//! GitHub Actions API: repo secrets (NaCl sealed box), workflow dispatch, run listing.

use serde::{Deserialize, Serialize};

use super::{GitHubApiError, GitHubClient};

#[derive(Debug, Deserialize)]
struct GhPublicKey {
    key_id: String,
    key: String,
}

#[derive(Debug, Deserialize)]
struct GhRun {
    id: i64,
    run_number: i64,
    status: Option<String>,
    conclusion: Option<String>,
    head_branch: Option<String>,
    head_sha: Option<String>,
    html_url: String,
    created_at: Option<String>,
    updated_at: Option<String>,
}

/// Normalized run row for the dashboard.
#[derive(Debug, Serialize, utoipa::ToSchema)]
pub struct WorkflowRun {
    pub id: i64,
    pub run_number: i64,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: Option<String>,
    pub html_url: String,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GhRunList {
    workflow_runs: Vec<GhRun>,
}

impl GitHubClient {
    /// Sealed-box encrypt with the repo's public key, then PUT.
    pub async fn put_secret(&self, owner: &str, repo: &str, name: &str, value: &str) -> Result<(), GitHubApiError> {
        use base64::Engine as _;
        use dryoc::dryocbox::DryocBox;
        use dryoc::keypair::PublicKey;

        let pk: GhPublicKey = self.json(self.get(&format!("/repos/{owner}/{repo}/actions/secrets/public-key"))).await?;
        let key_bytes = base64::engine::general_purpose::STANDARD
            .decode(&pk.key)
            .map_err(|e| GitHubApiError::Api { status: 502, message: format!("invalid repo public key: {e}") })?;
        let recipient = PublicKey::try_from(key_bytes.as_slice())
            .map_err(|e| GitHubApiError::Api { status: 502, message: format!("invalid repo public key: {e}") })?;
        let sealed = DryocBox::seal_to_vecbox(value.as_bytes(), &recipient)
            .map_err(|e| GitHubApiError::Api { status: 502, message: format!("secret encryption failed: {e}") })?;
        let encrypted = base64::engine::general_purpose::STANDARD.encode(sealed.to_vec());

        let body = serde_json::json!({ "encrypted_value": encrypted, "key_id": pk.key_id });
        self.empty(
            self.send(reqwest::Method::PUT, &format!("/repos/{owner}/{repo}/actions/secrets/{name}")).json(&body),
        )
        .await
    }

    /// Best-effort secret delete (unlink cleanup). Missing secret is success.
    pub async fn delete_secret(&self, owner: &str, repo: &str, name: &str) -> Result<(), GitHubApiError> {
        let resp =
            self.send(reqwest::Method::DELETE, &format!("/repos/{owner}/{repo}/actions/secrets/{name}")).send().await?;
        match resp.status().as_u16() {
            200..=299 | 404 => Ok(()),
            status => {
                let body = resp.text().await.unwrap_or_default();
                Err(GitHubApiError::from_response(
                    reqwest::StatusCode::from_u16(status).unwrap_or(reqwest::StatusCode::BAD_GATEWAY),
                    &body,
                ))
            }
        }
    }

    /// Trigger the generated workflow on `ref`.
    pub async fn dispatch_workflow(
        &self,
        owner: &str,
        repo: &str,
        workflow_file: &str,
        git_ref: &str,
    ) -> Result<(), GitHubApiError> {
        let body = serde_json::json!({ "ref": git_ref });
        self.empty(
            self.send(
                reqwest::Method::POST,
                &format!("/repos/{owner}/{repo}/actions/workflows/{workflow_file}/dispatches"),
            )
            .json(&body),
        )
        .await
    }

    /// Recent runs of the generated workflow, newest first.
    pub async fn list_workflow_runs(
        &self,
        owner: &str,
        repo: &str,
        workflow_file: &str,
        branch: Option<&str>,
        per_page: u32,
    ) -> Result<Vec<WorkflowRun>, GitHubApiError> {
        let mut path = format!("/repos/{owner}/{repo}/actions/workflows/{workflow_file}/runs?per_page={per_page}");
        if let Some(b) = branch {
            path.push_str(&format!("&branch={b}"));
        }
        let list: GhRunList = self.json(self.get(&path)).await?;
        Ok(list
            .workflow_runs
            .into_iter()
            .map(|r| WorkflowRun {
                id: r.id,
                run_number: r.run_number,
                status: r.status,
                conclusion: r.conclusion,
                head_branch: r.head_branch,
                head_sha: r.head_sha,
                html_url: r.html_url,
                created_at: r.created_at,
                updated_at: r.updated_at,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {

    use base64::Engine as _;
    use dryoc::dryocbox::DryocBox;

    /// Sealed-box roundtrip, mirroring put_secret's encryption half.
    #[test]
    fn sealed_box_roundtrip() {
        let keypair = dryoc::keypair::KeyPair::gen_with_defaults();
        let secret = "l8b-deploy-token-value";
        let sealed = DryocBox::seal_to_vecbox(secret.as_bytes(), &keypair.public_key).unwrap();
        let encoded = base64::engine::general_purpose::STANDARD.encode(sealed.to_vec());
        assert_ne!(encoded, secret);

        let bytes = base64::engine::general_purpose::STANDARD.decode(&encoded).unwrap();
        let reopened = DryocBox::from_sealed_bytes(&bytes).unwrap();
        let plaintext = reopened.unseal_to_vec(&keypair).unwrap();
        assert_eq!(plaintext, secret.as_bytes());
    }
}
