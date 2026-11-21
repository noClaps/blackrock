use std::{
    cell::OnceCell,
    collections::HashMap,
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use base64::{Engine, engine::general_purpose::STANDARD};
use percent_encoding::{AsciiSet, CONTROLS, percent_encode};

/// What to escape in an SVG `data:` URL: what would end the URL or the attribute it's in, or
/// change its meaning. Everything else, including spaces, can stay as-is.
const SVG_ESCAPES: &AsciiSet = &CONTROLS.add(b'"').add(b'#').add(b'%').add(b'&');

#[derive(Debug)]
pub struct Vault {
    /// The canonicalized vault root.
    root: PathBuf,
    /// All files in the vault, sorted and grouped by file name. Collected on first use.
    files_by_name: OnceCell<HashMap<OsString, Vec<PathBuf>>>,
}

impl Vault {
    /// Opens the vault rooted at `root`.
    pub fn open(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot open vault {}", root.display()))?;
        Ok(Self {
            root,
            files_by_name: OnceCell::new(),
        })
    }

    /// Finds an image linked from a note in `note_dir` and reads it into a `data:` URL.
    ///
    /// SVGs are percent-encoded rather than base64-encoded, which keeps them smaller and leaves
    /// them compressible.
    pub fn image_data_url(&self, link: &str, note_dir: &Path) -> Result<String> {
        let path = self
            .resolve(link, note_dir)
            .ok_or_else(|| anyhow!("image not found in vault: {link}"))?;
        let mime = image_mime_type(&path)
            .ok_or_else(|| anyhow!("{} is not a supported image type", path.display()))?;
        let bytes = fs::read(&path).with_context(|| format!("cannot read {}", path.display()))?;
        Ok(if mime == "image/svg+xml" {
            format!("data:{mime},{}", percent_encode(&bytes, SVG_ESCAPES))
        } else {
            format!("data:{mime};base64,{}", STANDARD.encode(&bytes))
        })
    }

    /// Finds the file a link in a note in `note_dir` points to, the way Obsidian does: relative
    /// to the note, then relative to the vault root, and finally anywhere in the vault whose path
    /// ends with the link (preferring the shortest such path).
    ///
    /// Files outside the vault are never returned.
    fn resolve(&self, link: &str, note_dir: &Path) -> Option<PathBuf> {
        // A leading `/` makes a link relative to the vault root only.
        let (relative, from_root) = match link.trim().strip_prefix('/') {
            Some(relative) => (Path::new(relative), true),
            None => (Path::new(link.trim()), false),
        };
        if relative.as_os_str().is_empty() {
            return None;
        }

        let from_note = (!from_root).then(|| self.root.join(note_dir).join(relative));
        for candidate in from_note.into_iter().chain([self.root.join(relative)]) {
            if let Ok(path) = candidate.canonicalize()
                && path.is_file()
                && path.starts_with(&self.root)
            {
                return Some(path);
            }
        }

        // Only files with the link's file name can end with the link.
        self.files_by_name()
            .get(relative.file_name()?)?
            .iter()
            .filter(|file| {
                file.strip_prefix(&self.root)
                    .is_ok_and(|f| f.ends_with(relative))
            })
            .min_by_key(|file| file.components().count())
            .cloned()
    }

    fn files_by_name(&self) -> &HashMap<OsString, Vec<PathBuf>> {
        self.files_by_name.get_or_init(|| {
            let mut files = Vec::new();
            collect_files(&self.root, &mut files);
            files.sort();
            let mut by_name: HashMap<_, Vec<_>> = HashMap::new();
            for file in files {
                if let Some(name) = file.file_name() {
                    by_name.entry(name.to_owned()).or_default().push(file);
                }
            }
            by_name
        })
    }
}

/// Recursively collects the files in `dir`, skipping hidden files and directories like
/// `.git` and `.trash`.
fn collect_files(dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        // `is_file` follows symlinks, but symlinked directories aren't entered, to avoid cycles.
        if path.is_file() {
            files.push(path);
        } else if entry.file_type().is_ok_and(|t| t.is_dir()) {
            collect_files(&path, files);
        }
    }
}

/// The MIME type of an image file, based on its extension.
pub fn image_mime_type(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        _ => return None,
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A temporary directory, removed when dropped.
    pub struct TempDir(PathBuf);

    impl TempDir {
        pub fn new() -> Self {
            static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let n = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir = std::env::temp_dir().join(format!("blackrock-{}-{n}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn vault() -> (TempDir, Vault) {
        let dir = TempDir::new();
        for file in [
            "a.png",
            "notes/b.png",
            "notes/deep/b.png",
            "assets/deep/c.png",
            "links/placeholder.png",
            ".trash/hidden.png",
        ] {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        std::os::unix::fs::symlink(
            dir.path().join("a.png"),
            dir.path().join("links/linked.png"),
        )
        .unwrap();
        let vault = Vault::open(dir.path()).unwrap();
        (dir, vault)
    }

    #[test]
    fn resolves_links() {
        let (_dir, vault) = vault();
        let resolve = |link, note_dir: &str| {
            vault
                .resolve(link, Path::new(note_dir))
                .map(|p| p.strip_prefix(&vault.root).unwrap().to_owned())
        };
        // Relative to the note.
        assert_eq!(resolve("b.png", "notes"), Some("notes/b.png".into()));
        assert_eq!(resolve("../a.png", "notes"), Some("a.png".into()));
        // Relative to the vault root.
        assert_eq!(resolve("a.png", "notes"), Some("a.png".into()));
        assert_eq!(resolve("/a.png", "notes"), Some("a.png".into()));
        // Anywhere in the vault, preferring the shortest path.
        assert_eq!(resolve("b.png", ""), Some("notes/b.png".into()));
        assert_eq!(resolve("deep/b.png", ""), Some("notes/deep/b.png".into()));
        assert_eq!(resolve("c.png", ""), Some("assets/deep/c.png".into()));
        // Symlinked files found by searching the vault.
        assert_eq!(resolve("linked.png", ""), Some("links/linked.png".into()));
        // Hidden directories, missing files and files outside the vault.
        assert_eq!(resolve("hidden.png", ""), None);
        assert_eq!(resolve("missing.png", ""), None);
        assert_eq!(resolve("../../../../etc/hosts", "notes"), None);
    }

    #[test]
    fn data_urls() {
        let (dir, vault) = vault();
        let data_url = |link| vault.image_data_url(link, Path::new(""));
        assert_eq!(data_url("a.png").unwrap(), "data:image/png;base64,YS5wbmc=");
        std::fs::write(
            dir.path().join("s.svg"),
            "<svg a=\"#1\">\n<text>50% & é</text></svg>",
        )
        .unwrap();
        assert_eq!(
            data_url("s.svg").unwrap(),
            "data:image/svg+xml,<svg a=%22%231%22>%0A<text>50%25 %26 %C3%A9</text></svg>"
        );
        assert_eq!(
            data_url("nope.png").unwrap_err().to_string(),
            "image not found in vault: nope.png"
        );
        std::fs::write(dir.path().join("x.txt"), "").unwrap();
        let err = data_url("x.txt").unwrap_err().to_string();
        assert!(err.ends_with("is not a supported image type"), "{err}");
    }
}
