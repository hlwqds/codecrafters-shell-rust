// ─── Types & Global State ───────────────────────────────────────────

use libc::{exit, fork};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, Write};
use std::os::fd::FromRawFd;
use std::os::raw::c_int;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, Command, Stdio};
use std::sync::Mutex;
use std::{collections::HashMap, env};

use once_cell::sync::Lazy;
use rustyline::completion::{Completer, Pair};
use rustyline::config::{BellStyle, CompletionType};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::history::DefaultHistory;
use rustyline::validate::Validator;
use rustyline::{Config, Context, Editor, Helper};

struct Redirect {
    stdout: Option<PathBuf>,
    stdout_append: bool,
    stderr: Option<PathBuf>,
    stderr_append: bool,
    pipe_read_fd: Option<c_int>,
    pipe_write_fd: Option<c_int>,
}

impl Default for Redirect {
    fn default() -> Self {
        Self {
            stdout: None,
            stdout_append: false,
            stderr: None,
            stderr_append: false,
            pipe_read_fd: None,
            pipe_write_fd: None,
        }
    }
}

#[derive(Debug)]
struct Job {
    id: usize,
    pid: c_int,
    command: String,
    running: bool,
}

static BUILTINS: Lazy<HashMap<&str, bool>> = Lazy::new(|| {
    HashMap::from([
        ("echo", true),
        ("exit", true),
        ("type", true),
        ("pwd", true),
        ("cd", true),
        ("complete", true),
        ("jobs", true),
        ("history", true),
    ])
});

static COMPLETIONS: Lazy<Mutex<HashMap<String, String>>> = Lazy::new(|| Mutex::new(HashMap::new()));

static JOBS: Lazy<Mutex<HashMap<usize, Job>>> = Lazy::new(|| Mutex::new(HashMap::new()));

static HISTORY: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));
static LAST_SYNCED: Lazy<Mutex<usize>> = Lazy::new(|| Mutex::new(0));

// ─── Parsing ────────────────────────────────────────────────────────

