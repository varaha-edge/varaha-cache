//! vmod_purge -- cache purge operations.
//!
//! Provides hard and soft purge mechanisms as VMOD functions.
//! Hard purge immediately removes an object; soft purge sets
//! its TTL to zero while preserving the grace period.

use crate::error::VmodError;
use crate::registry::VmodFunction;
use crate::types::{VclValue, VclValueExt};

/// purge.hard() - Hard purge: immediately remove the current object.
///
/// In a full VCL runtime this would remove the object from the cache.
/// Here it returns a Bool indicating the purge was requested.
pub struct PurgeHard;

impl VmodFunction for PurgeHard {
    fn name(&self) -> &str {
        "hard"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        if !args.is_empty() {
            return Err(VmodError::InvalidArgument(
                "hard() takes no arguments".to_string(),
            ));
        }
        // In a real VCL context, this would interact with the request
        // context to mark the current object for immediate removal.
        // Here we signal success.
        tracing::info!(target: "vmod_purge", "hard purge requested");
        Ok(VclValue::Bool(true))
    }
}

/// purge.soft(ttl, grace, keep) - Soft purge: set TTL to 0, keep grace.
///
/// This allows the object to be served stale while a new copy is fetched,
/// rather than immediately removing it. Optional arguments override the
/// TTL, grace, and keep values.
pub struct PurgeSoft;

impl VmodFunction for PurgeSoft {
    fn name(&self) -> &str {
        "soft"
    }

    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
        // soft() can be called with 0 to 3 optional duration arguments:
        // soft()           - use defaults (ttl=0, grace=preserved, keep=preserved)
        // soft(ttl)        - set TTL explicitly
        // soft(ttl, grace) - set TTL and grace
        // soft(ttl, grace, keep) - set all three
        if args.len() > 3 {
            return Err(VmodError::InvalidArgument(
                "soft([ttl [, grace [, keep]]]) takes at most 3 arguments".to_string(),
            ));
        }

        let ttl = if !args.is_empty() {
            args[0].try_to_duration()?.as_secs()
        } else {
            0.0
        };

        let grace = if args.len() >= 2 {
            Some(args[1].try_to_duration()?.as_secs())
        } else {
            None
        };

        let keep = if args.len() >= 3 {
            Some(args[2].try_to_duration()?.as_secs())
        } else {
            None
        };

        tracing::info!(
            target: "vmod_purge",
            "soft purge requested: ttl={ttl}s, grace={grace:?}s, keep={keep:?}s"
        );

        Ok(VclValue::Bool(true))
    }
}

/// Build the full set of purge module functions.
pub fn purge_module() -> Vec<Box<dyn VmodFunction>> {
    vec![Box::new(PurgeHard), Box::new(PurgeSoft)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hard_purge() {
        let f = PurgeHard;
        let result = f.call(&[]).unwrap();
        assert!(result.to_bool());
    }

    #[test]
    fn test_hard_purge_no_args() {
        let f = PurgeHard;
        let result = f.call(&[VclValue::Int(1)]);
        assert!(result.is_err());
    }

    #[test]
    fn test_soft_purge_no_args() {
        let f = PurgeSoft;
        let result = f.call(&[]).unwrap();
        assert!(result.to_bool());
    }

    #[test]
    fn test_soft_purge_with_ttl() {
        let f = PurgeSoft;
        let result = f
            .call(&[VclValue::Duration(0.0)])
            .unwrap();
        assert!(result.to_bool());
    }

    #[test]
    fn test_soft_purge_with_ttl_and_grace() {
        let f = PurgeSoft;
        let result = f
            .call(&[
                VclValue::Duration(0.0),
                VclValue::Duration(60.0),
            ])
            .unwrap();
        assert!(result.to_bool());
    }

    #[test]
    fn test_soft_purge_with_all_args() {
        let f = PurgeSoft;
        let result = f
            .call(&[
                VclValue::Duration(0.0),
                VclValue::Duration(60.0),
                VclValue::Duration(300.0),
            ])
            .unwrap();
        assert!(result.to_bool());
    }

    #[test]
    fn test_soft_purge_too_many_args() {
        let f = PurgeSoft;
        let result = f.call(&[
            VclValue::Duration(0.0),
            VclValue::Duration(60.0),
            VclValue::Duration(300.0),
            VclValue::Duration(600.0),
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn test_purge_module_factory() {
        let funcs = purge_module();
        let names: Vec<&str> = funcs.iter().map(|f| f.name()).collect();
        assert!(names.contains(&"hard"));
        assert!(names.contains(&"soft"));
    }
}
