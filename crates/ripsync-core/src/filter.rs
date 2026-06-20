//! Path filtering for the walk: ordered include/exclude rules plus an optional
//! explicit `--files-from` allowlist.
//!
//! Rules are matched against a tree-relative path, first match wins, and the
//! default for an unmatched path is *include*. Precedence, highest first:
//!
//! 1. `--filter "+ pat"` / `--filter "- pat"` rules, in the order given;
//! 2. `--include pat` rules;
//! 3. `--exclude pat` rules.
//!
//! So the common idiom `--include '*.rs' --exclude '*'` keeps only Rust files.
//!
//! Filtering applies to **files and symlinks**; directory entries are always
//! kept so their contents can still be reached and parent directories exist for
//! the files that survive. An exclude pattern also matches everything beneath a
//! matching directory (e.g. `--exclude node_modules` drops `node_modules/**`),
//! so excluded subtrees are not copied even though the (now empty) directory may
//! be created at the destination.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use globset::{Glob, GlobSet, GlobSetBuilder};

use crate::error::{Error, Result};

struct Rule {
    include: bool,
    set: GlobSet,
}

#[derive(Default)]
struct Inner {
    rules: Vec<Rule>,
    files_from: Option<HashSet<PathBuf>>,
}

/// A compiled set of include/exclude rules and an optional files-from allowlist.
/// Cheap to clone (it is reference-counted) so it can be handed to worker
/// threads and the TUI.
#[derive(Clone, Default)]
pub struct Filter {
    inner: Arc<Inner>,
}

impl Filter {
    /// An empty filter that includes everything.
    #[must_use]
    pub fn none() -> Self {
        Self::default()
    }

    /// Whether this filter would drop nothing (fast path for the walk).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.rules.is_empty() && self.inner.files_from.is_none()
    }

    /// Whether `rel` (relative to the walk root) should be excluded. `is_dir`
    /// entries are always kept (see the module docs).
    #[must_use]
    pub fn is_excluded(&self, rel: &Path, is_dir: bool) -> bool {
        if is_dir {
            return false;
        }
        if let Some(allow) = &self.inner.files_from {
            return !allow.contains(rel);
        }
        for rule in &self.inner.rules {
            if rule.set.is_match(rel) {
                return !rule.include;
            }
        }
        false
    }
}

/// Builds a [`Filter`] from CLI inputs.
#[derive(Default)]
pub struct FilterBuilder {
    filters: Vec<(bool, String)>,
    includes: Vec<String>,
    excludes: Vec<String>,
    files_from: Option<Vec<PathBuf>>,
}

impl FilterBuilder {
    /// Add a raw `--filter` rule of the form `"+ pattern"` or `"- pattern"`.
    ///
    /// # Errors
    /// Returns an error if the rule does not start with `+`/`-` and a pattern.
    pub fn filter_rule(&mut self, rule: &str) -> Result<&mut Self> {
        let trimmed = rule.trim();
        let (sign, pat) = trimmed
            .split_once(char::is_whitespace)
            .ok_or_else(|| Error::Filter(format!("filter rule needs '+ pat' or '- pat': {rule}")))?;
        let include = match sign {
            "+" => true,
            "-" => false,
            other => {
                return Err(Error::Filter(format!(
                    "filter rule must start with + or -, got {other:?}"
                )));
            }
        };
        self.filters.push((include, pat.trim().to_string()));
        Ok(self)
    }

    /// Add an `--include` pattern.
    pub fn include(&mut self, pat: impl Into<String>) -> &mut Self {
        self.includes.push(pat.into());
        self
    }

    /// Add an `--exclude` pattern.
    pub fn exclude(&mut self, pat: impl Into<String>) -> &mut Self {
        self.excludes.push(pat.into());
        self
    }

    /// Set the explicit allowlist of relative paths (`--files-from`).
    pub fn files_from(&mut self, paths: Vec<PathBuf>) -> &mut Self {
        self.files_from = Some(paths);
        self
    }

    /// Compile the [`Filter`].
    ///
    /// # Errors
    /// Returns an error if any glob pattern is invalid.
    pub fn build(self) -> Result<Filter> {
        let mut rules = Vec::new();
        for (include, pat) in self.filters {
            rules.push(compile_rule(include, &pat)?);
        }
        for pat in self.includes {
            rules.push(compile_rule(true, &pat)?);
        }
        for pat in self.excludes {
            rules.push(compile_rule(false, &pat)?);
        }
        let files_from = self
            .files_from
            .map(|paths| paths.into_iter().collect::<HashSet<_>>());
        Ok(Filter {
            inner: Arc::new(Inner { rules, files_from }),
        })
    }
}

/// Compile one pattern into a rule. Each pattern matches at the root, nested
/// anywhere, and everything beneath a matching directory.
fn compile_rule(include: bool, pat: &str) -> Result<Rule> {
    let mut builder = GlobSetBuilder::new();
    let mut add = |g: &str| -> Result<()> {
        builder
            .add(Glob::new(g).map_err(|e| Error::Filter(format!("invalid pattern {g:?}: {e}")))?);
        Ok(())
    };
    add(pat)?;
    if !pat.contains("**") {
        add(&format!("**/{pat}"))?;
        add(&format!("{pat}/**"))?;
        add(&format!("**/{pat}/**"))?;
    }
    let set = builder
        .build()
        .map_err(|e| Error::Filter(format!("compiling pattern {pat:?}: {e}")))?;
    Ok(Rule { include, set })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn excluded(f: &Filter, rel: &str, is_dir: bool) -> bool {
        f.is_excluded(Path::new(rel), is_dir)
    }

    #[test]
    fn empty_includes_everything() {
        let f = Filter::none();
        assert!(!excluded(&f, "a/b.txt", false));
    }

    #[test]
    fn include_only_idiom() {
        let mut b = FilterBuilder::default();
        b.include("*.rs").exclude("*");
        let f = b.build().unwrap();
        assert!(!excluded(&f, "src/main.rs", false));
        assert!(excluded(&f, "src/notes.txt", false));
        assert!(!excluded(&f, "src", true)); // dirs always kept
    }

    #[test]
    fn exclude_subtree() {
        let mut b = FilterBuilder::default();
        b.exclude("node_modules");
        let f = b.build().unwrap();
        assert!(excluded(&f, "node_modules/pkg/index.js", false));
        assert!(excluded(&f, "a/node_modules/x.js", false));
        assert!(!excluded(&f, "src/index.js", false));
    }

    #[test]
    fn filter_rules_ordered() {
        let mut b = FilterBuilder::default();
        b.filter_rule("+ keep.log").unwrap();
        b.filter_rule("- *.log").unwrap();
        let f = b.build().unwrap();
        assert!(!excluded(&f, "keep.log", false));
        assert!(excluded(&f, "other.log", false));
    }

    #[test]
    fn files_from_allowlist() {
        let mut b = FilterBuilder::default();
        b.files_from(vec![PathBuf::from("a/keep.txt")]);
        let f = b.build().unwrap();
        assert!(!excluded(&f, "a/keep.txt", false));
        assert!(excluded(&f, "a/drop.txt", false));
        assert!(!excluded(&f, "a", true));
    }
}
