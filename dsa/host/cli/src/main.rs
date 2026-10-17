//! dsa: asks the DSA service about the Android of the user who runs it. See the library
//! documentation for the commands.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match dsa_cli::parse(&args) {
        Ok(invocation) => invocation,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    let result = dsa_cli::connector(invocation.endpoint.as_deref())
        .and_then(|mut connector| dsa_cli::run(&invocation, &mut *connector));
    match result {
        Ok(line) => {
            println!("{line}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("dsa: {error}");
            ExitCode::FAILURE
        }
    }
}
