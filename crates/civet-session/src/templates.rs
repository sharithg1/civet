//! Worktree templates: per-repository settings applied to new worktrees
//! (setup script, files copied from the main checkout, default agent and
//! prompt), and the `CIVET_PORT` given to every worktree.
//!
//! Two sources, merged field by field (the first that sets a field wins):
//! the template saved on the host (`<state>/worktree-templates.json`, edited
//! in the desktop's project settings), then `.civet/worktree.toml` in the
//! repository's main checkout (committed, shared with the team). See
//! `docs/WORKTREE_TEMPLATES.md`.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use civet_proto::WorktreeTemplate;
use serde::Deserialize;

/// The repository file, relative to the main checkout.
pub(crate) const TEMPLATE_FILE: &str = ".civet/worktree.toml";
/// Host-saved templates by repo id, in the state dir.
pub(crate) const HOST_TEMPLATES_FILE: &str = "worktree-templates.json";
/// Assigned `CIVET_PORT`s by worktree path, in the state dir.
pub(crate) const PORTS_FILE: &str = "worktree-ports.json";
/// Setup scripts as run by the setup panes, in the state dir.
pub(crate) const SETUP_DIR: &str = "setup";
/// Largest repository file read.
const MAX_TEMPLATE_FILE_BYTES: u64 = 64 * 1024;

/// First `CIVET_PORT` handed out; ports go up in blocks of [`PORT_BLOCK`].
pub(crate) const PORT_FIRST: u16 = 41_000;
/// Last block start.
pub(crate) const PORT_LAST: u16 = 49_990;
/// Ports reserved per worktree (`CIVET_PORT` … `CIVET_PORT + 9`).
pub(crate) const PORT_BLOCK: u16 = 10;

/// A JSON state file (missing or unreadable = default; a corrupt one is
/// moved aside, like `repos.json`).
pub(crate) fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> T {
    let data = match std::fs::read(path) {
        Ok(d) => d,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return T::default(),
        Err(e) => {
            tracing::warn!("cannot read {}: {e}", path.display());
            return T::default();
        }
    };
    serde_json::from_slice(&data).unwrap_or_else(|e| {
        let backup = path.with_extension("json.corrupt");
        tracing::warn!(
            "corrupt {} ({e}); moved to {}",
            path.display(),
            backup.display()
        );
        let _ = std::fs::rename(path, &backup);
        T::default()
    })
}

/// `.civet/worktree.toml` as written (`copy` may be one string).
#[derive(Debug, Deserialize)]
struct TemplateFile {
    setup: Option<String>,
    copy: Option<OneOrMany>,
    agent: Option<String>,
    prompt: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

/// Parse the repository file. Unknown keys are ignored (newer Civet
/// versions may add some).
pub(crate) fn parse_template_file(text: &str) -> Result<WorktreeTemplate, String> {
    let f: TemplateFile = toml::from_str(text).map_err(|e| e.to_string().trim().to_owned())?;
    Ok(normalize(WorktreeTemplate {
        setup: f.setup,
        copy: f.copy.map(|c| match c {
            OneOrMany::One(s) => vec![s],
            OneOrMany::Many(v) => v,
        }),
        agent: f.agent,
        prompt: f.prompt,
    }))
}

/// Read and parse `<main>/.civet/worktree.toml`: Ok(None) if there is none.
pub(crate) fn read_template_file(main: &Path) -> Result<Option<WorktreeTemplate>, String> {
    use std::io::Read as _;
    let path = main.join(TEMPLATE_FILE);
    let file = match std::fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{TEMPLATE_FILE}: {e}")),
    };
    let mut text = String::new();
    file.take(MAX_TEMPLATE_FILE_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("{TEMPLATE_FILE}: {e}"))?;
    if text.len() as u64 > MAX_TEMPLATE_FILE_BYTES {
        return Err(format!(
            "{TEMPLATE_FILE} is larger than {} KiB",
            MAX_TEMPLATE_FILE_BYTES / 1024
        ));
    }
    parse_template_file(&text)
        .map(Some)
        .map_err(|e| format!("{TEMPLATE_FILE}: {e}"))
}

