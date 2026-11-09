use super::coder_version_meets_baseline;

#[test]
fn newer_coder_release_meets_minimum_baseline() {
    assert!(coder_version_meets_baseline("v2.38.0+c3c6a67", "v2.37.3"));
    assert!(coder_version_meets_baseline("v2.37.3+build", "v2.37.3"));
    assert!(!coder_version_meets_baseline("v2.37.2", "v2.37.3"));
    assert!(!coder_version_meets_baseline("v2.37.3-rc.1", "v2.37.3"));
    assert!(!coder_version_meets_baseline("unknown", "v2.37.3"));
}
