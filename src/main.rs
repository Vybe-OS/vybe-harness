//!
//!   small command
//!   agent                 interactive chat in the current folder
//!   agent "fix the bug"   start with a prompt
//!   agent --continue      resume the last session
//!   agent --plan          start in read-only plan mode
//!   agent -m <model>      pick a model
//!   agent --gui           open a browser window instead of the terminal UI
//!   agent --gui --port N  choose the local port (default 7878)

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use std::{env, fs, thread};

const GUI_HTML: &str = include_str!("../ui/index.html");

const MAX_STEPS: usize = 30;
const IGNORE: [&str; 7] = [".git", "target", "node_modules", ".venv", "venv", "__pycache__", ".agent"];
const READ_ONLY: [&str; 3] = ["read", "glob", "grep"];
const DEFAULT_MODEL: &str = "claude-sonnet-5-5";

const HELP: &str = "/mode          toggle build / plan
/model <id>    switch model
/clear         start a new session
/help          this help
/exit          quit";

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(n).collect();
        t.push_str("\n...(truncated)");
        t
    }
}

fn arg<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v[k].as_str().ok_or_else(|| anyhow!("missing argument: {k}"))
}

fn tool_defs() -> Vec<Value> {
    let s = json!({"type": "string"});
    let i = json!({"type": "integer"});
    let b = json!({"type": "boolean"});
    let def = |name: &str, desc: &str, props: Value, req: Value| {
        json!({"name": name, "description": desc,
               "input_schema": {"type": "object", "properties": props, "required": req}})
    };
    vec![
        def("read", "Read a file (with line numbers) or list a directory.",
            json!({"path": s, "offset": i, "limit": i}), json!(["path"])),
        def("write", "Create or overwrite a file.",
            json!({"path": s, "content": s}), json!(["path", "content"])),
        def("edit", "Replace exact text in a file. Prefer this over write for changes.",
            json!({"path": s, "old": s, "new": s, "replace_all": b}), json!(["path", "old", "new"])),
        def("glob", "Find files by glob pattern, e.g. **/*.rs",
            json!({"pattern": s}), json!(["pattern"])),
        def("grep", "Regex search in file contents. Optional `include` is a filename glob.",
            json!({"pattern": s, "include": s}), json!(["pattern"])),
        def("bash", "Run a shell command in the workspace.",
            json!({"command": s}), json!(["command"])),
    ]
}

struct Agent {
    root: PathBuf,
    model: String,
    api_key: String,
    plan_mode: bool,
    messages: Vec<Value>,
    allowed: HashSet<String>,
    http: reqwest::blocking::Client,
    ui: Ui,
}

/// Events shared between the agent thread and the local web server.
struct Shared {
    events: Mutex<Vec<Value>>,
    busy: AtomicBool,
}

enum Ui {
    Cli,
    Gui { shared: Arc<Shared>, answers: mpsc::Receiver<String> },
}

