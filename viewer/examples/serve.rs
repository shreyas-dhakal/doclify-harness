//! Serves a repository for manual checks and the browser tests: `cargo run --example serve -- <repo>`.
#[tokio::main]
async fn main() -> std::io::Result<()> {
    let repo = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let (url, task) = doclify_viewer::Viewer::new(repo)?.serve().await?;
    println!("{url}");
    task.await?
}
