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

use once_cell::sync::Lazy;
use rustyline::Editor;
use rustyline::Helper;
use rustyline::completion::Completer;
use rustyline::completion::Pair;
use rustyline::config::{BellStyle, CompletionType};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Config, Context};
struct ShellHelper;

impl Helper for ShellHelper {}
impl Highlighter for ShellHelper {}
impl Validator for ShellHelper {}
impl Hinter for ShellHelper {
    type Hint = String;

    fn hint(&self, _line: &str, _pos: usize, _ctx: &Context<'_>) -> Option<String> {
        None
    }
}

impl Completer for ShellHelper {
    type Candidate = Pair;
    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let before_cursor = &line[..pos];

        let mut res: Vec<Pair> = vec![];

        let parts: Vec<&str> = before_cursor.split_whitespace().collect();

        let current = if before_cursor.ends_with(' ') || before_cursor.ends_with('\t') {
            ""
        } else {
            parts.last().copied().unwrap_or("")
        };

        let arg_index = if before_cursor.ends_with(' ') || before_cursor.ends_with('\t') {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };

        let start = pos - current.len();

        if arg_index == 0 {
            let path = env::var("PATH").unwrap_or_default();

            let mut cmds: Vec<String> = BUILTINS
                .keys()
                .filter(|cmd| cmd.starts_with(current))
                .map(|cmd| cmd.to_string())
                .collect();
            cmds.extend(find_prefix_executables_in_path(current, &path));
            cmds.sort();
            cmds.dedup();
            let matches: Vec<Pair> = cmds
                .into_iter()
                .map(|cmd| Pair {
                    display: cmd.clone(),
                    replacement: format!("{} ", cmd),
                })
                .collect();
            res.extend(matches);
        } else {
            let mut files = find_prefix_file_in_cwd(current);
            files.sort();
            let matches: Vec<Pair> = files
                .into_iter()
                .map(|file| Pair {
                    display: file.clone(),
                    replacement: if file.ends_with("/") {
                        file.clone()
                    } else {
                        format!("{} ", file)
                    },
                })
                .collect();
            res.extend(matches);
        }

        Ok((start, res))
    }
}

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

fn find_prefix_file_in_cwd(prefix: &str) -> Vec<String> {
    let path = Path::new(prefix);
    let (dir, filename) = if prefix.ends_with('/') {
        (path, "")
    } else {
        let dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let filename = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        (dir, filename)
    };
    let files = find_prefix_file_in_dir(filename, dir, false);

    files
        .into_iter()
        .map(|name| {
            if dir == Path::new(".") {
                name
            } else {
                dir.join(name).to_string_lossy().to_string()
            }
        })
        .collect()
}

fn find_prefix_file_in_dir(prefix: &str, dir: &Path, executable: bool) -> Vec<String> {
    let mut cmds = vec![];
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let mut name = entry.file_name().to_string_lossy().to_string();
            let full_path = dir.join(&name);
            if executable && !is_executable(&full_path) {
                continue;
            }
            if full_path.is_dir() {
                name.push('/');
            }
            if name.starts_with(prefix) {
                cmds.push(name);
            }
        }
    }
    cmds
}

fn find_prefix_executables_in_path(prefix: &str, path: &str) -> Vec<String> {
    let mut cmds = vec![];
    for dir in env::split_paths(path) {
        cmds.extend(find_prefix_file_in_dir(
            prefix,
            Path::new(dir.to_str().unwrap_or(".")),
            true,
        ));
    }
    cmds
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

fn handle_type(target: &str, path: &str, redirect: &Redirect) {
    if let Some(_value) = BUILTINS.get(target) {
        let s = format!("{} is a shell builtin", target);
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

fn handle_command(args: &[String], path: &str) {
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
            handle_type(args[1].as_str(), path, &redirect);
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

static BUILTINS: Lazy<HashMap<&str, bool>> = Lazy::new(|| {
    HashMap::from([
        ("echo", true),
        ("exit", true),
        ("type", true),
        ("pwd", true),
        ("cd", true),
        ("complete", true),
    ])
});

fn main() {
    let path = env::var("PATH").unwrap_or_default();

    let config = Config::builder()
        .bell_style(BellStyle::Audible)
        .completion_type(CompletionType::List)
        .build();
    let mut rl: Editor<ShellHelper, DefaultHistory> = Editor::with_config(config).unwrap();
    rl.set_helper(Some(ShellHelper));
    loop {
        match rl.readline("$ ") {
            Ok(line) => {
                let args = parse_args(&line);
                handle_command(&args, &path);
            }
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => {
                process::exit(0);
            }
            Err(_) => {
                process::exit(1);
            }
        }
    }
}
