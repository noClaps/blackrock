//! Finding files in an Obsidian vault.

use std::cell::OnceCell;
use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

pub struct Vault {
    /// The canonicalized vault root.
    root: PathBuf,
    /// All files in the vault, sorted and grouped by file name. Collected on first use.
    files_by_name: OnceCell<HashMap<OsString, Vec<PathBuf>>>,
}

impl Vault {
    /// Opens the vault rooted at `root`, which must contain Obsidian's `.obsidian` directory.
    pub fn open(root: &Path) -> Result<Self, String> {
        if !root.join(".obsidian").is_dir() {
            return Err(format!(
                "{} is not the root of an Obsidian vault (it has no .obsidian directory); \
                 run blackrock from the vault root",
                root.display()
            ));
        }
        let root = root
            .canonicalize()
            .map_err(|e| format!("cannot open vault {}: {e}", root.display()))?;
        Ok(Self {
            root,
            files_by_name: OnceCell::new(),
        })
    }

    /// Finds an image linked from a note in `note_dir` and reads it into a `data:` URL.
    pub fn image_data_url(&self, link: &str, note_dir: &Path) -> Result<String, String> {
        let path = self
            .resolve(link, note_dir)
            .ok_or_else(|| format!("image not found in vault: {link}"))?;
        let mime = image_mime_type(&path)
            .ok_or_else(|| format!("{} is not a supported image type", path.display()))?;
        let bytes = fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Ok(format!("data:{mime};base64,{}", base64(&bytes)))
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
/// `.obsidian`, `.git` and `.trash`.
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

/// Encodes bytes as standard, padded base64.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (chunk.iter().enumerate()).fold(0, |n, (i, &b)| n | usize::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                char::from(ALPHABET[(n >> (18 - 6 * i)) & 63])
            } else {
                '='
            });
        }
    }
    out
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
            ".obsidian/hidden.png",
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
    fn requires_obsidian_directory() {
        let dir = TempDir::new();
        let err = Vault::open(dir.path()).err().unwrap();
        assert!(err.contains("not the root of an Obsidian vault"), "{err}");
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
    fn encodes_base64() {
        let encoded =
            ["", "f", "fo", "foo", "foob", "fooba", "foobar"].map(|s| base64(s.as_bytes()));
        assert_eq!(
            encoded,
            [
                "", "Zg==", "Zm8=", "Zm9v", "Zm9vYg==", "Zm9vYmE=", "Zm9vYmFy"
            ]
        );
        assert_eq!(base64(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn data_urls() {
        let (dir, vault) = vault();
        let data_url = |link| vault.image_data_url(link, Path::new(""));
        assert_eq!(data_url("a.png").unwrap(), "data:image/png;base64,YS5wbmc=");
        assert_eq!(
            data_url("nope.png").unwrap_err(),
            "image not found in vault: nope.png"
        );
        std::fs::write(dir.path().join("x.txt"), "").unwrap();
        let err = data_url("x.txt").unwrap_err();
        assert!(err.ends_with("is not a supported image type"), "{err}");
    }
}
