//! A disposable, local search index built from documentation and current records.
use rusqlite::{params, Connection};
use serde::Serialize;
use std::{fs, io, path::Path};

#[derive(Debug, Serialize)]
pub struct Hit {
    pub kind: String,
    pub path: String,
    pub title: String,
}

fn error(e: impl std::fmt::Display) -> io::Error {
    io::Error::other(e.to_string())
}

// Never follow links while gathering sources; both source trees may be absent.
fn files(base: &Path, dir: &Path, out: &mut Vec<String>) -> io::Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    for entry in entries {
        let entry = entry?;
        let ty = entry.file_type()?;
        let path = entry.path();
        if ty.is_dir() {
            files(base, &path, out)?;
        } else if ty.is_file()
            && matches!(
                path.extension().and_then(|x| x.to_str()),
                Some("md" | "mdx")
            )
        {
            if let Ok(relative) = path.strip_prefix(base) {
                out.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

fn real_dir(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_dir() => Ok(true),
        Ok(_) => Err(error("search directory is not a real directory")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Rebuild from authoritative files on every request, atomically replacing the disposable cache.
/// This also detects external edits and recovers from deleted, stale or corrupt indexes.
pub fn search(root: &Path, query: &str) -> io::Result<Vec<Hit>> {
    let root = root.canonicalize()?;
    let doclify = root.join(".doclify");
    if !real_dir(&doclify)? {
        fs::create_dir(&doclify)?;
    }
    let local = doclify.join("local");
    if !real_dir(&local)? {
        fs::create_dir(&local)?;
    }
    let temp = tempfile::NamedTempFile::new_in(&local)?;
    let mut db = Connection::open(temp.path()).map_err(error)?;
    db.execute_batch("CREATE TABLE entries(kind TEXT NOT NULL, path TEXT NOT NULL, title TEXT NOT NULL, content TEXT NOT NULL);").map_err(error)?;
    let tx = db.transaction().map_err(error)?;
    let docs_parent = root.join("docs");
    if real_dir(&docs_parent)? {
        let docs = docs_parent.join("doclify");
        if real_dir(&docs)? {
            let mut paths = Vec::new();
            files(&docs, &docs, &mut paths)?;
            for path in paths {
                let content = fs::read_to_string(docs.join(&path))?;
                tx.execute(
                    "INSERT INTO entries VALUES ('document', ?1, ?1, ?2)",
                    params![path, content],
                )
                .map_err(error)?;
            }
        }
    }
    let records = doclify.join("records");
    if real_dir(&records)? {
        let store = doclify_domain::Store::open(&root)?;
        for entry in fs::read_dir(&records)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            // Store::history follows files, so reject links in a record directory.
            for version in fs::read_dir(entry.path())? {
                if !version?.file_type()?.is_file() {
                    return Err(error("invalid record entry"));
                }
            }
            let id = entry.file_name().to_string_lossy().into_owned();
            if let Some(record) = store.history(&id)?.last() {
                let content = serde_json::to_string(record).map_err(error)?;
                tx.execute(
                    "INSERT INTO entries VALUES ('record', ?1, ?2, ?3)",
                    params![id, record.title, content],
                )
                .map_err(error)?;
            }
        }
    }
    tx.commit().map_err(error)?;
    let needle = query.to_lowercase();
    let mut results = Vec::new();
    if !needle.trim().is_empty() {
        let mut stmt = db
            .prepare("SELECT kind, path, title, content FROM entries ORDER BY kind, path")
            .map_err(error)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })
            .map_err(error)?;
        for row in rows {
            let (kind, path, title, content) = row.map_err(error)?;
            if format!("{path}\n{content}")
                .to_lowercase()
                .contains(&needle)
            {
                results.push(Hit { kind, path, title });
            }
        }
    }
    drop(db);
    temp.persist(local.join("search.sqlite3")).map_err(error)?;
    Ok(results)
}
