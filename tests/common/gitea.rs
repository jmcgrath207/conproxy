//! Gitea REST API client for E2E tests.
//!
//! Provides `GiteaClient` to create repos, add files, create tags, and clean up
//! via the Gitea API. Tests using this client skip gracefully when `GITEA_URL`
//! and `GITEA_TOKEN` environment variables are not set.

use serde_json::json;

/// Skip the current test if Gitea env vars are not set.
macro_rules! skip_without_gitea {
    () => {
        if std::env::var("GITEA_URL").is_err() || std::env::var("GITEA_TOKEN").is_err() {
            eprintln!("SKIP: GITEA_URL or GITEA_TOKEN not set — skipping remote git test");
            return;
        }
    };
}
pub(crate) use skip_without_gitea;

/// Blocking HTTP client for the Gitea REST API.
pub struct GiteaClient {
    base_url: String,
    token: String,
    user: String,
}

impl GiteaClient {
    /// Create a client from `GITEA_URL`, `GITEA_TOKEN`, `GITEA_USER` env vars.
    /// Returns `None` if any are unset.
    pub fn from_env() -> Option<Self> {
        Some(Self {
            base_url: std::env::var("GITEA_URL").ok()?,
            token: std::env::var("GITEA_TOKEN").ok()?,
            user: std::env::var("GITEA_USER").ok()?,
        })
    }

    /// HTTP clone URL for a repo owned by this user.
    pub fn clone_url(&self, repo: &str) -> String {
        format!("{}/{}/{}.git", self.base_url, self.user, repo)
    }

    fn auth_header(&self) -> String {
        format!("token {}", self.token)
    }

    /// Create a new public repo with an initial commit (auto_init).
    pub fn create_repo(&self, name: &str) {
        let url = format!("{}/api/v1/user/repos", self.base_url);
        let body = json!({
            "name": name,
            "auto_init": true,
            "private": false,
            "default_branch": "main"
        });
        let resp = ureq::post(&url)
            .header("Authorization", &self.auth_header())
            .send_json(&body)
            .unwrap_or_else(|e| panic!("Failed to create repo '{}': {}", name, e));
        assert!(
            resp.status() == 201,
            "create_repo '{}' returned {}",
            name,
            resp.status()
        );
    }

    /// Add or update a file in a repo.
    pub fn add_file(&self, repo: &str, path: &str, content: &str, message: &str) {
        let url = format!(
            "{}/api/v1/repos/{}/{}/contents/{}",
            self.base_url, self.user, repo, path
        );
        let encoded = base64_encode(content.as_bytes());
        let body = json!({
            "content": encoded,
            "message": message
        });
        let resp = ureq::post(&url)
            .header("Authorization", &self.auth_header())
            .send_json(&body)
            .unwrap_or_else(|e| panic!("Failed to add file '{}/{}': {}", repo, path, e));
        assert!(
            resp.status() == 201,
            "add_file '{}/{}' returned {}",
            repo,
            path,
            resp.status()
        );
    }

    /// Create a lightweight tag pointing at the default branch HEAD.
    pub fn create_tag(&self, repo: &str, tag: &str) {
        let url = format!("{}/api/v1/repos/{}/{}/tags", self.base_url, self.user, repo);
        let body = json!({
            "tag_name": tag,
            "target": "main"
        });
        let resp = ureq::post(&url)
            .header("Authorization", &self.auth_header())
            .send_json(&body)
            .unwrap_or_else(|e| panic!("Failed to create tag '{}' on '{}': {}", tag, repo, e));
        assert!(
            resp.status() == 201,
            "create_tag '{}' on '{}' returned {}",
            tag,
            repo,
            resp.status()
        );
    }

    /// Convenience: create repo, add files, optionally create tags.
    pub fn create_repo_with_files(&self, name: &str, files: &[(&str, &str)], tags: &[&str]) {
        self.create_repo(name);
        for (path, content) in files {
            self.add_file(name, path, content, &format!("Add {}", path));
        }
        for tag in tags {
            self.create_tag(name, tag);
        }
    }

    /// Delete a repo (cleanup).
    pub fn delete_repo(&self, name: &str) {
        let url = format!("{}/api/v1/repos/{}/{}", self.base_url, self.user, name);
        let _ = ureq::delete(&url)
            .header("Authorization", &self.auth_header())
            .call();
    }
}

/// Minimal base64 encoder (standard alphabet, with padding).
/// Avoids adding a dependency for a trivial operation used only in tests.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity((input.len() + 2) / 3 * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"a"), "YQ==");
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }
}