/// Tidy a template: the agent command trimmed, blank copy entries dropped.
/// The setup script and prompt are kept as written (an empty one still
/// overrides the repository file).
pub(crate) fn normalize(mut t: WorktreeTemplate) -> WorktreeTemplate {
    t.agent = t.agent.map(|a| a.trim().to_owned());
    t.copy = t.copy.map(|c| {
        c.into_iter()
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect()
    });
    t
}

/// `host` fields over `file` fields.
pub(crate) fn merge(host: &WorktreeTemplate, file: Option<&WorktreeTemplate>) -> WorktreeTemplate {
    let empty = WorktreeTemplate::default();
    let file = file.unwrap_or(&empty);
    WorktreeTemplate {
        setup: host.setup.clone().or_else(|| file.setup.clone()),
        copy: host.copy.clone().or_else(|| file.copy.clone()),
        agent: host.agent.clone().or_else(|| file.agent.clone()),
        prompt: host.prompt.clone().or_else(|| file.prompt.clone()),
    }
}

/// True if nothing is set.
pub(crate) fn is_unset(t: &WorktreeTemplate) -> bool {
    t.setup.is_none() && t.copy.is_none() && t.agent.is_none() && t.prompt.is_none()
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?'])
}

/// `*` (any run) and `?` (one character) within one path component.
fn glob_match(pat: &str, name: &str) -> bool {
    let p: Vec<char> = pat.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ni));
            pi += 1;
        } else if let Some((sp, sn)) = star {
            pi = sp + 1;
            ni = sn + 1;
            star = Some((sp, sn + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// The paths (relative to `main`, sorted) that the `copy` patterns name.
/// Entries that leave the checkout or name `.git` are refused, and one
/// that matches nothing is reported; both as warnings.
pub(crate) fn expand_copy(main: &Path, patterns: &[String]) -> (Vec<String>, Vec<String>) {
    let mut found = Vec::new();
    let mut warnings = Vec::new();
    for pat in patterns {
        let parts: Vec<&str> = pat
            .trim_end_matches('/')
            .split('/')
            .filter(|c| !c.is_empty() && *c != ".")
            .collect();
        let bad = parts.is_empty()
            || pat.starts_with('/')
            || parts.iter().any(|c| *c == ".." || *c == ".git")
            || Path::new(pat)
                .components()
                .any(|c| matches!(c, Component::Prefix(_)));
        if bad {
            warnings.push(format!("{pat}: not a path inside the checkout, left out"));
            continue;
        }
        // Walk component by component, expanding globs with read_dir.
        let mut cur = vec![String::new()];
        for part in &parts {
            let mut next = Vec::new();
            for base in &cur {
                let dir = main.join(base);
                // Why: a symlinked directory could lead out of the checkout
                // (`link/*` with `link -> /etc`); copy the link itself instead.
                if !base.is_empty()
                    && std::fs::symlink_metadata(&dir).is_ok_and(|m| m.file_type().is_symlink())
                {
                    continue;
                }
                let join = |name: &str| {
                    if base.is_empty() {
                        name.to_owned()
                    } else {
                        format!("{base}/{name}")
                    }
                };
                if has_glob(part) {
                    let Ok(entries) = std::fs::read_dir(&dir) else {
                        continue;
                    };
                    let mut names: Vec<String> = entries
                        .flatten()
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .filter(|n| n != ".git" && glob_match(part, n))
                        // `*` does not match dotfiles unless asked to.
                        .filter(|n| !n.starts_with('.') || part.starts_with('.'))
                        .collect();
                    names.sort();
                    next.extend(names.iter().map(|n| join(n)));
                } else if std::fs::symlink_metadata(dir.join(part)).is_ok() {
                    next.push(join(part));
                }
            }
            cur = next;
        }
        if cur.is_empty() {
            warnings.push(format!("{pat}: not in the main checkout"));
        }
        found.extend(cur);
    }
    found.sort();
    found.dedup();
    (found, warnings)
}

/// Copy `rel` from `main` into `wt`, keeping whatever `wt` already has
/// (directories are merged). Returns whether anything was copied.
pub(crate) fn copy_missing(main: &Path, wt: &Path, rel: &str) -> std::io::Result<bool> {
    let dst = wt.join(rel);
    // Why: a symlinked directory in the worktree (tracked, say) would send
    // the copy outside it; the last component is checked by copy_entry.
    let mut cur = wt.to_path_buf();
    for comp in Path::new(rel)
        .parent()
        .into_iter()
        .flat_map(Path::components)
    {
        cur.push(comp);
        if std::fs::symlink_metadata(&cur).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(std::io::Error::other(format!(
                "{} is a symlink; not copying through it",
                cur.display()
            )));
        }
    }
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }
    copy_entry(&main.join(rel), &dst)
}

fn copy_entry(src: &Path, dst: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let meta = std::fs::symlink_metadata(src)?;
    let ft = meta.file_type();
    if ft.is_dir() {
        if std::fs::symlink_metadata(dst).is_ok_and(|m| !m.is_dir()) {
            return Ok(false);
        }
        std::fs::create_dir_all(dst)?;
        let mut any = false;
        for entry in std::fs::read_dir(src)? {
            let entry = entry?;
            let name = entry.file_name();
            if name == ".git" {
                continue;
            }
            any |= copy_entry(&entry.path(), &dst.join(&name))?;
        }
        return Ok(any);
    }
    if std::fs::symlink_metadata(dst).is_ok() {
        return Ok(false);
    }
    if ft.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(src)?, dst)?;
    } else if ft.is_file() {
        let mut from = std::fs::File::open(src)?;
        let mut to = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(meta.permissions().mode() & 0o777)
            .open(dst)?;
        std::io::copy(&mut from, &mut to)?;
    } else {
        // fifos, sockets, devices
        return Ok(false);
    }
    Ok(true)
}

