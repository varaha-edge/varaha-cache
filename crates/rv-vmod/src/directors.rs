//! vmod_directors -- director management wrappers.
//!
//! Re-exports the director types from rv-backend and provides a
//! VMOD function interface for listing available director types.

pub use rv_backend::director::{
    FallbackDirector, HashDirector, RandomDirector, RoundRobinDirector,
};

use crate::error::VmodError;
use crate::registry::VmodFunction;
use crate::types::VclValue;

/// directors.list_types() - List all available director types.
pub struct DirectorsListTypes;

impl VmodFunction for DirectorsListTypes {
    fn name(&self) -> &str {
        "list_types"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "list_types() takes no arguments".to_string(),
            ));
        }
        let types = "round_robin, random, hash, fallback";
        Ok(VclValue::String(types.to_string()))
    }
}

/// directors.round_robin() - Indicate round-robin director type (returns type name).
pub struct DirectorsRoundRobin;

impl VmodFunction for DirectorsRoundRobin {
    fn name(&self) -> &str {
        "round_robin"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "round_robin() takes no arguments".to_string(),
            ));
        }
        Ok(VclValue::String("round_robin".to_string()))
    }
}

/// directors.random() - Indicate random director type (returns type name).
pub struct DirectorsRandom;

impl VmodFunction for DirectorsRandom {
    fn name(&self) -> &str {
        "random"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "random() takes no arguments".to_string(),
            ));
        }
        Ok(VclValue::String("random".to_string()))
    }
}

/// directors.hash() - Indicate hash director type (returns type name).
pub struct DirectorsHash;

impl VmodFunction for DirectorsHash {
    fn name(&self) -> &str {
        "hash"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "hash() takes no arguments".to_string(),
            ));
        }
        Ok(VclValue::String("hash".to_string()))
    }
}

/// directors.fallback() - Indicate fallback director type (returns type name).
pub struct DirectorsFallback;

impl VmodFunction for DirectorsFallback {
    fn name(&self) -> &str {
        "fallback"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "fallback() takes no arguments".to_string(),
            ));
        }
        Ok(VclValue::String("fallback".to_string()))
    }
}

/// Build the full set of directors module functions.
pub fn directors_module() -> Vec<Box<dyn VmodFunction>> {
    vec![
        Box::new(DirectorsListTypes),
        Box::new(DirectorsRoundRobin),
        Box::new(DirectorsRandom),
        Box::new(DirectorsHash),
        Box::new(DirectorsFallback),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_list_types() {
        let f = DirectorsListTypes;
        let result = f.call(&[]).unwrap();
        let s = result.to_string_value();
        assert!(s.contains("round_robin"));
        assert!(s.contains("random"));
        assert!(s.contains("hash"));
        assert!(s.contains("fallback"));
    }

    #[test]
    fn test_round_robin_type() {
        let f = DirectorsRoundRobin;
        let result = f.call(&[]).unwrap();
        assert_eq!(result.to_string_value(), "round_robin");
    }

    #[test]
    fn test_random_type() {
        let f = DirectorsRandom;
        let result = f.call(&[]).unwrap();
        assert_eq!(result.to_string_value(), "random");
    }

    #[test]
    fn test_hash_type() {
        let f = DirectorsHash;
        let result = f.call(&[]).unwrap();
        assert_eq!(result.to_string_value(), "hash");
    }

    #[test]
    fn test_fallback_type() {
        let f = DirectorsFallback;
        let result = f.call(&[]).unwrap();
        assert_eq!(result.to_string_value(), "fallback");
    }

    #[test]
    fn test_directors_module_factory() {
        let funcs = directors_module();
        let names: Vec<&str> = funcs.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"list_types"));
        assert!(names.contains(&"round_robin"));
        assert!(names.contains(&"random"));
        assert!(names.contains(&"hash"));
        assert!(names.contains(&"fallback"));
    }

    #[test]
    fn test_list_types_no_args() {
        let f = DirectorsListTypes;
        let result = f.call(&[VclValue::Int(1)]);
        assert!(result.is_err());
    }
}
