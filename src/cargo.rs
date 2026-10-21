use crate::error::Error;
use cargo_metadata::MetadataCommand;
use std::path::{Path, PathBuf};

/// What `cargo metadata` says about the workspace.
pub struct Workspace {
	/// Where cargo puts build artifacts (honours `CARGO_TARGET_DIR` and `build.target-dir`).
	pub target_directory: PathBuf,
	packages: Vec<Package>,
}

struct Package {
	name: String,
	manifest_path: PathBuf,
	packaged: bool,
}

impl Workspace {
	pub fn load(manifest_path: Option<&Path>) -> Result<Workspace, Error> {
		let mut cmd = MetadataCommand::new();
		cmd.no_deps();
		if let Some(p) = manifest_path {
			cmd.manifest_path(p);
		}
		let meta = cmd
			.exec()
			.map_err(|e| Error::Config(format!("cargo metadata failed: {e}")))?;
		Ok(Workspace {
			target_directory: meta.target_directory.into_std_path_buf(),
			packages: meta
				.packages
				.into_iter()
				.map(|p| Package {
					packaged: p.metadata.get(crate::config::METADATA_KEY).is_some(),
					name: p.name.to_string(),
					manifest_path: p.manifest_path.into_std_path_buf(),
				})
				.collect(),
		})
	}

	/// `-p` takes a package name or a path to its directory (or `Cargo.toml`).
	fn by_name_or_path(&self, name: &str) -> Result<PathBuf, Error> {
		if let Some(p) = self.packages.iter().find(|p| p.name == name) {
			return Ok(p.manifest_path.clone());
		}
		let path = Path::new(name).canonicalize().ok();
		path.and_then(|path| {
			self.packages.iter().find(|p| {
				p.manifest_path == path || p.manifest_path.parent() == Some(path.as_path())
			})
		})
		.map(|p| p.manifest_path.clone())
		.ok_or_else(|| Error::Config(format!("no package `{name}` in the workspace")))
	}

	/// Manifest of the package to pack: `-p NAME_OR_PATH`, otherwise the package in the
	/// current directory (or the one `--manifest-path` points at), otherwise the only
	/// workspace member that has `[package.metadata.generate-macpkg]`.
	pub fn manifest_of(
		&self,
		package: Option<&str>,
		manifest_path: Option<&Path>,
	) -> Result<PathBuf, Error> {
		if let Some(name) = package {
			return self.by_name_or_path(name);
		}
		let wanted = manifest_path
			.map(Path::to_path_buf)
			.unwrap_or_else(|| PathBuf::from("Cargo.toml"))
			.canonicalize()
			.ok();
		if let Some(p) = self
			.packages
			.iter()
			.find(|p| Some(&p.manifest_path) == wanted.as_ref())
		{
			return Ok(p.manifest_path.clone());
		}
		let mut packaged = self.packages.iter().filter(|p| p.packaged);
		match (packaged.next(), packaged.next()) {
			(Some(p), None) => Ok(p.manifest_path.clone()),
			(None, _) => Err(Error::Config(format!(
				"no package has [package.metadata.{}]",
				crate::config::METADATA_KEY
			))),
			(Some(a), Some(b)) => {
				let names: Vec<_> = [a, b]
					.into_iter()
					.chain(packaged)
					.map(|p| p.name.as_str())
					.collect();
				Err(Error::Config(format!(
					"more packages can be packed ({}), choose one with -p",
					names.join(", ")
				)))
			}
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn pkg(name: &str, packaged: bool) -> Package {
		Package {
			name: name.into(),
			manifest_path: format!("/w/{name}/Cargo.toml").into(),
			packaged,
		}
	}

	fn ws(packages: Vec<Package>) -> Workspace {
		Workspace {
			target_directory: "/w/target".into(),
			packages,
		}
	}

	fn sample() -> Workspace {
		ws(vec![pkg("a", true), pkg("b", false), pkg("c", true)])
	}

	#[test]
	fn picks_by_name() {
		let w = sample();
		assert_eq!(
			w.manifest_of(Some("c"), None).unwrap(),
			PathBuf::from("/w/c/Cargo.toml")
		);
		assert!(w.manifest_of(Some("x"), None).is_err());
	}

	#[test]
	fn picks_by_path() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path().canonicalize().unwrap();
		let pkg = root.join("bins/c");
		std::fs::create_dir_all(&pkg).unwrap();
		std::fs::write(pkg.join("Cargo.toml"), "").unwrap();
		let w = ws(vec![Package {
			name: "c".into(),
			manifest_path: pkg.join("Cargo.toml"),
			packaged: false,
		}]);
		let want = pkg.join("Cargo.toml");
		assert_eq!(
			w.manifest_of(Some(pkg.to_str().unwrap()), None).unwrap(),
			want
		);
		assert_eq!(
			w.manifest_of(Some(want.to_str().unwrap()), None).unwrap(),
			want
		);
	}

	#[test]
	fn ambiguous_without_name() {
		let err = sample()
			.manifest_of(None, Some(Path::new("/nonexistent")))
			.unwrap_err();
		assert!(err.to_string().contains("a, c"), "{err}");
	}

	#[test]
	fn single_packaged_member_is_default() {
		let w = ws(vec![pkg("a", true), pkg("b", false)]);
		assert_eq!(
			w.manifest_of(None, Some(Path::new("/nonexistent")))
				.unwrap(),
			PathBuf::from("/w/a/Cargo.toml")
		);
	}
}
