use crate::error::Error;
use cargo_toml::{Manifest, Value};
use std::path::{Path, PathBuf};

pub(crate) const METADATA_KEY: &str = "generate-macpkg";

#[derive(Debug, PartialEq)]
pub struct Asset {
	pub source: String,
	pub dest: String,
	pub mode: u32,
}

#[derive(Debug)]
pub struct Config {
	pub project_dir: PathBuf,
	/// Used for the file name of the package.
	pub name: String,
	pub version: String,
	/// Reverse-DNS identifier of the package, e.g. `org.example.tool`.
	pub identifier: String,
	/// Where the payload is rooted on the target volume.
	pub install_location: String,
	/// Directory with `preinstall`, `postinstall` and helper files for the installer.
	pub scripts: Option<PathBuf>,
	pub assets: Vec<Asset>,
}

fn str_field(table: &Value, key: &str) -> Result<Option<String>, Error> {
	match table.get(key) {
		None => Ok(None),
		Some(Value::String(s)) => Ok(Some(s.clone())),
		Some(_) => Err(Error::Config(format!(
			"{METADATA_KEY}.{key} must be a string"
		))),
	}
}

fn parse_mode(s: &str) -> Result<u32, Error> {
	u32::from_str_radix(s, 8)
		.map_err(|_| Error::Config(format!("invalid mode {s:?}, expected octal like \"0755\"")))
}

fn parse_assets(table: &Value) -> Result<Vec<Asset>, Error> {
	let items = match table.get("assets") {
		Some(Value::Array(a)) => a,
		_ => {
			return Err(Error::Config(format!(
				"{METADATA_KEY}.assets is mandatory and must be an array"
			)));
		}
	};
	items
		.iter()
		.map(|item| {
			let source = str_field(item, "source")?
				.ok_or_else(|| Error::Config("asset without `source`".into()))?;
			let dest = str_field(item, "dest")?
				.ok_or_else(|| Error::Config(format!("asset {source:?} without `dest`")))?;
			let mode = str_field(item, "mode")?.map_or(Ok(0o644), |m| parse_mode(&m))?;
			Ok(Asset { source, dest, mode })
		})
		.collect()
}

/// `org.example.tool`: letters, digits, `-` and `.`, at least two parts.
fn valid_identifier(id: &str) -> bool {
	id.contains('.')
		&& !id.starts_with('.')
		&& !id.ends_with('.')
		&& id
			.chars()
			.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

impl Config {
	pub fn new(manifest_path: &Path) -> Result<Config, Error> {
		let manifest_path = &manifest_path
			.canonicalize()
			.map_err(|e| Error::FileIo(manifest_path.to_path_buf(), e))?;
		let manifest = Manifest::<Value>::from_path(manifest_path)?;
		let package = manifest
			.package
			.as_ref()
			.ok_or_else(|| Error::Config("Cargo.toml has no [package]".into()))?;
		let meta = package
			.metadata
			.as_ref()
			.and_then(|m| m.get(METADATA_KEY))
			.ok_or_else(|| {
				Error::Config(format!(
					"missing [package.metadata.{METADATA_KEY}] in Cargo.toml"
				))
			})?;

		let name = str_field(meta, "name")?.unwrap_or_else(|| package.name().to_string());
		let version = str_field(meta, "version")?.unwrap_or_else(|| package.version().to_string());
		let identifier = str_field(meta, "identifier")?.ok_or_else(|| {
			Error::Config(format!(
				"{METADATA_KEY}.identifier is mandatory, e.g. \"org.example.{name}\""
			))
		})?;
		if !valid_identifier(&identifier) {
			return Err(Error::Config(format!(
				"invalid identifier {identifier:?}, expected reverse-DNS like \"org.example.tool\""
			)));
		}
		let install_location = str_field(meta, "install_location")?.unwrap_or_else(|| "/".into());
		if !install_location.starts_with('/') {
			return Err(Error::Config(format!(
				"{METADATA_KEY}.install_location must be an absolute path"
			)));
		}
		let project_dir = match manifest_path.parent() {
			Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
			_ => PathBuf::from("."),
		};
		Ok(Config {
			project_dir,
			scripts: str_field(meta, "scripts")?.map(PathBuf::from),
			assets: parse_assets(meta)?,
			name,
			version,
			identifier,
			install_location,
		})
	}
}

#[cfg(test)]
pub(crate) mod tests {
	use super::*;

	pub fn write_manifest(dir: &Path, metadata: &str) -> PathBuf {
		std::fs::create_dir_all(dir.join("src")).unwrap();
		std::fs::write(dir.join("src/main.rs"), "fn main() {}").unwrap();
		let path = dir.join("Cargo.toml");
		std::fs::write(
			&path,
			format!(
				"[package]\nname = \"demo\"\nversion = \"1.2.3\"\nedition = \"2024\"\n\
				 [package.metadata.generate-macpkg]\n{metadata}"
			),
		)
		.unwrap();
		path
	}

	#[test]
	fn defaults_come_from_package() {
		let dir = tempfile::tempdir().unwrap();
		let path = write_manifest(
			dir.path(),
			"identifier = \"org.example.demo\"\n\
			 assets = [{ source = \"a\", dest = \"/usr/local/bin/a\", mode = \"0755\" }]\n",
		);
		let c = Config::new(&path).unwrap();
		assert_eq!((c.name.as_str(), c.version.as_str()), ("demo", "1.2.3"));
		assert_eq!(c.identifier, "org.example.demo");
		assert_eq!(c.install_location, "/");
		assert_eq!(
			c.assets,
			vec![Asset {
				source: "a".into(),
				dest: "/usr/local/bin/a".into(),
				mode: 0o755
			}]
		);
	}

	#[test]
	fn identifier_is_mandatory_and_validated() {
		let dir = tempfile::tempdir().unwrap();
		let path = write_manifest(dir.path(), "assets = []\n");
		assert!(matches!(Config::new(&path), Err(Error::Config(m)) if m.contains("identifier")));
		let path = write_manifest(dir.path(), "identifier = \"demo\"\nassets = []\n");
		assert!(
			matches!(Config::new(&path), Err(Error::Config(m)) if m.contains("invalid identifier"))
		);
	}

	#[test]
	fn install_location_must_be_absolute() {
		let dir = tempfile::tempdir().unwrap();
		let path = write_manifest(
			dir.path(),
			"identifier = \"org.example.demo\"\ninstall_location = \"usr\"\nassets = []\n",
		);
		assert!(matches!(Config::new(&path), Err(Error::Config(m)) if m.contains("absolute")));
	}

	#[test]
	fn assets_are_mandatory() {
		let dir = tempfile::tempdir().unwrap();
		let path = write_manifest(dir.path(), "identifier = \"org.example.demo\"\n");
		assert!(matches!(Config::new(&path), Err(Error::Config(m)) if m.contains("assets")));
	}
}
