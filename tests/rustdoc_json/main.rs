use std::{
    env,
    ffi::OsStr,
    fs,
    path::Path,
    process::{Command, Output},
};

const BINARY: &str = env!("CARGO_BIN_EXE_cargo-insert-docs");

#[test]
fn without_compilation_target() {
    check_extraction(None);
}

#[test]
fn configured_host_tuple() {
    check_extraction(Some("host-tuple"));
}

fn check_extraction(target: Option<&str>) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::create_dir(root.join("cargo-home")).unwrap();
    fs::create_dir(root.join("src")).unwrap();
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = 'rustdoc-target-fixture'\nversion = '0.1.0'\nedition = '2024'\n[workspace]\n",
    )
    .unwrap();
    fs::write(root.join("src/lib.rs"), "//! Documentation from the target fixture.\n").unwrap();
    fs::write(
        root.join("README.md"),
        "<!-- crate documentation start -->\nDocumentation from the target fixture.\n<!-- crate documentation end -->\n",
    )
    .unwrap();
    let config = target.map(|target| format!("[build]\ntarget = '{target}'\n")).unwrap_or_default();
    fs::write(root.join("cargo-home/config.toml"), config).unwrap();

    let target_dir = root.join("target");
    let mut expected = target_dir.join("insert-docs");
    if target.is_some() {
        let toolchain = run(command(&root, BINARY).arg("--print-supported-toolchain"));
        let toolchain = String::from_utf8(toolchain.stdout).unwrap();
        let version = run(command(&root, "rustc").arg(format!("+{}", toolchain.trim())).arg("-vV"));
        let version = String::from_utf8(version.stdout).unwrap();
        let host = version.lines().find_map(|line| line.strip_prefix("host: ")).unwrap();
        expected.push(host);
    }
    expected.push("doc/rustdoc_target_fixture.json");

    run(command(&root, BINARY).arg("--manifest-path").arg(root.join("Cargo.toml")).args([
        "--check",
        "--quiet-cargo",
        "crate-into-readme",
    ]));

    assert!(expected.is_file(), "rustdoc JSON missing at {}", expected.display());
    if target.is_some() {
        assert!(!target_dir.join("insert-docs/doc/rustdoc_target_fixture.json").exists());
        assert!(!target_dir.join("insert-docs/host-tuple").exists());
    }
}

fn command(root: &Path, program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    // Cargo searches from its working directory, even with --manifest-path.
    // Starting at the filesystem root prevents it from loading configuration
    // from the checkout or the user's home, including on Windows.
    command.current_dir(root.ancestors().last().unwrap());
    for (key, _) in env::vars_os() {
        if key.to_str().is_some_and(|key| {
            key.starts_with("CARGO_")
                || matches!(
                    key,
                    "RUSTC"
                        | "RUSTDOC"
                        | "RUSTC_WRAPPER"
                        | "RUSTC_WORKSPACE_WRAPPER"
                        | "RUSTFLAGS"
                        | "RUSTDOCFLAGS"
                )
        }) {
            command.env_remove(key);
        }
    }
    command
        .env("CARGO_HOME", root.join("cargo-home"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env("CARGO_BUILD_BUILD_DIR", root.join("build"))
        .env("CARGO_NET_OFFLINE", "true");
    command
}

fn run(command: &mut Command) -> Output {
    let output =
        command.output().unwrap_or_else(|error| panic!("failed to run {command:?}: {error}"));
    assert!(
        output.status.success(),
        "{command:?} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}
