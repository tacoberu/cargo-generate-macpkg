use crate::cli::Cli;
use std::path::{Path, PathBuf};

/// Where cargo put the build artifacts.
pub struct BuildTarget {
	target: Option<String>,
	profile: String,
	base: PathBuf,
}

impl BuildTarget {
	/// `base` is cargo's target directory.
	pub fn new(args: &Cli, base: &Path) -> Self {
		let base = base.to_path_buf();
		BuildTarget {
			target: args.target.clone(),
			profile: args.profile.clone(),
			base,
		}
	}

	/// Directory with built binaries, e.g. `target/aarch64-unknown-linux-musl/release`.
	pub fn artifact_dir(&self) -> PathBuf {
		let profile_dir = if self.profile == "dev" {
			"debug"
		} else {
			&self.profile
		};
		match &self.target {
			Some(t) => self.base.join(t).join(profile_dir),
			None => self.base.join(profile_dir),
		}
	}

	/// Architecture part of the file name: `arm64`, `x86_64`, `universal`, or the first part of
	/// another triple; `any` without `--target`.
	pub fn arch_label(&self) -> String {
		match self.target.as_deref().and_then(|t| t.split('-').next()) {
			None => "any".into(),
			Some("aarch64") | Some("arm64") => "arm64".into(),
			Some(other) => other.into(),
		}
	}

	pub fn output_dir(&self) -> PathBuf {
		self.base.join("generate-macpkg")
	}

	/// `target/release/x` in an asset source means the artifact directory of this build.
	pub fn resolve(&self, project_dir: &Path, source: &str) -> PathBuf {
		match source.strip_prefix("target/release/") {
			Some(rest) => self.artifact_dir().join(rest),
			None => project_dir.join(source),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn target(triple: Option<&str>, profile: &str) -> BuildTarget {
		BuildTarget {
			target: triple.map(String::from),
			profile: profile.into(),
			base: PathBuf::from("/p/target"),
		}
	}

	#[test]
	fn resolves_release_prefix() {
		let t = target(Some("aarch64-unknown-linux-musl"), "release");
		assert_eq!(
			t.resolve(Path::new("/p"), "target/release/x"),
			PathBuf::from("/p/target/aarch64-unknown-linux-musl/release/x")
		);
		assert_eq!(
			t.resolve(Path::new("/p"), "README.md"),
			PathBuf::from("/p/README.md")
		);
	}

	#[test]
	fn arch_label_follows_the_target() {
		assert_eq!(
			target(Some("aarch64-apple-darwin"), "release").arch_label(),
			"arm64"
		);
		assert_eq!(
			target(Some("x86_64-apple-darwin"), "release").arch_label(),
			"x86_64"
		);
		assert_eq!(
			target(Some("universal2-apple-darwin"), "release").arch_label(),
			"universal2"
		);
		assert_eq!(target(None, "release").arch_label(), "any");
	}

	#[test]
	fn dev_profile_is_debug_dir() {
		assert_eq!(
			target(None, "dev").artifact_dir(),
			PathBuf::from("/p/target/debug")
		);
	}
}
