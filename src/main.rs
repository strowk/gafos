mod cli;
mod config;
mod openapi;
mod route;
mod translate;

fn main() {
    let args: cli::Args = argh::from_env();
    let code = match cli::run(args) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            2
        }
    };
    std::process::exit(code);
}
