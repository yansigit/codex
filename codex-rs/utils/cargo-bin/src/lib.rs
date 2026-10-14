use std::ffi::OsString;
use std::io;
use std::path::Path;
use std::path::PathBuf;

pub use runfiles;

mod executable;
pub use executable::copy_executable;
#[cfg(unix)]
pub use executable::write_executable;

/// Bazel sets this when runfiles directories are disabled, which we do on all platforms for consistency.
const RUNFILES_MANIFEST_ONLY_ENV: &str = "RUNFILES_MANIFEST_ONLY";

#[derive(Debug, thiserror::Error)]
pub enum CargoBinError {
    #[error("failed to read current exe")]
    CurrentExe {
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read current directory")]
    CurrentDir {
        #[source]
        source: std::io::Error,
    },
    #[error("CARGO_BIN_EXE env var {key} resolved to {path:?}, but it does not exist")]
    ResolvedPathDoesNotExist { key: String, path: PathBuf },
    #[error("could not locate binary {name:?}; tried env vars {env_keys:?}; {fallback}")]
    NotFound {
        name: String,
        env_keys: Vec<String>,
        fallback: String,
    },
}

/// Returns an absolute path to a binary target built for the current test run.
///
/// In `cargo test`, `CARGO_BIN_EXE_*` env vars are absolute.
/// In `bazel test`, `CARGO_BIN_EXE_*` env vars are rlocationpaths, intended to be consumed by `rlocation`.
/// This helper allows callers to transparently support both.
pub fn cargo_bin(name: &str) -> Result<PathBuf, CargoBinError> {
    let env_keys = cargo_bin_env_keys(name);
    for key in &env_keys {
        if let Some(value) = std::env::var_os(key) {
            return resolve_bin_from_env(key, value);
        }
    }
    let executable =
        std::env::current_exe().map_err(|source| CargoBinError::CurrentExe { source })?;
    resolve_adjacent_binary(name, env_keys, executable)
}

fn resolve_adjacent_binary(
    name: &str,
    env_keys: Vec<String>,
    mut executable: PathBuf,
) -> Result<PathBuf, CargoBinError> {
    // Cross-crate test helpers are built beside the deps directory. Do not use
    // assert_cmd's discovery here: its missing-binary path panics, while callers
    // rely on our Result to handle optional helpers and report missing fixtures.
    executable.pop();
    if executable.ends_with("deps") {
        executable.pop();
    }
    let path = executable.join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if path.is_file() {
        Ok(path)
    } else {
        Err(CargoBinError::NotFound {
            name: name.to_owned(),
            env_keys,
            fallback: format!("build the test helper at {}", path.display()),
        })
    }
}

fn cargo_bin_env_keys(name: &str) -> Vec<String> {
    let mut keys = Vec::with_capacity(2);
    keys.push(format!("CARGO_BIN_EXE_{name}"));

    // Cargo replaces dashes in target names when exporting env vars.
    let underscore_name = name.replace('-', "_");
    if underscore_name != name {
        keys.push(format!("CARGO_BIN_EXE_{underscore_name}"));
    }

    keys
}

pub fn runfiles_available() -> bool {
    std::env::var_os(RUNFILES_MANIFEST_ONLY_ENV).is_some()
}

fn resolve_bin_from_env(key: &str, value: OsString) -> Result<PathBuf, CargoBinError> {
    let raw = PathBuf::from(&value);
    if runfiles_available() {
        let runfiles = runfiles::Runfiles::create().map_err(|err| CargoBinError::CurrentExe {
            source: std::io::Error::other(err),
        })?;
        if let Some(mut resolved) = runfiles::rlocation!(runfiles, &raw) {
            if !resolved.is_absolute() {
                resolved = std::env::current_dir()
                    .map_err(|source| CargoBinError::CurrentDir { source })?
                    .join(resolved);
            }
            if resolved.exists() {
                return Ok(resolved);
            }
        }
    } else if raw.is_absolute() && raw.exists() {
        return Ok(raw);
    }

    Err(CargoBinError::ResolvedPathDoesNotExist {
        key: key.to_owned(),
        path: raw,
    })
}

/// Macro that derives the path to a test resource at runtime, the value of
/// which depends on whether Cargo or Bazel is being used to build and run a
/// test. Note the return value may be a relative or absolute path.
/// (Incidentally, this is a macro rather than a function because it reads
/// compile-time environment variables that need to be captured at the call
/// site.)
///
/// This is expected to be used exclusively in test code because Codex CLI is a
/// standalone binary with no packaged resources.
#[macro_export]
macro_rules! find_resource {
    ($resource:expr) => {{
        let resource = std::path::Path::new(&$resource);
        if $crate::runfiles_available() {
            // When this code is built and run with Bazel:
            // - we inject `BAZEL_PACKAGE` as a compile-time environment variable
            //   that points to native.package_name()
            // - at runtime, Bazel will set runfiles-related env vars
            $crate::resolve_bazel_runfile(option_env!("BAZEL_PACKAGE"), resource)
        } else {
            let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
            Ok(manifest_dir.join(resource))
        }
    }};
}

pub fn resolve_bazel_runfile(
    bazel_package: Option<&str>,
    resource: &Path,
) -> std::io::Result<PathBuf> {
    let runfiles = runfiles::Runfiles::create()
        .map_err(|err| std::io::Error::other(format!("failed to create runfiles: {err}")))?;
    let runfile_path = match bazel_package {
        Some(bazel_package) => PathBuf::from("_main").join(bazel_package).join(resource),
        None => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "BAZEL_PACKAGE was not set at compile time",
            ));
        }
    };
    let runfile_path = normalize_runfile_path(&runfile_path);
    if let Some(resolved) = runfiles::rlocation!(runfiles, &runfile_path)
        && resolved.exists()
    {
        return Ok(resolved);
    }
    let runfile_path_display = runfile_path.display();
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("runfile does not exist at: {runfile_path_display}"),
    ))
}

