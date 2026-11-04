//! Private factory integration. No Cargo or Godot invocation and no compiler provenance claim.
use super::*;
use crate::candidate_workspace::Workspace;
use crate::native_operations::NativeRuntime;
use std::path::{Component, Prefix};

struct Fixture {
    root: PathBuf,
    workspace: Workspace,
}
impl Fixture {
    fn new() -> Self {
        let temp = std::env::temp_dir().canonicalize().unwrap();
        assert!(
            matches!(temp.components().next(), Some(Component::Prefix(p))
            if matches!(p.kind(), Prefix::Disk(d) | Prefix::VerbatimDisk(d)
                if d.eq_ignore_ascii_case(&crate::test_drive().as_bytes()[0])))
        );
        let root = temp.join(format!(
            "aurum-command-factory-{}",
            crate::random::session_id()
        ));
        std::fs::create_dir(&root).unwrap();
        let workspace = Workspace::for_test_project(&root).unwrap();
        Self { root, workspace }
    }
    fn command(&self, owner: &CompilerOwner<'_>) -> Command {
        owner
            .command(
                std::env::current_exe().unwrap(),
                &self.workspace.path,
                &self.workspace.lifetime,
            )
            .unwrap()
    }
    fn clean(&mut self) {
        assert!(!self.workspace.lifetime.unresolved());
        self.workspace.clean().unwrap();
    }
}

#[test]
fn factory_commands_and_clones_bind_exact_runtime_parent() {
    let mut f = Fixture::new();
    let mut other = Fixture::new();
    let runtime = NativeRuntime::controller(&f.root);
    let foreign = NativeRuntime::controller(&other.root);
    let owner = CompilerOwner::acquire(Some(&runtime), &f.root).unwrap();
    let base = f.command(&owner);
    for command in [base.clone().args(["metadata"]), base.args(["build"])] {
        let controller = command
            .native_output_controller_for_test()
            .expect("factory must select checked capture");
        runtime
            .check_managed_native_output(&f.root, controller)
            .unwrap();
        assert!(foreign
            .check_managed_native_output(&other.root, controller)
            .is_err());
    }
    drop(owner);
    runtime.close();
    foreign.close();
    f.clean();
    other.clean();
}

#[test]
fn missing_foreign_and_unbound_compiler_factories_refuse() {
    let mut f = Fixture::new();
    let mut other = Fixture::new();
    let runtime = NativeRuntime::controller(&f.root);
    assert!(CompilerOwner::acquire(None, &f.root).is_err());
    assert!(CompilerOwner::acquire(Some(&runtime), &other.root).is_err());
    let unbound = CompilerOwner {
        runtime: &runtime,
        root: f.root.clone(),
        controller: crate::worker_windows::StreamController::for_runtime(),
    };
    assert!(unbound
        .command(
            std::env::current_exe().unwrap(),
            &f.workspace.path,
            &f.workspace.lifetime
        )
        .is_err());
    assert!(!f.workspace.lifetime.unresolved());
    drop(unbound);
    runtime.close();
    f.clean();
    other.clean();
}

#[test]
fn runtime_close_refuses_factory_and_prepared_command_without_admission() {
    let mut f = Fixture::new();
    let runtime = NativeRuntime::controller(&f.root);
    let owner = CompilerOwner::acquire(Some(&runtime), &f.root).unwrap();
    let command = f.command(&owner);
    let controller = command.native_output_controller_for_test().unwrap().clone();
    runtime.close();
    assert!(owner
        .command(
            std::env::current_exe().unwrap(),
            &f.workspace.path,
            &f.workspace.lifetime
        )
        .is_err());
    let error = command.run(Duration::from_secs(10)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(controller.retained(), 0);
    assert!(!f.workspace.lifetime.unresolved());
    // Admission refusal is not a native Job no-spawn or retirement receipt.
    drop(owner);
    f.clean();
}
