//! Third-party skill import.
//!
//! Third-party skills are distributed as Markdown folders (Claude-Code style
//! `SKILL.md`, optionally with companion scripts/docs). This module fetches a
//! skill from GitHub through the public API + raw.githubusercontent.com.
//! No token is required for public repositories.

use anyhow::{Context, Result, bail};
use serde::Deserialize;

/// Hard caps for a single import, guarding against runaway downloads.
const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
const MAX_TOTAL_BYTES: usize = 20 * 1024 * 1024;
const MAX_FILES: usize = 120;
const MAX_DESCRIPTION_CHARS: usize = 2000;

/// A skill parsed out of a third-party source, ready to be stored.
pub struct ImportedSkill {
    /// Skill name (frontmatter `name`, falling back to the repo/folder name).
    pub name: String,
    pub description: Option<String>,
    /// Canonical repository URL, kept for reference/attribution.
    pub repo_url: String,
    /// Relative path (inside the skill folder) of the entry document.
    pub skill_file: String,
    /// Downloaded text files as `(relative path, content)` pairs.
    pub files: Vec<(String, String)>,
}

/// Parsed skill metadata from a SKILL.md document.
pub struct SkillDocument {
    pub name: String,
    pub description: Option<String>,
}

/// Where a skill should be fetched from.
#[derive(Debug, Clone, PartialEq)]
pub enum GithubSource {
    /// A repository (optionally a sub-folder / specific ref).
    Repo {
        owner: String,
        repo: String,
        git_ref: Option<String>,
        subpath: Option<String>,
    },
    /// A single file inside a repository.
    File {
        owner: String,
        repo: String,
        git_ref: String,
        path: String,
    },
}

/// Parse a GitHub URL into a fetchable source.
///
/// Supported forms:
/// - `https://github.com/{owner}/{repo}` (optionally `.git`)
/// - `https://github.com/{owner}/{repo}/tree/{ref}[/{subpath}]`
/// - `https://github.com/{owner}/{repo}/blob/{ref}/{path}`
/// - `https://raw.githubusercontent.com/{owner}/{repo}/{ref}/{path}`
pub fn parse_github_url(url: &str) -> Result<GithubSource> {
    let trimmed = url.trim().trim_end_matches('/');
    let rest = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .ok_or_else(|| anyhow::anyhow!("仅支持 http(s) 链接"))?;

    let (host, path) = rest
        .split_once('/')
        .ok_or_else(|| anyhow::anyhow!("链接格式不正确: {}", url))?;
    let host = host.to_ascii_lowercase();
    let path = path.trim_end_matches(".git");

    let mut segs = path.split('/').filter(|s| !s.is_empty());
    let owner = segs.next().unwrap_or_default().to_string();
    let repo = segs.next().unwrap_or_default().to_string();
    if owner.is_empty() || repo.is_empty() {
        bail!("链接中缺少 owner/repo: {}", url);
    }

    match host.as_str() {
        "github.com" | "www.github.com" => match segs.next() {
            None => Ok(GithubSource::Repo { owner, repo, git_ref: None, subpath: None }),
            Some("tree") => {
                let git_ref = segs.next().map(|s| s.to_string());
                let subpath: Vec<&str> = segs.collect();
                Ok(GithubSource::Repo {
                    owner,
                    repo,
                    git_ref,
                    subpath: if subpath.is_empty() { None } else { Some(subpath.join("/")) },
                })
            }
            Some("blob") => {
                let git_ref = segs.next().unwrap_or_default().to_string();
                let file: Vec<&str> = segs.collect();
                if git_ref.is_empty() || file.is_empty() {
                    bail!("blob 链接缺少分支或文件路径: {}", url);
                }
                Ok(GithubSource::File { owner, repo, git_ref, path: file.join("/") })
            }
            Some(other) => bail!("暂不支持该 GitHub 链接类型: /{}", other),
        },
        "raw.githubusercontent.com" => {
            let git_ref = segs.next().unwrap_or_default().to_string();
            let file: Vec<&str> = segs.collect();
            if git_ref.is_empty() || file.is_empty() {
                bail!("raw 链接缺少分支或文件路径: {}", url);
            }
            Ok(GithubSource::File { owner, repo, git_ref, path: file.join("/") })
        }
        other => bail!("暂不支持该站点: {}（目前支持 GitHub）", other),
    }
}

