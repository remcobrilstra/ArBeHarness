//! Permission rules: `tool` or `tool(pattern)`, matched against a call's
//! tool name and its *subject* (what the call acts on — a path for file
//! tools, the command line for `execute`; see `ToolExecutor::subject`).
//!
//! `*` matches any run of characters, including `/`, in both parts:
//! `read_file`, `github__*`, `execute(cargo test*)`, `write_file(src/*)`,
//! `edit_file(*.md)`.

/// One parsed rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolRule {
    tool: String,
    pattern: Option<Pattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    /// From config: `*` is a wildcard.
    Wildcard(String),
    /// From an "approve this exact call" answer: compared literally, so a
    /// `*` in an approved command can never widen what it approves.
    Exact(String),
}

impl ToolRule {
    /// Parses `tool` or `tool(pattern)`.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (tool, pattern) = match text.split_once('(') {
            None => (text, None),
            Some((tool, rest)) => {
                let pattern = rest
                    .strip_suffix(')')
                    .ok_or_else(|| format!("rule {text:?} is missing its closing `)`"))?;
                (tool.trim(), Some(pattern.to_string()))
            }
        };
        if tool.is_empty() {
            return Err(format!("rule {text:?} has no tool name"));
        }
        Ok(Self {
            tool: tool.to_string(),
            pattern: pattern.map(Pattern::Wildcard),
        })
    }

    /// A rule for exactly this tool and (optionally) exactly this subject,
    /// both compared literally.
    pub fn exact(tool: &str, subject: Option<&str>) -> Self {
        Self {
            tool: tool.to_string(),
            pattern: subject.map(|s| Pattern::Exact(s.to_string())),
        }
    }

    /// Whether the rule names a subject pattern (not just a tool).
    pub fn is_specific(&self) -> bool {
        self.pattern.is_some()
    }

    /// A rule with a pattern only matches calls that have a subject.
    pub fn matches(&self, tool: &str, subject: Option<&str>) -> bool {
        let tool_matches = match &self.pattern {
            Some(Pattern::Exact(_)) => self.tool == tool,
            _ => wildcard_match(&self.tool, tool),
        };
        if !tool_matches {
            return false;
        }
        match (&self.pattern, subject) {
            (None, _) => true,
            (Some(Pattern::Wildcard(pattern)), Some(subject)) => wildcard_match(pattern, subject),
            (Some(Pattern::Exact(exact)), Some(subject)) => exact == subject,
            (Some(_), None) => false,
        }
    }
}

impl std::fmt::Display for ToolRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.pattern {
            Some(Pattern::Wildcard(p) | Pattern::Exact(p)) => write!(f, "{}({p})", self.tool),
            None => write!(f, "{}", self.tool),
        }
    }
}

/// `*` matches any (possibly empty) run of characters; everything else is
/// literal. Iterative with backtracking to the last `*`, so it's linear in
/// practice and never recurses.
pub fn wildcard_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((star_pi, star_ti)) = star {
            pi = star_pi + 1;
            ti = star_ti + 1;
            star = Some((star_pi, star_ti + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wildcards_match_any_run_including_slashes() {
        assert!(wildcard_match("cargo test*", "cargo test -p arbe-tools"));
        assert!(wildcard_match("cargo test*", "cargo test"));
        assert!(!wildcard_match("cargo test*", "cargo build"));
        assert!(wildcard_match("src/*", "src/a/b/c.rs"));
        assert!(wildcard_match("*.md", "docs/guide.md"));
        assert!(!wildcard_match("*.md", "docs/guide.rs"));
        assert!(wildcard_match("a*b*c", "a-x-b-y-c"));
        assert!(!wildcard_match("a*b*c", "a-x-c"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
    }

    #[test]
    fn parses_bare_and_patterned_rules() {
        assert_eq!(
            ToolRule::parse("read_file").unwrap().to_string(),
            "read_file"
        );
        let rule = ToolRule::parse(" execute(cargo test*) ").unwrap();
        assert!(rule.is_specific());
        assert_eq!(rule.to_string(), "execute(cargo test*)");
        // Parentheses inside the pattern are fine; only the last `)` closes.
        assert!(
            ToolRule::parse("execute(echo (hi))")
                .unwrap()
                .matches("execute", Some("echo (hi)"))
        );
        assert!(ToolRule::parse("execute(cargo").is_err());
        assert!(ToolRule::parse("(x)").is_err());
    }

    #[test]
    fn patterned_rules_need_a_matching_subject() {
        let rule = ToolRule::parse("write_file(src/*)").unwrap();
        assert!(rule.matches("write_file", Some("src/lib.rs")));
        assert!(!rule.matches("write_file", Some("Cargo.toml")));
        assert!(!rule.matches("write_file", None));
        assert!(!rule.matches("edit_file", Some("src/lib.rs")));
        let bare = ToolRule::parse("github__*").unwrap();
        assert!(bare.matches("github__search", None));
        assert!(bare.matches("github__search", Some("anything")));
    }

    #[test]
    fn exact_rules_match_only_that_call() {
        let rule = ToolRule::exact("execute", Some("cargo test"));
        assert!(rule.matches("execute", Some("cargo test")));
        assert!(!rule.matches("execute", Some("cargo test && rm -rf /")));
        // A `*` in an approved command is literal, never a wildcard.
        let globbed = ToolRule::exact("execute", Some("ls *.rs"));
        assert!(globbed.matches("execute", Some("ls *.rs")));
        assert!(!globbed.matches("execute", Some("ls x.rs; rm -rf /tmp/a.rs")));
    }
}
