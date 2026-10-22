use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub fn replace_with_backup(path: &Path, original: &[u8], updated: &[u8]) -> Result<PathBuf> {
    let meta = fs::symlink_metadata(path)?;
    ensure!(
        meta.file_type().is_file(),
        "Subtitle must be a regular file, not a symbolic link"
    );
    ensure!(
        fs::read(path)? == original,
        "Subtitle changed during analysis; refusing to overwrite it"
    );
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut replacement = tempfile::NamedTempFile::new_in(parent)?;
    replacement.write_all(updated)?;
    replacement
        .as_file()
        .set_permissions(meta.permissions().clone())?;
    replacement.as_file().sync_all()?;
    let mut suffix = 0u64;
    let backup = loop {
        let mut name = path.as_os_str().to_owned();
        name.push(if suffix == 0 {
            ".bak".to_owned()
        } else {
            format!(".bak.{suffix}")
        });
        let candidate = PathBuf::from(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(original)
                    .context("Could not write backup; original left untouched")?;
                file.set_permissions(meta.permissions().clone())?;
                file.sync_all()
                    .context("Could not flush backup; original left untouched")?;
                break candidate;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
            Err(e) => return Err(e).context("Could not create backup; original left untouched"),
        }
    };
    // Check again after preparing the output. Persist is an atomic same-directory rename.
    ensure!(
        fs::read(path)? == original,
        "Subtitle changed while backing up; refusing to overwrite it"
    );
    replacement
        .persist(path)
        .context("Could not replace subtitle; backup has been preserved")?;
    #[cfg(unix)]
    fs::File::open(parent)?.sync_all()?;
    Ok(backup)
}
