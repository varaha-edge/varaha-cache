//! VCL program lifecycle management.
//!
//! The [`VclManager`] keeps track of all loaded VCL programs, which one is
//! active, and provides methods to load, activate, list, and discard programs.
//! It is designed to be shared across async tasks via `Arc` and uses interior
//! mutability with `std::sync::Mutex` for thread safety.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use rv_vcl::ast::VclProgram;
use rv_vcl::interpreter::VclInterpreter;
use rv_vcl::{Lexer, Parser};

/// The lifecycle state of a loaded VCL program.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VclProgramState {
    /// Loaded and available for activation.
    Available,
    /// Currently the active program handling requests.
    Active,
    /// Marked for removal (cannot be re-activated).
    Discarded,
}

impl std::fmt::Display for VclProgramState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VclProgramState::Available => write!(f, "available"),
            VclProgramState::Active => write!(f, "active"),
            VclProgramState::Discarded => write!(f, "discarded"),
        }
    }
}

/// A single loaded VCL program with its parsed AST, interpreter, and metadata.
pub struct VclProgramEntry {
    /// Human-readable name for this VCL configuration.
    pub name: String,
    /// The parsed VCL abstract syntax tree.
    pub program: Arc<VclProgram>,
    /// The interpreter instance ready to execute this program.
    pub interpreter: Arc<VclInterpreter>,
    /// Current lifecycle state.
    pub state: VclProgramState,
    /// When the program was loaded.
    pub loaded_at: Instant,
}

/// Manages the set of loaded VCL programs and tracks which one is active.
///
/// All mutation goes through interior `Mutex` locks so that the manager can
/// be shared via `Arc` without external synchronisation.
pub struct VclManager {
    /// Map of program name to entry.
    programs: Mutex<HashMap<String, VclProgramEntry>>,
    /// Name of the currently active program, if any.
    active_name: Mutex<Option<String>>,
}

impl VclManager {
    /// Create a new, empty VCL manager.
    pub fn new() -> Self {
        Self {
            programs: Mutex::new(HashMap::new()),
            active_name: Mutex::new(None),
        }
    }

    /// Parse and load a VCL program from source text.
    ///
    /// The program is stored as `Available`. If a program with the same name
    /// already exists and is not active, it is replaced.
    pub fn load(&self, name: &str, vcl_source: &str) -> Result<(), String> {
        // Tokenize
        let tokens = Lexer::tokenize(vcl_source).map_err(|e| format!("VCL lexer error: {e}"))?;

        // Parse
        let program = Parser::parse(&tokens).map_err(|e| format!("VCL parse error: {e}"))?;

        let program = Arc::new(program);
        let interpreter = Arc::new(VclInterpreter::new(Arc::clone(&program)));

        let entry = VclProgramEntry {
            name: name.to_string(),
            program,
            interpreter,
            state: VclProgramState::Available,
            loaded_at: Instant::now(),
        };

        let mut programs = self.programs.lock().unwrap();

        // Do not allow overwriting the currently active program.
        if let Some(existing) = programs.get(name)
            && existing.state == VclProgramState::Active
        {
            return Err(format!(
                "VCL program '{name}' is currently active; discard or use a different name"
            ));
        }

        programs.insert(name.to_string(), entry);
        Ok(())
    }

    /// Set the named program as the active VCL configuration.
    ///
    /// The previously active program (if any) is moved to `Available` state.
    /// Returns the interpreter for the newly active program.
    pub fn use_program(&self, name: &str) -> Result<Arc<VclInterpreter>, String> {
        let mut programs = self.programs.lock().unwrap();
        let mut active_name = self.active_name.lock().unwrap();

        // Verify the target exists and is not discarded.
        let target = programs
            .get(name)
            .ok_or_else(|| format!("VCL program '{name}' not found"))?;

        if target.state == VclProgramState::Discarded {
            return Err(format!("VCL program '{name}' has been discarded"));
        }

        let interpreter = Arc::clone(&target.interpreter);

        // Deactivate the current active program.
        if let Some(prev_name) = active_name.as_ref() {
            if let Some(prev) = programs.get_mut(prev_name) {
                if prev.state == VclProgramState::Active {
                    prev.state = VclProgramState::Available;
                }
            }
        }

        // Activate the new program.
        let entry = programs.get_mut(name).unwrap();
        entry.state = VclProgramState::Active;
        *active_name = Some(name.to_string());

        Ok(interpreter)
    }

