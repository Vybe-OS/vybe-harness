//!
//!   small command
//!   agent                 interactive chat in the current folder
//!   agent "fix the bug"   start with a prompt
//!   agent --continue      resume the last session
//!   agent --plan          start in read-only plan mode
//!   agent -m <model>      pick a model

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::{env, fs};

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
}

impl Agent {
    fn mode_name(&self) -> &'static str {
        if self.plan_mode { "plan" } else { "build" }
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
            println!("(no changes)");
        } else {
            println!("{diff}");
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
        println!("  $ {cmd}");
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
            .post("https://api.anthropic.com/v1/messages")
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
                            println!("\n{t}");
                        }
                    }
                    Some("tool_use") => {
                        let name = b["name"].as_str().unwrap_or("");
                        let id = b["id"].as_str().unwrap_or("");
                        println!("\n> {name} {}", truncate(&b["input"].to_string(), 100));
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
        println!("\n[stopped: reached step limit]");
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

fn main() -> Result<()> {
    let _ = dotenvy::dotenv();

    let mut model = env::var("MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string());
    let mut plan_mode = false;
    let mut cont = false;
    let mut prompt = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--plan" => plan_mode = true,
            "-c" | "--continue" => cont = true,
            "-m" | "--model" => model = args.next().ok_or_else(|| anyhow!("-m needs a model id"))?,
            _ => prompt.push(a),
        }
    }

    let api_key = env::var("ANTHROPIC_API_KEY")
        .map_err(|_| anyhow!("ANTHROPIC_API_KEY is not set (put it in a .env file or your environment)"))?;
    let root = env::current_dir()?.canonicalize()?;

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
    };

    if cont {
        if let Ok(s) = fs::read_to_string(agent.root.join(".agent/session.json")) {
            if let Ok(Value::Array(m)) = serde_json::from_str(&s) {
                println!("Resumed {} messages.", m.len());
                agent.messages = m;
            }
        }
    }

    println!("agent  {}\nmodel: {}   mode: {}   (/help)", agent.root.display(), agent.model, agent.mode_name());
    let mut queued = prompt.join(" ");

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
        match text.as_str() {
            "/exit" | "/quit" => break,
            "/help" => println!("{HELP}"),
            "/mode" => {
                agent.plan_mode = !agent.plan_mode;
                println!("mode: {}", agent.mode_name());
            }
            "/clear" => {
                agent.messages.clear();
                agent.allowed.clear();
                println!("new session");
            }
            t if t.starts_with("/model") => {
                if let Some(m) = t.split_whitespace().nth(1) {
                    agent.model = m.to_string();
                }
                println!("model: {}", agent.model);
            }
            _ => {
                let mark = agent.messages.len();
                agent.messages.push(json!({"role": "user", "content": text}));
                if let Err(e) = agent.run_turn() {
                    agent.messages.truncate(mark); // drop the half-finished turn
                    println!("\nError: {e}");
                }
                agent.save();
            }
        }
    }
    Ok(())
}
