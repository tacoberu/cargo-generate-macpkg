use std::path::PathBuf;

#[derive(thiserror::Error, Debug)]
pub enum Error {
	#[error("{0}: {1}")]
	FileIo(PathBuf, std::io::Error),
	#[error("cannot read manifest: {0}")]
	Manifest(#[from] cargo_toml::Error),
	#[error("invalid glob pattern: {0}")]
	Pattern(#[from] glob::PatternError),
	#[error("{0}")]
	Config(String),
	#[error("{0}")]
	Tool(String),
	#[error(transparent)]
	Io(#[from] std::io::Error),
}
