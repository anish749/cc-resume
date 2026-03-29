# claude-resume

Semantic search and resume for Claude Code sessions.

Search across all your past Claude Code conversations — by topic, concept, or meaning — regardless of which project or directory you were in. Pick a result and resume it instantly.

## How it works

Claude Code stores every session as JSONL files in `~/.claude/projects/`. This tool exports them to clean markdown, indexes them with [QMD](https://github.com/tobi/qmd) (a local semantic search engine), and gives you a TUI to search and resume.

A background file watcher keeps the index fresh in real-time — even mid-session.

## Usage

```
claude-resume              # Launch TUI
claude-resume search "q"   # CLI search
claude-resume index        # Reindex all sessions
claude-resume daemon start # Start live file watcher
claude-resume setup        # Guided first-time setup
```

## TUI

- Type to search (debounced, hybrid semantic + keyword)
- Arrow keys / j/k to navigate results
- Preview pane shows conversation content
- Enter to resume the selected session via `claude --resume`
- Tab to switch between search and results

## Install

```
npm install -g @tobilu/qmd
cargo install --git https://github.com/anish749/cc-resume
claude-resume setup
```

Requires Node.js (for QMD) and a Rust toolchain.

## License

MIT