impl Agent {
    fn mode_name(&self) -> &'static str {
        if self.plan_mode { "plan" } else { "build" }
    }

    // ---------------------------------------------------------- output
    /// All user-visible output goes through here so it works in both the terminal and the GUI.
    fn emit(&self, kind: &str, text: &str) {
        match &self.ui {
            Ui::Cli => match kind {
                "assistant" => println!("\n{text}"),
                "tool" => println!("\n> {text}"),
                "cmd" => println!("  $ {text}"),
                "error" => println!("\nError: {text}"),
                _ => println!("{text}"),
            },
            Ui::Gui { shared, .. } => {
                shared.events.lock().unwrap().push(json!({"kind": kind, "text": text}));
            }
        }
    }

    fn emit_status(&self) {
        if let Ui::Gui { shared, .. } = &self.ui {
            shared.events.lock().unwrap().push(json!({
                "kind": "status",
                "mode": self.mode_name(),
                "model": self.model,
                "root": self.root.display().to_string(),
            }));
        }
    }

    // ---------------------------------------------------------- helpers
    /// Resolve a path inside the workspace; refuse anything that escapes it.
    fn safe(&self, p: &str) -> Result<PathBuf> {
        let given = Path::new(p);
        let rel: PathBuf = if given.is_absolute() {
            given
                .strip_prefix(&self.root)
                .map_err(|_| anyhow!("path outside workspace: {p}"))?
                .to_path_buf()
        } else {
            given.to_path_buf()
        };
        let mut out = self.root.clone();
        for c in rel.components() {
            match c {
                Component::ParentDir => {
                    if out == self.root {
                        bail!("path outside workspace: {p}");
                    }
                    out.pop();
                }
                Component::Normal(s) => out.push(s),
                _ => {}
            }
        }
        Ok(out)
    }

    fn rel(&self, p: &Path) -> String {
        p.strip_prefix(&self.root).unwrap_or(p).to_string_lossy().replace('\\', "/")
    }

    fn files(&self) -> Vec<PathBuf> {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_entry(|e| {
                let n = e.file_name().to_string_lossy();
                e.depth() == 0 || !IGNORE.contains(&n.as_ref())
            })
            .filter_map(|e| e.ok())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.into_path())
            .collect()
    }

    fn confirm(&mut self, name: &str) -> bool {
        if self.allowed.contains(name) {
            return true;
        }
        if let Ui::Gui { answers, .. } = &self.ui {
            while answers.try_recv().is_ok() {} // drop stale answers
            self.emit("confirm", name);
            let a = answers.recv_timeout(Duration::from_secs(900)).unwrap_or_default();
            return match a.trim().chars().next() {
                Some('a') => {
                    self.allowed.insert(name.to_string());
                    true
                }
                Some('y') => true,
                _ => false,
            };
        }
        print!("  allow? [y]es / [n]o / [a]lways this session: ");
        io::stdout().flush().ok();
        let mut line = String::new();
        if io::stdin().read_line(&mut line).is_err() {
            return false;
        }
        match line.trim().to_lowercase().chars().next() {
            Some('a') => {
                self.allowed.insert(name.to_string());
                true
            }
            Some('y') => true,
            _ => false,
        }
    }

    fn show_diff(&self, path: &str, old: &str, new: &str) {
        let diff = similar::TextDiff::from_lines(old, new)
            .unified_diff()
            .header(&format!("a/{path}"), &format!("b/{path}"))
            .to_string();
        if diff.is_empty() {
            self.emit("info", "(no changes)");
        } else {
            self.emit("diff", &diff);
        }
    }

    // ------------------------------------------------------------ tools
    fn exec(&mut self, name: &str, input: &Value) -> String {
        let r = match name {
            "read" => self.t_read(input),
            "write" => self.t_write(input),
            "edit" => self.t_edit(input),
            "glob" => self.t_glob(input),
            "grep" => self.t_grep(input),
            "bash" => self.t_bash(input),
            other => Err(anyhow!("unknown tool: {other}")),
        };
        r.unwrap_or_else(|e| format!("Error: {e}"))
    }

    fn t_read(&self, v: &Value) -> Result<String> {
        let p = self.safe(arg(v, "path")?)?;
        if p.is_dir() {
            let mut names: Vec<String> = fs::read_dir(&p)?
                .filter_map(|e| e.ok())
                .map(|e| {
                    let n = e.file_name().to_string_lossy().to_string();
                    if e.path().is_dir() { format!("{n}/") } else { n }
                })
                .collect();
            names.sort();
            return Ok(names.join("\n"));
        }
        let text = String::from_utf8_lossy(&fs::read(&p).with_context(|| format!("reading {}", p.display()))?).to_string();
        let offset = v["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = v["limit"].as_u64().unwrap_or(2000) as usize;
        let out: Vec<String> = text
            .lines()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(i, l)| format!("{}\t{}", i + 1, l))
            .collect();
        Ok(if out.is_empty() { "(empty file)".into() } else { truncate(&out.join("\n"), 60_000) })
    }

    fn t_write(&mut self, v: &Value) -> Result<String> {
        let path = arg(v, "path")?;
        let content = arg(v, "content")?;
        let p = self.safe(path)?;
        let old = fs::read_to_string(&p).unwrap_or_default();
        self.show_diff(path, &old, content);
        if !self.confirm("write") {
            return Ok("User denied this write.".into());
        }
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&p, content)?;
        Ok(format!("Wrote {path}"))
    }

    fn t_edit(&mut self, v: &Value) -> Result<String> {
        let path = arg(v, "path")?;
        let old = arg(v, "old")?;
        let new = arg(v, "new")?;
        let all = v["replace_all"].as_bool().unwrap_or(false);
        let p = self.safe(path)?;
        let text = fs::read_to_string(&p).with_context(|| format!("reading {path}"))?;
        let n = text.matches(old).count();
        if n == 0 {
            return Ok("Error: `old` text not found. Read the file again and copy it exactly.".into());
        }
        if n > 1 && !all {
            return Ok(format!("Error: `old` matches {n} places. Add context or set replace_all."));
        }
        let updated = if all { text.replace(old, new) } else { text.replacen(old, new, 1) };
        self.show_diff(path, &text, &updated);
        if !self.confirm("edit") {
            return Ok("User denied this edit.".into());
        }
        fs::write(&p, updated)?;
        Ok(format!("Edited {path}"))
    }

    fn t_glob(&self, v: &Value) -> Result<String> {
        let m = globset::Glob::new(arg(v, "pattern")?)?.compile_matcher();
        let mut hits: Vec<String> = self
            .files()
            .iter()
            .map(|f| self.rel(f))
            .filter(|r| m.is_match(r))
            .collect();
        hits.sort();
        hits.truncate(200);
        Ok(if hits.is_empty() { "No matches.".into() } else { hits.join("\n") })
    }

    fn t_grep(&self, v: &Value) -> Result<String> {
        let rx = regex::Regex::new(arg(v, "pattern")?)?;
        let inc = match v["include"].as_str() {
            Some(g) => Some(globset::Glob::new(g)?.compile_matcher()),
            None => None,
        };
        let mut out = Vec::new();
        'files: for f in self.files() {
            if let Some(m) = &inc {
                if !m.is_match(f.file_name().unwrap_or_default()) {
                    continue;
                }
            }
            if fs::metadata(&f).map(|m| m.len()).unwrap_or(0) > 1_000_000 {
                continue;
            }
            let Ok(text) = fs::read_to_string(&f) else { continue };
            for (i, line) in text.lines().enumerate() {
                if rx.is_match(line) {
                    out.push(format!("{}:{}: {}", self.rel(&f), i + 1, truncate(line.trim(), 200)));
                    if out.len() >= 100 {
                        out.push("(truncated at 100)".into());
                        break 'files;
                    }
                }
            }
        }
        Ok(if out.is_empty() { "No matches.".into() } else { out.join("\n") })
    }

    fn t_bash(&mut self, v: &Value) -> Result<String> {
        let cmd = arg(v, "command")?;
        self.emit("cmd", cmd);
        if !self.confirm("bash") {
            return Ok("User denied this command.".into());
        }
        let mut c = if cfg!(windows) {
            let mut c = Command::new("cmd");
            c.args(["/C", cmd]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", cmd]);
            c
        };
        let o = c.current_dir(&self.root).output()?;
        let mut s = String::from_utf8_lossy(&o.stdout).to_string();
        s.push_str(&String::from_utf8_lossy(&o.stderr));
        let s = s.trim();
        Ok(if s.is_empty() {
            format!("(exit {}, no output)", o.status.code().unwrap_or(-1))
        } else {
            truncate(s, 20_000)
        })
    }

    // ------------------------------------------------------------ model
    fn system_prompt(&self) -> String {
        let mut s = format!(
            "You are a coding agent in a terminal, working in {}. Explore with read/glob/grep \
             before changing things. Make small, targeted edits with the edit tool. Be concise. \
             Stop when the task is done.",
            self.root.display()
        );
        if self.plan_mode {
            s.push_str(" You are in PLAN mode: you cannot modify anything. Investigate and propose a plan.");
        }
        if let Ok(rules) = fs::read_to_string(self.root.join("AGENTS.md")) {
            s.push_str(&format!("\n\nProject instructions (AGENTS.md):\n{}", truncate(&rules, 8000)));
        }
        s
    }

    fn call_api(&self) -> Result<Value> {
        let tools: Vec<Value> = tool_defs()
            .into_iter()
            .filter(|t| !self.plan_mode || READ_ONLY.contains(&t["name"].as_str().unwrap_or("")))
            .collect();
        let body = json!({
            "model": self.model,
            "max_tokens": 4096,
            "system": self.system_prompt(),
            "tools": tools,
            "messages": self.messages,
        });
        let resp = self
            .http
            .post(format!(
                "{}/v1/messages",
                env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| "https://api.anthropic.com".into()).trim_end_matches('/')
            ))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .context("request failed")?;
        let status = resp.status();
        let text = resp.text()?;
        if !status.is_success() {
            bail!("API error {status}: {text}");
        }
        Ok(serde_json::from_str(&text)?)
    }

    fn run_turn(&mut self) -> Result<()> {
        for _ in 0..MAX_STEPS {
            let resp = self.call_api()?;
            let content = resp["content"].as_array().cloned().unwrap_or_default();
            self.messages.push(json!({"role": "assistant", "content": content}));

            let mut results = Vec::new();
            for b in &content {
                match b["type"].as_str() {
                    Some("text") => {
                        let t = b["text"].as_str().unwrap_or("");
                        if !t.trim().is_empty() {
                            self.emit("assistant", t);
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or("");
                        let id = b["id"].as_str().unwrap_or("");
                        self.emit("tool", &format!("{name} {}", truncate(&b["input"].to_string(), 100)));
                        let out = self.exec(name, &b["input"]);
                        results.push(json!({"type": "tool_result", "tool_use_id": id, "content": out}));
                    }
                    _ => {}
                }
            }
            if results.is_empty() {
                return Ok(());
            }
            self.messages.push(json!({"role": "user", "content": results}));
        }
        self.emit("info", "[stopped: reached step limit]");
        Ok(())
    }

    fn save(&self) {
        let dir = self.root.join(".agent");
        let _ = fs::create_dir_all(&dir);
        if let Ok(s) = serde_json::to_string_pretty(&self.messages) {
            let _ = fs::write(dir.join("session.json"), s);
        }
    }
}

