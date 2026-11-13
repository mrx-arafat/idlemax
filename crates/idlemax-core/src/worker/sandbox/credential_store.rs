//! Domain: the files git's `store` credential helper keeps passwords in (review finding N2),
//! from the `credential.helper` values in effect, so the sandbox can hide each one. Pure.

use std::path::{Path, PathBuf};

/// Every store file the helpers may use: the `--file` of each `store` helper, and for a
/// `store` without one, git's two defaults (`~/.git-credentials`, `~/.config/git/credentials`).
/// The XDG default is always listed, since git reads it whatever the helper says.
pub fn store_files(helpers: &[String], home: &Path) -> Vec<PathBuf> {
    let mut out = vec![home.join(".config/git/credentials")];
    for helper in helpers {
        let words: Vec<&str> = helper.split_whitespace().collect();
        if !matches!(
            words.first(),
            Some(&"store") | Some(&"git-credential-store")
        ) {
            continue;
        }
        let file = words
            .iter()
            .enumerate()
            .find_map(|(i, w)| match w.strip_prefix("--file=") {
                Some(f) => Some(f.to_string()),
                None => (*w == "--file")
                    .then(|| words.get(i + 1).map(|f| f.to_string()))
                    .flatten(),
            });
        match file {
            Some(f) => out.push(expand(&f, home)),
            None => out.push(home.join(".git-credentials")),
        }
    }
    out.sort();
    out.dedup();
    out
}

fn expand(path: &str, home: &Path) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home.join(rest),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_store_helper_names_its_file_and_the_defaults() {
        let home = Path::new("/h");
        let helpers = [
            "store --file /srv/creds".to_string(),
            "store --file=~/x".to_string(),
            "store".to_string(),
            "cache --timeout 900".to_string(),
        ];
        let files = store_files(&helpers, home);
        let want = [
            "/h/.config/git/credentials",
            "/h/.git-credentials",
            "/h/x",
            "/srv/creds",
        ];
        assert_eq!(files, want.map(PathBuf::from));
        assert_eq!(
            store_files(&[], home),
            [PathBuf::from("/h/.config/git/credentials")]
        );
    }
}
