//! Process entry point for the standalone C37.118 simulator.

use std::env;

use c37_118_simulator::{config::load_profile, server::Server};

fn main() {
    let arguments = env::args().skip(1).collect::<Vec<_>>();
    match arguments.as_slice() {
        [command] if command == "healthcheck" => println!("ok"),
        [command, flag, profile] if command == "run" && flag == "--profile" => {
            if let Err(error) = run(profile) {
                eprintln!("c37-118-simulator: {error}");
                std::process::exit(2);
            }
        }
        _ => {
            eprintln!("usage: c37-118-simulator healthcheck | run --profile <path>");
            std::process::exit(2);
        }
    }
}

fn run(profile_path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let profile = load_profile(profile_path)?;
    let endpoint_count = profile.endpoints.len();
    let data_rate_hz = profile.endpoints[0].data_rate_hz;
    let mut server = Server::bind(profile)?;
    eprintln!(
        "starting C37.118 simulator with {endpoint_count} PMU endpoint(s) at {data_rate_hz} Hz"
    );
    Ok(server.run()?)
}