/// Sanitize a raw skill name into a safe folder name.
pub fn sanitize_skill_name(raw: &str, fallback: &str) -> String {
    let mut name: String = raw
        .trim()
        .chars()
        .map(|c| {
            if c.is_whitespace() || c == '/' || c == '\\' {
                '-'
            } else if c.is_ascii_alphanumeric() || c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Collapse duplicate dashes produced by the mapping above.
    while name.contains("--") {
        name = name.replace("--", "-");
    }
    name = name.trim_matches(|c| c == '-' || c == '.').to_string();

    if name.is_empty() || name == "." || name == ".." {
        name = fallback.to_string();
    }
    name.chars().take(64).collect()
}

/// Parse a SKILL.md document: frontmatter `name`/`description` plus body.
///
/// Returns the sanitized name and the description. Falls back to
/// `fallback_name` when the frontmatter has no usable name.
pub fn parse_skill_document(content: &str, fallback_name: &str) -> SkillDocument {
    let mut raw_name: Option<String> = None;
    let mut description: Option<String> = None;

    if let Some(rest) = content.strip_prefix("---") {
        if let Some(end) = rest.find("\n---") {
            let frontmatter = &rest[..end];
            for line in frontmatter.lines() {
                let Some((key, value)) = line.split_once(':') else { continue };
                let value = value.trim().trim_matches('"').trim_matches('\'').trim();
                match key.trim() {
                    "name" if !value.is_empty() => raw_name = Some(value.to_string()),
                    "description" if !value.is_empty() => {
                        description = Some(value.to_string());
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(desc) = &description {
        // Frontmatter values are usually single-line; collapse any stray
        // whitespace and guard against oversized descriptions.
        let collapsed = desc.split_whitespace().collect::<Vec<_>>().join(" ");
        let mut d: String = collapsed.chars().take(MAX_DESCRIPTION_CHARS).collect();
        if collapsed.chars().count() > MAX_DESCRIPTION_CHARS {
            d.push('…');
        }
        description = Some(d);
    }

    let name = sanitize_skill_name(raw_name.as_deref().unwrap_or(fallback_name), fallback_name);
    SkillDocument { name, description }
}

/// Whether a repo file looks like a readable text asset worth importing.
pub fn is_text_asset(path: &str) -> bool {
    const TEXT_EXTS: &[&str] = &[
        "md", "markdown", "txt", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "sh", "bash",
        "zsh", "ps1", "bat", "json", "yaml", "yml", "toml", "csv", "tsv", "sql", "html", "htm",
        "css", "rst", "ini", "cfg", "conf", "xml", "ipynb", "r", "rb", "go", "rs", "java", "c",
        "h", "cpp", "hpp",
    ];
    const TEXT_NAMES: &[&str] = &["LICENSE", "Makefile", "Dockerfile", "requirements", "NOTICE"];

    let file_name = path.rsplit('/').next().unwrap_or(path);
    if TEXT_NAMES.iter().any(|n| file_name.eq_ignore_ascii_case(n)) {
        return true;
    }
    match path.rsplit_once('.') {
        Some((_, ext)) => TEXT_EXTS.iter().any(|e| ext.eq_ignore_ascii_case(e)),
        None => false,
    }
}

/// Whether a repo path should be skipped during import.
fn is_skipped_path(path: &str) -> bool {
    const SKIP_DIRS: &[&str] = &[
        ".git", "node_modules", "__pycache__", ".venv", "venv", "target", "dist", "build",
        ".idea", ".vscode", ".jj",
    ];
    path.split('/').any(|seg| SKIP_DIRS.contains(&seg))
}

#[derive(Deserialize)]
struct RepoInfo {
    default_branch: String,
}

#[derive(Deserialize)]
struct TreeResponse {
    tree: Vec<TreeEntry>,
    #[serde(default)]
    truncated: bool,
}

#[derive(Deserialize)]
struct TreeEntry {
    path: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    size: Option<u64>,
}

fn http_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent("jcowork-skill-importer/1.0")
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .context("创建 HTTP 客户端失败")
}

async fn github_api_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let resp = client
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .with_context(|| format!("请求 GitHub API 失败: {}", url))?;

    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        let hint = if status.as_u16() == 404 {
            "（仓库不存在，或是私有仓库需要登录）"
        } else if status.as_u16() == 403 {
            "（GitHub API 限流，请稍后再试）"
        } else {
            ""
        };
        bail!("GitHub API 返回 {} {} {}", status.as_u16(), hint, body.trim());
    }

    let bytes = resp.bytes().await.context("读取 GitHub API 响应失败")?;
    serde_json::from_slice(&bytes).context("解析 GitHub API 响应失败")
}

/// Download a repository's text files, mirroring its directory layout.
///
/// Returns files relative to the requested sub-folder (when the URL points at
/// a sub-folder rather than the repo root).
async fn fetch_repo_files(
    client: &reqwest::Client,
    owner: &str,
    repo: &str,
    git_ref: &str,
    subpath: Option<&str>,
) -> Result<(Vec<(String, String)>, bool)> {
    if git_ref.len() > 200
        || !git_ref
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./".contains(c))
    {
        bail!("非法的分支名: {}", git_ref);
    }
    if let Some(sub) = subpath {
        if sub.split('/').any(|seg| seg == "..") {
            bail!("非法路径: {}", sub);
        }
    }

    let tree_url = format!(
        "https://api.github.com/repos/{}/{}/git/trees/{}?recursive=1",
        owner, repo, git_ref
    );
    let tree: TreeResponse = github_api_json(client, &tree_url).await?;

    let prefix = subpath.map(|s| format!("{}/", s.trim_matches('/')));
    let mut wanted: Vec<(String, String, u64)> = Vec::new();
    let mut skill_md_found = false;

    for entry in tree.tree.iter().filter(|e| e.kind == "blob") {
        let rel = match &prefix {
            Some(p) => match entry.path.strip_prefix(p.as_str()) {
                Some(r) if !r.is_empty() => r.to_string(),
                _ => continue,
            },
            None => entry.path.clone(),
        };
        if !is_text_asset(&rel) || is_skipped_path(&rel) {
            continue;
        }
        if rel == "SKILL.md" || rel == "skill.md" {
            skill_md_found = true;
        }
        let size = entry.size.unwrap_or(0);
        if size > MAX_FILE_BYTES {
            tracing::warn!(path = %rel, size, "Skipping oversized skill asset");
            continue;
        }
        wanted.push((rel, entry.path.clone(), size));
    }

    if !skill_md_found {
        bail!("该仓库中未找到 SKILL.md（技能入口文件）");
    }

    // SKILL.md first, then the rest by size (small files first), so a partial
    // failure still leaves the most valuable content downloaded.
    wanted.sort_by(|a, b| {
        let a_entry = !(a.0 == "SKILL.md" || a.0 == "skill.md");
        let b_entry = !(b.0 == "SKILL.md" || b.0 == "skill.md");
        a_entry.cmp(&b_entry).then(a.2.cmp(&b.2))
    });

    let mut files: Vec<(String, String)> = Vec::new();
    let mut total: usize = 0;
    for (rel, full_path, _) in wanted.into_iter().take(MAX_FILES) {
        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            owner, repo, git_ref, full_path
        );
        let resp = client
            .get(&raw_url)
            .send()
            .await
            .with_context(|| format!("下载文件失败: {}", rel))?;
        if !resp.status().is_success() {
            tracing::warn!(path = %rel, status = %resp.status(), "Skipping unreadable skill asset");
            continue;
        }
        let text = resp
            .text()
            .await
            .with_context(|| format!("读取文件失败: {}", rel))?;
        total += text.len();
        if total > MAX_TOTAL_BYTES {
            tracing::warn!("Skill import exceeded total size limit, stopping early");
            break;
        }
        files.push((rel, text));
    }

    if !files.iter().any(|(p, _)| p == "SKILL.md" || p == "skill.md") {
        bail!("SKILL.md 下载失败，请稍后重试");
    }

    Ok((files, tree.truncated))
}

/// Fetch a third-party skill from a GitHub URL.
pub async fn fetch_github_skill(url: &str) -> Result<ImportedSkill> {
    let source = parse_github_url(url)?;
    let client = http_client()?;

    let (owner, repo, git_ref, subpath, single_file) = match source {
        GithubSource::Repo { owner, repo, git_ref, subpath } => (owner, repo, git_ref, subpath, None),
        GithubSource::File { owner, repo, git_ref, path } => (owner, repo, Some(git_ref), None, Some(path)),
    };

    // Resolve the default branch when the URL doesn't pin one.
    let git_ref = match git_ref {
        Some(r) => r,
        None => {
            let info: RepoInfo = github_api_json(
                &client,
                &format!("https://api.github.com/repos/{}/{}", owner, repo),
            )
            .await
            .context("无法读取仓库信息")?;
            info.default_branch
        }
    };

    let repo_url = format!("https://github.com/{}/{}", owner, repo);

    // Single-file form (raw / blob URL): only that file is imported.
    if let Some(path) = single_file {
        if path.split('/').any(|seg| seg == "..") || path.starts_with('/') {
            bail!("非法文件路径: {}", path);
        }
        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}/{}",
            owner, repo, git_ref, path
        );
        let content = client
            .get(&raw_url)
            .send()
            .await
            .with_context(|| format!("下载文件失败: {}", path))?
            .error_for_status()
            .with_context(|| format!("文件不存在或不可访问: {}", path))?
            .text()
            .await
            .with_context(|| format!("读取文件失败: {}", path))?;

        if content.len() as u64 > MAX_FILE_BYTES {
            bail!("文件过大（>{} MB），不支持导入", MAX_FILE_BYTES / 1024 / 1024);
        }

        let fallback = path
            .rsplit('/')
            .next()
            .and_then(|f| f.rsplit_once('.').map(|(stem, _)| stem).or(Some(f)))
            .unwrap_or(&repo);
        let doc = parse_skill_document(&content, fallback);
        return Ok(ImportedSkill {
            name: doc.name,
            description: doc.description,
            repo_url,
            skill_file: "SKILL.md".to_string(),
            files: vec![("SKILL.md".to_string(), content)],
        });
    }

    let (files, truncated) = fetch_repo_files(&client, &owner, &repo, &git_ref, subpath.as_deref()).await?;
    if truncated {
        tracing::warn!(repo = %repo_url, "GitHub tree response was truncated; some files may be missing");
    }

    let entry = files
        .iter()
        .find(|(p, _)| p == "SKILL.md" || p == "skill.md")
        .map(|(_, c)| c.clone())
        .ok_or_else(|| anyhow::anyhow!("该仓库中未找到 SKILL.md（技能入口文件）"))?;

    let doc = parse_skill_document(&entry, &repo);
    Ok(ImportedSkill {
        name: doc.name,
        description: doc.description,
        repo_url,
        skill_file: "SKILL.md".to_string(),
        files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_repo_url() {
        assert_eq!(
            parse_github_url("https://github.com/simonlin1212/a-stock-data").unwrap(),
            GithubSource::Repo {
                owner: "simonlin1212".into(),
                repo: "a-stock-data".into(),
                git_ref: None,
                subpath: None
            }
        );
        assert_eq!(
            parse_github_url("https://github.com/owner/repo.git").unwrap(),
            GithubSource::Repo {
                owner: "owner".into(),
                repo: "repo".into(),
                git_ref: None,
                subpath: None
            }
        );
    }

    #[test]
    fn test_parse_tree_url() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo/tree/dev/skills/foo").unwrap(),
            GithubSource::Repo {
                owner: "owner".into(),
                repo: "repo".into(),
                git_ref: Some("dev".into()),
                subpath: Some("skills/foo".into())
            }
        );
    }

    #[test]
    fn test_parse_blob_and_raw_url() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo/blob/main/SKILL.md").unwrap(),
            GithubSource::File {
                owner: "owner".into(),
                repo: "repo".into(),
                git_ref: "main".into(),
                path: "SKILL.md".into()
            }
        );
        assert_eq!(
            parse_github_url("https://raw.githubusercontent.com/owner/repo/main/skills/x/SKILL.md").unwrap(),
            GithubSource::File {
                owner: "owner".into(),
                repo: "repo".into(),
                git_ref: "main".into(),
                path: "skills/x/SKILL.md".into()
            }
        );
    }

    #[test]
    fn test_parse_url_rejects_other_hosts() {
        assert!(parse_github_url("https://gitlab.com/owner/repo").is_err());
        assert!(parse_github_url("ftp://github.com/owner/repo").is_err());
        assert!(parse_github_url("https://github.com/owner").is_err());
    }

    #[test]
    fn test_sanitize_skill_name() {
        assert_eq!(sanitize_skill_name("a-stock-data", "fallback"), "a-stock-data");
        assert_eq!(sanitize_skill_name("  My Skill  ", "fb"), "My-Skill");
        assert_eq!(sanitize_skill_name("技能 名称", "fb"), "技能-名称");
        assert_eq!(sanitize_skill_name("..//evil", "fb"), "evil");
        assert_eq!(sanitize_skill_name("", "fb"), "fb");
    }

    #[test]
    fn test_parse_skill_document_with_frontmatter() {
        let doc = "---\nname: a-stock-data\ndescription: 当任务需要写代码实际获取A股数据时使用\norigin: custom\n---\n\n# Body\n";
        let parsed = parse_skill_document(doc, "repo-name");
        assert_eq!(parsed.name, "a-stock-data");
        assert_eq!(parsed.description.as_deref(), Some("当任务需要写代码实际获取A股数据时使用"));
    }

    #[test]
    fn test_parse_skill_document_without_frontmatter() {
        let parsed = parse_skill_document("# Just a body", "repo-name");
        assert_eq!(parsed.name, "repo-name");
        assert!(parsed.description.is_none());
    }

    #[test]
    fn test_is_text_asset() {
        assert!(is_text_asset("SKILL.md"));
        assert!(is_text_asset("docs/guide.md"));
        assert!(is_text_asset("scripts/fetch_data.py"));
        assert!(is_text_asset("LICENSE"));
        assert!(!is_text_asset("assets/logo.png"));
        assert!(!is_text_asset("bin/tool"));
    }

    #[test]
    fn test_is_skipped_path() {
        assert!(is_skipped_path(".git/config"));
        assert!(is_skipped_path("node_modules/x/index.js"));
        assert!(!is_skipped_path("docs/guide.md"));
    }
}
