//! Immutable startup loading for the PMU profile and scenario catalog.

use std::{fmt, fs, path::Path};

use crate::{
    config::{parse_profile, CompiledProfile},
    identity::RuntimeIdentity,
    scenario::{parse_catalog, ScenarioCatalog},
};

pub const DEFAULT_IMAGE_REF: &str = "local-build";

#[derive(Debug, Clone, PartialEq)]
pub struct Startup {
    pub profile: CompiledProfile,
    pub scenario_catalog: ScenarioCatalog,
    pub runtime_identity: RuntimeIdentity,
    pub deployment_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartupError(String);

impl StartupError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for StartupError {}

pub fn load_startup(
    profile_path: impl AsRef<Path>,
    scenario_catalog_path: impl AsRef<Path>,
    deployment_label: impl Into<String>,
    image_ref: impl Into<String>,
) -> Result<Startup, StartupError> {
    let deployment_label = deployment_label.into();
    validate_text("deployment label", &deployment_label, 128)?;
    let image_ref = image_ref.into();
    validate_text("image reference", &image_ref, 255)?;

    let profile_path = profile_path.as_ref();
    let scenario_catalog_path = scenario_catalog_path.as_ref();
    let profile_contents = read_file(profile_path, "profile")?;
    let scenario_catalog_contents = read_file(scenario_catalog_path, "scenario catalog")?;
    let profile_text = std::str::from_utf8(&profile_contents).map_err(|error| {
        StartupError::new(format!("profile {} is not UTF-8: {error}", profile_path.display()))
    })?;
    let scenario_catalog_text = std::str::from_utf8(&scenario_catalog_contents).map_err(|error| {
        StartupError::new(format!(
            "scenario catalog {} is not UTF-8: {error}",
            scenario_catalog_path.display()
        ))
    })?;
    let profile = parse_profile(profile_text)
        .map_err(|error| StartupError::new(format!("invalid profile: {error}")))?;
    let scenario_catalog = parse_catalog(scenario_catalog_text)
        .map_err(|error| StartupError::new(format!("invalid scenario catalog: {error}")))?;

    Ok(Startup {
        profile,
        scenario_catalog,
        runtime_identity: RuntimeIdentity::new(image_ref, &profile_contents, &scenario_catalog_contents),
        deployment_label,
    })
}

fn read_file(path: &Path, kind: &str) -> Result<Vec<u8>, StartupError> {
    fs::read(path)
        .map_err(|error| StartupError::new(format!("cannot read {kind} {}: {error}", path.display())))
}

fn validate_text(name: &str, value: &str, maximum_bytes: usize) -> Result<(), StartupError> {
    if value.is_empty() || value.len() > maximum_bytes || value.bytes().any(|byte| byte.is_ascii_control()) {
        return Err(StartupError::new(format!(
            "{name} must contain 1 to {maximum_bytes} non-control bytes"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::identity::sha256_hex;

    use super::{load_startup, DEFAULT_IMAGE_REF};

    #[test]
    fn loads_the_shipped_immutable_startup_inputs() {
        let startup = load_startup(
            concat!(env!("CARGO_MANIFEST_DIR"), "/profiles/five-pmu-v2.yaml"),
            concat!(env!("CARGO_MANIFEST_DIR"), "/scenarios/baseline.yaml"),
            "test-deployment",
            DEFAULT_IMAGE_REF,
        )
        .expect("startup inputs must load");

        assert_eq!(startup.profile.endpoints.len(), 5);
        assert_eq!(startup.scenario_catalog.scenarios().len(), 6);
        assert_eq!(startup.deployment_label, "test-deployment");
        assert_eq!(
            startup.runtime_identity.profile_sha256,
            sha256_hex(include_bytes!("../profiles/five-pmu-v2.yaml"))
        );
        assert_eq!(
            startup.runtime_identity.scenario_catalog_sha256,
            sha256_hex(include_bytes!("../scenarios/baseline.yaml"))
        );
    }

    #[test]
    fn rejects_an_empty_deployment_label_before_reading_files() {
        let error = load_startup("missing-profile", "missing-catalog", "", DEFAULT_IMAGE_REF)
            .expect_err("empty deployment labels must fail");

        assert!(error.to_string().contains("deployment label"));
    }
}