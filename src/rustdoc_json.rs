use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
};

use crate::config::is_lib_like;
use cargo_metadata::{Message, Metadata, Package, PackageId, Target};
use color_eyre::eyre::{Context, Result, bail, eyre};
use rustdoc_types::Crate;
use serde::Deserialize;
use tracing::error_span;

pub struct Options<'a> {
    // metadata
    pub metadata: &'a Metadata,
    pub package: &'a Package,
    pub package_target: &'a Target,

    // flags for cargo
    pub toolchain: Option<&'a str>,
    pub all_features: bool,
    pub no_default_features: bool,
    pub features: &'a mut dyn Iterator<Item = &'a str>,
    pub manifest_path: Option<&'a Path>,
    pub target: Option<&'a str>,
    pub target_dir: Option<&'a Path>,
    pub quiet: bool,
    pub no_deps: bool,

    // flags for rustdoc
    pub document_private_items: bool,

    // process handling
    pub output: CommandOutput,
}

#[derive(Clone, Copy, PartialEq)]
pub enum CommandOutput {
    Inherit,
    Ignore,
    Collect,
}

#[derive(Deserialize)]
struct UnitGraph {
    version: u32,
    roots: Vec<usize>,
    units: Vec<Unit>,
}

#[derive(Deserialize)]
struct Unit {
    pkg_id: PackageId,
    target: Target,
    platform: Option<String>,
    mode: String,
}

/// Generate documentation for the selected library or binary.
///
/// Return discovery errors separately so callers can report process failures first.
pub fn generate(options: Options) -> Result<(Output, Result<PathBuf>)> {
    let Options {
        metadata,
        package,
        package_target,
        toolchain,
        all_features,
        no_default_features,
        features,
        document_private_items,
        manifest_path,
        target,
        target_dir,
        no_deps,
        quiet,
        output: output_option,
    } = options;

    let mut command = Command::new("cargo");

    if let Some(toolchain) = toolchain {
        command.arg(format!("+{toolchain}"));
    }

    command.arg("rustdoc");
    // Cargo must know that the output is JSON so its freshness checks cannot
    // reuse an HTML build instead of running rustdoc.
    command.args([
        "-Z",
        "unstable-options",
        "--output-format",
        "json",
        "--message-format=json-render-diagnostics",
    ]);

    if is_lib_like(package_target) {
        command.arg("--lib");
    } else if package_target.is_bin() {
        command.arg("--bin").arg(&package_target.name);
    } else {
        bail!("target must be lib or bin")
    }

    if quiet {
        command.arg("--quiet");
    }

    command.arg("--color").arg("always");

    if let Some(manifest_path) = manifest_path {
        command.arg("--manifest-path");
        command.arg(manifest_path);
    }

    if let Some(target) = target {
        command.arg("--target");
        command.arg(target);
    }

    if let Some(target_dir) = target_dir {
        command.arg("--target-dir");
        command.arg(target_dir);
    }

    if all_features {
        command.arg("--all-features");
    }

    if no_default_features {
        command.arg("--no-default-features");
    }

    for feature in features {
        command.arg("--features").arg(feature);
    }

    if no_deps {
        command.arg("--no-deps");
    }

    command.arg("--package").arg(&package.id.repr);

    // Resolve targets with the same Cargo invocation, without duplicating its
    // configuration or host-tuple resolution. The graph also lets us reject
    // ambiguous targets before they can overwrite each other's documentation.
    let mut graph_command = Command::new(command.get_program());
    graph_command.args(command.get_args());
    graph_command.arg("--unit-graph");
    let graph_output = run(&mut graph_command, output_option)?;

    let target_dir = target_dir.unwrap_or(metadata.target_directory.as_std_path());
    let context = format!(
        "failed to discover rustdoc JSON for package `{}`, target `{}` ({:?}), \
         compilation target {}, toolchain `{}`, target directory `{}`",
        package.name,
        package_target.name,
        package_target.kind,
        target.unwrap_or("inherited from Cargo configuration/environment (or the host default)"),
        toolchain.unwrap_or("inherited"),
        target_dir.display(),
    );
    let directory = documentation_directory(&graph_output, package, package_target, target_dir)
        .and_then(|directory| {
            // With a separate build-dir, the supported Cargo can reuse cached
            // JSON before creating a new target-dir's doc directory. Create the
            // selected directory so Cargo can place its artifact there.
            fs::create_dir_all(&directory).wrap_err_with(|| {
                format!("failed to create rustdoc output directory `{}`", directory.display())
            })?;
            Ok(directory)
        })
        .wrap_err_with(|| format!("{context}; command: {graph_command:?}"));
    let directory = match directory {
        Ok(directory) => directory,
        Err(error) => return Ok((graph_output, Err(error))),
    };

    if document_private_items {
        command.args(["--", "--document-private-items"]);
    }

    let output = run(&mut command, output_option)?;
    let path = generated_artifact_path(&output, package, package_target).wrap_err_with(|| {
        format!(
            "{context}; documentation directory `{}`; command: {command:?}",
            directory.display()
        )
    });

    Ok((output, path))
}

