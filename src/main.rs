//! `cj` binary entrypoint — argument parsing and dispatch.
//!
//! All logic lives in `claude_jam::*` (the lib) or in the binary-side
//! `commands` / `tui` modules. This file is intentionally thin.

mod commands;
mod tui;

use claude_jam::db::open_db;

use commands::{
    cmd_context, cmd_hook, cmd_import, cmd_init, cmd_milestone, cmd_remove, cmd_setup,
    cmd_teardown, cmd_topic,
};
use tui::run_tui;

fn print_help() {
    println!("cj - Claude Jam: monitor and manage Claude Code sessions");
    println!();
    println!("Usage:");
    println!("  cj                   Launch TUI dashboard");
    println!("  cj -q                Launch TUI, quit after selecting a session");
    println!("  cj -b                Borderless mode (no title bar / border)");
    println!("  cj -v                Vertical mode (detail line below the title)");
    println!("  cj init [-s name] <topic>  Pre-register session with topic (-s for explicit tmux session)");
    println!("  cj topic [--session-id <id>] <text>      Set topic (auto-detects via tmux, or use --session-id)");
    println!("  cj milestone [--session-id <id>] <text>  Add milestone (auto-detects via tmux, or use --session-id)");
    println!(
        "  cj context [--session-id <id>]           Print context usage as 'used/total' tokens"
    );
    println!(
        "  cj import            Import all tmux sessions into cj (skips ones already tracked)"
    );
    println!("  cj remove <tmux>     Remove all sessions for a tmux session");
    println!(
        "  cj setup [--check]   Wire cj into Claude Code (hooks, permission, CLAUDE.md). --check is read-only"
    );
    println!("  cj teardown          Reverse cj setup; preserves the database");
    println!("  cj hook              Process hook event from stdin (used by claude-jam.sh)");
    println!();
    println!("TUI keys:");
    println!("  j/k        Navigate sessions");
    println!("  1-9        Jump to session by number");
    println!("  Ctrl-a..z  Jump to session by letter (after 9)");
    println!("  Enter      Switch to session's tmux session");
    println!("  o          Expand/collapse milestone history");
    println!("  d          Delete session");
    println!("  q          Quit");
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();

    // Subcommands that must work BEFORE the database exists — fresh installs,
    // help text, and the setup/teardown entrypoints themselves.
    if args.iter().any(|a| a == "-h" || a == "--help") {
        print_help();
        return Ok(());
    }
    match args.get(1).map(|s| s.as_str()) {
        Some("setup") => {
            let check_only = args[2..].iter().any(|a| a == "--check");
            cmd_setup(check_only);
            return Ok(());
        }
        Some("teardown") => {
            cmd_teardown();
            return Ok(());
        }
        _ => {}
    }

    // Everything below needs the database.
    let conn = open_db()?;

    match args.get(1).map(|s| s.as_str()) {
        Some("hook") => cmd_hook(&conn),
        Some("init") => {
            let rest = &args[2..];
            let (tmux_override, topic_args) = if rest.len() >= 2 && rest[0] == "-s" {
                (Some(rest[1].as_str()), &rest[2..])
            } else {
                (None, rest)
            };
            let text = topic_args.join(" ");
            if text.is_empty() {
                eprintln!("Usage: cj init [-s <tmux-session>] <topic>");
                std::process::exit(1);
            }
            cmd_init(&conn, tmux_override, &text);
        }
        Some("import") => cmd_import(&conn),
        Some("remove") => {
            let name = args[2..].join(" ");
            if name.is_empty() {
                eprintln!("Usage: cj remove <tmux-session>");
                std::process::exit(1);
            }
            cmd_remove(&conn, &name);
        }
        Some("topic") => {
            let rest = &args[2..];
            let (sid_override, text_args) = if rest.len() >= 2 && rest[0] == "--session-id" {
                (Some(rest[1].as_str()), &rest[2..])
            } else {
                (None, rest)
            };
            let text = text_args.join(" ");
            if text.is_empty() {
                eprintln!("Usage: cj topic [--session-id <id>] <description>");
                std::process::exit(1);
            }
            cmd_topic(&conn, sid_override, &text);
        }
        Some("milestone") => {
            let rest = &args[2..];
            let (sid_override, text_args) = if rest.len() >= 2 && rest[0] == "--session-id" {
                (Some(rest[1].as_str()), &rest[2..])
            } else {
                (None, rest)
            };
            let text = text_args.join(" ");
            if text.is_empty() {
                eprintln!("Usage: cj milestone [--session-id <id>] <description>");
                std::process::exit(1);
            }
            cmd_milestone(&conn, sid_override, &text);
        }
        Some("context") => {
            let rest = &args[2..];
            let sid_override = if rest.len() >= 2 && rest[0] == "--session-id" {
                Some(rest[1].as_str())
            } else {
                None
            };
            cmd_context(&conn, sid_override);
        }
        _ => {
            let quit_on_select = args.iter().any(|a| a == "-q" || a == "--quit");
            let borderless = args.iter().any(|a| a == "-b" || a == "--borderless");
            let vertical = args.iter().any(|a| a == "-v" || a == "--vertical");
            run_tui(&conn, quit_on_select, borderless, vertical)?;
        }
    }

    Ok(())
}
