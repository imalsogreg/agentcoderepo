//! AgentCodeRepo prelude — built-in type names shared across all packages.
//!
//! These are structural/nominal-by-convention types that all AgentCodeRepo packages
//! can reference without importing. Custom types beyond this set will
//! eventually require explicit imports.

use std::collections::HashSet;
use std::sync::LazyLock;

/// The set of built-in type constructor names recognized by AgentCodeRepo.
///
/// These cover common data structures, error handling, and concurrency
/// patterns across languages. Primitives (Int, String, etc.) are handled
/// separately by `Ty::Prim` and are not included here.
pub static PRELUDE_TYPES: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    HashSet::from([
        // Collections
        "List",
        "Map",
        "Set",
        "Array",
        "Vec",
        // Option/Result
        "Option",
        "Result",
        "Maybe",
        // Concurrency
        "Future",
        "Promise",
        "Stream",
        "Channel",
        // I/O
        "Path",
        "Handle",
        "Response",
        "Request",
        // Common abstractions
        "Pair",
        "Either",
        "Ref",
        "Box",
    ])
});

/// Check whether a type name is in the AgentCodeRepo prelude.
pub fn is_prelude_type(name: &str) -> bool {
    PRELUDE_TYPES.contains(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_types_in_prelude() {
        assert!(is_prelude_type("List"));
        assert!(is_prelude_type("Option"));
        assert!(is_prelude_type("Result"));
        assert!(is_prelude_type("Map"));
    }

    #[test]
    fn custom_types_not_in_prelude() {
        assert!(!is_prelude_type("MyCustomType"));
        assert!(!is_prelude_type("HttpError"));
    }

    #[test]
    fn primitives_not_in_prelude() {
        // Primitives are Ty::Prim, not Named, so they aren't in the prelude set
        assert!(!is_prelude_type("Int"));
        assert!(!is_prelude_type("String"));
    }
}
