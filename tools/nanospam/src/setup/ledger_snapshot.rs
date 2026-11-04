use std::{fs, io, path::Path};

const ACCOUNT_KEYS: &str = "account_keys.txt";

/// The account keys belong to the saved ledgers: read before the account map
/// is created from the data directory
pub(crate) fn restore_account_keys(snapshot: &Path, data_dir: &Path) -> io::Result<()> {
    fs::create_dir_all(data_dir)?;
    fs::copy(snapshot.join(ACCOUNT_KEYS), data_dir.join(ACCOUNT_KEYS))?;
    Ok(())
}

/// Copies each node's saved ledger and wallets into its freshly configured
/// directory; the configuration stays this run's own
pub(crate) fn restore_node_ledgers(
    snapshot: &Path,
    data_dir: &Path,
    node_count: usize,
) -> io::Result<()> {
    for i in 0..node_count {
        let node = format!("pr{i}");
        copy_files(&snapshot.join(&node), &data_dir.join(&node))?;
    }
    Ok(())
}

fn copy_files(from: &Path, to: &Path) -> io::Result<()> {
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            fs::copy(entry.path(), to.join(entry.file_name()))?;
        }
    }
    Ok(())
}
