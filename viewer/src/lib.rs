//! Local documentation viewer. The host owns the returned server task and URL.
#![allow(clippy::result_large_err)] // Handlers return axum Responses as errors.
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
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use rand::{rngs::OsRng, RngCore};
use rust_embed::RustEmbed;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
};

#[derive(RustEmbed)]
#[folder = "web/dist/"]
struct Assets;
#[derive(Clone)]
pub struct Viewer {
    root: PathBuf,
    token: String,
    dir: Arc<Dir>,
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
            dir: Arc::new(Dir::open_ambient_dir(&root, ambient_authority())?),
            root,
            token: bytes.iter().map(|b| format!("{b:02x}")).collect(),
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
            .route("/api/search", get(search))
            .route("/api/record-source", get(record_source))
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
    /// Opens the document's directory as a descriptor beneath the root opened at launch. Every
    /// later access is relative to it, so a component swapped for a symlink cannot escape.
    fn open_doc(&self, path: &str) -> Result<(Dir, Dir, String), StatusCode> {
        let rel = Path::new(path);
        if path.is_empty()
            || !rel.components().all(|c| matches!(c, Component::Normal(_)))
            || !matches!(rel.extension().and_then(|s| s.to_str()), Some("md" | "mdx"))
        {
            return Err(StatusCode::BAD_REQUEST);
        }
        let not_found = |_| StatusCode::NOT_FOUND;
        let docs = self.dir.open_dir("docs/doclify").map_err(not_found)?;
        let dir = match rel.parent().filter(|p| !p.as_os_str().is_empty()) {
            Some(parent) => docs.open_dir(parent).map_err(not_found)?,
            None => docs.try_clone().map_err(not_found)?,
        };
        let name = rel.file_name().unwrap().to_string_lossy().into_owned();
        match dir.symlink_metadata(&name) {
            Ok(m) if m.is_file() => Ok((docs, dir, name)),
            Ok(_) => Err(StatusCode::FORBIDDEN),
            Err(_) => Err(StatusCode::NOT_FOUND),
        }
    }
    /// Revision-checked atomic replace. An exclusive lock on the docs directory serializes viewer
    /// tabs and processes; a writer that ignores it (an editor, git) is caught by the second check
    /// unless it lands in the instant between that check and the rename.
    fn write(&self, data: Save) -> Result<Json<Document>, Response> {
        use std::io::Write;
        let (docs, dir, name) = self
            .open_doc(&data.path)
            .map_err(IntoResponse::into_response)?;
        let lock = docs.try_clone().map_err(internal)?.into_std_file();
        lock.lock().map_err(internal)?;
        let check = || -> Result<(), Response> {
            let current = dir.read_to_string(&name).map_err(internal)?;
            if revision(&current) == data.revision {
                return Ok(());
            }
            Err((
                StatusCode::CONFLICT,
                Json(Document {
                    path: data.path.clone(),
                    revision: revision(&current),
                    content: current,
                }),
            )
                .into_response())
        };
        check()?;
        let temp = format!(".{name}.{:016x}.tmp", OsRng.next_u64());
        let result = (|| {
            let mut file = dir
                .open_with(&temp, OpenOptions::new().write(true).create_new(true))
                .map_err(internal)?;
            file.set_permissions(dir.metadata(&name).map_err(internal)?.permissions())
                .map_err(internal)?;
            file.write_all(data.content.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(internal)?;
            check()?;
            dir.rename(&temp, &dir, &name).map_err(internal)
        })();
        if result.is_err() {
            let _ = dir.remove_file(&temp);
        }
        result?;
        dir.try_clone()
            .and_then(|d| d.into_std_file().sync_all())
            .map_err(internal)?;
        Ok(Json(Document {
            path: data.path,
            revision: revision(&data.content),
            content: data.content,
        }))
    }
}
fn internal<E>(_: E) -> Response {
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}
fn authorize(app: &App, headers: &HeaderMap) -> Result<(), StatusCode> {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) != Some(app.origin.as_str())
        || headers.get("x-doclify-token").and_then(|v| v.to_str().ok()) != Some(app.viewer.token())
    {
        return Err(StatusCode::FORBIDDEN);
    }
    Ok(())
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
    let (_, dir, name) = app.viewer.open_doc(&q.path)?;
    let content = dir
        .read_to_string(name)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
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
) -> Result<Json<Document>, Response> {
    authorize(&app, &headers).map_err(IntoResponse::into_response)?;
    let viewer = app.viewer.clone();
    tokio::task::spawn_blocking(move || viewer.write(data))
        .await
        .map_err(internal)?
}
#[derive(Deserialize)]
struct SearchQuery {
    q: String,
}
async fn search(
    State(app): State<App>,
    Query(query): Query<SearchQuery>,
) -> Result<Json<Vec<doclify_search::Hit>>, StatusCode> {
    tokio::task::spawn_blocking(move || doclify_search::search(&app.viewer.root, &query.q))
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?
        .map(Json)
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)
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
struct RecordSource {
    id: String,
    version: u64,
}

async fn record_source(
    State(app): State<App>,
    Query(q): Query<RecordSource>,
) -> Result<Json<doclify_domain::Record>, StatusCode> {
    if q.id.is_empty()
        || !q
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
        || q.version == 0
    {
        return Err(StatusCode::BAD_REQUEST);
    }
    let records = app
        .viewer
        .dir
        .open_dir(".doclify/records")
        .map_err(|_| StatusCode::NOT_FOUND)?;
    if !records
        .symlink_metadata(&q.id)
        .is_ok_and(|m| m.file_type().is_dir())
    {
        return Err(StatusCode::NOT_FOUND);
    }
    let dir = records.open_dir(&q.id).map_err(|_| StatusCode::NOT_FOUND)?;
    let name = format!("{:020}.json", q.version);
    if !dir.symlink_metadata(&name).is_ok_and(|m| m.is_file()) {
        return Err(StatusCode::NOT_FOUND);
    }
    let text = dir
        .read_to_string(name)
        .map_err(|_| StatusCode::NOT_FOUND)?;
    let value: serde_json::Value =
        serde_json::from_str(&text).map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let record: doclify_domain::Record = serde_json::from_value(
        value
            .get("record")
            .cloned()
            .ok_or(StatusCode::INTERNAL_SERVER_ERROR)?,
    )
    .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if record.id != q.id || record.version != q.version {
        return Err(StatusCode::INTERNAL_SERVER_ERROR);
    }
    Ok(Json(record))
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
    authorize(&app, &headers)?;
    let state = match data.action.as_str() {
        "accept" => doclify_domain::ReviewState::Accepted,
        "reject" => doclify_domain::ReviewState::Rejected,
        "supersede" => doclify_domain::ReviewState::Superseded,
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    // Store appends with an exclusive hard link, so concurrent reviews of one version cannot both win.
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
