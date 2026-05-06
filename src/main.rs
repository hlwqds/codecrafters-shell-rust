#[allow(unused_imports)]
use std::io::{self, Write};
use std::process::{self};

fn main() {
    // TODO: Uncomment the code below to pass the first stage
    let mut buffer = String::new();
    loop {
        print!("$ ");
        io::stdout().flush().unwrap();

        buffer.clear();
        // Wait for user input
        io::stdin().read_line(&mut buffer).unwrap();
        let args: Vec<&str> = buffer.split_whitespace().collect();

        if args.is_empty() {
            continue;
        }

        let cmd = args[0];
        if cmd == "exit" {
            process::exit(0)
        } else if cmd == "echo" {
            println!("{}", args[1..].join(" "));
        } else {
            println!("{}: command not found", cmd);
        }
    }
}