/// Handle one line of input (a slash command or a prompt). Returns false to quit.
fn handle_input(agent: &mut Agent, text: &str) -> bool {
    match text {
        "/exit" | "/quit" => return false,
        "/help" => agent.emit("info", HELP),
        "/mode" => {
            agent.plan_mode = !agent.plan_mode;
            agent.emit("info", &format!("mode: {}", agent.mode_name()));
        }
        "/clear" => {
            agent.messages.clear();
            agent.allowed.clear();
            agent.emit("info", "new session");
        }
        t if t.starts_with("/model") => {
            if let Some(m) = t.split_whitespace().nth(1) {
                agent.model = m.to_string();
            }
            agent.emit("info", &format!("model: {}", agent.model));
        }
        _ => {
            let mark = agent.messages.len();
            agent.messages.push(json!({"role": "user", "content": text}));
            if let Err(e) = agent.run_turn() {
                agent.messages.truncate(mark); // drop the half-finished turn
                agent.emit("error", &e.to_string());
            }
            agent.save();
        }
    }
    agent.emit_status();
    true
}

// ------------------------------------------------------------------ GUI
fn make_token() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    (0..2).map(|_| format!("{:016x}", RandomState::new().build_hasher().finish())).collect()
}

fn open_browser(url: &str) {
    let _ = if cfg!(windows) {
        Command::new("cmd").args(["/C", "start", "", url]).spawn()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(url).spawn()
    } else {
        Command::new("xdg-open").arg(url).spawn()
    };
}

