//! Port of src/main/sounds.js: custom alert sounds. A file the user picks is copied into `<data dir>/sounds`,
//! so it keeps working if the original moves, and the UI only ever names a file inside that folder (never a
//! path). The file dialog itself (pickSound's `dialog.showOpenDialog`) is the UI's now: it passes the chosen
//! path to `import`.
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::highlight_rules::is_sound_file_name;
use crate::util::{js_trim, slice_utf16};

pub const EXTENSIONS: [&str; 9] = ["mp3", "wav", "ogg", "oga", "m4a", "aac", "flac", "webm", "opus"];
const MAX_BYTES: u64 = 5 * 1024 * 1024;

pub struct Sounds {
    dir: PathBuf,
}

impl Sounds {
    /// `data_dir` is the app data folder; the sounds live in its `sounds` subfolder.
    pub fn new(data_dir: &Path) -> Self {
        Sounds { dir: data_dir.join("sounds") }
    }

    /// Copy a chosen audio file into the sounds folder. Returns the bare file name to store in settings.
    pub fn import(&self, src: &Path) -> Result<String, String> {
        let ext = src.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !EXTENSIONS.contains(&ext.as_str()) {
            return Err("That file type is not supported. Choose an mp3, wav, ogg, m4a or flac file.".into());
        }
        let meta = std::fs::metadata(src).map_err(|e| e.to_string())?;
        if meta.len() > MAX_BYTES {
            return Err("That file is too big for an alert sound (5 MB at most).".into());
        }
        let data = std::fs::read(src).map_err(|e| e.to_string())?;
        // Readable name plus a short content hash: the same file picked twice lands on the same name.
        let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let cleaned: String = stem.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '_' | '-')).collect();
        let base = slice_utf16(js_trim(&cleaned), 60);
        let base = if base.is_empty() { "sound".to_string() } else { base };
        let hash: String = Sha1::digest(&data).iter().map(|b| format!("{b:02x}")).collect::<String>()[..8].to_string();
        let file = format!("{base}-{hash}.{ext}");
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        std::fs::write(self.dir.join(&file), data).map_err(|e| e.to_string())?;
        Ok(file)
    }

    /// The bytes of a file in the sounds folder. Only bare names are accepted.
    pub fn read(&self, file: &str) -> Result<Vec<u8>, String> {
        if !is_sound_file_name(file) {
            return Err("Not a sound file.".into());
        }
        let ext = Path::new(file).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        if !EXTENSIONS.contains(&ext.as_str()) {
            return Err("Not a sound file.".into());
        }
        let full = self.dir.join(file);
        if full.parent() != Some(self.dir.as_path()) {
            return Err("Not a sound file.".into());
        }
        let meta = std::fs::metadata(&full).map_err(|e| e.to_string())?;
        if meta.len() > MAX_BYTES {
            return Err("Sound file is too big.".into());
        }
        std::fs::read(&full).map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_then_read_and_only_bare_names() {
        let dir = std::env::temp_dir().join(format!("agu-sounds-{}", crate::util::uuid_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("My Ping!.MP3");
        std::fs::write(&src, b"abc").unwrap();
        let s = Sounds::new(&dir);
        let name = s.import(&src).unwrap();
        assert_eq!(name, "My Ping-a9993e36.mp3"); // sha1("abc") = a9993e36...
        assert_eq!(s.read(&name).unwrap(), b"abc");
        assert!(s.read("../secrets.bin").is_err());
        assert!(s.read("a/b.mp3").is_err());
        assert!(s.read("note.txt").is_err());
        let bad = dir.join("x.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(s.import(&bad).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }
}
