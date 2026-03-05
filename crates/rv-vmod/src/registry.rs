use dashmap::DashMap;

use crate::error::VmodError;
use crate::types::VclValue;

/// A function exposed by a VMOD.
///
/// Each VMOD function has a name and can be called with a slice of VCL values.
/// Implementations must be Send + Sync for safe concurrent access.
pub trait VmodFunction: Send + Sync {
    /// The name of this function within its module.
    fn name(&self) -> &str;

    /// Invoke this function with the given arguments.
    fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError>;
}

/// Registry for VMOD modules and their functions.
///
/// Maintains a thread-safe mapping from module names to their exported functions.
/// Uses DashMap for lock-free concurrent reads with sharded locking on writes.
pub struct VmodRegistry {
    modules: DashMap<String, Vec<Box<dyn VmodFunction>>>,
}

impl VmodRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self {
            modules: DashMap::new(),
        }
    }

    /// Register a module with its set of exported functions.
    ///
    /// If a module with the same name already exists, it is replaced.
    pub fn register_module(&self, name: impl Into<String>, functions: Vec<Box<dyn VmodFunction>>) {
        self.modules.insert(name.into(), functions);
    }

    /// Call a function within a registered module.
    ///
    /// Returns `VmodError::NotFound` if the module or function does not exist.
    pub fn call(
        &self,
        module: &str,
        function: &str,
        args: &[VclValue],
    ) -> Result<VclValue, VmodError> {
        let funcs = self
            .modules
            .get(module)
            .ok_or_else(|| VmodError::NotFound(format!("module '{module}' not found")))?;

        let func = funcs.iter().find(|f| f.name() == function).ok_or_else(|| {
            VmodError::NotFound(format!(
                "function '{function}' not found in module '{module}'"
            ))
        })?;

        func.call(args)
    }

    /// List all registered module names.
    pub fn list_modules(&self) -> Vec<String> {
        self.modules
            .iter()
            .map(|entry| entry.key().clone())
            .collect()
    }

    /// Check whether a specific module is registered.
    pub fn has_module(&self, name: &str) -> bool {
        self.modules.contains_key(name)
    }

    /// List all function names within a module.
    pub fn list_functions(&self, module: &str) -> Option<Vec<String>> {
        self.modules
            .get(module)
            .map(|funcs| funcs.iter().map(|f| f.name().to_string()).collect())
    }
}

impl rv_vcl::FunctionResolver for VmodRegistry {
    fn call(&self, module: &str, function: &str, args: &[VclValue]) -> Result<VclValue, String> {
        VmodRegistry::call(self, module, function, args).map_err(|e| e.to_string())
    }

    fn has_function(&self, module: &str, function: &str) -> bool {
        self.list_functions(module)
            .map(|fns| fns.contains(&function.to_string()))
            .unwrap_or(false)
    }
}

impl Default for VmodRegistry {
    /// Create a registry pre-populated with all built-in VMODs.
    fn default() -> Self {
        let registry = Self::new();
        registry.register_module("std", crate::std_vmod::std_module());
        registry.register_module("blob", crate::blob::blob_module());
        registry.register_module("purge", crate::purge::purge_module());
        registry.register_module("directors", crate::directors::directors_module());
        registry
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct AddFunc;

    impl VmodFunction for AddFunc {
        fn name(&self) -> &str {
            "add"
        }

        fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
            if args.len() != 2 {
                return Err(VmodError::InvalidArgument(
                    "add requires exactly 2 arguments".to_string(),
                ));
            }
            let a = args[0].to_int();
            let b = args[1].to_int();
            Ok(VclValue::Int(a + b))
        }
    }

    struct NegateFunc;

    impl VmodFunction for NegateFunc {
        fn name(&self) -> &str {
            "negate"
        }

        fn call(&self, args: &[VclValue]) -> Result<VclValue, VmodError> {
            if args.len() != 1 {
                return Err(VmodError::InvalidArgument(
                    "negate requires exactly 1 argument".to_string(),
                ));
            }
            let val = args[0].to_int();
            Ok(VclValue::Int(-val))
        }
    }

    #[test]
    fn test_register_and_call() {
        let registry = VmodRegistry::new();
        let funcs: Vec<Box<dyn VmodFunction>> = vec![Box::new(AddFunc), Box::new(NegateFunc)];
        registry.register_module("math", funcs);

        let result = registry
            .call("math", "add", &[VclValue::Int(3), VclValue::Int(4)])
            .unwrap();
        assert_eq!(result.to_int(), 7);

        let result = registry
            .call("math", "negate", &[VclValue::Int(5)])
            .unwrap();
        assert_eq!(result.to_int(), -5);
    }

    #[test]
    fn test_module_not_found() {
        let registry = VmodRegistry::new();
        let result = registry.call("nonexistent", "func", &[]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmodError::NotFound(_)));
    }

    #[test]
    fn test_function_not_found() {
        let registry = VmodRegistry::new();
        registry.register_module("math", vec![Box::new(AddFunc)]);
        let result = registry.call("math", "subtract", &[]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), VmodError::NotFound(_)));
    }

    #[test]
    fn test_list_modules() {
        let registry = VmodRegistry::new();
        registry.register_module("std", vec![]);
        registry.register_module("blob", vec![]);

        let mut modules = registry.list_modules();
        modules.sort();
        assert_eq!(modules, vec!["blob", "std"]);
    }

    #[test]
    fn test_has_module() {
        let registry = VmodRegistry::new();
        registry.register_module("std", vec![]);
        assert!(registry.has_module("std"));
        assert!(!registry.has_module("nonexistent"));
    }

    #[test]
    fn test_list_functions() {
        let registry = VmodRegistry::new();
        registry.register_module("math", vec![Box::new(AddFunc), Box::new(NegateFunc)]);

        let funcs = registry.list_functions("math").unwrap();
        assert!(funcs.contains(&"add".to_string()));
        assert!(funcs.contains(&"negate".to_string()));
        assert_eq!(funcs.len(), 2);

        assert!(registry.list_functions("nonexistent").is_none());
    }

    #[test]
    fn test_default_registry() {
        let registry = VmodRegistry::default();
        assert!(registry.has_module("std"));
        assert!(registry.has_module("blob"));
        assert!(registry.has_module("purge"));
        assert!(registry.has_module("directors"));
    }
}
