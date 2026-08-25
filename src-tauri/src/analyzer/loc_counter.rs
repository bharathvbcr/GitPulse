use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct LineCounts {
    pub total_lines: usize,
    pub code_lines: usize,
    pub comment_lines: usize,
    pub blank_lines: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct DiffChurn {
    pub additions: usize,
    pub deletions: usize,
    pub files_changed: usize,
}

impl DiffChurn {
    pub fn net(&self) -> i64 {
        self.additions as i64 - self.deletions as i64
    }

    pub fn total_changes(&self) -> usize {
        self.additions + self.deletions
    }

    /// Parses `git diff --shortstat` output such as
    /// ` 3 files changed, 42 insertions(+), 7 deletions(-)`.
    pub fn parse_shortstat(stat: &str) -> Self {
        let mut churn = DiffChurn::default();
        for part in stat.split(',') {
            let part = part.trim();
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            let n: usize = digits.parse().unwrap_or(0);
            if part.contains("file") {
                churn.files_changed = n;
            } else if part.contains("insertion") {
                churn.additions = n;
            } else if part.contains("deletion") {
                churn.deletions = n;
            }
        }
        churn
    }
}

pub struct LocCounter;

impl LocCounter {
    /// Fast count of code, comment, and blank lines in a source string.
    pub fn count(content: &str, line_comment_prefix: Option<&str>) -> LineCounts {
        let mut total = 0;
        let mut code = 0;
        let mut comments = 0;
        let mut blank = 0;

        let comment_prefix = line_comment_prefix.unwrap_or("//");

        for line in content.lines() {
            total += 1;
            let trimmed = line.trim();
            if trimmed.is_empty() {
                blank += 1;
            } else if trimmed.starts_with(comment_prefix) {
                comments += 1;
            } else {
                code += 1;
            }
        }

        LineCounts {
            total_lines: total,
            code_lines: code,
            comment_lines: comments,
            blank_lines: blank,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_loc_count() {
        let code = r#"
// This is a comment
fn main() {

}
"#;
        let counts = LocCounter::count(code, Some("//"));
        assert_eq!(counts.total_lines, 5);
        assert_eq!(counts.comment_lines, 1);
        assert_eq!(counts.code_lines, 2);
        assert_eq!(counts.blank_lines, 2);
    }

    #[test]
    fn test_parse_shortstat_all_fields() {
        let churn =
            DiffChurn::parse_shortstat(" 3 files changed, 42 insertions(+), 7 deletions(-)");
        assert_eq!(churn.files_changed, 3);
        assert_eq!(churn.additions, 42);
        assert_eq!(churn.deletions, 7);
        assert_eq!(churn.total_changes(), 49);
        assert_eq!(churn.net(), 35);
    }

    #[test]
    fn test_parse_shortstat_singular_and_partial() {
        assert_eq!(
            DiffChurn::parse_shortstat(" 1 file changed, 1 insertion(+)"),
            DiffChurn {
                additions: 1,
                deletions: 0,
                files_changed: 1
            }
        );
        assert_eq!(
            DiffChurn::parse_shortstat(" 1 file changed, 4 deletions(-)"),
            DiffChurn {
                additions: 0,
                deletions: 4,
                files_changed: 1
            }
        );
        assert_eq!(DiffChurn::parse_shortstat(""), DiffChurn::default());
    }
}
