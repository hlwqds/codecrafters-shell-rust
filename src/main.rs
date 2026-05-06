#[allow(unused_imports)]
use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::{collections::HashMap, env};

fn is_executable(path: &Path) -> bool {
    if let Ok(metadata) = fs::metadata(path) {
        if metadata.is_file() {
            return metadata.permissions().mode() & 0o111 != 0;
        }
    }
    false
}

fn find_in_path(cmd: &str, path: &str) -> Option<PathBuf> {
    for dir in env::split_paths(path) {
        let full_path = dir.join(cmd);
        if is_executable(&full_path) {
            return Some(full_path);
        }
    }
    None
}

fn handle_type(target: &str, builtins: &HashMap<&str, &str>, path: &str) {
    if let Some(value) = builtins.get(target) {
        println!("{} is a shell {}", target, value);
    } else if let Some(full_path) = find_in_path(target, path) {
        println!("{} is {}", target, full_path.display());
    } else {
        println!("{}: not found", target);
    }
}

fn execute_external(target: &str, args: &[&str], path: &str) {
    if let Some(full_path) = find_in_path(target, path) {
        let status = Command::new(full_path).arg0(target).args(args).status();
        if status.is_err() {
            println!("{}: exec error", target);
        }
    } else {
        println!("{}: command not found", target);
    }
}

fn handle_command(args: &[&str], builtins: &HashMap<&str, &str>, path: &str) {
    if args.is_empty() {
        return;
    }

    let cmd = args[0];
    match cmd {
        "exit" => process::exit(0),
        "echo" => println!("{}", args[1..].join(" ")),
        "type" => {
            if args.len() != 2 {
                println!("type needs one arg");
                return;
            }
            handle_type(args[1], builtins, path);
        }
        "pwd" => {
            if args.len() != 1 {
                println!("pwd needs no arg");
                return;
            }
            let cwd = std::env::current_dir().unwrap_or_default();
            println!("{}", cwd.display());
        }
        _ => execute_external(cmd, &args[1..], path),
    }
}

fn main() {
    let builtins = HashMap::from([
        ("type", "builtin"),
        ("exit", "builtin"),
        ("echo", "builtin"),
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
        handle_command(&args, &builtins, &path);
    }
}
