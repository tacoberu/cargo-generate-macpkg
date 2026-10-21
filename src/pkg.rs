use crate::bom;
use crate::build_target::BuildTarget;
use crate::cli::{Cli, Tool};
use crate::config::{Asset, Config};
use crate::error::Error;
use crate::xar;
use flate2::{Compression, write::GzEncoder};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

/// Group `wheel`, the group of root-owned files on macOS.
const WHEEL_GID: u32 = 80;

fn mtime() -> u64 {
	std::env::var("SOURCE_DATE_EPOCH")
		.ok()
		.and_then(|s| s.parse().ok())
		.unwrap_or(0)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Node {
	Dir,
	File(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
	pub node: Node,
	pub mode: u32,
}

/// Files of a package, keyed by path relative to the root (`usr/local/bin/tool`, no leading `/`).
/// Every file has its parent directories in the map.
pub type Tree = BTreeMap<String, Entry>;

pub fn file_name(config: &Config, target: &BuildTarget) -> String {
	format!(
		"{}-{}-{}.pkg",
		config.name,
		config.version,
		target.arch_label()
	)
}

/// Normalised relative path of an asset (`/usr/local/bin/x` -> `usr/local/bin/x`).
fn relative(dest: &str) -> Result<String, Error> {
	let mut parts = Vec::new();
	for c in Path::new(dest.trim_start_matches('/')).components() {
		match c {
			Component::Normal(p) => parts.push(p.to_string_lossy().into_owned()),
			Component::CurDir => {}
			_ => {
				return Err(Error::Config(format!(
					"dest {dest:?} must not contain `..`"
				)));
			}
		}
	}
	if parts.is_empty() {
		return Err(Error::Config(format!("dest {dest:?} is empty")));
	}
	Ok(parts.join("/"))
}

fn add_parents(tree: &mut Tree, path: &str) {
	let mut at = 0;
	while let Some(i) = path[at..].find('/') {
		at += i;
		tree.entry(path[..at].to_string()).or_insert(Entry {
			node: Node::Dir,
			mode: 0o755,
		});
		at += 1;
	}
}

/// Files an asset stands for, as (path in the package, source file).
fn expand_asset(
	config: &Config,
	target: &BuildTarget,
	asset: &Asset,
) -> Result<Vec<(String, PathBuf)>, Error> {
	let source = target.resolve(&config.project_dir, &asset.source);
	let into_dir = asset.dest.ends_with('/');
	if !asset.source.contains('*') {
		let name = source
			.file_name()
			.map(|n| n.to_string_lossy().into_owned())
			.unwrap_or_default();
		let dest = if into_dir {
			format!("{}{name}", asset.dest)
		} else {
			asset.dest.clone()
		};
		return Ok(vec![(relative(&dest)?, source)]);
	}
	if !into_dir {
		return Err(Error::Config(format!(
			"dest of {:?} must be a directory ending with `/`",
			asset.source
		)));
	}
	let mut files = Vec::new();
	for path in glob::glob(&source.to_string_lossy())? {
		let path = path.map_err(|e| Error::FileIo(e.path().to_path_buf(), e.into()))?;
		if path.is_file() {
			let name = path.file_name().unwrap().to_string_lossy();
			files.push((relative(&format!("{}{name}", asset.dest))?, path));
		}
	}
	if files.is_empty() {
		return Err(Error::Config(format!("no file matches {:?}", asset.source)));
	}
	Ok(files)
}

pub fn payload_tree(config: &Config, target: &BuildTarget) -> Result<Tree, Error> {
	let mut tree = Tree::new();
	for asset in &config.assets {
		for (path, source) in expand_asset(config, target, asset)? {
			let data = fs::read(&source).map_err(|e| Error::FileIo(source.clone(), e))?;
			add_parents(&mut tree, &path);
			let entry = Entry {
				node: Node::File(data),
				mode: asset.mode,
			};
			if tree.insert(path.clone(), entry).is_some() {
				return Err(Error::Config(format!("{path:?} is installed twice")));
			}
		}
	}
	if tree.is_empty() {
		return Err(Error::Config("the package has no files".into()));
	}
	Ok(tree)
}

/// Installer scripts (`preinstall`, `postinstall`, helpers): all executable.
pub fn scripts_tree(dir: &Path) -> Result<Tree, Error> {
	fn walk(tree: &mut Tree, prefix: &str, dir: &Path) -> Result<(), Error> {
		let mut entries: Vec<_> = fs::read_dir(dir)
			.map_err(|e| Error::FileIo(dir.to_path_buf(), e))?
			.collect::<Result<_, _>>()?;
		entries.sort_by_key(|e| e.file_name());
		for e in entries {
			let name = e.file_name().to_string_lossy().into_owned();
			let path = if prefix.is_empty() {
				name
			} else {
				format!("{prefix}/{name}")
			};
			if e.file_type()?.is_dir() {
				tree.insert(
					path.clone(),
					Entry {
						node: Node::Dir,
						mode: 0o755,
					},
				);
				walk(tree, &path, &e.path())?;
			} else {
				let data = fs::read(e.path()).map_err(|er| Error::FileIo(e.path(), er))?;
				tree.insert(
					path,
					Entry {
						node: Node::File(data),
						mode: 0o755,
					},
				);
			}
		}
		Ok(())
	}
	let mut tree = Tree::new();
	walk(&mut tree, "", dir)?;
	Ok(tree)
}

/// Archive in the `odc` cpio format that the macOS Installer expects: paths start with `./`,
/// everything is owned by `root:wheel`, the order is sorted, so the output is reproducible.
pub fn cpio_odc(tree: &Tree, mtime: u64) -> Vec<u8> {
	let mut out = Vec::new();
	let mut ino = 0u32;
	let mut entry = |name: &str, mode: u32, data: &[u8], nlink: u32| {
		ino += 1;
		let raw = format!("{name}\0");
		let fields: [(u64, usize); 10] = [
			(0, 6),                  // dev
			(ino.into(), 6),         // ino
			(mode.into(), 6),        // mode
			(0, 6),                  // uid
			(WHEEL_GID.into(), 6),   // gid
			(nlink.into(), 6),       // nlink
			(0, 6),                  // rdev
			(mtime, 11),             // mtime
			(raw.len() as u64, 6),   // namesize
			(data.len() as u64, 11), // filesize
		];
		out.extend_from_slice(b"070707");
		for (value, width) in fields {
			out.extend_from_slice(format!("{value:0width$o}").as_bytes());
		}
		out.extend_from_slice(raw.as_bytes());
		out.extend_from_slice(data);
	};
	entry(".", 0o040755, b"", 2);
	for (path, e) in tree {
		let name = format!("./{path}");
		match &e.node {
			Node::Dir => entry(&name, 0o040000 | (e.mode & 0o7777), b"", 2),
			Node::File(data) => entry(&name, 0o100000 | (e.mode & 0o7777), data, 1),
		}
	}
	entry("TRAILER!!!", 0, b"", 1);
	out
}

fn gzip(data: &[u8]) -> Result<Vec<u8>, Error> {
	let mut gz = GzEncoder::new(Vec::new(), Compression::best());
	gz.write_all(data)?;
	Ok(gz.finish()?)
}

fn xml_escape(s: &str) -> String {
	s.replace('&', "&amp;")
		.replace('<', "&lt;")
		.replace('>', "&gt;")
		.replace('"', "&quot;")
}

pub fn package_info(config: &Config, payload: &Tree, scripts: &Tree) -> String {
	let files = payload.len() + 1;
	let kbytes: usize = payload
		.values()
		.map(|e| match &e.node {
			Node::File(d) => d.len(),
			Node::Dir => 0,
		})
		.sum::<usize>()
		/ 1024 + 1;
	let mut script_tags = String::new();
	for name in ["preinstall", "postinstall"] {
		if scripts.contains_key(name) {
			script_tags.push_str(&format!("    <{name} file=\"./{name}\"/>\n"));
		}
	}
	let scripts_xml = if script_tags.is_empty() {
		String::new()
	} else {
		format!("  <scripts>\n{script_tags}  </scripts>\n")
	};
	format!(
		"<?xml version=\"1.0\" encoding=\"utf-8\"?>\n\
		 <pkg-info overwrite-permissions=\"true\" relocatable=\"false\" identifier=\"{}\" \
		 postinstall-action=\"none\" version=\"{}\" format-version=\"2\" install-location=\"{}\" \
		 auth=\"root\">\n  <payload numberOfFiles=\"{files}\" installKBytes=\"{kbytes}\"/>\n\
		 {scripts_xml}  <bundle-version/>\n</pkg-info>\n",
		xml_escape(&config.identifier),
		xml_escape(&config.version),
		xml_escape(&config.install_location),
	)
}

/// Writes a tree to disk with its modes (what `pkgbuild` and `mkbom` read).
#[cfg(unix)]
fn write_tree(root: &Path, tree: &Tree) -> Result<(), Error> {
	use std::os::unix::fs::PermissionsExt;
	fs::create_dir_all(root).map_err(|e| Error::FileIo(root.to_path_buf(), e))?;
	for (path, e) in tree {
		let target = root.join(path);
		match &e.node {
			Node::Dir => fs::create_dir_all(&target),
			Node::File(data) => fs::write(&target, data),
		}
		.map_err(|er| Error::FileIo(target.clone(), er))?;
		fs::set_permissions(&target, fs::Permissions::from_mode(e.mode & 0o7777))
			.map_err(|er| Error::FileIo(target.clone(), er))?;
	}
	Ok(())
}

#[cfg(not(unix))]
fn write_tree(_root: &Path, _tree: &Tree) -> Result<(), Error> {
	Err(Error::Tool(
		"building a package needs a Unix system (macOS or Linux)".into(),
	))
}

/// Programs that can put a package together.
#[derive(Debug, Default)]
pub struct Tools {
	pub pkgbuild: Option<PathBuf>,
}

fn which(name: &str) -> Option<PathBuf> {
	std::env::var_os("PATH").and_then(|paths| {
		std::env::split_paths(&paths)
			.map(|d| d.join(name))
			.find(|p| p.is_file())
	})
}

impl Tools {
	/// From `--pkgbuild`, otherwise from PATH.
	pub fn find(args: &Cli) -> Tools {
		Tools {
			pkgbuild: args.pkgbuild.clone().or_else(|| which("pkgbuild")),
		}
	}
}

fn run(cmd: &mut Command, what: &str) -> Result<(), Error> {
	let status = cmd
		.status()
		.map_err(|e| Error::Tool(format!("cannot run {what}: {e}")))?;
	if status.success() {
		Ok(())
	} else {
		Err(Error::Tool(format!("{what} failed ({status})")))
	}
}

enum Method<'a> {
	Pkgbuild(&'a Path),
	Builtin,
}

fn choose(tool: Tool, tools: &Tools) -> Result<Method<'_>, Error> {
	match tool {
		Tool::Pkgbuild => tools
			.pkgbuild
			.as_deref()
			.map(Method::Pkgbuild)
			.ok_or_else(|| Error::Tool("`pkgbuild` not found (it is part of macOS)".into())),
		Tool::Builtin => Ok(Method::Builtin),
		Tool::Auto => Ok(match &tools.pkgbuild {
			Some(p) => Method::Pkgbuild(p),
			None => Method::Builtin,
		}),
	}
}

/// The flat package written by the tool itself: a `xar` archive of `Bom`, `Payload`, `Scripts`
/// and `PackageInfo`.
fn builtin_package(config: &Config, payload: &Tree, scripts: &Tree) -> Result<Vec<u8>, Error> {
	let t = mtime();
	let bom = bom::build(payload, 0, WHEEL_GID, t.min(u64::from(u32::MAX)) as u32)?;
	let payload_gz = gzip(&cpio_odc(payload, t))?;
	let info = package_info(config, payload, scripts);
	let scripts_gz = if scripts.is_empty() {
		None
	} else {
		Some(gzip(&cpio_odc(scripts, t))?)
	};
	let mut files = vec![
		xar::File {
			name: "Bom",
			data: &bom,
			mode: 0o644,
		},
		xar::File {
			name: "Payload",
			data: &payload_gz,
			mode: 0o644,
		},
		xar::File {
			name: "PackageInfo",
			data: info.as_bytes(),
			mode: 0o644,
		},
	];
	if let Some(data) = &scripts_gz {
		files.push(xar::File {
			name: "Scripts",
			data,
			mode: 0o644,
		});
	}
	xar::build(&files, t)
}

pub fn build(
	config: &Config,
	target: &BuildTarget,
	output: &Path,
	tool: Tool,
	tools: &Tools,
) -> Result<(), Error> {
	let method = choose(tool, tools)?;
	let payload = payload_tree(config, target)?;
	let scripts = match &config.scripts {
		Some(dir) => scripts_tree(&config.project_dir.join(dir))?,
		None => Tree::new(),
	};

	if let Some(parent) = output.parent() {
		fs::create_dir_all(parent).map_err(|e| Error::FileIo(parent.to_path_buf(), e))?;
	}
	let output = std::path::absolute(output)?;
	if output.exists() {
		fs::remove_file(&output).map_err(|e| Error::FileIo(output.clone(), e))?;
	}
	match method {
		Method::Builtin => {
			let bytes = builtin_package(config, &payload, &scripts)?;
			fs::write(&output, bytes).map_err(|e| Error::FileIo(output.clone(), e))
		}
		Method::Pkgbuild(pkgbuild) => {
			let work = tempfile::tempdir()?;
			let root = work.path().join("root");
			write_tree(&root, &payload)?;
			let mut cmd = Command::new(pkgbuild);
			cmd.arg("--root")
				.arg(&root)
				.args(["--identifier", &config.identifier])
				.args(["--version", &config.version])
				.args(["--install-location", &config.install_location]);
			if !scripts.is_empty() {
				let dir = work.path().join("scripts");
				write_tree(&dir, &scripts)?;
				cmd.arg("--scripts").arg(dir);
			}
			run(cmd.arg(&output), "pkgbuild")
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Independent reader of the `odc` format: (name, mode, uid, gid, data).
	fn read_odc(bytes: &[u8]) -> Vec<(String, u32, u32, u32, Vec<u8>)> {
		let num = |b: &[u8]| u64::from_str_radix(std::str::from_utf8(b).unwrap(), 8).unwrap();
		let mut at = 0;
		let mut out = Vec::new();
		loop {
			assert_eq!(&bytes[at..at + 6], b"070707", "magic");
			let h = &bytes[at + 6..at + 76];
			let (mode, uid, gid) = (
				num(&h[12..18]) as u32,
				num(&h[18..24]) as u32,
				num(&h[24..30]) as u32,
			);
			let namesize = num(&h[53..59]) as usize;
			let filesize = num(&h[59..70]) as usize;
			let name_at = at + 76;
			let name = String::from_utf8(bytes[name_at..name_at + namesize - 1].to_vec()).unwrap();
			let data_at = name_at + namesize;
			let data = bytes[data_at..data_at + filesize].to_vec();
			at = data_at + filesize;
			if name == "TRAILER!!!" {
				break;
			}
			out.push((name, mode, uid, gid, data));
		}
		assert_eq!(at, bytes.len(), "nothing after the trailer");
		out
	}

	fn file(data: &str, mode: u32) -> Entry {
		Entry {
			node: Node::File(data.as_bytes().to_vec()),
			mode,
		}
	}

	fn config(dir: &Path, metadata: &str) -> Config {
		let manifest = crate::config::tests::write_manifest(dir, metadata);
		Config::new(&manifest).unwrap()
	}

	#[test]
	fn cpio_has_dot_paths_root_wheel_and_modes() {
		let mut tree = Tree::new();
		add_parents(&mut tree, "usr/local/bin/tool");
		tree.insert("usr/local/bin/tool".into(), file("#!/bin/sh\n", 0o755));
		let entries = read_odc(&cpio_odc(&tree, 7));
		let names: Vec<_> = entries.iter().map(|e| e.0.as_str()).collect();
		assert_eq!(
			names,
			[
				".",
				"./usr",
				"./usr/local",
				"./usr/local/bin",
				"./usr/local/bin/tool"
			]
		);
		let tool = entries.last().unwrap();
		assert_eq!((tool.1, tool.2, tool.3), (0o100755, 0, 80));
		assert_eq!(tool.4, b"#!/bin/sh\n");
		assert_eq!(entries[1].1, 0o040755);
	}

	#[test]
	fn cpio_is_readable_by_gnu_cpio_when_available() {
		let Some(cpio) = which("cpio") else { return };
		let mut tree = Tree::new();
		add_parents(&mut tree, "usr/bin/x");
		tree.insert("usr/bin/x".into(), file("data", 0o644));
		let mut child = Command::new(cpio)
			.arg("-t")
			.stdin(std::process::Stdio::piped())
			.stdout(std::process::Stdio::piped())
			.stderr(std::process::Stdio::null())
			.spawn()
			.unwrap();
		child
			.stdin
			.take()
			.unwrap()
			.write_all(&cpio_odc(&tree, 0))
			.unwrap();
		let out = child.wait_with_output().unwrap();
		let listing = String::from_utf8(out.stdout).unwrap();
		assert_eq!(
			listing.lines().collect::<Vec<_>>(),
			[".", "./usr", "./usr/bin", "./usr/bin/x"]
		);
	}

	#[test]
	fn payload_adds_parent_directories_and_resolves_sources() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path();
		fs::create_dir_all(root.join("target/aarch64-apple-darwin/release")).unwrap();
		fs::write(root.join("target/aarch64-apple-darwin/release/demo"), "bin").unwrap();
		fs::write(root.join("README.md"), "doc").unwrap();
		let c = config(
			root,
			"identifier = \"org.example.demo\"\nassets = [\
			 { source = \"target/release/demo\", dest = \"/usr/local/bin/demo\", mode = \"0755\" },\
			 { source = \"README.md\", dest = \"/usr/local/share/doc/demo/\" }]\n",
		);
		let target = BuildTarget::new(
			&Cli {
				target: Some("aarch64-apple-darwin".into()),
				profile: "release".into(),
				..Cli::default()
			},
			&root.join("target"),
		);
		let tree = payload_tree(&c, &target).unwrap();
		let keys: Vec<_> = tree.keys().map(String::as_str).collect();
		assert_eq!(
			keys,
			[
				"usr",
				"usr/local",
				"usr/local/bin",
				"usr/local/bin/demo",
				"usr/local/share",
				"usr/local/share/doc",
				"usr/local/share/doc/demo",
				"usr/local/share/doc/demo/README.md"
			]
		);
		assert_eq!(tree["usr/local/bin/demo"], file("bin", 0o755));
		assert_eq!(tree["usr/local/share/doc/demo/README.md"].mode, 0o644);
		assert_eq!(file_name(&c, &target), "demo-1.2.3-arm64.pkg");
	}

	#[test]
	fn payload_rejects_parent_dirs_duplicates_and_missing_files() {
		let dir = tempfile::tempdir().unwrap();
		let root = dir.path();
		fs::write(root.join("a"), "a").unwrap();
		let target = BuildTarget::new(&Cli::default(), &root.join("target"));
		let up = config(
			root,
			"identifier = \"org.example.demo\"\nassets = [{ source = \"a\", dest = \"/usr/../etc/a\" }]\n",
		);
		assert!(matches!(payload_tree(&up, &target), Err(Error::Config(m)) if m.contains("..")));
		let twice = config(
			root,
			"identifier = \"org.example.demo\"\nassets = [\
			 { source = \"a\", dest = \"/x/a\" }, { source = \"a\", dest = \"/x/\" }]\n",
		);
		assert!(
			matches!(payload_tree(&twice, &target), Err(Error::Config(m)) if m.contains("twice"))
		);
		let missing = config(
			root,
			"identifier = \"org.example.demo\"\nassets = [{ source = \"nope\", dest = \"/x/nope\" }]\n",
		);
		assert!(matches!(
			payload_tree(&missing, &target),
			Err(Error::FileIo(..))
		));
	}

	#[test]
	fn package_info_lists_the_scripts_that_exist() {
		let dir = tempfile::tempdir().unwrap();
		let c = config(
			dir.path(),
			"identifier = \"org.example.demo\"\nassets = []\n",
		);
		let mut payload = Tree::new();
		payload.insert("a".into(), file(&"x".repeat(2048), 0o644));
		let mut scripts = Tree::new();
		scripts.insert("postinstall".into(), file("#!/bin/sh\n", 0o755));
		let xml = package_info(&c, &payload, &scripts);
		assert!(xml.contains("identifier=\"org.example.demo\""));
		assert!(xml.contains("version=\"1.2.3\""));
		assert!(xml.contains("numberOfFiles=\"2\" installKBytes=\"3\""));
		assert!(xml.contains("<postinstall file=\"./postinstall\"/>"));
		assert!(!xml.contains("preinstall"));
		assert!(!package_info(&c, &payload, &Tree::new()).contains("<scripts>"));
	}

	#[cfg(unix)]
	fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
		use std::os::unix::fs::PermissionsExt;
		let path = dir.join(name);
		fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
		fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
		path
	}

	#[cfg(unix)]
	fn project(root: &Path) -> (Config, BuildTarget) {
		fs::create_dir_all(root.join("target/release")).unwrap();
		fs::write(root.join("target/release/demo"), "binary").unwrap();
		fs::create_dir_all(root.join("macpkg/scripts")).unwrap();
		fs::write(
			root.join("macpkg/scripts/postinstall"),
			"#!/bin/sh\nexit 0\n",
		)
		.unwrap();
		let c = config(
			root,
			"identifier = \"org.example.demo\"\nscripts = \"macpkg/scripts\"\n\
			 assets = [{ source = \"target/release/demo\", dest = \"/usr/local/bin/demo\", mode = \"0755\" }]\n",
		);
		let cli = Cli {
			profile: "release".into(),
			..Cli::default()
		};
		(c, BuildTarget::new(&cli, &root.join("target")))
	}

	#[test]
	fn builtin_package_holds_bom_payload_scripts_and_package_info() {
		use std::io::Read;
		let dir = tempfile::tempdir().unwrap();
		let (c, target) = project(dir.path());
		let out = dir.path().join("out/demo.pkg");
		build(&c, &target, &out, Tool::Builtin, &Tools::default()).unwrap();

		let files = crate::xar::tests::read(&fs::read(&out).unwrap());
		let names: Vec<_> = files.iter().map(|f| f.0.as_str()).collect();
		assert_eq!(names, ["Bom", "Payload", "PackageInfo", "Scripts"]);
		let part = |n: &str| files.iter().find(|f| f.0 == n).unwrap().2.clone();
		let unpack = |n: &str| {
			let mut v = Vec::new();
			flate2::read::GzDecoder::new(&part(n)[..])
				.read_to_end(&mut v)
				.unwrap();
			read_odc(&v)
		};

		// Payload and Bom describe the same paths
		let payload = unpack("Payload");
		let names: Vec<_> = payload.iter().map(|e| e.0.as_str()).collect();
		assert_eq!(
			names,
			[
				".",
				"./usr",
				"./usr/local",
				"./usr/local/bin",
				"./usr/local/bin/demo"
			]
		);
		let bom = crate::bom::tests::read(&part("Bom"));
		let in_bom: Vec<_> = bom.keys().map(String::as_str).collect();
		assert_eq!(in_bom, names);
		let demo = &bom["./usr/local/bin/demo"];
		assert_eq!((demo.0, demo.1, demo.2, demo.3), (0o100755, 0, 80, 6));
		assert_eq!(demo.4, crate::bom::cksum(b"binary"));

		assert_eq!(unpack("Scripts")[1].0, "./postinstall");
		let info = String::from_utf8(part("PackageInfo")).unwrap();
		assert!(info.contains("identifier=\"org.example.demo\""), "{info}");
		assert!(
			info.contains("<postinstall file=\"./postinstall\"/>"),
			"{info}"
		);

		// reproducible
		let again = dir.path().join("again.pkg");
		build(&c, &target, &again, Tool::Builtin, &Tools::default()).unwrap();
		assert_eq!(fs::read(&out).unwrap(), fs::read(&again).unwrap());
	}

	#[cfg(unix)]
	#[test]
	fn pkgbuild_method_passes_identity_location_and_scripts() {
		let dir = tempfile::tempdir().unwrap();
		let (c, target) = project(dir.path());
		let seen = dir.path().join("pkgbuild.args");
		let pkgbuild = stub(
			dir.path(),
			"pkgbuild",
			&format!(
				"echo \"$@\" > {}\nfor a; do last=\"$a\"; done\necho pkg > \"$last\"",
				seen.display()
			),
		);
		let out = dir.path().join("demo.pkg");
		let tools = Tools {
			pkgbuild: Some(pkgbuild),
		};
		build(&c, &target, &out, Tool::Pkgbuild, &tools).unwrap();
		let args = fs::read_to_string(&seen).unwrap();
		assert!(out.is_file());
		for want in [
			"--root ",
			"--identifier org.example.demo",
			"--version 1.2.3",
			"--install-location /",
			"--scripts ",
		] {
			assert!(args.contains(want), "{want} v {args}");
		}
	}

	#[test]
	fn auto_uses_the_builtin_writer_without_pkgbuild() {
		let none = Tools::default();
		assert!(matches!(choose(Tool::Auto, &none), Ok(Method::Builtin)));
		let err = choose(Tool::Pkgbuild, &none).err().unwrap().to_string();
		assert!(err.contains("pkgbuild"), "{err}");
		let with = Tools {
			pkgbuild: Some("/usr/bin/pkgbuild".into()),
		};
		assert!(matches!(choose(Tool::Auto, &with), Ok(Method::Pkgbuild(_))));
	}
}