    /// List all loaded programs with their name, state, and load time.
    pub fn list(&self) -> Vec<(String, VclProgramState, Instant)> {
        let programs = self.programs.lock().unwrap();
        let mut entries: Vec<_> = programs
            .values()
            .map(|e| (e.name.clone(), e.state, e.loaded_at))
            .collect();
        entries.sort_by(|a, b| a.2.cmp(&b.2));
        entries
    }

    /// Discard a non-active VCL program.
    ///
    /// The active program cannot be discarded. Discarded programs are removed
    /// from the manager entirely.
    pub fn discard(&self, name: &str) -> Result<(), String> {
        let mut programs = self.programs.lock().unwrap();

        let entry = programs
            .get(name)
            .ok_or_else(|| format!("VCL program '{name}' not found"))?;

        if entry.state == VclProgramState::Active {
            return Err(format!(
                "VCL program '{name}' is active and cannot be discarded"
            ));
        }

        programs.remove(name);
        Ok(())
    }

    /// Return the interpreter for the currently active VCL program, if any.
    pub fn active_interpreter(&self) -> Option<Arc<VclInterpreter>> {
        let active_name = self.active_name.lock().unwrap();
        let programs = self.programs.lock().unwrap();

        active_name
            .as_ref()
            .and_then(|name| programs.get(name))
            .map(|entry| Arc::clone(&entry.interpreter))
    }

    /// Return the name of the currently active VCL program, if any.
    pub fn active_name(&self) -> Option<String> {
        self.active_name.lock().unwrap().clone()
    }
}

impl Default for VclManager {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_VCL: &str = r#"
vcl 4.0;
backend default {
    .host = "127.0.0.1";
    .port = "8080";
}
"#;

    const MINIMAL_VCL_2: &str = r#"
vcl 4.0;
backend api {
    .host = "10.0.0.1";
    .port = "9090";
}
"#;

    #[test]
    fn load_and_list() {
        let mgr = VclManager::new();
        mgr.load("test1", MINIMAL_VCL).unwrap();

        let list = mgr.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "test1");
        assert_eq!(list[0].1, VclProgramState::Available);
    }

    #[test]
    fn load_invalid_vcl_returns_error() {
        let mgr = VclManager::new();
        let result = mgr.load("bad", "this is not valid VCL at all {{{{");
        assert!(result.is_err());
    }

    #[test]
    fn use_program_activates_it() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.use_program("v1").unwrap();

        let list = mgr.list();
        assert_eq!(list[0].1, VclProgramState::Active);
        assert_eq!(mgr.active_name(), Some("v1".to_string()));
    }

    #[test]
    fn use_program_deactivates_previous() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.load("v2", MINIMAL_VCL_2).unwrap();

        mgr.use_program("v1").unwrap();
        mgr.use_program("v2").unwrap();

        let list = mgr.list();
        for (name, state, _) in &list {
            if name == "v1" {
                assert_eq!(*state, VclProgramState::Available);
            } else if name == "v2" {
                assert_eq!(*state, VclProgramState::Active);
            }
        }
    }

    #[test]
    fn use_nonexistent_program_returns_error() {
        let mgr = VclManager::new();
        let result = mgr.use_program("nope");
        assert!(result.is_err());
    }

    #[test]
    fn discard_available_program() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.discard("v1").unwrap();
        assert!(mgr.list().is_empty());
    }

    #[test]
    fn discard_active_program_returns_error() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.use_program("v1").unwrap();

        let result = mgr.discard("v1");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("active"));
    }

    #[test]
    fn discard_nonexistent_program_returns_error() {
        let mgr = VclManager::new();
        let result = mgr.discard("nope");
        assert!(result.is_err());
    }

    #[test]
    fn load_over_available_program_replaces_it() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.load("v1", MINIMAL_VCL_2).unwrap();
        assert_eq!(mgr.list().len(), 1);
    }

    #[test]
    fn load_over_active_program_returns_error() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.use_program("v1").unwrap();

        let result = mgr.load("v1", MINIMAL_VCL_2);
        assert!(result.is_err());
    }

    #[test]
    fn active_interpreter_returns_none_when_empty() {
        let mgr = VclManager::new();
        assert!(mgr.active_interpreter().is_none());
    }

    #[test]
    fn active_interpreter_returns_some_after_use() {
        let mgr = VclManager::new();
        mgr.load("v1", MINIMAL_VCL).unwrap();
        mgr.use_program("v1").unwrap();
        assert!(mgr.active_interpreter().is_some());
    }
}
