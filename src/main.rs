use std::collections::HashMap;
#[allow(unused_imports)]
use std::io::{self, Write};
use std::process::{self};

fn main() {
    let map = HashMap::from([
        ("type", "builtin"),
        ("exit", "builtin"),
        ("echo", "builtin"),
    ]);
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
        match cmd {
            "exit" => process::exit(0),
            "echo" => println!("{}", args[1..].join(" ")),
            "type" => {
                if args.len() != 2 {
                    println!("type needs one arg");
                    continue;
                }
                let target = args[1];
                if let Some(value) = map.get(target) {
                    println!("{} is a shell {}", args[1], value);
                } else {
                    println!("{}: not found", target);
                }
            }
            _ => println!("{}: command not found", cmd),
        }
    }
}
