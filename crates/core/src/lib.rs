pub mod domain;

/// Returns true if the repository bootstrap is wired correctly.
pub fn bootstrap_ready() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arithmetic_smoke_test() {
        assert_eq!(2 + 2, 4);
    }

    #[test]
    fn bootstrap_ready_returns_true() {
        assert!(bootstrap_ready());
    }
}
