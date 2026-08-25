use crate::graph::RawCommitNode;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitFilter {
    pub author: Option<String>,
    /// `path:` query token. Deliberately NOT consulted by
    /// [`CommitFilter::matches_commit`]: its only caller
    /// (`cmd_get_commit_graph`) narrows rows server-side via
    /// `GitReader::commits_touching_path` before running the filter, so the
    /// rows reaching here are already path-filtered and a per-commit path
    /// check would be redundant.
    pub path: Option<String>,
    pub sha: Option<String>,
    pub commit_type: Option<String>,
    pub text: String,
}

impl CommitFilter {
    /// Parses queries such as `author:alice path:src/ feat: sha:abc123 oauth`.
    pub fn parse(query: &str) -> Self {
        let mut filter = CommitFilter::default();
        let mut free = Vec::new();

        for token in query.split_whitespace() {
            if let Some(value) = token.strip_prefix("author:") {
                if !value.is_empty() {
                    filter.author = Some(value.to_lowercase());
                }
            } else if let Some(value) = token.strip_prefix("path:") {
                if !value.is_empty() {
                    filter.path = Some(value.to_string());
                }
            } else if let Some(value) = token.strip_prefix("sha:") {
                if !value.is_empty() {
                    filter.sha = Some(value.to_lowercase());
                }
            } else if let Some(value) = token.strip_prefix("type:") {
                if !value.is_empty() {
                    filter.commit_type = Some(value.to_lowercase());
                }
            } else if token.ends_with(':') {
                let kind = token.trim_end_matches(':').to_lowercase();
                if matches!(
                    kind.as_str(),
                    "feat"
                        | "fix"
                        | "chore"
                        | "docs"
                        | "refactor"
                        | "perf"
                        | "test"
                        | "build"
                        | "ci"
                ) {
                    filter.commit_type = Some(kind);
                } else {
                    free.push(token.to_string());
                }
            } else {
                free.push(token.to_string());
            }
        }

        filter.text = free.join(" ").to_lowercase();
        filter
    }

    pub fn is_empty(&self) -> bool {
        self.author.is_none()
            && self.path.is_none()
            && self.sha.is_none()
            && self.commit_type.is_none()
            && self.text.is_empty()
    }

    pub fn matches_commit(&self, commit: &RawCommitNode) -> bool {
        if let Some(ref author) = self.author {
            let hay = format!(
                "{} {}",
                commit.author_name.to_lowercase(),
                commit.author_email.to_lowercase()
            );
            if !hay.contains(author) {
                return false;
            }
        }
        if let Some(ref sha) = self.sha {
            if !commit.id.to_lowercase().starts_with(sha) {
                return false;
            }
        }
        if let Some(ref kind) = self.commit_type {
            let header = commit.summary.to_lowercase();
            if !header.starts_with(&format!("{}:", kind))
                && !header.starts_with(&format!("{}(", kind))
                && !header.starts_with(&format!("{}!:", kind))
                && !header.starts_with(&format!("{}!(", kind))
            {
                return false;
            }
        }
        if !self.text.is_empty() {
            let hay = format!(
                "{} {} {} {}",
                commit.summary.to_lowercase(),
                commit.author_name.to_lowercase(),
                commit.author_email.to_lowercase(),
                commit.id.to_lowercase()
            );
            for word in self.text.split_whitespace() {
                if !hay.contains(word) {
                    return false;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn commit(summary: &str, author: &str, id: &str) -> RawCommitNode {
        RawCommitNode {
            id: id.to_string(),
            parent_ids: vec![],
            timestamp: 1,
            author_name: author.to_string(),
            author_email: format!("{}@example.com", author.to_lowercase()),
            summary: summary.to_string(),
        }
    }

    #[test]
    fn test_parse_mixed_query() {
        let filter = CommitFilter::parse("author:Alice feat: oauth sha:abc123");
        assert_eq!(filter.author.as_deref(), Some("alice"));
        assert_eq!(filter.commit_type.as_deref(), Some("feat"));
        assert_eq!(filter.sha.as_deref(), Some("abc123"));
        assert_eq!(filter.text, "oauth");
    }

    #[test]
    fn test_matches_author_and_type() {
        let filter = CommitFilter::parse("author:alice feat:");
        assert!(filter.matches_commit(&commit("feat: add login", "Alice", "aaa111")));
        assert!(filter.matches_commit(&commit("feat!: breaking api change", "Alice", "aaa112")));
        assert!(filter.matches_commit(&commit("feat(auth)!: breaking login", "Alice", "aaa113")));
        assert!(!filter.matches_commit(&commit("fix: typo", "Alice", "aaa111")));
        assert!(!filter.matches_commit(&commit("feat: add login", "Bob", "aaa111")));
    }

    #[test]
    fn test_matches_multiword_free_text() {
        let filter = CommitFilter::parse("alice oauth");
        assert!(filter.matches_commit(&commit("feat: oauth flow", "Alice", "aaa111")));
        assert!(!filter.matches_commit(&commit("feat: oauth flow", "Bob", "aaa111")));
    }
}
