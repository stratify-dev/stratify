use serde::{Deserialize, Serialize};

/// How sure the adapter is about a symbol or reference.
/// Ordering matters: Unknown < Likely < Certain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confidence {
    Unknown,
    Likely,
    Certain,
}

impl Confidence {
    /// The weaker of two confidences. Used when combining edges along a path.
    pub fn min_with(self, other: Confidence) -> Confidence {
        self.min(other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_unknown_lowest() {
        assert!(Confidence::Unknown < Confidence::Likely);
        assert!(Confidence::Likely < Confidence::Certain);
    }

    #[test]
    fn min_with_picks_weaker() {
        assert_eq!(
            Confidence::Certain.min_with(Confidence::Unknown),
            Confidence::Unknown
        );
        assert_eq!(
            Confidence::Likely.min_with(Confidence::Certain),
            Confidence::Likely
        );
    }
}
