//! Per-worktree user scheduler registration. Files are owned by a stable hash
//! of the Git directory; never overwrite or remove an unrelated user unit.
use doclify_capture::RepoInfo;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

const MARKER: &str = "Doclify-managed recovery";
fn name(repo: &RepoInfo) -> String {
    let hash = Sha256::digest(repo.git_dir.to_string_lossy().as_bytes());
    format!("doclify-recovery-{:x}", hash)
}
fn home() -> io::Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("HOME is not set"))
}
fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn unit(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('\n', "\\n")
    )
}
fn put(path: &Path, contents: &str) -> io::Result<()> {
    if path.exists() && !fs::read_to_string(path)?.contains(MARKER) {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not managed by Doclify", path.display()),
        ));
    }
    fs::create_dir_all(path.parent().unwrap())?;
    fs::write(path, contents)
}
fn call(program: &str, args: &[&str]) -> io::Result<()> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{program} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}
fn paths(repo: &RepoInfo) -> io::Result<Vec<PathBuf>> {
    let name = name(repo);
    if cfg!(target_os = "macos") {
        Ok(vec![home()?
            .join("Library/LaunchAgents")
            .join(format!("{name}.plist"))])
    } else if cfg!(target_os = "linux") {
        let d = home()?.join(".config/systemd/user");
        Ok(vec![
            d.join(format!("{name}.service")),
            d.join(format!("{name}.timer")),
        ])
    } else {
        Err(io::Error::other(
            "automatic recovery is supported on macOS and Linux",
        ))
    }
}
fn domain() -> io::Result<String> {
    let o = Command::new("id").arg("-u").output()?;
    if !o.status.success() {
        return Err(io::Error::other("cannot determine user ID"));
    }
    Ok(format!("gui/{}", String::from_utf8_lossy(&o.stdout).trim()))
}
pub fn install(repo: &RepoInfo) -> io::Result<Value> {
    let files = paths(repo)?;
    let exe = std::env::current_exe()?;
    let name = name(repo);
    let exe = exe
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF-8 executable path"))?;
    let cwd = repo
        .worktree
        .to_str()
        .ok_or_else(|| io::Error::other("non-UTF-8 worktree path"))?;
    let path = std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin".into());
    if cfg!(target_os = "macos") {
        let contents = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!-- {MARKER} -->\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{name}</string><key>ProgramArguments</key><array><string>{}</string><string>background-worker</string></array><key>WorkingDirectory</key><string>{}</string><key>EnvironmentVariables</key><dict><key>PATH</key><string>{}</string></dict><key>RunAtLoad</key><true/><key>StartInterval</key><integer>60</integer></dict></plist>\n", xml(exe), xml(cwd), xml(&path));
        put(&files[0], &contents)?;
        let domain = domain()?;
        let path = files[0].to_string_lossy();
        let _ = call("launchctl", &["bootout", &domain, &path]);
        call("launchctl", &["bootstrap", &domain, &path])?;
    } else {
        let service = format!("# {MARKER}\n[Unit]\nDescription=Doclify recovery\n[Service]\nType=oneshot\nWorkingDirectory={}\nEnvironment={}\nExecStart={} background-worker\n", unit(cwd), unit(&format!("PATH={path}")), unit(exe));
        let timer = format!("# {MARKER}\n[Unit]\nDescription=Doclify recovery timer\n[Timer]\nOnBootSec=1min\nOnUnitActiveSec=1min\nUnit={name}.service\n[Install]\nWantedBy=timers.target\n");
        // Check all ownership before changing either file.
        for f in &files {
            if f.exists() && !fs::read_to_string(f)?.contains(MARKER) {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("{} is not managed by Doclify", f.display()),
                ));
            }
        }
        put(&files[0], &service)?;
        put(&files[1], &timer)?;
        call("systemctl", &["--user", "daemon-reload"])?;
        call(
            "systemctl",
            &["--user", "enable", "--now", &format!("{name}.timer")],
        )?;
        call(
            "systemctl",
            &["--user", "start", &format!("{name}.service")],
        )?;
    }
    Ok(json!({"active":true,"files":files}))
}
pub fn uninstall(repo: &RepoInfo) -> io::Result<()> {
    let files = paths(repo)?;
    for f in &files {
        if f.exists() && !fs::read_to_string(f)?.contains(MARKER) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} is not managed by Doclify", f.display()),
            ));
        }
    }
    if !files.iter().any(|f| f.exists()) {
        return Ok(());
    }
    if cfg!(target_os = "macos") {
        let domain = domain()?;
        let path = files[0].to_string_lossy();
        call("launchctl", &["bootout", &domain, &path])?;
    } else {
        call(
            "systemctl",
            &[
                "--user",
                "disable",
                "--now",
                &format!("{}.timer", name(repo)),
            ],
        )?;
    }
    for f in files {
        match fs::remove_file(f) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::NotFound => (),
            Err(e) => return Err(e),
        }
    }
    if cfg!(target_os = "linux") {
        call("systemctl", &["--user", "daemon-reload"])?;
    }
    Ok(())
}
