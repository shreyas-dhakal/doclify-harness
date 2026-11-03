//! Local documentation viewer. The host owns the returned server task and URL.
use axum::{
    body::Body,
    extract::Request,
    extract::{Query, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use rand::{rngs::OsRng, RngCore};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};
use tokio::sync::Mutex;

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;
#[derive(Clone)]
pub struct Viewer {
    root: PathBuf,
    token: String,
    lock: Arc<Mutex<()>>,
}
#[derive(Clone)]
struct App {
    viewer: Viewer,
    origin: String,
}
#[derive(Serialize)]
struct Document {
    path: String,
    content: String,
    revision: String,
}
#[derive(Deserialize)]
struct DocumentPath {
    path: String,
}
#[derive(Deserialize)]
struct Save {
    path: String,
    content: String,
    revision: String,
}

impl Viewer {
    pub fn new(repo: impl AsRef<Path>) -> std::io::Result<Self> {
        let root = repo.as_ref().canonicalize()?;
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Ok(Self {
            root,
            token: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            lock: Arc::new(Mutex::new(())),
        })
    }
    pub fn token(&self) -> &str {
        &self.token
    }
    pub fn router(&self, origin: &str) -> Router {
        let state = App {
            viewer: self.clone(),
            origin: origin.to_owned(),
        };
        Router::new()
            .route("/api/documents", get(list))
            .route("/api/document", get(read).put(save))
            .route("/api/records", get(records))
            .route("/api/review", axum::routing::post(review))
            .fallback(get(asset))
            .layer(middleware::from_fn_with_state(state.clone(), check_host))
            .with_state(state)
    }
    /// Binds an ephemeral IPv4 loopback port. The token is in the fragment, never sent as a URL parameter.
    pub async fn serve(
        &self,
    ) -> std::io::Result<(String, tokio::task::JoinHandle<std::io::Result<()>>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let origin = format!("http://{}", listener.local_addr()?);
        let url = format!("{origin}/#{}", self.token);
        let app = self.router(&origin);
        Ok((
            url,
            tokio::spawn(async move { axum::serve(listener, app).await }),
        ))
    }
    fn doc_path(&self, path: &str) -> Result<PathBuf, StatusCode> {
        let rel = Path::new(path);
        if path.is_empty()
            || !rel.components().all(|c| matches!(c, Component::Normal(_)))
            || !matches!(rel.extension().and_then(|s| s.to_str()), Some("md" | "mdx"))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let base = self
            .root
            .join("docs/doclify")
            .canonicalize()
            .map_err(|_| StatusCode::NOT_FOUND)?;
        if !base.starts_with(&self.root) {
            return Err(StatusCode::FORBIDDEN);
        }
        let file = base
            .join(rel)
            .canonicalize()
            .map_err(|_| StatusCode::NOT_FOUND)?;
        if !file.starts_with(&base) || !file.is_file() {
            return Err(StatusCode::FORBIDDEN);
        }
        Ok(file)
    }
}
async fn check_host(State(app): State<App>, request: Request, next: Next) -> Response {
    let expected = app.origin.strip_prefix("http://").unwrap_or("");
    if request
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        != Some(expected)
    {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}
fn revision(s: &str) -> String {
    format!("{:x}", Sha256::digest(s.as_bytes()))
}
fn walk(base: &Path, dir: &Path, out: &mut Vec<String>, extensions: &[&str]) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if kind.is_dir() {
            walk(base, &path, out, extensions);
        } else if kind.is_file()
            && path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|s| extensions.contains(&s))
        {
            if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}
async fn list(State(app): State<App>) -> Json<Vec<String>> {
    let base = app.viewer.root.join("docs/doclify");
    let mut files = Vec::new();
    if base
        .canonicalize()
        .is_ok_and(|p| p.starts_with(&app.viewer.root))
    {
        walk(&base, &base, &mut files, &["md", "mdx"]);
    }
    files.sort();
    Json(files)
}
async fn read(
    State(app): State<App>,
    Query(q): Query<DocumentPath>,
) -> Result<Json<Document>, StatusCode> {
    let file = app.viewer.doc_path(&q.path)?;
    let content = std::fs::read_to_string(file).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(Document {
        path: q.path,
        revision: revision(&content),
        content,
    }))
}
async fn save(
    State(app): State<App>,
    headers: HeaderMap,
    Json(data): Json<Save>,
) -> Result<Json<Document>, StatusCode> {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(app.origin.as_str())
        || headers.get("x-doclify-token").and_then(|v| v.to_str().ok()) != Some(app.viewer.token())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let _guard = app.viewer.lock.lock().await;
    let file = app.viewer.doc_path(&data.path)?;
    let current = std::fs::read_to_string(&file).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if revision(&current) != data.revision {
        return Err(StatusCode::CONFLICT);
    }
    use std::io::Write;
    let mut temp =
        tempfile::NamedTempFile::new_in(file.parent().ok_or(StatusCode::INTERNAL_SERVER_ERROR)?)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    temp.as_file()
        .set_permissions(
            std::fs::metadata(&file)
                .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
                .permissions(),
        )
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    temp.write_all(data.content.as_bytes())
        .and_then(|_| temp.as_file().sync_all())
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    temp.persist(file)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(Document {
        path: data.path,
        revision: revision(&data.content),
        content: data.content,
    }))
}
async fn records(State(app): State<App>) -> Result<Json<Vec<doclify_domain::Record>>, StatusCode> {
    let base = app.viewer.root.join(".doclify/records");
    if !base.exists() {
        return Ok(Json(Vec::new()));
    }
    if !base
        .canonicalize()
        .is_ok_and(|p| p.starts_with(&app.viewer.root))
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let store = doclify_domain::Store::open(&app.viewer.root)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut result = Vec::new();
    for entry in std::fs::read_dir(base)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .flatten()
    {
        if !entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let id = entry.file_name().to_string_lossy().into_owned();
        let history = store
            .history(&id)
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
        if let Some(record) = history.last() {
            result.push(record.clone());
        }
    }
    result.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(Json(result))
}
#[derive(Deserialize)]
struct Review {
    id: String,
    version: u64,
    action: String,
}
async fn review(
    State(app): State<App>,
    headers: HeaderMap,
    Json(data): Json<Review>,
) -> Result<Json<doclify_domain::Record>, StatusCode> {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(app.origin.as_str())
        || headers.get("x-doclify-token").and_then(|v| v.to_str().ok()) != Some(app.viewer.token())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let state = match data.action.as_str() {
        "accept" => doclify_domain::ReviewState::Accepted,
        "reject" => doclify_domain::ReviewState::Rejected,
        "supersede" => doclify_domain::ReviewState::Superseded,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let _guard = app.viewer.lock.lock().await;
    let base = app.viewer.root.join(".doclify/records");
    if !base
        .canonicalize()
        .is_ok_and(|p| p.starts_with(&app.viewer.root))
    {
        return Err(StatusCode::FORBIDDEN);
    }
    let dir = base.join(&data.id);
    if !dir.symlink_metadata().is_ok_and(|m| m.file_type().is_dir()) {
        return Err(StatusCode::BAD_REQUEST);
    }
    let store = doclify_domain::Store::open(&app.viewer.root)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    store
        .review(&data.id, data.version, state)
        .map(Json)
        .map_err(|_| StatusCode::CONFLICT)
}
async fn asset(uri: axum::http::Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => Response::builder().header(header::CONTENT_TYPE, mime_guess::from_path(path).first_or_octet_stream().as_ref()).header("content-security-policy", "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'").header("x-content-type-options", "nosniff").body(Body::from(file.data.into_owned())).unwrap(),
        None => StatusCode::NOT_FOUND.into_response()
    }
}