/// Split input by `|` respecting quotes and backslash escapes.
fn parse_pipeline(input: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backslash = false;

    for (i, c) in input.char_indices() {
        if in_backslash {
            in_backslash = false;
            continue;
        }
        match c {
            '\\' if !in_single => in_backslash = true,
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            '|' if !in_single && !in_double => {
                segments.push(input[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
    }
    let last = input[start..].trim();
    if !last.is_empty() {
        segments.push(last);
    }
    segments
}

/// Tokenize a command segment into args, respecting quotes and escapes.
fn parse_args(input: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut in_backslash = false;

    for c in input.chars() {
        if in_backslash {
            current.push(c);
            in_backslash = false;
            continue;
        }
        match c {
            '\\' if !in_single => in_backslash = true,
            '"' if !in_single => in_double = !in_double,
            '\'' if !in_double => in_single = !in_single,
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() {
                    args.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(c),
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Split args from redirect operators (>, >>, 2>, 2>>).
fn split_redirect(args: Vec<String>) -> (Vec<String>, Redirect) {
    let mut cmd_args = Vec::new();
    let mut redirect = Redirect::default();
    let mut i = 0;

    while i < args.len() {
        let token = &args[i];
        let (target_field, append) = match token.as_str() {
            ">" | "1>" => (0, false),
            ">>" | "1>>" => (0, true),
            "2>" => (1, false),
            "2>>" => (1, true),
            _ => {
                cmd_args.push(token.clone());
                i += 1;
                continue;
            }
        };
        if i + 1 < args.len() {
            let path = PathBuf::from(&args[i + 1]);
            match target_field {
                0 => {
                    redirect.stdout = Some(path);
                    redirect.stdout_append = append;
                }
                _ => {
                    redirect.stderr = Some(path);
                    redirect.stderr_append = append;
                }
            }
        }
        i += 2;
    }

    (cmd_args, redirect)
}

// ─── I/O ────────────────────────────────────────────────────────────

fn open_file_for_write(path: &Path, append: bool) -> File {
    OpenOptions::new()
        .create(true)
        .write(true)
        .append(append)
        .truncate(!append)
        .open(path)
        .unwrap()
}

fn write_output(text: &str, redirect: &Redirect) {
    let msg = format!("{}\n", text);
    if let Some(ref path) = redirect.stdout {
        open_file_for_write(path, redirect.stdout_append)
            .write_all(msg.as_bytes())
            .unwrap();
    } else if let Some(fd) = redirect.pipe_write_fd {
        unsafe {
            libc::write(fd, msg.as_ptr() as *const libc::c_void, msg.len());
        }
    } else {
        print!("{}", msg);
        let _ = std::io::stdout().flush();
    }
}

fn write_error(text: &str, redirect: &Redirect) {
    if let Some(ref path) = redirect.stderr {
        writeln!(
            open_file_for_write(path, redirect.stderr_append),
            "{}",
            text
        )
        .unwrap();
    } else {
        let _ = writeln!(std::io::stderr(), "{}", text);
    }
}

fn apply_redirect(command: &mut Command, redirect: &Redirect) {
    if let Some(fd) = redirect.pipe_read_fd {
        unsafe {
            command.stdin(Stdio::from_raw_fd(fd));
        }
    }
    if let Some(ref path) = redirect.stdout {
        command.stdout(Stdio::from(open_file_for_write(
            path,
            redirect.stdout_append,
        )));
    } else if let Some(fd) = redirect.pipe_write_fd {
        unsafe {
            command.stdout(Stdio::from_raw_fd(fd));
        }
    }
    if let Some(ref path) = redirect.stderr {
        command.stderr(Stdio::from(open_file_for_write(
            path,
            redirect.stderr_append,
        )));
    }
}

// ─── Path Utilities ─────────────────────────────────────────────────

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn find_in_path(cmd: &str, path: &str) -> Option<PathBuf> {
    env::split_paths(path).find_map(|dir| {
        let full = dir.join(cmd);
        if is_executable(&full) {
            Some(full)
        } else {
            None
        }
    })
}

fn find_prefix_file_in_dir(prefix: &str, dir: &Path, executable_only: bool) -> Vec<String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if !name.starts_with(prefix) {
                return None;
            }
            if executable_only && !is_executable(&dir.join(&name)) {
                return None;
            }
            Some(if dir.join(&name).is_dir() {
                format!("{}/", name)
            } else {
                name
            })
        })
        .collect()
}

fn find_prefix_file_in_cwd(prefix: &str) -> Vec<String> {
    let path = Path::new(prefix);
    let (dir, filename) = if prefix.ends_with('/') {
        (path, "")
    } else {
        (
            path.parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new(".")),
            path.file_name().and_then(|s| s.to_str()).unwrap_or(""),
        )
    };
    find_prefix_file_in_dir(filename, dir, false)
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

fn find_prefix_executables_in_path(prefix: &str, path: &str) -> Vec<String> {
    env::split_paths(path)
        .flat_map(|dir| find_prefix_file_in_dir(prefix, &dir, true))
        .collect()
}

// ─── Command Execution ──────────────────────────────────────────────

fn execute_external(target: &str, args: &[String], path: &str, redirect: &Redirect) {
    if let Some(full_path) = find_in_path(target, path) {
        let mut command = Command::new(full_path);
        command.arg0(target).args(args);
        apply_redirect(&mut command, redirect);
        if command.status().is_err() {
            write_error(&format!("{}: exec error", target), redirect);
        }
    } else {
        write_error(&format!("{}: command not found", target), redirect);
    }
}

fn handle_type(target: &str, path: &str, redirect: &Redirect) {
    if BUILTINS.contains_key(target) {
        write_output(&format!("{} is a shell builtin", target), redirect);
    } else if let Some(full_path) = find_in_path(target, path) {
        write_output(&format!("{} is {}", target, full_path.display()), redirect);
    } else {
        write_error(&format!("{}: not found", target), redirect);
    }
}

fn handle_complete(args: &[String], redirect: &Redirect) {
    match args.first().map(|s| s.as_str()) {
        Some("-p") => {
            if let Some(cmd) = args.get(1) {
                let completions = COMPLETIONS.lock().unwrap();
                if let Some(p) = completions.get(cmd.as_str()) {
                    write_output(&format!("complete -C '{}' {}", p, cmd), redirect);
                } else {
                    write_error(
                        &format!("complete: {}: no completion specification", cmd),
                        redirect,
                    );
                }
            }
        }
        Some("-C") => {
            if args.len() == 3 {
                COMPLETIONS
                    .lock()
                    .unwrap()
                    .insert(args[2].clone(), args[1].clone());
            }
        }
        Some("-r") => {
            if let Some(cmd) = args.get(1) {
                COMPLETIONS.lock().unwrap().remove(cmd.as_str());
            }
        }
        _ => write_error("complete: invalid usage", redirect),
    }
}

fn handle_cd(args: &[String]) {
    let target = match args.first().map(|s| s.as_str()) {
        None | Some("~") => std::env::var("HOME").unwrap(),
        Some(s) if s.starts_with("~/") => {
            format!("{}/{}", std::env::var("HOME").unwrap(), &s[2..])
        }
        Some(s) => s.to_string(),
    };
    if let Err(e) = std::env::set_current_dir(&target) {
        let msg = match e.kind() {
            io::ErrorKind::NotFound => "No such file or directory",
            io::ErrorKind::PermissionDenied => "Permission denied",
            _ => "Error",
        };
        eprintln!("cd: {}: {}", target, msg);
    }
}

fn handle_history(args: &[String], redirect: &Redirect) {
    if args.len() > 2 {
        write_error("arg num not invalid", redirect);
        return;
    }
    if args.len() == 0 {
        list_history(-1, redirect);
        return;
    }
    if args.len() == 1 {
        let n: i32 = args[0].parse().unwrap();
        list_history(n, redirect);
        return;
    }
    if args[0] == "-r" {
        let file = File::open(args[1].clone()).unwrap();
        let reader = std::io::BufReader::new(file);
        for line in reader.lines() {
            let line = line.unwrap();
            add_to_history(line);
        }
        return;
    }
    if args[0] == "-w" {
        let path = Path::new(&args[1]);
        let mut file = open_file_for_write(path, false);
        let history_list = HISTORY.lock().unwrap();
        for history in history_list.iter() {
            writeln!(file, "{}", history).unwrap();
        }
        return;
    }
    if args[0] == "-a" {
        let path = Path::new(&args[1]);
        let mut file = open_file_for_write(path, true);
        let history_list = HISTORY.lock().unwrap();
        let start = *LAST_SYNCED.lock().unwrap();
        for history in history_list[start..].iter() {
            writeln!(file, "{}", history).unwrap();
        }
        *LAST_SYNCED.lock().unwrap() = history_list.len();
        return;
    }
}

fn handle_jobs(redirect: &Redirect) {
    let mut jobs = JOBS.lock().unwrap();

    // Reap finished jobs
    loop {
        let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        if let Some(id) = jobs.values().find(|j| j.pid == pid).map(|j| j.id) {
            jobs.get_mut(&id).unwrap().running = false;
        }
    }

    // Sort all jobs by id, assign markers based on position
    let mut ids: Vec<usize> = jobs.keys().copied().collect();
    ids.sort();
    let len = ids.len();
    for (num, &id) in ids.iter().enumerate() {
        let job = jobs.get(&id).unwrap();
        let mark = if num + 1 == len {
            "+"
        } else if num + 2 == len {
            "-"
        } else {
            " "
        };
        if job.running {
            write_output(
                &format!("[{}]{}  {:<24}{} &", id, mark, "Running", job.command),
                redirect,
            );
        } else {
            write_output(
                &format!("[{}]{}  {:<24}{}", id, mark, "Done", job.command),
                redirect,
            );
        }
    }

    // Remove finished jobs
    jobs.retain(|_, job| job.running);
}

/// Ensure redirect target files exist (bash creates them even if nothing is written).
fn ensure_redirect_files(redirect: &Redirect) {
    if let Some(ref p) = redirect.stdout {
        let _ = open_file_for_write(p, redirect.stdout_append);
    }
    if let Some(ref p) = redirect.stderr {
        let _ = open_file_for_write(p, redirect.stderr_append);
    }
}

/// Central dispatch for all commands (builtins + external).
fn handle_command(args: Vec<String>, path: &str, redirect: &Redirect) {
    if args.is_empty() {
        return;
    }
    ensure_redirect_files(redirect);
    let cmd = args[0].as_str();
    match cmd {
        "exit" => process::exit(0),
        "echo" => write_output(&args[1..].join(" "), redirect),
        "type" => {
            if let Some(target) = args.get(1) {
                handle_type(target, path, redirect);
            }
        }
        "pwd" => {
            let cwd = std::env::current_dir().unwrap_or_default();
            write_output(&format!("{}", cwd.display()), redirect);
        }
        "cd" => handle_cd(&args[1..]),
        "jobs" => handle_jobs(redirect),
        "complete" => handle_complete(&args[1..], redirect),
        "history" => handle_history(&args[1..], redirect),
        _ => execute_external(cmd, &args[1..], path, redirect),
    }
}

// ─── Job Management ─────────────────────────────────────────────────

fn add_job(command: String) -> usize {
    let mut jobs = JOBS.lock().unwrap();
    let id = (1..).find(|id| !jobs.contains_key(id)).unwrap();
    jobs.insert(
        id,
        Job {
            id,
            pid: 0,
            command,
            running: true,
        },
    );
    id
}

fn reap_children() {
    let mut jobs = JOBS.lock().unwrap();
    loop {
        let pid = unsafe { libc::waitpid(-1, std::ptr::null_mut(), libc::WNOHANG) };
        if pid <= 0 {
            break;
        }
        let found_id = jobs.values().find(|j| j.pid == pid).map(|j| j.id);
        let Some(id) = found_id else { continue };

        let max_id = jobs.values().filter(|j| j.running).map(|j| j.id).max();
        let mark = if max_id.is_none() || id >= max_id.unwrap() {
            "+"
        } else {
            "-"
        };

        let cmd = jobs.get(&id).unwrap().command.clone();
        println!("[{}]{}  {:<24}{}", id, mark, "Done", cmd);

        jobs.remove(&id);
    }
}

fn run_background(args: &[String], path: &str, redirect: &Redirect) {
    let id = add_job(args.join(" "));
    let pid: c_int;
    unsafe {
        pid = fork();
        if pid == 0 {
            let cmd = args.first().map(|s| s.as_str()).unwrap_or("");
            if let Some(full_path) = find_in_path(cmd, path) {
                let mut command = Command::new(full_path);
                command.arg0(cmd).args(&args[1..]);
                apply_redirect(&mut command, redirect);
                let _ = command.exec();
                exit(1);
            } else {
                eprintln!("{}: command not found", cmd);
                exit(127);
            }
        } else {
            JOBS.lock().unwrap().get_mut(&id).unwrap().pid = pid;
        }
    }
    if pid < 0 {
        JOBS.lock().unwrap().remove(&id);
    } else {
        write_output(&format!("[{}] {}", id, pid), redirect);
    }
}

// ─── Tab Completion ─────────────────────────────────────────────────

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
        let before = &line[..pos];
        let parts: Vec<&str> = before.split_whitespace().collect();
        let trailing_space = before.ends_with(' ') || before.ends_with('\t');

        let current = if trailing_space {
            ""
        } else {
            parts.last().copied().unwrap_or("")
        };
        let arg_index = if trailing_space {
            parts.len()
        } else {
            parts.len().saturating_sub(1)
        };
        let start = pos - current.len();

        // Registered command completion
        if arg_index > 0 {
            if COMPLETIONS.lock().unwrap().contains_key(parts[0]) {
                let prev = parts.get(arg_index - 1).copied().unwrap_or("");
                let completions = run_completion_script(parts[0], current, prev, before);
                let matches: Vec<String> = completions
                    .into_iter()
                    .filter(|c| c.starts_with(current))
                    .collect();
                if !matches.is_empty() {
                    let pairs: Vec<Pair> = if matches.len() == 1 {
                        vec![Pair {
                            display: matches[0].clone(),
                            replacement: format!("{} ", matches[0]),
                        }]
                    } else {
                        matches
                            .into_iter()
                            .map(|c| Pair {
                                display: c.clone(),
                                replacement: c,
                            })
                            .collect()
                    };
                    return Ok((start, pairs));
                }
            }
        }

        // Default completions
        let path = env::var("PATH").unwrap_or_default();
        let pairs = if arg_index == 0 {
            let mut cmds: Vec<String> = BUILTINS
                .keys()
                .filter(|cmd| cmd.starts_with(current))
                .map(|cmd| cmd.to_string())
                .collect();
            cmds.extend(find_prefix_executables_in_path(current, &path));
            cmds.sort();
            cmds.dedup();
            cmds.into_iter()
                .map(|c| Pair {
                    display: c.clone(),
                    replacement: format!("{} ", c),
                })
                .collect()
        } else {
            let mut files = find_prefix_file_in_cwd(current);
            files.sort();
            files
                .into_iter()
                .map(|f| Pair {
                    display: f.clone(),
                    replacement: if f.ends_with('/') {
                        f
                    } else {
                        format!("{} ", f)
                    },
                })
                .collect()
        };
        Ok((start, pairs))
    }
}

fn run_completion_script(
    cmd_name: &str,
    current_word: &str,
    prev_word: &str,
    comp_line: &str,
) -> Vec<String> {
    let script = COMPLETIONS.lock().unwrap().get(cmd_name).cloned();
    let Some(script) = script else {
        return vec![];
    };

    let output = Command::new(&script)
        .arg(cmd_name)
        .arg(current_word)
        .arg(prev_word)
        .env("COMP_LINE", comp_line)
        .env("COMP_POINT", comp_line.len().to_string())
        .output();

    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout)
            .lines()
            .map(|s| s.to_string())
            .collect(),
        Err(_) => vec![],
    }
}

// ─── Pipeline Execution ─────────────────────────────────────────────

/// Execute a single pipeline segment in a forked child process.
/// Handles both builtins (except cd/complete/jobs which need parent state)
/// and external commands. The child calls exit() when done.
fn exec_segment_in_child(args: Vec<String>, path: &str, redirect: &Redirect) {
    if args.is_empty() {
        unsafe {
            exit(0);
        }
    }
    let cmd = args[0].as_str();
    match cmd {
        "echo" => {
            write_output(&args[1..].join(" "), redirect);
            unsafe {
                exit(0);
            }
        }
        "type" => {
            if let Some(target) = args.get(1) {
                handle_type(target, path, redirect);
            }
            unsafe {
                exit(0);
            }
        }
        "pwd" => {
            let cwd = std::env::current_dir().unwrap_or_default();
            write_output(&format!("{}", cwd.display()), redirect);
            unsafe {
                exit(0);
            }
        }
        "exit" => unsafe {
            exit(0);
        },
        _ => {
            // External command: exec replaces process
            if let Some(full_path) = find_in_path(cmd, path) {
                let mut command = Command::new(full_path);
                command.arg0(cmd).args(&args[1..]);
                apply_redirect(&mut command, redirect);
                let _ = command.exec();
                unsafe {
                    exit(1);
                }
            } else {
                write_error(&format!("{}: command not found", cmd), redirect);
                unsafe {
                    exit(127);
                }
            }
        }
    }
}

fn add_to_history(line: String) {
    HISTORY.lock().unwrap().push(line);
}

fn list_history(recent_num: i32, redirect: &Redirect) {
    let history_list = HISTORY.lock().unwrap();
    if recent_num < 0 {
        for (i, history) in history_list.iter().enumerate() {
            let s = format!("{:>4}  {}", i + 1, history);
            write_output(&s, redirect);
        }
    } else {
        let len = history_list.len();
        let tail_len: usize = len.min(recent_num as usize);
        for i in len - tail_len..len {
            let s = format!("{:>4}  {}", i + 1, history_list[i]);
            write_output(&s, redirect)
        }
    }
}

// ─── Main Loop ──────────────────────────────────────────────────────

fn main() {
    let path = env::var("PATH").unwrap_or_default();
    let config = Config::builder()
        .bell_style(BellStyle::Audible)
        .completion_type(CompletionType::List)
        .build();
    let mut rl: Editor<ShellHelper, DefaultHistory> = Editor::with_config(config).unwrap();
    rl.set_helper(Some(ShellHelper));

    loop {
        reap_children();
        match rl.readline("$ ") {
            Ok(line) => {
                rl.add_history_entry(line.as_str()).unwrap();
                add_to_history(line.clone());
                let segments = parse_pipeline(&line);
                if segments.is_empty() {
                    continue;
                }
                let n = segments.len();

                // Single command, no pipe: execute in parent (cd/complete/jobs need parent state)
                if n == 1 {
                    let args = parse_args(segments[0]);
                    let (mut args, redirect) = split_redirect(args);
                    let background = !args.is_empty() && args.last() == Some(&"&".to_string());
                    if background {
                        args.pop();
                    }
                    if background {
                        run_background(&args, &path, &redirect);
                    } else {
                        handle_command(args, &path, &redirect);
                    }
                    continue;
                }

                // Pipeline: create N-1 pipes, fork all segments concurrently
                let mut pipes: Vec<[c_int; 2]> = Vec::with_capacity(n - 1);
                for _ in 0..n.saturating_sub(1) {
                    let mut fds: [c_int; 2] = [0, 0];
                    unsafe {
                        libc::pipe(fds.as_mut_ptr());
                    }
                    pipes.push(fds);
                }

                let mut child_pids: Vec<c_int> = Vec::with_capacity(n);

                for (i, segment) in segments.iter().enumerate() {
                    let args = parse_args(segment);
                    let (mut args, mut redirect) = split_redirect(args);

                    if i > 0 {
                        redirect.pipe_read_fd = Some(pipes[i - 1][0]);
                    }
                    if i < n - 1 {
                        redirect.pipe_write_fd = Some(pipes[i][1]);
                    }

                    let background = !args.is_empty() && args.last() == Some(&"&".to_string());
                    if background {
                        args.pop();
                    }

                    if background {
                        run_background(&args, &path, &redirect);
                    } else {
                        let pid: c_int;
                        unsafe {
                            pid = fork();
                            if pid == 0 {
                                // Child: close all pipe fds that don't belong to us
                                for (j, fds) in pipes.iter().enumerate() {
                                    if j != i {
                                        // Not our write end
                                        if i == n - 1 || j != i {
                                            libc::close(fds[1]);
                                        }
                                    }
                                    if j + 1 != i && i > 0 {
                                        // Not our read end
                                        libc::close(fds[0]);
                                    }
                                }
                                exec_segment_in_child(args, &path, &redirect);
                            }
                        }
                        if pid > 0 {
                            child_pids.push(pid);
                        }
                    }

                    // Parent closes write end immediately so next command can start
                    if i < n - 1 {
                        unsafe {
                            libc::close(pipes[i][1]);
                        }
                    }
                }

                // Close all read ends in parent
                for fds in &pipes {
                    unsafe {
                        libc::close(fds[0]);
                    }
                }

                // Wait for all pipeline children
                for pid in &child_pids {
                    unsafe {
                        libc::waitpid(*pid, std::ptr::null_mut(), 0);
                    }
                }
            }
            Err(ReadlineError::Eof) | Err(ReadlineError::Interrupted) => process::exit(0),
            Err(_) => process::exit(1),
        }
    }
}
