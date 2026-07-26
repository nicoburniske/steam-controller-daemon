use crate::{Error, Result};
use std::{env, ffi::OsString, path::PathBuf};

pub fn config(explicit: Option<PathBuf>) -> Result<PathBuf> {
    resolve_config(
        explicit,
        env::var_os("XDG_CONFIG_HOME"),
        env::var_os("HOME"),
    )
}

pub fn socket(explicit: Option<PathBuf>) -> PathBuf {
    resolve_socket(explicit, env::var_os("XDG_RUNTIME_DIR"))
}

fn resolve_config(
    explicit: Option<PathBuf>,
    xdg_config_home: Option<OsString>,
    home: Option<OsString>,
) -> Result<PathBuf> {
    if let Some(path) = explicit {
        return Ok(path);
    }

    if let Some(path) = xdg_config_home {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Ok(path.join("scd/config.toml"));
        }
    }

    if let Some(path) = home {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Ok(path.join(".config/scd/config.toml"));
        }
    }

    Err(Error::message(
        "could not locate configuration: provide a path or set XDG_CONFIG_HOME or HOME to an absolute path",
    ))
}

fn resolve_socket(explicit: Option<PathBuf>, xdg_runtime_dir: Option<OsString>) -> PathBuf {
    if let Some(path) = explicit {
        return path;
    }

    if let Some(path) = xdg_runtime_dir {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return path.join("scd/control.sock");
        }
    }

    PathBuf::from("/run/scd/control.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_config_by_precedence() {
        let explicit = PathBuf::from("config.toml");
        assert_eq!(
            resolve_config(
                Some(explicit.clone()),
                Some("/xdg".into()),
                Some("/home/user".into())
            )
            .unwrap(),
            explicit
        );
        assert_eq!(
            resolve_config(None, Some("/xdg".into()), Some("/home/user".into())).unwrap(),
            PathBuf::from("/xdg/scd/config.toml")
        );
        assert_eq!(
            resolve_config(None, Some("relative".into()), Some("/home/user".into())).unwrap(),
            PathBuf::from("/home/user/.config/scd/config.toml")
        );
    }

    #[test]
    fn rejects_unusable_config_environment() {
        let error =
            resolve_config(None, Some(OsString::new()), Some("relative".into())).unwrap_err();
        assert!(
            error
                .to_string()
                .starts_with("could not locate configuration:")
        );
    }

    #[test]
    fn resolves_socket_by_precedence() {
        let explicit = PathBuf::from("control.sock");
        assert_eq!(
            resolve_socket(Some(explicit.clone()), Some("/run/user/1000".into())),
            explicit
        );
        assert_eq!(
            resolve_socket(None, Some("/run/user/1000".into())),
            PathBuf::from("/run/user/1000/scd/control.sock")
        );
        assert_eq!(
            resolve_socket(None, Some("relative".into())),
            PathBuf::from("/run/scd/control.sock")
        );
    }
}
