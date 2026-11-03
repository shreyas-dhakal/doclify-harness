# Doclify viewer

A standalone library for integration by the Doclify host. Build the checked-in production assets with `npm ci --prefix viewer/web && npm run build --prefix viewer/web`, then build the Rust crate. `Viewer::new(repo)?.serve().await?` returns a loopback URL (including the per-launch token in its fragment) and a server task; open the URL in the browser and retain or abort the task with the host's lifecycle. Add this crate to the workspace and call it from `/docs` when integrating; this branch deliberately does not change the CLI or workspace manifest.

Documents are existing `.md` and `.mdx` files in `docs/doclify`. The review queue uses the latest version from the domain store and explicit review appends a new version. Writes check the current content hash and serialize requests in the viewer process; external writers do not share its lock. MDX with JSX, expressions or imports is displayed and edited as inert source. This conservative fallback avoids evaluating or discarding unsupported blocks; rich editing is available for simple Markdown. No regeneration or conflict-merging service is provided here.

Run `cargo test --manifest-path viewer/Cargo.toml` for the local server tests. Assets in `web/dist` are committed so a release build does not require Node.
