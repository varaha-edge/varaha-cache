use rv_types::VclValue;

/// Trait for resolving VMOD function calls from the VCL interpreter.
///
/// Implementations can delegate to a VmodRegistry or any other
/// function dispatch mechanism (e.g., WASM modules).
pub trait FunctionResolver: Send + Sync {
    /// Call a function within a module.
    fn call(&self, module: &str, function: &str, args: &[VclValue]) -> Result<VclValue, String>;

    /// Check if a module/function pair exists.
    fn has_function(&self, module: &str, function: &str) -> bool;
}
