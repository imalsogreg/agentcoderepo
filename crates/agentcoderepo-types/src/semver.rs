//! Semantic versioning: parse versions, classify bumps, diff signature sets.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::subsumption::Compat;
use crate::FunctionSig;

// ---------------------------------------------------------------------------
// Version
// ---------------------------------------------------------------------------

/// A semantic version: major.minor.patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parse "1.2.3" into a Version. Returns None on invalid format.
    pub fn parse(s: &str) -> Option<Self> {
        let parts: Vec<&str> = s.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some(Version {
            major: parts[0].parse().ok()?,
            minor: parts[1].parse().ok()?,
            patch: parts[2].parse().ok()?,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

// ---------------------------------------------------------------------------
// Version requirements (ranges)
// ---------------------------------------------------------------------------

/// A version requirement that matches a range of versions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VersionReq {
    /// ^1.2.3 — compatible updates
    Caret(Version),
    /// ~1.2.3 — patch-level updates
    Tilde(Version),
    /// Exact version match
    Exact(Version),
    /// >= version
    Gte(Version),
    /// > version
    Gt(Version),
    /// < version
    Lt(Version),
    /// <= version
    Lte(Version),
    /// Intersection of multiple requirements
    And(Vec<VersionReq>),
}

impl VersionReq {
    /// Parse a version requirement string.
    ///
    /// Supported formats:
    /// - `^1.2.3` (caret)
    /// - `~1.2.3` (tilde)
    /// - `>=1.0.0` / `>1.0.0` / `<2.0.0` / `<=2.0.0`
    /// - `1.2.3` (exact)
    /// - `>=1.0, <2.0` (compound, comma-separated)
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();

        // Compound: comma-separated
        if s.contains(',') {
            let parts: Vec<VersionReq> = s
                .split(',')
                .map(|p| VersionReq::parse(p.trim()))
                .collect::<Option<Vec<_>>>()?;
            if parts.len() == 1 {
                return Some(parts.into_iter().next().unwrap());
            }
            return Some(VersionReq::And(parts));
        }

        if let Some(rest) = s.strip_prefix('^') {
            return Some(VersionReq::Caret(Version::parse(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix('~') {
            return Some(VersionReq::Tilde(Version::parse(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix(">=") {
            return Some(VersionReq::Gte(Version::parse(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix('>') {
            return Some(VersionReq::Gt(Version::parse(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix("<=") {
            return Some(VersionReq::Lte(Version::parse(rest.trim())?));
        }
        if let Some(rest) = s.strip_prefix('<') {
            return Some(VersionReq::Lt(Version::parse(rest.trim())?));
        }

        // Bare version = exact
        Some(VersionReq::Exact(Version::parse(s)?))
    }

    /// Check if a version satisfies this requirement.
    pub fn matches(&self, v: &Version) -> bool {
        match self {
            VersionReq::Exact(req) => v == req,

            VersionReq::Caret(req) => {
                if v < req {
                    return false;
                }
                if req.major > 0 {
                    // ^1.2.3 → >=1.2.3, <2.0.0
                    v.major == req.major
                } else if req.minor > 0 {
                    // ^0.2.3 → >=0.2.3, <0.3.0
                    v.major == 0 && v.minor == req.minor
                } else {
                    // ^0.0.3 → >=0.0.3, <0.0.4
                    v.major == 0 && v.minor == 0 && v.patch == req.patch
                }
            }

            VersionReq::Tilde(req) => {
                // ~1.2.3 → >=1.2.3, <1.3.0
                v >= req && v.major == req.major && v.minor == req.minor
            }

            VersionReq::Gte(req) => v >= req,
            VersionReq::Gt(req) => v > req,
            VersionReq::Lt(req) => v < req,
            VersionReq::Lte(req) => v <= req,

            VersionReq::And(reqs) => reqs.iter().all(|r| r.matches(v)),
        }
    }
}

impl fmt::Display for VersionReq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VersionReq::Caret(v) => write!(f, "^{v}"),
            VersionReq::Tilde(v) => write!(f, "~{v}"),
            VersionReq::Exact(v) => write!(f, "{v}"),
            VersionReq::Gte(v) => write!(f, ">={v}"),
            VersionReq::Gt(v) => write!(f, ">{v}"),
            VersionReq::Lt(v) => write!(f, "<{v}"),
            VersionReq::Lte(v) => write!(f, "<={v}"),
            VersionReq::And(reqs) => {
                for (i, r) in reqs.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{r}")?;
                }
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Bump classification
// ---------------------------------------------------------------------------

/// The kind of version bump between two versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Bump {
    /// No version change.
    None,
    /// Patch bump (0.1.0 → 0.1.1).
    Patch,
    /// Minor bump (0.1.0 → 0.2.0).
    Minor,
    /// Major bump (0.1.0 → 1.0.0).
    Major,
}

impl fmt::Display for Bump {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Bump::None => write!(f, "none"),
            Bump::Patch => write!(f, "patch"),
            Bump::Minor => write!(f, "minor"),
            Bump::Major => write!(f, "major"),
        }
    }
}

/// Determine what kind of bump occurred between two versions.
pub fn classify_bump(old: &Version, new: &Version) -> Bump {
    if new.major != old.major {
        Bump::Major
    } else if new.minor != old.minor {
        Bump::Minor
    } else if new.patch != old.patch {
        Bump::Patch
    } else {
        Bump::None
    }
}

// ---------------------------------------------------------------------------
// Signature diff
// ---------------------------------------------------------------------------

/// A single function's change between versions.
#[derive(Debug, Clone)]
pub enum SigChange {
    /// Function was added (not present in old version).
    Added,
    /// Function was removed.
    Removed,
    /// Type unchanged (after alpha normalization).
    Unchanged,
    /// Type changed but new type subsumes old (backward compatible).
    Compatible,
    /// Type changed in a breaking way.
    TypeBreaking,
    /// Type unchanged but LLM detected a logic-breaking change.
    LogicBreaking { reason: String },
}

impl SigChange {
    /// Whether this change requires at least a major version bump.
    pub fn is_breaking(&self) -> bool {
        matches!(
            self,
            SigChange::Removed | SigChange::TypeBreaking | SigChange::LogicBreaking { .. }
        )
    }

    /// Whether this change requires at least a minor version bump.
    pub fn is_additive(&self) -> bool {
        matches!(self, SigChange::Added)
    }
}

/// The full diff between two sets of function signatures.
#[derive(Debug, Clone)]
pub struct SigDiff {
    /// Per-function changes, keyed by function name.
    pub changes: Vec<(String, SigChange)>,
}

impl SigDiff {
    /// The minimum version bump required by these changes.
    pub fn required_bump(&self) -> Bump {
        let mut required = Bump::None;
        for (_, change) in &self.changes {
            if change.is_breaking() {
                return Bump::Major; // Can't get higher, short-circuit
            }
            if change.is_additive() && required < Bump::Minor {
                required = Bump::Minor;
            }
        }
        required
    }

    /// All breaking changes in this diff.
    pub fn breaking_changes(&self) -> Vec<&(String, SigChange)> {
        self.changes.iter().filter(|(_, c)| c.is_breaking()).collect()
    }

    /// True if no functions changed at all.
    pub fn is_empty(&self) -> bool {
        self.changes
            .iter()
            .all(|(_, c)| matches!(c, SigChange::Unchanged))
    }
}

/// Compare two sets of function signatures and produce a diff.
///
/// This performs structural comparison only (type subsumption). Logic-breaking
/// changes detected by the LLM should be added separately via `add_logic_breaking`.
pub fn diff_signatures(old: &[FunctionSig], new: &[FunctionSig]) -> SigDiff {
    let old_map: HashMap<&str, &FunctionSig> = old.iter().map(|s| (s.name.as_str(), s)).collect();
    let new_map: HashMap<&str, &FunctionSig> = new.iter().map(|s| (s.name.as_str(), s)).collect();

    let mut changes = Vec::new();

    // Check each old function
    for (name, old_sig) in &old_map {
        if let Some(new_sig) = new_map.get(name) {
            let old_norm = old_sig.ty.alpha_normalize();
            let new_norm = new_sig.ty.alpha_normalize();
            if old_norm == new_norm {
                changes.push((name.to_string(), SigChange::Unchanged));
            } else {
                match crate::subsumption::check_compat(&old_sig.ty, &new_sig.ty) {
                    Compat::Compatible => {
                        changes.push((name.to_string(), SigChange::Compatible));
                    }
                    Compat::Breaking => {
                        changes.push((name.to_string(), SigChange::TypeBreaking));
                    }
                }
            }
        } else {
            changes.push((name.to_string(), SigChange::Removed));
        }
    }

    // Check for newly added functions
    for name in new_map.keys() {
        if !old_map.contains_key(name) {
            changes.push((name.to_string(), SigChange::Added));
        }
    }

    // Sort by name for deterministic output
    changes.sort_by(|a, b| a.0.cmp(&b.0));

    SigDiff { changes }
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

/// Validate that a version bump is sufficient for the given signature diff.
///
/// Returns `Ok(())` if the bump is adequate, or `Err` with a message explaining
/// why the bump is insufficient.
pub fn validate_bump(old_version: &Version, new_version: &Version, diff: &SigDiff) -> Result<(), String> {
    let actual_bump = classify_bump(old_version, new_version);
    let required = diff.required_bump();

    if actual_bump < required {
        let breaking: Vec<String> = diff
            .changes
            .iter()
            .filter(|(_, c)| c.is_breaking() || (required == Bump::Minor && c.is_additive()))
            .map(|(name, change)| {
                match change {
                    SigChange::Removed => format!("  - {name}: removed"),
                    SigChange::TypeBreaking => format!("  - {name}: type changed (breaking)"),
                    SigChange::LogicBreaking { reason } => {
                        format!("  - {name}: logic change ({reason})")
                    }
                    SigChange::Added => format!("  - {name}: added (requires minor bump)"),
                    _ => format!("  - {name}: changed"),
                }
            })
            .collect();

        Err(format!(
            "version bump {old_version} → {new_version} ({actual_bump}) is insufficient; \
             changes require at least a {required} bump:\n{}",
            breaking.join("\n")
        ))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{parse::parse_ty, FunctionSig};

    #[test]
    fn parse_version() {
        assert_eq!(Version::parse("1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(Version::parse("0.0.0"), Some(Version::new(0, 0, 0)));
        assert_eq!(Version::parse("1.2"), None);
        assert_eq!(Version::parse("abc"), None);
    }

    #[test]
    fn version_display() {
        assert_eq!(Version::new(1, 2, 3).to_string(), "1.2.3");
    }

    #[test]
    fn classify_bumps() {
        let v = |s: &str| Version::parse(s).unwrap();
        assert_eq!(classify_bump(&v("1.0.0"), &v("1.0.0")), Bump::None);
        assert_eq!(classify_bump(&v("1.0.0"), &v("1.0.1")), Bump::Patch);
        assert_eq!(classify_bump(&v("1.0.0"), &v("1.1.0")), Bump::Minor);
        assert_eq!(classify_bump(&v("1.0.0"), &v("2.0.0")), Bump::Major);
    }

    fn sig(name: &str, ty_str: &str) -> FunctionSig {
        FunctionSig {
            name: name.to_string(),
            ty: parse_ty(ty_str).unwrap(),
            description: String::new(),
        }
    }

    #[test]
    fn diff_identical() {
        let old = vec![sig("foo", "Int -> Int")];
        let new = vec![sig("foo", "Int -> Int")];
        let diff = diff_signatures(&old, &new);
        assert!(diff.is_empty());
        assert_eq!(diff.required_bump(), Bump::None);
    }

    #[test]
    fn diff_added_function() {
        let old = vec![sig("foo", "Int -> Int")];
        let new = vec![sig("foo", "Int -> Int"), sig("bar", "String -> String")];
        let diff = diff_signatures(&old, &new);
        assert_eq!(diff.required_bump(), Bump::Minor);
    }

    #[test]
    fn diff_removed_function() {
        let old = vec![sig("foo", "Int -> Int"), sig("bar", "String -> String")];
        let new = vec![sig("foo", "Int -> Int")];
        let diff = diff_signatures(&old, &new);
        assert_eq!(diff.required_bump(), Bump::Major);
    }

    #[test]
    fn diff_breaking_type_change() {
        let old = vec![sig("foo", "Int -> Int")];
        let new = vec![sig("foo", "String -> String")];
        let diff = diff_signatures(&old, &new);
        assert_eq!(diff.required_bump(), Bump::Major);
    }

    #[test]
    fn diff_alpha_equivalent_is_unchanged() {
        let old = vec![sig("foo", "forall a. a -> a")];
        let new = vec![sig("foo", "forall b. b -> b")];
        let diff = diff_signatures(&old, &new);
        assert!(diff.is_empty());
    }

    #[test]
    fn validate_bump_ok() {
        let old = vec![sig("foo", "Int -> Int")];
        let new = vec![sig("foo", "Int -> Int"), sig("bar", "String -> String")];
        let diff = diff_signatures(&old, &new);
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(validate_bump(&v("1.0.0"), &v("1.1.0"), &diff).is_ok());
    }

    #[test]
    fn validate_bump_insufficient() {
        let old = vec![sig("foo", "Int -> Int"), sig("bar", "String -> String")];
        let new = vec![sig("foo", "Int -> Int")]; // bar removed
        let diff = diff_signatures(&old, &new);
        let v = |s: &str| Version::parse(s).unwrap();
        assert!(validate_bump(&v("1.0.0"), &v("1.1.0"), &diff).is_err());
        assert!(validate_bump(&v("1.0.0"), &v("2.0.0"), &diff).is_ok());
    }

    // VersionReq tests
    #[test]
    fn parse_caret() {
        assert_eq!(
            VersionReq::parse("^1.2.3"),
            Some(VersionReq::Caret(Version::new(1, 2, 3)))
        );
    }

    #[test]
    fn parse_tilde() {
        assert_eq!(
            VersionReq::parse("~0.4.0"),
            Some(VersionReq::Tilde(Version::new(0, 4, 0)))
        );
    }

    #[test]
    fn parse_compound() {
        let req = VersionReq::parse(">=1.0.0, <2.0.0").unwrap();
        assert!(matches!(req, VersionReq::And(_)));
    }

    #[test]
    fn caret_major() {
        let req = VersionReq::Caret(Version::new(1, 2, 0));
        assert!(req.matches(&Version::new(1, 2, 0)));
        assert!(req.matches(&Version::new(1, 9, 9)));
        assert!(!req.matches(&Version::new(2, 0, 0)));
        assert!(!req.matches(&Version::new(1, 1, 0)));
    }

    #[test]
    fn caret_zero_minor() {
        let req = VersionReq::Caret(Version::new(0, 2, 0));
        assert!(req.matches(&Version::new(0, 2, 0)));
        assert!(req.matches(&Version::new(0, 2, 9)));
        assert!(!req.matches(&Version::new(0, 3, 0)));
    }

    #[test]
    fn caret_zero_zero() {
        let req = VersionReq::Caret(Version::new(0, 0, 3));
        assert!(req.matches(&Version::new(0, 0, 3)));
        assert!(!req.matches(&Version::new(0, 0, 4)));
    }

    #[test]
    fn tilde_matches() {
        let req = VersionReq::Tilde(Version::new(1, 2, 0));
        assert!(req.matches(&Version::new(1, 2, 0)));
        assert!(req.matches(&Version::new(1, 2, 9)));
        assert!(!req.matches(&Version::new(1, 3, 0)));
    }

    #[test]
    fn compound_range() {
        let req = VersionReq::parse(">=1.0.0, <2.0.0").unwrap();
        assert!(req.matches(&Version::new(1, 0, 0)));
        assert!(req.matches(&Version::new(1, 9, 9)));
        assert!(!req.matches(&Version::new(2, 0, 0)));
        assert!(!req.matches(&Version::new(0, 9, 0)));
    }

    #[test]
    fn generalization_is_compatible() {
        // Changing Int -> Int to forall a. a -> a is backward compatible
        let old = vec![sig("id", "Int -> Int")];
        let new = vec![sig("id", "forall a. a -> a")];
        let diff = diff_signatures(&old, &new);
        assert_eq!(diff.required_bump(), Bump::None);
        assert!(!diff.changes.iter().any(|(_, c)| c.is_breaking()));
    }
}
