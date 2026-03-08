//! Parse `agentcoderepo.toml` package manifests.

use serde::Deserialize;
use thiserror::Error;

use crate::semver::Version;

/// A parsed `agentcoderepo.toml` manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    pub package: PackageSection,
}

/// The `[package]` section of a manifest.
#[derive(Debug, Clone)]
pub struct PackageSection {
    pub version: Version,
}

#[derive(Debug, Error)]
pub enum ManifestError {
    #[error("failed to parse agentcoderepo.toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("missing [package] section")]
    MissingPackage,
    #[error("missing or invalid version: {0}")]
    BadVersion(String),
}

/// Raw TOML structure (private, for deserialization only).
#[derive(Deserialize)]
struct RawManifest {
    package: Option<RawPackage>,
}

#[derive(Deserialize)]
struct RawPackage {
    version: Option<String>,
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

    Ok(Manifest {
        package: PackageSection { version },
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
}
