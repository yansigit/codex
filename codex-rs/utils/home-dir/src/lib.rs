use codex_utils_absolute_path::AbsolutePathBuf;
use dirs::home_dir;
use std::path::PathBuf;

/// Returns the Nyacode configuration directory. `NYACODE_HOME` takes precedence
/// over the legacy `CODEX_HOME` override; otherwise defaults to `~/.nyacode`.
///
/// - If either override is set, the value must exist and be a directory. The
///   value will be canonicalized and this function will Err otherwise.
/// - If neither override is set, this function does not verify that the
///   directory exists.
pub fn find_codex_home() -> std::io::Result<AbsolutePathBuf> {
    let selected = home_override();
    resolve_home(
        selected
            .as_ref()
            .map(|(name, value)| (*name, value.as_str())),
    )
}

/// The explicit home override, shared with subprocess and symlink handling.
pub fn home_override() -> Option<(&'static str, String)> {
    select_home_override(
        std::env::var("NYACODE_HOME").ok(),
        std::env::var("CODEX_HOME").ok(),
    )
}

fn select_home_override(
    nyacode: Option<String>,
    codex: Option<String>,
) -> Option<(&'static str, String)> {
    nyacode
        .filter(|value| !value.is_empty())
        .map(|value| ("NYACODE_HOME", value))
        .or_else(|| {
            codex
                .filter(|value| !value.is_empty())
                .map(|value| ("CODEX_HOME", value))
        })
}

#[cfg(test)]
fn find_codex_home_from_env(codex_home_env: Option<&str>) -> std::io::Result<AbsolutePathBuf> {
    resolve_home(codex_home_env.map(|value| ("CODEX_HOME", value)))
}

fn resolve_home(home_env: Option<(&str, &str)>) -> std::io::Result<AbsolutePathBuf> {
    // Honor the selected home environment variable when it is set to allow users
    // (and tests) to override the default location.
    match home_env {
        Some((name, val)) => {
            let path = PathBuf::from(val);
            let metadata = std::fs::metadata(&path).map_err(|err| match err.kind() {
                std::io::ErrorKind::NotFound => std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("{name} points to {val:?}, but that path does not exist"),
                ),
                _ => {
                    std::io::Error::new(err.kind(), format!("failed to read {name} {val:?}: {err}"))
                }
            })?;

            if !metadata.is_dir() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    format!("{name} points to {val:?}, but that path is not a directory"),
                ))
            } else {
                let canonical = path.canonicalize().map_err(|err| {
                    std::io::Error::new(
                        err.kind(),
                        format!("failed to canonicalize {name} {val:?}: {err}"),
                    )
                })?;
                AbsolutePathBuf::from_absolute_path(canonical)
            }
        }
        None => {
            let mut p = home_dir().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "Could not find home directory",
                )
            })?;
            p.push(".nyacode");
            AbsolutePathBuf::from_absolute_path(p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::find_codex_home_from_env;
    use codex_utils_absolute_path::AbsolutePathBuf;
    use dirs::home_dir;
    use pretty_assertions::assert_eq;
    use std::fs;
    use std::io::ErrorKind;
    use tempfile::TempDir;

    #[test]
    fn find_codex_home_env_missing_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let missing = temp_home.path().join("missing-codex-home");
        let missing_str = missing
            .to_str()
            .expect("missing codex home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(missing_str)).expect_err("missing CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::NotFound);
        assert!(
            err.to_string().contains("CODEX_HOME"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_home_env_file_path_is_fatal() {
        let temp_home = TempDir::new().expect("temp home");
        let file_path = temp_home.path().join("codex-home.txt");
        fs::write(&file_path, "not a directory").expect("write temp file");
        let file_str = file_path
            .to_str()
            .expect("file codex home path should be valid utf-8");

        let err = find_codex_home_from_env(Some(file_str)).expect_err("file CODEX_HOME");
        assert_eq!(err.kind(), ErrorKind::InvalidInput);
        assert!(
            err.to_string().contains("not a directory"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn find_codex_home_env_valid_directory_canonicalizes() {
        let temp_home = TempDir::new().expect("temp home");
        let temp_str = temp_home
            .path()
            .to_str()
            .expect("temp codex home path should be valid utf-8");

        let resolved = find_codex_home_from_env(Some(temp_str)).expect("valid CODEX_HOME");
        let expected = temp_home
            .path()
            .canonicalize()
            .expect("canonicalize temp home");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn find_codex_home_without_env_uses_default_home_dir() {
        let resolved =
            find_codex_home_from_env(/*codex_home_env*/ None).expect("default CODEX_HOME");
        let mut expected = home_dir().expect("home dir");
        expected.push(".nyacode");
        let expected = AbsolutePathBuf::from_absolute_path(expected).expect("absolute home");
        assert_eq!(resolved, expected);
    }

    #[test]
    fn nyacode_home_precedes_legacy_override_without_falling_back_on_errors() {
        use super::select_home_override;
        assert_eq!(
            select_home_override(Some("new".into()), Some("old".into())),
            Some(("NYACODE_HOME", "new".into()))
        );
        assert_eq!(
            select_home_override(Some(String::new()), Some("old".into())),
            Some(("CODEX_HOME", "old".into()))
        );
        assert_eq!(select_home_override(None, None), None);
        let temp = TempDir::new().expect("temp home");
        let missing = temp.path().join("missing");
        let error = super::resolve_home(Some(("NYACODE_HOME", missing.to_str().expect("utf8"))))
            .expect_err("invalid primary override is fatal");
        assert!(error.to_string().contains("NYACODE_HOME"));
    }
}
