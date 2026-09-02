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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordering_is_unknown_lowest() {
        assert!(Confidence::Unknown < Confidence::Likely);
        assert!(Confidence::Likely < Confidence::Certain);
    }
}