fn header_of(req: &tiny_http::Request, name: &'static str) -> Option<String> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str().to_string())
}

fn reply(req: tiny_http::Request, code: u16, ctype: &str, body: String) {
    let h = tiny_http::Header::from_bytes(&b"Content-Type"[..], ctype.as_bytes()).unwrap();
    let _ = req.respond(tiny_http::Response::from_string(body).with_header(h).with_status_code(tiny_http::StatusCode(code)));
}

fn serve(agent: Agent, shared: Arc<Shared>, answers: mpsc::Sender<String>, port: u16, first: String) -> Result<()> {
    let token = make_token();
    let (cmd_tx, cmd_rx) = mpsc::channel::<String>();

    // The agent lives on its own thread so the web server never blocks on the model.
    {
        let shared = shared.clone();
        thread::spawn(move || {
            let mut agent = agent;
            agent.emit_status();
            while let Ok(text) = cmd_rx.recv() {
                let keep = handle_input(&mut agent, &text);
                shared.busy.store(false, Ordering::SeqCst);
                if !keep {
                    std::process::exit(0);
                }
            }
        });
    }

    let submit = |text: String| {
        shared.busy.store(true, Ordering::SeqCst);
        shared.events.lock().unwrap().push(json!({"kind": "user", "text": text}));
        let _ = cmd_tx.send(text);
    };
    if !first.is_empty() {
        submit(first);
    }

    let server = tiny_http::Server::http(("127.0.0.1", port)).map_err(|e| anyhow!("cannot listen on port {port}: {e}"))?;
    let url = format!("http://127.0.0.1:{port}/?t={token}");
    println!("GUI running at {url}\n(press Ctrl+C here to stop)");
    open_browser(&url);

    let hosts = [format!("127.0.0.1:{port}"), format!("localhost:{port}")];
    for mut req in server.incoming_requests() {
        // Anything that can run shell commands must not be reachable from other web pages:
        // only accept our own Host header and a secret token.
        let host_ok = header_of(&req, "Host").map(|h| hosts.contains(&h)).unwrap_or(false);
        let full = req.url().to_string();
        let (path, query) = full.split_once('?').unwrap_or((full.as_str(), ""));
        let tok_ok = header_of(&req, "X-Token").as_deref() == Some(token.as_str())
            || query.split('&').any(|kv| kv == format!("t={token}"));
        if !host_ok || !tok_ok {
            reply(req, 403, "text/plain", "forbidden".into());
            continue;
        }
        let method = req.method().as_str().to_string();
        match (method.as_str(), path) {
            ("GET", "/") => reply(req, 200, "text/html; charset=utf-8", GUI_HTML.to_string()),
            ("GET", "/events") => {
                let since: usize = query.split('&').find_map(|kv| kv.strip_prefix("since=")).and_then(|v| v.parse().ok()).unwrap_or(0);
                let ev = shared.events.lock().unwrap();
                let from = since.min(ev.len());
                let body = json!({"events": &ev[from..], "next": ev.len(), "busy": shared.busy.load(Ordering::SeqCst)});
                drop(ev);
                reply(req, 200, "application/json", body.to_string());
            }
            ("POST", "/send") => {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let text = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["text"].as_str().map(|t| t.trim().to_string())).unwrap_or_default();
                if text.is_empty() {
                    reply(req, 400, "application/json", r#"{"error":"empty"}"#.into());
                } else if shared.busy.load(Ordering::SeqCst) {
                    reply(req, 409, "application/json", r#"{"error":"busy"}"#.into());
                } else {
                    submit(text);
                    reply(req, 200, "application/json", "{}".into());
                }
            }
            ("POST", "/answer") => {
                let mut body = String::new();
                let _ = req.as_reader().read_to_string(&mut body);
                let a = serde_json::from_str::<Value>(&body).ok().and_then(|v| v["answer"].as_str().map(String::from)).unwrap_or_default();
                let _ = answers.send(a);
                reply(req, 200, "application/json", "{}".into());
            }
            _ => reply(req, 404, "text/plain", "not found".into()),
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let mut model = env::var("MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let mut plan_mode = false;
    let mut cont = false;
    let mut gui = false;
    let mut port: u16 = 7878;
    let mut prompt = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--plan" => plan_mode = true,
            "-c" | "--continue" => cont = true,
            "--gui" => gui = true,
            "--port" => port = args.next().and_then(|p| p.parse().ok()).ok_or_else(|| anyhow!("--port needs a number"))?,
            "-m" | "--model" => model = args.next().ok_or_else(|| anyhow!("-m needs a model id"))?,
            _ => prompt.push(a),
        }
    }

    let api_key = env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow!("ANTHROPIC_API_KEY is not set (put it in a .env file or your environment)"))?;
    let root = env::current_dir()?.canonicalize()?;

    let shared = Arc::new(Shared { events: Mutex::new(Vec::new()), busy: AtomicBool::new(false) });
    let (answer_tx, answer_rx) = mpsc::channel::<String>();
    let ui = if gui { Ui::Gui { shared: shared.clone(), answers: answer_rx } } else { Ui::Cli };

    let mut agent = Agent {
        root,
        model,
        api_key,
        plan_mode,
        messages: Vec::new(),
        allowed: HashSet::new(),
        http: reqwest::blocking::Client::builder()
            .timeout(std::time::Duration::from_secs(300))
            .build()?,
        ui,
    };

    if cont {
        if let Ok(s) = fs::read_to_string(agent.root.join(".agent/session.json")) {
            if let Ok(Value::Array(m)) = serde_json::from_str(&s) {
                let n = m.len();
                agent.messages = m;
                agent.emit("info", &format!("Resumed {n} messages."));
            }
        }
    }

    agent.emit("info", &format!("agent  {}\nmodel: {}   mode: {}   (/help)", agent.root.display(), agent.model, agent.mode_name()));
    let mut queued = prompt.join(" ");

    if gui {
        return serve(agent, shared, answer_tx, port, queued);
    }

    loop {
        let text = if !queued.is_empty() {
            std::mem::take(&mut queued)
        } else {
            print!("\n{}> ", agent.mode_name());
            io::stdout().flush()?;
            let mut line = String::new();
            if io::stdin().read_line(&mut line)? == 0 {
                break;
            }
            line.trim().to_string()
        };
        if text.is_empty() {
            continue;
        }
        if !handle_input(&mut agent, &text) {
            break;
        }
    }
    Ok(())
}
