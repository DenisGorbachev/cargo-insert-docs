use std::{
    env,
    ffi::OsStr,
    fs,
    path::PathBuf,
    process::{Command, Output},
};

use indoc::indoc;
use tempfile::TempDir;

const BINARY: &str = env!("CARGO_BIN_EXE_cargo-insert-docs");

#[test]
fn without_compilation_target() {
    let fixture = Fixture::new("");
    fixture.check_readme();

    let expected = fixture.root.join("target/insert-docs/doc/rustdoc_target_fixture.json");
    assert!(expected.is_file(), "rustdoc JSON missing at {}", expected.display());
}

#[test]
fn configured_host_tuple() {
    let fixture = Fixture::new("[build]\ntarget = 'host-tuple'\n");
    let host = fixture.host_triple();
    fixture.check_readme();

    let output_dir = fixture.root.join("target/insert-docs");
    let expected = output_dir.join(host).join("doc/rustdoc_target_fixture.json");
    assert!(expected.is_file(), "rustdoc JSON missing at {}", expected.display());
    assert!(!output_dir.join("doc/rustdoc_target_fixture.json").exists());
    assert!(!output_dir.join("host-tuple").exists());
}

struct Fixture {
    _temp_dir: TempDir,
    root: PathBuf,
}

impl Fixture {
    fn new(config: &str) -> Self {
        let temp_dir = tempfile::tempdir().unwrap();
        let root = temp_dir.path().canonicalize().unwrap();
        fs::create_dir(root.join("cargo-home")).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            indoc! {"
                [package]
                name = 'rustdoc-target-fixture'
                version = '0.1.0'
                edition = '2024'
                [workspace]
            "},
        )
        .unwrap();
        fs::write(root.join("src/lib.rs"), "//! Documentation from the target fixture.\n").unwrap();
        fs::write(
            root.join("README.md"),
            indoc! {"
                <!-- crate documentation start -->
                Documentation from the target fixture.
                <!-- crate documentation end -->
            "},
        )
        .unwrap();
        fs::write(root.join("cargo-home/config.toml"), config).unwrap();
        Self { _temp_dir: temp_dir, root }
    }

    fn check_readme(&self) {
        run(self.command(BINARY).arg("--manifest-path").arg(self.root.join("Cargo.toml")).args([
            "--check",
            "--quiet-cargo",
            "crate-into-readme",
        ]));
    }

    fn host_triple(&self) -> String {
        let toolchain = run(self.command(BINARY).arg("--print-supported-toolchain"));
        let toolchain = String::from_utf8(toolchain.stdout).unwrap();
        let version = run(self.command("rustc").arg(format!("+{}", toolchain.trim())).arg("-vV"));
        let version = String::from_utf8(version.stdout).unwrap();
        version.lines().find_map(|line| line.strip_prefix("host: ")).unwrap().to_owned()
    }

    fn command(&self, program: impl AsRef<OsStr>) -> Command {
        let mut command = Command::new(program);
        // Cargo searches from its working directory, even with --manifest-path.
        // Starting at the filesystem root prevents it from loading configuration
        // from the checkout or the user's home, including on Windows.
        command.current_dir(self.root.ancestors().last().unwrap());
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
            .env("CARGO_HOME", self.root.join("cargo-home"))
            .env("CARGO_TARGET_DIR", self.root.join("target"))
            .env("CARGO_BUILD_BUILD_DIR", self.root.join("build"))
            .env("CARGO_NET_OFFLINE", "true");
        command
    }
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
