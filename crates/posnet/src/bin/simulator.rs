use std::io::{self, Write};

use posnet::simulator::Simulator;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut port = 0;
    let mut header = None;
    while let Some(argument) = args.next() {
        match argument.as_str() {
            "--port" => port = args.next().ok_or("--port requires a number")?.parse()?,
            "--header" => header = Some(args.next().ok_or("--header requires text")?),
            "--help" | "-h" => {
                println!("posnet-simulator [--port PORT] [--header TEXT]");
                return Ok(());
            }
            _ => return Err(format!("Unknown argument: {argument}").into()),
        }
    }
    let sim = Simulator::start_on(port)?;
    if let Some(header) = header {
        sim.update(|state| state.header = header);
    }
    println!("{}", sim.connection.address);
    io::stdout().flush()?;
    loop {
        std::thread::park();
    }
}
