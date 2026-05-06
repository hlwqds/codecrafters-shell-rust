#[allow(unused_imports)]
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::{self};
use std::{collections::HashMap, env};

fn is_executable(path: &Path) -> bool {
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.is_file() {
            return metadata.permissions().mode() & 0o111 != 0;
        }
    }
    false
}

fn main() {
    let map = HashMap::from([
        ("type", "builtin"),
        ("exit", "builtin"),
        ("echo", "builtin"),
    ]);
    let path = env::var("PATH").unwrap_or_default();

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
                    continue;
                }
                let mut found = false;
                for p in env::split_paths(&path) {
                    let full_path = p.join(target);
                    if is_executable(&full_path) {
                        println!("{} is {}", target, full_path.display());
                        found = true;
                        break;
                    }
                }
                if !found {
                    println!("{}: not found", target);
                }
            }
            _ => println!("{}: command not found", cmd),
        }
    }
}
