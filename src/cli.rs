use clap::{Parser, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;

/// How the package is put together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Tool {
	/// `pkgbuild` if it exists (macOS), otherwise the built-in writer
	#[default]
	Auto,
	/// Apple's `pkgbuild` (macOS only)
	Pkgbuild,
	/// The built-in writer of the package; needs no other program, works everywhere
	Builtin,
}

#[derive(Parser, Debug, Default)]
#[command(
	name = "cargo-generate-macpkg",
	bin_name = "cargo generate-macpkg",
	version,
	about
)]
pub struct Cli {
	/// Path to Cargo.toml
	#[arg(long, value_name = "PATH")]
	pub manifest_path: Option<PathBuf>,

	/// Package to pack, by name or by path to its directory (default: the package in the
	/// current directory, or the only workspace member with [package.metadata.generate-macpkg])
	#[arg(short, long, value_name = "NAME_OR_PATH")]
	pub package: Option<String>,

	/// Build target triple the binaries were built for (e.g. aarch64-apple-darwin)
	#[arg(long)]
	pub target: Option<String>,

	/// Cargo profile the binaries were built with
	#[arg(long, default_value = "release")]
	pub profile: String,

	/// Output file or directory (default: target/generate-macpkg/)
	#[arg(short, long)]
	pub output: Option<PathBuf>,

	/// How to assemble the package
	#[arg(long, value_enum, default_value_t)]
	pub tool: Tool,

	/// Path to `pkgbuild` (default: found in PATH)
	#[arg(long, value_name = "PATH")]
	pub pkgbuild: Option<PathBuf>,
}

impl Cli {
	/// Cargo invokes external subcommands as `cargo-generate-macpkg generate-macpkg ...`.
	pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> Cli {
		let mut args: Vec<OsString> = args.into_iter().collect();
		if args.get(1).is_some_and(|a| a == "generate-macpkg") {
			args.remove(1);
		}
		Cli::parse_from(args)
	}
}
