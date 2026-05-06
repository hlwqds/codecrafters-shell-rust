#[allow(unused_imports)]
use std::fs;
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::process::{self, Command};
use std::{collections::HashMap, env};

struct Redirect {
    stdout: Option<PathBuf>,
    stdout_append: bool,
    stderr: Option<PathBuf>,
    stderr_append: bool,
}

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

fn open_redirect_file(path: &Path, append: bool) -> File {
    OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .unwrap()
}

fn handle_type(target: &str, builtins: &HashMap<&str, &str>, path: &str, redirect: &Redirect) {
    if let Some(value) = builtins.get(target) {
        let s = format!("{} is a shell {}", target, value);
        write_output(&s, redirect);
    } else if let Some(full_path) = find_in_path(target, path) {
        let s = format!("{} is {}", target, full_path.display());
        write_output(&s, redirect);
    } else {
        let s = format!("{}: not found", target);
        write_error(&s, redirect);
    }
}

fn handle_cd(args: &[String], redirect: &Redirect) {
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

        let s = format!("cd: {}: {}", target, msg);
        write_error(&s, redirect);
    }
}

fn apply_redirect(command: &mut Command, redirect: &Redirect) {
    if let Some(path) = &redirect.stdout {
        let file = open_redirect_file(path, redirect.stdout_append);
        command.stdout(Stdio::from(file));
    }

    if let Some(path) = &redirect.stderr {
        let file = open_redirect_file(path, redirect.stderr_append);
        command.stderr(Stdio::from(file));
    }
}

fn execute_external(target: &str, args: &[String], path: &str, redirect: &Redirect) {
    if let Some(full_path) = find_in_path(target, path) {
        let mut command = Command::new(full_path);
        command.arg0(target).args(args);
        apply_redirect(&mut command, redirect);
        let status = command.status();
        if status.is_err() {
            let s = format!("{}: exec error", target);
            write_error(&s, redirect);
        }
    } else {
        let s = format!("{}: command not found", target);
        write_error(&s, redirect);
    }
}

fn split_redirect(args: &[String]) -> (Vec<String>, Redirect) {
    let mut cmd_args = Vec::new();
    let mut redirect = Redirect {
        stdout: None,
        stdout_append: false,
        stderr: None,
        stderr_append: false,
    };

    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if token == ">" || token == "1>" {
            if i + 1 < args.len() {
                redirect.stdout = Some(PathBuf::from(&args[i + 1]));
            }
            i += 1
        } else if token == "2>" {
            if i + 1 < args.len() {
                redirect.stderr = Some(PathBuf::from(&args[i + 1]));
            }
            i += 1
        } else if token == ">>" || token == "1>>" {
            if i + 1 < args.len() {
                redirect.stdout = Some(PathBuf::from(&args[i + 1]));
                redirect.stdout_append = true;
            }
            i += 1
        } else if token == "2>>" {
            if i + 1 < args.len() {
                redirect.stderr = Some(PathBuf::from(&args[i + 1]));
                redirect.stderr_append = true;
            }
            i += 1
        } else {
            cmd_args.push(token.clone());
        }
        i += 1
    }

    (cmd_args, redirect)
}

fn write_output(text: &str, redirect: &Redirect) {
    if let Some(path) = &redirect.stdout {
        let mut file = open_redirect_file(path, redirect.stdout_append);
        writeln!(file, "{}", text).unwrap();
    } else {
        println!("{}", text);
    }
}

fn write_error(text: &str, redirect: &Redirect) {
    if let Some(path) = &redirect.stderr {
        let mut file = open_redirect_file(path, redirect.stderr_append);
        writeln!(file, "{}", text).unwrap();
    } else {
        let _ = writeln!(std::io::stderr(), "{}", text);
    }
}

fn prepare_redirect(redirect: &Redirect) {
    if let Some(path) = &redirect.stdout {
        let _ = open_redirect_file(path, redirect.stdout_append);
    }
    if let Some(path) = &redirect.stderr {
        let _ = open_redirect_file(path, redirect.stderr_append);
    }
}

fn handle_command(args: &[String], builtins: &HashMap<&str, &str>, path: &str) {
    if args.is_empty() {
        return;
    }

    let (args, redirect) = split_redirect(args);
    prepare_redirect(&redirect);

    if args.is_empty() {
        return;
    }

    let cmd = args[0].as_str();
    match cmd {
        "exit" => process::exit(0),
        "echo" => {
            write_output(args[1..].join(" ").as_str(), &redirect);
        }
        "type" => {
            if args.len() != 2 {
                write_error("type needs one arg", &redirect);
                return;
            }
            handle_type(args[1].as_str(), builtins, path, &redirect);
        }
        "pwd" => {
            if args.len() != 1 {
                write_error("pwd needs no arg", &redirect);
                return;
            }
            let cwd = std::env::current_dir().unwrap_or_default();
            let s = format!("{}", cwd.display());
            write_output(&s, &redirect);
        }
        "cd" => {
            if args.len() > 2 {
                write_error("cd needs less args", &redirect);
                return;
            }
            handle_cd(&args[1..], &redirect);
        }
        _ => execute_external(cmd, &args[1..], path, &redirect),
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
            '\\' if !in_single_quote => {
                in_backslash = !in_backslash;
            }
            '"' if !in_single_quote => {
                in_double_quote = !in_double_quote;
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
