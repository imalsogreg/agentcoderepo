//! Parse `agentcoderepo.toml` package manifests.

use std::collections::HashMap;

use serde::Deserialize;
use thiserror::Error;

use crate::semver::{Version, VersionReq};

/// A parsed `agentcoderepo.toml` manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: PackageSection,
    pub dependencies: Vec<Dependency>,
}

/// The `[package]` section of a manifest.
#[derive(Debug, Clone)]
pub struct PackageSection {
    pub version: Version,
}

/// A declared dependency on another AgentCodeRepo repo.
#[derive(Debug, Clone)]
pub struct Dependency {
    /// Local alias (the TOML key).
    pub name: String,
    /// "owner/repo" reference.
    pub repo: String,
    /// Version requirement (e.g. "^1.0").
    pub version_req: VersionReq,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to parse agentcoderepo.toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("missing [package] section")]
    MissingPackage,
    #[error("missing or invalid version: {0}")]
    BadVersion(String),
    #[error("invalid dependency '{0}': {1}")]
    BadDependency(String, String),
}

/// Raw TOML structure (private, for deserialization only).
#[derive(Deserialize)]
struct RawManifest {
    package: Option<RawPackage>,
    #[serde(default)]
    dependencies: HashMap<String, RawDep>,
}

#[derive(Deserialize)]
struct RawPackage {
    version: Option<String>,
}

#[derive(Deserialize)]
struct RawDep {
    repo: String,
    version: String,
}

/// Parse a `agentcoderepo.toml` file from its text content.
pub fn parse_manifest(content: &str) -> Result<Manifest, ManifestError> {
    let raw: RawManifest = toml::from_str(content)?;
    let pkg = raw.package.ok_or(ManifestError::MissingPackage)?;
    let version_str = pkg
        .version
        .ok_or_else(|| ManifestError::BadVersion("missing".into()))?;
    let version = Version::parse(&version_str)
        .ok_or_else(|| ManifestError::BadVersion(version_str.clone()))?;

    let mut dependencies = Vec::new();
    for (name, raw_dep) in raw.dependencies {
        let version_req = VersionReq::parse(&raw_dep.version).ok_or_else(|| {
            ManifestError::BadDependency(name.clone(), format!("invalid version: {}", raw_dep.version))
        })?;

        // Validate repo format: "owner/repo"
        if !raw_dep.repo.contains('/') {
            return Err(ManifestError::BadDependency(
                name.clone(),
                format!("repo must be 'owner/repo', got: {}", raw_dep.repo),
            ));
        }

        dependencies.push(Dependency {
            name,
            repo: raw_dep.repo,
            version_req,
        });
    }

    // Sort for deterministic ordering
    dependencies.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(Manifest {
        package: PackageSection { version },
        dependencies,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_minimal_manifest() {
        let manifest = parse_manifest(
            r#"
            [package]
            version = "1.2.3"
            "#,
        )
        .unwrap();
        assert_eq!(manifest.package.version, Version::new(1, 2, 3));
        assert!(manifest.dependencies.is_empty());
    }

    #[test]
    fn missing_package_section() {
        let err = parse_manifest("").unwrap_err();
        assert!(matches!(err, ManifestError::MissingPackage));
    }

    #[test]
    fn missing_version() {
        let err = parse_manifest("[package]\n").unwrap_err();
        assert!(matches!(err, ManifestError::BadVersion(_)));
    }

    #[test]
    fn invalid_version_format() {
        let err = parse_manifest(
            r#"
            [package]
            version = "not-a-version"
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadVersion(_)));
    }

    #[test]
    fn extra_fields_ignored() {
        let manifest = parse_manifest(
            r#"
            [package]
            version = "0.1.0"
            some_future_field = "hello"
            "#,
        )
        .unwrap();
        assert_eq!(manifest.package.version, Version::new(0, 1, 0));
    }

    #[test]
    fn parse_with_dependencies() {
        let manifest = parse_manifest(
            r#"
            [package]
            version = "1.0.0"

            [dependencies.sort-lib]
            repo = "agent-a/sort-lib"
            version = "^1.0.0"

            [dependencies.http-lib]
            repo = "agent-b/http-lib"
            version = ">=2.1.0, <3.0.0"
            "#,
        )
        .unwrap();
        assert_eq!(manifest.dependencies.len(), 2);
        // Sorted by name
        assert_eq!(manifest.dependencies[0].name, "http-lib");
        assert_eq!(manifest.dependencies[0].repo, "agent-b/http-lib");
        assert_eq!(manifest.dependencies[1].name, "sort-lib");
        assert_eq!(manifest.dependencies[1].repo, "agent-a/sort-lib");
    }

    #[test]
    fn parse_dependencies_inline() {
        let manifest = parse_manifest(
            r#"
            [package]
            version = "1.0.0"

            [dependencies]
            sort-lib = { repo = "agent-a/sort-lib", version = "^1.0.0" }
            "#,
        )
        .unwrap();
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].name, "sort-lib");
    }

    #[test]
    fn invalid_dependency_repo_format() {
        let err = parse_manifest(
            r#"
            [package]
            version = "1.0.0"

            [dependencies]
            bad = { repo = "no-slash", version = "^1.0.0" }
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadDependency(..)));
    }

    #[test]
    fn invalid_dependency_version() {
        let err = parse_manifest(
            r#"
            [package]
            version = "1.0.0"

            [dependencies]
            bad = { repo = "a/b", version = "not-valid" }
            "#,
        )
        .unwrap_err();
        assert!(matches!(err, ManifestError::BadDependency(..)));
    }
}
