//! Finding files in an Obsidian vault.

use std::cell::OnceCell;
use std::fs;
use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;

pub struct Vault {
    /// The canonicalized vault root.
    root: PathBuf,
    /// All files in the vault, relative to the root, sorted. Collected on first use.
    files: OnceCell<Vec<PathBuf>>,
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
            files: OnceCell::new(),
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
        Ok(format!("data:{mime};base64,{}", STANDARD.encode(bytes)))
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

        self.files()
            .iter()
            .filter(|file| file.ends_with(relative))
            .min_by_key(|file| file.components().count())
            .map(|file| self.root.join(file))
    }

    fn files(&self) -> &[PathBuf] {
        self.files.get_or_init(|| {
            let mut files = Vec::new();
            collect_files(&self.root, Path::new(""), &mut files);
            files.sort();
            files
        })
    }
}

/// Recursively collects the files in `dir`, skipping hidden files and directories like
/// `.obsidian`, `.git` and `.trash`.
fn collect_files(root: &Path, dir: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(root.join(dir)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = dir.join(&name);
        match entry.file_type() {
            Ok(t) if t.is_dir() => collect_files(root, &path, files),
            Ok(t) if t.is_file() => files.push(path),
            // Follow symlinks to files, but not to directories, to avoid cycles.
            Ok(t) if t.is_symlink() && root.join(&path).is_file() => files.push(path),
            _ => {}
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
        "bmp" => "image/bmp",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "tif" | "tiff" => "image/tiff",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault() -> (tempfile::TempDir, Vault) {
        let dir = tempfile::tempdir().unwrap();
        for file in [
            "a.png",
            "notes/b.png",
            "notes/deep/b.png",
            "assets/deep/c.png",
            ".obsidian/hidden.png",
        ] {
            let path = dir.path().join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, file).unwrap();
        }
        let vault = Vault::open(dir.path()).unwrap();
        (dir, vault)
    }

    #[test]
    fn requires_obsidian_directory() {
        let dir = tempfile::tempdir().unwrap();
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
        assert_eq!(
            data_url("nope.png").unwrap_err(),
            "image not found in vault: nope.png"
        );
        std::fs::write(dir.path().join("x.txt"), "").unwrap();
        let err = data_url("x.txt").unwrap_err();
        assert!(err.ends_with("is not a supported image type"), "{err}");
    }
}
