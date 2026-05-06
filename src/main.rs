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

fn handle_cd(args: &[String]) {
    let target = if args.len() == 0 {
        std::env::var("HOME").unwrap()
    } else {
        let input = args[0].as_str();
        if input == "~" {
            std::env::var("HOME").unwrap()
        } else if input.starts_with("~/") {
            let home = std::env::var("HOME").unwrap();
            format!("{}/{}", home, &input[2..])
        } else {
            input.to_string()
        }
    };
    if let Err(e) = std::env::set_current_dir(&target) {
        let msg = match e.kind() {
            io::ErrorKind::NotFound => "No such file or directory",
            io::ErrorKind::PermissionDenied => "Permission denied",
            _ => "Error",
        };
        println!("cd: {}: {}", target, msg);
    }
}

fn execute_external(target: &str, args: &[String], path: &str) {
    if let Some(full_path) = find_in_path(target, path) {
        let status = Command::new(full_path).arg0(target).args(args).status();
        if status.is_err() {
            println!("{}: exec error", target);
        }
    } else {
        println!("{}: command not found", target);
    }
}

fn handle_command(args: &[String], builtins: &HashMap<&str, &str>, path: &str) {
    if args.is_empty() {
        return;
    }

    let cmd = args[0].as_str();
    match cmd {
        "exit" => process::exit(0),
        "echo" => println!("{}", args[1..].join(" ")),
        "type" => {
            if args.len() != 2 {
                println!("type needs one arg");
                return;
            }
            handle_type(args[1].as_str(), builtins, path);
        }
        "pwd" => {
            if args.len() != 1 {
                println!("pwd needs no arg");
                return;
            }
            let cwd = std::env::current_dir().unwrap_or_default();
            println!("{}", cwd.display());
        }
        "cd" => {
            if args.len() > 2 {
                println!("cd needs less args");
                return;
            }
            handle_cd(&args[1..]);
        }
        _ => execute_external(cmd, &args[1..], path),
    }
}

fn parse_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single_quote = false;
    let mut in_double_quote = false;
    let mut in_backslash = false;

    for c in input.chars() {
        if in_backslash {
            current.push(c);
            in_backslash = !in_backslash;
            continue;
        }
        match c {
            '\\' if !in_single_quote && !in_double_quote => {
                in_backslash = !in_backslash;
            }
            '\"' => {
                if !in_single_quote {
                    in_double_quote = !in_double_quote;
                }
            }
            '\'' if !in_double_quote => {
                in_single_quote = !in_single_quote;
            }
            ' ' | '\t' if !in_single_quote && !in_double_quote => {
                if !current.is_empty() {
                    args.push(current.clone());
                    current.clear();
                }
            }

            _ => {
                current.push(c);
            }
        }
    }

    if !current.is_empty() {
        args.push(current);
    }

    args
}

fn main() {
    let builtins = HashMap::from([
        ("type", "builtin"),
        ("exit", "builtin"),
        ("echo", "builtin"),
        ("pwd", "builtin"),
        ("cd", "builtin"),
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
        let args: Vec<String> = parse_args(buffer.trim_end_matches(&['\n', '\r']));
        handle_command(&args, &builtins, &path);
    }
}
