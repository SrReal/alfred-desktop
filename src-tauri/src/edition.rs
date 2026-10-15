//! Public edition profile is isolated from the personal installation.
pub const PROFILE_DIRECTORY: &str = "AlfredPetPublic";
pub const MAINTENANCE_DIRECTORY: &str = "AlfredPetPublic-maintenance";

#[cfg(test)]
pub const PET_REFERENCE: &[u8] = include_bytes!("../../assets/public/pet-reference.png");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_profile_never_targets_the_personal_profile() {
        assert_eq!(PROFILE_DIRECTORY, "AlfredPetPublic");
        assert_eq!(MAINTENANCE_DIRECTORY, "AlfredPetPublic-maintenance");
        assert_ne!(PROFILE_DIRECTORY, "AlfredPet");
    }
}
