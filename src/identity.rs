//! Immutable identifiers for the startup inputs of one emulator process.

use sha2::{Digest, Sha256};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RuntimeIdentity {
    pub image_ref: String,
    pub profile_sha256: String,
    pub scenario_catalog_sha256: String,
}

impl RuntimeIdentity {
    pub fn new(image_ref: impl Into<String>, profile: &[u8], scenario_catalog: &[u8]) -> Self {
        Self {
            image_ref: image_ref.into(),
            profile_sha256: sha256_hex(profile),
            scenario_catalog_sha256: sha256_hex(scenario_catalog),
        }
    }
}

pub fn sha256_hex(contents: &[u8]) -> String {
    format!("{:x}", Sha256::digest(contents))
}

#[cfg(test)]
mod tests {
    use super::{sha256_hex, RuntimeIdentity};

    #[test]
    fn identifies_each_immutable_startup_input() {
        let identity = RuntimeIdentity::new("local-build", b"profile", b"catalog");

        assert_eq!(identity.image_ref, "local-build");
        assert_eq!(identity.profile_sha256, sha256_hex(b"profile"));
        assert_eq!(identity.scenario_catalog_sha256, sha256_hex(b"catalog"));
        assert_ne!(identity.profile_sha256, identity.scenario_catalog_sha256);
    }

    #[test]
    fn produces_the_standard_sha256_hex_encoding() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}