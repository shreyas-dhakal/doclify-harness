# Doclify

Doclify captures Codex and Claude Code sessions, turns them into documentation proposals, and carries reviewed decisions into later agent sessions. It runs locally in a Git repository. Decisions become accepted only when you review them.

## Setup

Install Rust and Cargo, then build and install Doclify:

```sh
git clone https://github.com/shreyas-dhakal/doclify-harness.git
cd doclify-harness
cargo build --release -p doclify
install -m 755 target/release/doclify /usr/local/bin/doclify
```

Use another directory on your `PATH` if `/usr/local/bin` is not writable. Node is only needed to rebuild the viewer assets.

In the repository you want to document:

```sh
cd /path/to/your/repository
doclify init
doclify model set claude
doclify doctor
```

`init` installs agent hooks and configures background recovery. Codex requires you to trust the project and approve the hooks in `/hooks`. The Claude model bridge uses your existing Claude Code login. You can instead configure an OpenAI-compatible API:

```sh
export OPENAI_API_KEY='your-key'
doclify model set openai https://api.openai.com/v1 OPENAI_API_KEY your-model-id
```

## Use

Run `doclify` for the interactive prompt, or use the commands directly:

```sh
doclify sync                    # capture sessions and update documentation
doclify status                  # inspect capture and review queues
doclify review                  # list proposals
doclify review <id> <version> accept
doclify handoff "authentication" # get reviewed context for another session
doclify docs                    # open the local viewer and editor
```

In the prompt, use `/model`, `/sync`, `/review`, `/handoff`, `/docs`, `/status`, and `/quit`. Generated documentation is stored in `docs/doclify/`; reviewed records are stored in `.doclify/records/`. Share them through your normal Git workflow.

The Codex documentation-model bridge is currently disabled because the installed CLI does not provide a verified tool-free mode. Codex session capture remains supported. See [live verification](docs/live-verification.md) for the tested agent versions and known limitations.

## Development

```sh
cargo test --workspace
npm ci --prefix viewer/web
npm test --prefix viewer/web
```

To rebuild the embedded viewer assets before a release build, run `npm run build --prefix viewer/web`. Use `doclify uninstall` in a repository to remove its hooks and recovery entry; captured data is retained.
