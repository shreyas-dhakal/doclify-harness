# Doclify

Doclify captures Codex and Claude Code sessions, turns them into documentation proposals, and carries reviewed decisions into later agent sessions. It runs locally in a Git repository. Decisions become accepted only when you review them.

## Setup

### Homebrew

On macOS, install the current `main` branch with Homebrew (Rust is installed as a build dependency):

```sh
brew tap shreyas-dhakal/doclify https://github.com/shreyas-dhakal/doclify-harness
brew install --HEAD doclify
```

Then, in the repository you want to document:

```sh
cd /path/to/your/repository
doclify init
doclify doctor
```

### Build from source

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
doclify init --model claude
doclify doctor
```

`init` installs agent hooks and configures background recovery. Codex requires you to trust the project and approve the hooks in `/hooks`. The Claude model bridge uses your existing Claude Code login. You can instead configure an OpenAI-compatible API:

```sh
export OPENAI_API_KEY='your-key'
doclify init --model openai https://api.openai.com/v1 OPENAI_API_KEY your-model-id
```

## Use

Run `doclify` inside a Git repository to open the chat-first TUI. Describe in
free text what the documentation should explain; Doclify drafts suggestions
for review. Suggestions are not added to the docs until you approve them. Use
`/review` to inspect pending suggestions, then accept, reject, or supersede each
one. `/help` lists the chat commands. Other slash commands are `/sync` to
import saved sessions, `/status` to see setup and pending work, `/handoff [topic]` to prepare approved
project notes, `/docs` to open the docs in your
browser, `/model` to choose an AI, and `/quit` to exit.

The TUI shows these keyboard hints: Enter sends your message, Up/Down browse
input history, Page Up/Page Down scroll the conversation, and Ctrl-C exits.
Left/Right move the cursor; Home/End move it to the start/end of the input.
Escape also exits when no operation is running. In the AI picker, Up/Down
select a model, Enter chooses it, and Escape closes the picker.

For scripts or direct terminal use, run the commands explicitly:

```sh
doclify sync                    # capture sessions and update documentation
doclify status                  # inspect capture and review queues
doclify unsupported             # inspect unsupported transcript details
doclify unsupported replay      # retry them after an adapter or transcript update
doclify review                  # list proposals
doclify review <id> <version> accept   # approve a proposal (or reject/supersede)
doclify handoff "authentication"      # get approved context for another session
doclify docs                    # open the local docs viewer
doclify metrics report          # inspect opt-in, local product measurements
```

Product measurements are disabled by default. Run `doclify metrics enable` to
count captured events, extracted jobs, rejected or superseded review proposals,
generated handoffs, and user-reported repeated explanations. The report also
shows last-sync capture coverage (sessions with captured content divided by
sessions discovered). Report a repeated explanation with
`doclify metrics repeated-explanation`. Data stays in the Git directory at
`.git/doclify/metrics.json`, is excluded from Git, and is never uploaded.
Disable collection with `doclify metrics disable`.

In the prompt, use `/model`, `/sync`, `/review`, `/handoff`, `/docs`, `/status`, and `/quit`. Generated documentation is stored in `docs/doclify/`; reviewed records are stored in `.doclify/records/`. Share them through your normal Git workflow.

The Codex documentation-model bridge is currently disabled because the installed CLI does not provide a verified tool-free mode. Codex session capture remains supported. See [live verification](docs/live-verification.md) for the tested agent versions and known limitations.

## Development

```sh
cargo test --workspace
npm ci --prefix viewer/web
npm test --prefix viewer/web
```

To rebuild the embedded viewer assets before a release build, run `npm run build --prefix viewer/web`. Use `doclify uninstall` in a repository to remove its hooks and recovery entry; captured data is retained.