/// The first free port block: not given to another worktree, and every
/// port of it bindable on the loopback interface right now.
pub(crate) fn pick_port(
    taken: &BTreeMap<String, u16>,
    is_free: impl Fn(u16) -> bool,
) -> Option<u16> {
    let used: std::collections::BTreeSet<u16> = taken.values().copied().collect();
    (PORT_FIRST..=PORT_LAST)
        .step_by(usize::from(PORT_BLOCK))
        .find(|p| !used.contains(p) && (*p..p + PORT_BLOCK).all(&is_free))
}

/// Whether nothing listens on TCP `port` (loopback and any address).
pub(crate) fn port_is_free(port: u16) -> bool {
    std::net::TcpListener::bind(("127.0.0.1", port)).is_ok()
        && std::net::TcpListener::bind(("0.0.0.0", port)).is_ok()
}

/// The setup pane's argv after `$SHELL -lc exec`: the script itself if it
/// starts with `#!`, else `sh -e <script>` (stops at the first failing
/// command, so the pane's exit code tells whether setup worked).
pub(crate) fn setup_command(script: &str, file: &Path) -> (String, Vec<String>) {
    let file = file.to_string_lossy().into_owned();
    if script.starts_with("#!") {
        (file, Vec::new())
    } else {
        ("sh".to_owned(), vec!["-e".to_owned(), file])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(setup: Option<&str>, copy: Option<&[&str]>, agent: Option<&str>) -> WorktreeTemplate {
        WorktreeTemplate {
            setup: setup.map(Into::into),
            copy: copy.map(|c| c.iter().map(|s| s.to_string()).collect()),
            agent: agent.map(Into::into),
            prompt: None,
        }
    }

    #[test]
    fn parses_the_repository_file() {
        let got = parse_template_file(
            r#"
setup = """
pnpm install
"""
copy = [".env", " apps/*/.env ", ""]
agent = "  claude --model opus "
future-key = 1
"#,
        )
        .unwrap();
        assert_eq!(got.setup.as_deref(), Some("pnpm install\n"));
        assert_eq!(
            got.copy,
            Some(vec![".env".to_owned(), "apps/*/.env".to_owned()])
        );
        assert_eq!(got.agent.as_deref(), Some("claude --model opus"));
        assert_eq!(got.prompt, None);
        let one = parse_template_file("copy = \".env\"").unwrap();
        assert_eq!(one.copy, Some(vec![".env".to_owned()]));
        assert!(parse_template_file("setup = 3").is_err());
        assert!(parse_template_file("setup = ").is_err());
        assert!(is_unset(&parse_template_file("").unwrap()));
    }

    #[test]
    fn host_fields_win() {
        let host = t(Some(""), None, Some("codex"));
        let file = t(Some("make"), Some(&[".env"]), Some("claude"));
        let m = merge(&host, Some(&file));
        assert_eq!(
            m.setup.as_deref(),
            Some(""),
            "an empty host script overrides"
        );
        assert_eq!(m.copy, Some(vec![".env".to_owned()]));
        assert_eq!(m.agent.as_deref(), Some("codex"));
        assert_eq!(merge(&host, None), host);
    }

    #[test]
    fn globs() {
        assert!(glob_match("*.env", "a.env"));
        assert!(glob_match(".env*", ".env.local"));
        assert!(glob_match("?b*", "abc"));
        assert!(glob_match("*", ""));
        assert!(!glob_match("*.env", "a.envx"));
        assert!(!glob_match("a?", "a"));
        assert!(glob_match("a*b*c", "aXbYbZc"));
    }

    #[test]
    fn expands_and_copies() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("main");
        let wt = dir.path().join("wt");
        for f in [
            ".env",
            "apps/a/.env",
            "apps/b/.env",
            "apps/b/x.txt",
            "conf/k/v",
        ] {
            let p = main.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, f).unwrap();
        }
        std::fs::create_dir_all(main.join("apps/.hidden")).unwrap();
        std::fs::write(main.join("apps/.hidden/.env"), "h").unwrap();
        std::fs::create_dir_all(dir.path().join("outside")).unwrap();
        std::fs::write(dir.path().join("outside/secret"), "s").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), main.join("link")).unwrap();
        std::fs::create_dir_all(wt.join("conf")).unwrap();
        std::fs::write(wt.join(".env"), "mine").unwrap();
        let pats: Vec<String> = [
            ".env",
            "apps/*/.env",
            "conf/",
            "missing",
            "../x",
            ".git/config",
            "/etc",
            "link/*",
            "link/secret",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let (paths, warnings) = expand_copy(&main, &pats);
        assert_eq!(paths, [".env", "apps/a/.env", "apps/b/.env", "conf"]);
        assert_eq!(warnings.len(), 6, "{warnings:?}");
        assert!(warnings[4].starts_with("link/*: not in"), "{warnings:?}");
        assert!(warnings[0].starts_with("missing: not in"));

        let copied: Vec<bool> = paths
            .iter()
            .map(|p| copy_missing(&main, &wt, p).unwrap())
            .collect();
        assert_eq!(copied, [false, true, true, true]);
        assert_eq!(std::fs::read_to_string(wt.join(".env")).unwrap(), "mine");
        assert_eq!(
            std::fs::read_to_string(wt.join("apps/b/.env")).unwrap(),
            "apps/b/.env"
        );
        assert!(!wt.join("apps/b/x.txt").exists());
        assert_eq!(
            std::fs::read_to_string(wt.join("conf/k/v")).unwrap(),
            "conf/k/v"
        );
        // A second run copies nothing new.
        assert!(!copy_missing(&main, &wt, "conf").unwrap());
        // Never through a symlinked directory of the worktree.
        let wt2 = dir.path().join("wt2");
        std::fs::create_dir_all(&wt2).unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside"), wt2.join("apps")).unwrap();
        assert!(copy_missing(&main, &wt2, "apps/a/.env").is_err());
        assert!(!dir.path().join("outside/a").exists());
    }

    #[test]
    fn ports_skip_taken_and_busy_blocks() {
        let mut taken = BTreeMap::new();
        assert_eq!(pick_port(&taken, |_| true), Some(PORT_FIRST));
        taken.insert("/a".to_owned(), PORT_FIRST);
        assert_eq!(pick_port(&taken, |_| true), Some(PORT_FIRST + PORT_BLOCK));
        // One busy port in the next block skips the whole block.
        let busy = PORT_FIRST + PORT_BLOCK + 7;
        assert_eq!(
            pick_port(&taken, |p| p != busy),
            Some(PORT_FIRST + 2 * PORT_BLOCK)
        );
        assert_eq!(pick_port(&taken, |_| false), None);
    }

    #[test]
    fn setup_commands() {
        let f = Path::new("/s/x.sh");
        assert_eq!(
            setup_command("pnpm i", f),
            ("sh".to_owned(), vec!["-e".to_owned(), "/s/x.sh".to_owned()])
        );
        assert_eq!(
            setup_command("#!/usr/bin/env bash\nset -eu", f),
            ("/s/x.sh".to_owned(), vec![])
        );
    }
}
