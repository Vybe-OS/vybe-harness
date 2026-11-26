# vybe-harness

Coding agent for the terminal, with an optional browser GUI.

## Setup
1. Install Rust: https://rustup.rs
2. `cp env.example .env` and put your ANTHROPIC_API_KEY in it
3. `cargo build --release`  ->  target/release/agent

## Run
Run it from the folder you want the agent to work in:

    agent --gui            # opens a window in your browser (http://127.0.0.1:7878)
    agent --gui --port 9000
    agent                  # classic terminal mode
    agent --plan | --continue | -m <model> | "first prompt"

If you run it outside this folder, set ANTHROPIC_API_KEY as an environment variable
(the .env file is only read from the current folder or its parents).

GUI notes: it only listens on 127.0.0.1 and requires a random token (printed URL),
so other websites/processes can't drive it. Writes, edits and shell commands
always ask for approval (Allow / Always this session / Deny).
