//! Secrets read from files named by `<VAR>_FILE`, the convention of Docker
//! secrets and systemd credentials (`LoadCredential=`), so a secret never has
//! to sit in the process environment or in `docker inspect`.

use std::fmt;
use std::io::Read;
use std::path::Path;

use crate::error::{Error, Result};

/// Suffix of the variable that names a file holding the secret.
pub const FILE_SUFFIX: &str = "_FILE";

/// Larger than any key Serbero uses; stops a wrong path such as `/dev/zero`
/// from exhausting memory.
pub const MAX_SECRET_BYTES: usize = 4096;

/// A secret file's contents and whether any user on the host may read it.
pub struct SecretFile {
    pub contents: String,
    pub shared: bool,
}

/// A secret found in the environment or in a file.
#[derive(PartialEq)]
pub struct Found {
    pub value: String,
    /// Set when any user on the host may read the file.
    pub warning: Option<String>,
}

impl fmt::Debug for Found {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Found")
            .field("value", &"<redacted>")
            .field("warning", &self.warning)
            .finish()
    }
}

/// Reads the secret from `name`, or from the file `name_FILE` points to.
///
/// Setting both is an error, so a stale value never silently wins. Leading and
/// trailing whitespace is dropped, which removes the newline editors and
/// `echo` add to a file.
pub fn lookup(
    name: &str,
    env: &impl Fn(&str) -> Option<String>,
    read_file: &impl Fn(&Path) -> std::io::Result<SecretFile>,
) -> Result<Option<Found>> {
    let set = |var: &str| {
        env(var)
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let file_var = format!("{name}{FILE_SUFFIX}");
    let path = match (set(name), set(&file_var)) {
        (Some(_), Some(_)) => {
            return Err(Error::Config(format!("set {name} or {file_var}, not both")));
        }
        (Some(value), None) => {
            return Ok(Some(Found {
                value,
                warning: None,
            }));
        }
        (None, None) => return Ok(None),
        (None, Some(path)) => path,
    };
    // Docker secrets and systemd credentials are absolute paths. Anything else
    // may be the secret itself, pasted into the wrong variable, so it is never
    // echoed.
    if !Path::new(&path).is_absolute() {
        return Err(Error::Config(format!(
            "{file_var} must be an absolute path to the file holding {name}"
        )));
    }

    let file = read_file(Path::new(&path))
        .map_err(|e| Error::Config(format!("cannot read {file_var} ({path}): {e}")))?;
    let value = file.contents.trim().to_owned();
    if value.is_empty() {
        return Err(Error::Config(format!("{file_var} ({path}) is empty")));
    }
    let warning = file.shared.then(|| {
        format!("{file_var} ({path}) is readable by any user; remove its access for others")
    });
    Ok(Some(Found { value, warning }))
}

/// Reads a secret file from disk. The permissions checked are those of the
/// file actually read: both come from one open handle.
pub fn read_from_disk(path: &Path) -> std::io::Result<SecretFile> {
    let file = std::fs::File::open(path)?;
    let shared = is_shared(&file.metadata()?);
    let mut contents = String::new();
    file.take(MAX_SECRET_BYTES as u64 + 1)
        .read_to_string(&mut contents)?;
    if contents.len() > MAX_SECRET_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("larger than {MAX_SECRET_BYTES} bytes"),
        ));
    }
    Ok(SecretFile { contents, shared })
}

#[cfg(unix)]
fn is_shared(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // Group access is left to the operator: systemd credentials are readable
    // by the service's own group.
    metadata.permissions().mode() & 0o007 != 0
}

