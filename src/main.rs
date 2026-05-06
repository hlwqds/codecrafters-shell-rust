#[allow(unused_imports)]
use std::io::{self, Write};
use std::process::{self, ExitCode};

fn main() {
    // TODO: Uncomment the code below to pass the first stage
    let mut command = String::new();
    loop {
        print!("$ ");
        io::stdout().flush().unwrap();

        command.clear();
        // Wait for user input
        io::stdin().read_line(&mut command).unwrap();
        let cmd = command.trim();
        if cmd == "exit" {
            process::exit(0)
        } else {
            println!("{}: command not found", cmd);
        }
    }
}
