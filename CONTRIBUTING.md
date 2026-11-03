# Contributing to Doclify

Thanks for helping improve Doclify. Bug reports, documentation improvements, and code contributions are welcome.

## Report a problem or suggest a change

Open an issue describing the problem or idea. For bugs, include the steps to reproduce it, the expected and actual behavior, and relevant versions or logs. Remove any credentials and private session content before sharing logs. For a substantial change, consider opening an issue first to discuss the approach.

## Set up for development

Install Rust and Cargo. Node.js and npm are also needed for work on the viewer. Clone the repository and run the checks relevant to your change:

```sh
cargo test --workspace
npm ci --prefix viewer/web
npm test --prefix viewer/web
```

To rebuild the embedded viewer assets, run `npm run build --prefix viewer/web`. See the [README](README.md) for setup and usage details.

## Submit a change

1. Create a branch for your work and make a focused change.
2. Add or update tests and documentation where appropriate.
3. Run the relevant checks above and note any that you could not run.
4. Open a pull request describing the change, why it is needed, and how you tested it. Link any related issues.

By submitting a contribution, you agree that it may be distributed under the [MIT License](LICENSE).