#[cfg(not(unix))]
fn is_shared(_metadata: &std::fs::Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;

    use super::*;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        move |name| map.get(name).cloned()
    }

    fn file(contents: &'static str, shared: bool) -> impl Fn(&Path) -> io::Result<SecretFile> {
        move |path| {
            assert_eq!(path, Path::new("/run/secrets/key"));
            Ok(SecretFile {
                contents: contents.to_owned(),
                shared,
            })
        }
    }

    fn no_file(_: &Path) -> io::Result<SecretFile> {
        Err(io::Error::new(io::ErrorKind::NotFound, "No such file"))
    }

    fn found(value: &str) -> Option<Found> {
        Some(Found {
            value: value.to_owned(),
            warning: None,
        })
    }

    #[test]
    fn reads_the_variable_itself() {
        let env = env(&[("KEY", " abc \n")]);
        assert_eq!(lookup("KEY", &env, &no_file).unwrap(), found("abc"));
    }

    #[test]
    fn reads_the_file_named_by_the_file_variable() {
        let env = env(&[("KEY_FILE", "/run/secrets/key")]);
        let got = lookup("KEY", &env, &file("abc\n", false)).unwrap();
        assert_eq!(got, found("abc"));
    }

    #[test]
    fn neither_set_is_none() {
        assert_eq!(lookup("KEY", &env(&[]), &no_file).unwrap(), None);
    }

    #[test]
    fn blank_values_count_as_unset() {
        let env = env(&[("KEY", "  "), ("KEY_FILE", "")]);
        assert_eq!(lookup("KEY", &env, &no_file).unwrap(), None);
    }

    #[test]
    fn both_set_is_an_error() {
        let env = env(&[("KEY", "abc"), ("KEY_FILE", "/run/secrets/key")]);
        let err = lookup("KEY", &env, &file("abc", false)).unwrap_err();
        assert!(err.to_string().contains("set KEY or KEY_FILE, not both"));
    }

    #[test]
    fn unreadable_file_is_an_error_naming_the_path() {
        let env = env(&[("KEY_FILE", "/run/secrets/key")]);
        let err = lookup("KEY", &env, &no_file).unwrap_err().to_string();
        assert!(err.contains("KEY_FILE"), "{err}");
        assert!(err.contains("/run/secrets/key"), "{err}");
    }

    #[test]
    fn empty_file_is_an_error() {
        let env = env(&[("KEY_FILE", "/run/secrets/key")]);
        let err = lookup("KEY", &env, &file(" \n", false)).unwrap_err();
        assert!(err.to_string().contains("is empty"));
    }

    #[test]
    fn shared_file_warns_without_showing_the_secret() {
        let env = env(&[("KEY_FILE", "/run/secrets/key")]);
        let got = lookup("KEY", &env, &file("abc", true)).unwrap().unwrap();
        assert_eq!(got.value, "abc");
        let warning = got.warning.unwrap();
        assert!(warning.contains("/run/secrets/key"), "{warning}");
        assert!(!warning.contains("abc"), "{warning}");
    }

    #[test]
    fn a_key_pasted_into_the_file_variable_is_not_echoed() {
        let key = "4444444444444444444444444444444444444444444444444444444444444444";
        let pairs = [("KEY_FILE", key)];
        let env = env(&pairs);
        let err = lookup("KEY", &env, &no_file).unwrap_err().to_string();
        assert!(err.contains("absolute path"), "{err}");
        assert!(!err.contains(key), "secret leaked: {err}");
    }

    #[test]
    fn debug_output_hides_the_value() {
        let found = Found {
            value: "abc-secret".to_owned(),
            warning: None,
        };
        assert!(!format!("{found:?}").contains("abc-secret"));
    }

    #[test]
    fn disk_reader_rejects_oversized_files() {
        let path = std::env::temp_dir().join(format!("serbero-secret-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, "a".repeat(MAX_SECRET_BYTES + 1)).unwrap();
        let result = read_from_disk(&path);
        std::fs::remove_file(&path).unwrap();
        assert!(result.is_err());
    }

    #[cfg(unix)]
    #[test]
    fn disk_reader_reports_access_by_any_user() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("serbero-secret-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, "abc\n").unwrap();

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let private = read_from_disk(&path).unwrap();
        // systemd credentials are readable by the service's own group.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o440)).unwrap();
        let group = read_from_disk(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let shared = read_from_disk(&path).unwrap();
        std::fs::remove_file(&path).unwrap();

        assert_eq!(private.contents, "abc\n");
        assert!(!private.shared);
        assert!(!group.shared);
        assert!(shared.shared);
    }
}
