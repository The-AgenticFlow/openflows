//! Coder version comparisons shared by the adapter's capability checks.

/// Whether `actual` meets the minimum supported `baseline` Coder version.
///
/// A release is acceptable when it is strictly newer than the baseline, or
/// when it matches the baseline and is not an unstable prerelease while the
/// baseline is stable. Any unparsable value fails closed.
pub(super) fn coder_version_meets_baseline(actual: &str, baseline: &str) -> bool {
    fn parse(value: &str) -> Option<((u64, u64, u64), bool)> {
        let value = value.strip_prefix('v').unwrap_or(value);
        let (version, _) = value.split_once('+').unwrap_or((value, ""));
        let (core, prerelease) = version.split_once('-').unwrap_or((version, ""));
        let mut parts = core.split('.').map(str::parse::<u64>);
        let numbers = (
            parts.next()?.ok()?,
            parts.next()?.ok()?,
            parts.next()?.ok()?,
        );
        parts
            .next()
            .is_none()
            .then_some((numbers, !prerelease.is_empty()))
    }
    match (parse(actual), parse(baseline)) {
        (Some((actual, actual_pre)), Some((baseline, baseline_pre))) => {
            actual > baseline || (actual == baseline && (!actual_pre || baseline_pre))
        }
        _ => false,
    }
}

#[cfg(test)]
#[path = "adapter_version_tests.rs"]
mod version_tests;
