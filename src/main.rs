use build_target::BuildTarget;
use cargo::Workspace;
use cli::Cli;
use config::Config;
use error::Error;

mod bom;
mod build_target;
mod cargo;
mod cli;
mod config;
mod error;
mod pkg;
mod xar;

fn run() -> Result<(), Error> {
	let args = Cli::parse_args(std::env::args_os());
	let workspace = Workspace::load(args.manifest_path.as_deref())?;
	let manifest_path =
		workspace.manifest_of(args.package.as_deref(), args.manifest_path.as_deref())?;
	let config = Config::new(&manifest_path)?;
	let target = BuildTarget::new(&args, &workspace.target_directory);

	let file_name = pkg::file_name(&config, &target);
	let output = match &args.output {
		Some(p) if p.is_dir() => p.join(&file_name),
		Some(p) => p.clone(),
		None => target.output_dir().join(&file_name),
	};
	let tools = pkg::Tools::find(&args);
	pkg::build(&config, &target, &output, args.tool, &tools)?;
	println!("{}", output.display());
	Ok(())
}

fn main() {
	if let Err(err) = run() {
		eprintln!("error: {err}");
		std::process::exit(1);
	}
}
