use clap::Parser;
use notebook::Runtime;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "notebook", about = "Open notebooks")]
struct Args {
    /// Path to notebook file to open or create
    path: Option<PathBuf>,

    /// Runtime for new notebooks (python, deno). Falls back to user settings if not specified.
    #[arg(long, short)]
    runtime: Option<Runtime>,
}

fn main() {
    let args = Args::parse();

    if let Err(e) = notebook::run(args.path.clone(), args.runtime) {
        // Show native error dialog before exiting
        let title = "Cannot Open Notebook";
        let message = match &args.path {
            Some(path) => format!("Failed to open '{}':\n\n{}", path.display(), e),
            None => format!("Failed to start notebook:\n\n{}", e),
        };

        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(&message)
            .set_level(rfd::MessageLevel::Error)
            .show();

        std::process::exit(1);
    }
}