fn run(command: &mut Command, output: CommandOutput) -> Result<Output> {
    command.stderr(match output {
        CommandOutput::Inherit => Stdio::inherit(),
        CommandOutput::Ignore => Stdio::null(),
        CommandOutput::Collect => Stdio::piped(),
    });

    // Stdout contains Cargo's machine-readable graph or artifact messages;
    // json-render-diagnostics keeps compiler diagnostics on stderr.
    command.output().wrap_err_with(|| format!("failed to run {command:?}"))
}

fn documentation_directory(
    output: &Output,
    package: &Package,
    target: &Target,
    target_dir: &Path,
) -> Result<PathBuf> {
    if !output.status.success() {
        bail!("Cargo's rustdoc unit graph command failed with {}", output.status);
    }

    let graph: UnitGraph =
        serde_json::from_slice(&output.stdout).wrap_err("failed to parse Cargo's unit graph")?;
    if graph.version != 1 {
        bail!(
            "unsupported Cargo unit graph version {}; use a supported nightly toolchain",
            graph.version
        );
    }

    let mut directories = Vec::new();
    for root in graph.roots {
        let unit =
            graph.units.get(root).ok_or_else(|| eyre!("invalid Cargo unit graph root {root}"))?;
        if unit.mode != "doc"
            || unit.pkg_id != package.id
            || unit.target.name != target.name
            || unit.target.kind != target.kind
            || unit.target.src_path != target.src_path
        {
            continue;
        }

        let mut path = target_dir.to_path_buf();
        if let Some(platform) = &unit.platform {
            if platform == "host-tuple" || platform.is_empty() {
                bail!("Cargo did not resolve the compilation target `{platform}`");
            }
            // Cargo names custom-target directories after the JSON file's stem.
            let directory = if platform.ends_with(".json") {
                Path::new(platform)
                    .file_stem()
                    .ok_or_else(|| eyre!("invalid custom target `{platform}`"))?
            } else {
                platform.as_ref()
            };
            path.push(directory);
        }
        path.push("doc");
        directories.push((unit.platform.as_deref(), path));
    }

    // Count compilation units, since custom targets can share an output path.
    if directories.len() > 1 {
        bail!(
            "Cargo selected multiple compilation targets and documentation directories: {directories:?}; \
             select one compilation target with --target"
        );
    }

    directories.into_iter().next().map(|(_, path)| path).ok_or_else(|| {
        eyre!("Cargo did not select a rustdoc unit for the requested package target")
    })
}

fn generated_artifact_path(output: &Output, package: &Package, target: &Target) -> Result<PathBuf> {
    if !output.status.success() {
        bail!("Cargo rustdoc failed with {}", output.status);
    }

    let mut paths = Vec::new();
    for message in Message::parse_stream(output.stdout.as_slice()) {
        if let Message::CompilerArtifact(artifact) = message?
            && artifact.package_id == package.id
            && artifact.target.name == target.name
            && artifact.target.kind == target.kind
            && artifact.target.src_path == target.src_path
        {
            paths.extend(
                artifact.filenames.into_iter().filter(|path| path.extension() == Some("json")),
            );
        }
    }

    if paths.len() != 1 {
        bail!(
            "expected one rustdoc JSON artifact from Cargo for the selected package target, got {paths:?}"
        );
    }
    let path = paths.remove(0).into_std_path_buf();
    if !path.is_file() {
        bail!("rustdoc JSON is missing at Cargo's reported artifact path `{}`", path.display());
    }
    Ok(path)
}

pub fn parse(rustdoc_json: &str, toolchain: &str) -> Result<Crate> {
    #[derive(Deserialize)]
    struct CrateWithJustTheFormatVersion {
        format_version: u32,
    }

    let krate: CrateWithJustTheFormatVersion =
        serde_json::from_str(rustdoc_json).wrap_err("failed to parse generated rustdoc json")?;

    if krate.format_version != rustdoc_types::FORMAT_VERSION {
        let expected = rustdoc_types::FORMAT_VERSION;
        let actual = krate.format_version;

        let _span = error_span!("",
            %toolchain,
            expected = format!("rustdoc json version {expected}"),
            actual = format!("rustdoc json version {actual}"),
        )
        .entered();

        bail!("the chosen rust toolchain is not compatible");
    }

    serde_json::from_str(rustdoc_json).wrap_err("failed to parse generated rustdoc json")
}