pub fn resolve_cargo_runfile(resource: &Path) -> std::io::Result<PathBuf> {
    let manifest_dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    Ok(manifest_dir.join(resource))
}

pub fn repo_root() -> io::Result<PathBuf> {
    let marker = if runfiles_available() {
        let runfiles = runfiles::Runfiles::create()
            .map_err(|err| io::Error::other(format!("failed to create runfiles: {err}")))?;
        let marker_path = option_env!("CODEX_REPO_ROOT_MARKER")
            .map(PathBuf::from)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "CODEX_REPO_ROOT_MARKER was not set at compile time",
                )
            })?;
        runfiles::rlocation!(runfiles, &marker_path).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "repo_root.marker not available in runfiles",
            )
        })?
    } else {
        resolve_cargo_runfile(Path::new("repo_root.marker"))?
    };
    let mut root = marker;
    for _ in 0..4 {
        root = root
            .parent()
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    "repo_root.marker did not have expected parent depth",
                )
            })?
            .to_path_buf();
    }
    Ok(root)
}

fn normalize_runfile_path(path: &Path) -> PathBuf {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(components.last(), Some(std::path::Component::Normal(_))) {
                    components.pop();
                } else {
                    components.push(component);
                }
            }
            _ => components.push(component),
        }
    }

    components
        .into_iter()
        .fold(PathBuf::new(), |mut acc, component| {
            acc.push(component.as_os_str());
            acc
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_optional_helper_returns_error_without_panicking() {
        let name = "codex-intentionally-missing-test-helper-72c139b1";
        let error = cargo_bin(name).expect_err("the fixture must not exist");
        assert!(matches!(error, CargoBinError::NotFound { .. }));
    }

    #[test]
    fn finds_cross_crate_helper_beside_deps_for_each_profile_layout() {
        let root = tempfile::tempdir().expect("temporary directory");
        for profile in ["debug", "release", "custom-target/debug"] {
            let directory = root.path().join(profile);
            std::fs::create_dir_all(directory.join("deps")).expect("test directory");
            let helper = directory.join(format!("fixture{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&helper, b"fixture").expect("test helper");
            for test_path in ["deps/test-hash", "test-runner"] {
                let actual = resolve_adjacent_binary(
                    "fixture",
                    cargo_bin_env_keys("fixture"),
                    directory.join(test_path),
                )
                .expect("sibling test helper");
                assert_eq!(actual, helper);
            }
        }
    }

    #[test]
    fn missing_helper_reports_expected_location_and_environment_keys() {
        let root = tempfile::tempdir().expect("temporary directory");
        let keys = cargo_bin_env_keys("missing-helper");
        let error = resolve_adjacent_binary(
            "missing-helper",
            keys.clone(),
            root.path().join("deps/test-hash"),
        )
        .expect_err("missing helper");
        let CargoBinError::NotFound {
            name,
            env_keys,
            fallback,
        } = error
        else {
            panic!("expected NotFound");
        };
        assert_eq!(name, "missing-helper");
        assert_eq!(env_keys, keys);
        let expected = root
            .path()
            .join(format!("missing-helper{}", std::env::consts::EXE_SUFFIX));
        assert!(fallback.contains(&expected.display().to_string()));
    }
}
