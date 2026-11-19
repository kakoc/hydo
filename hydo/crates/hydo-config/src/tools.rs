//! Helper programs: hydo-nix-fetch, hydo-grpc-gen, and the registry client of hydo compose and
//! hydo-fetch-image.

use crate::registry::*;
use crate::Env;
use std::path::PathBuf;

/// The tools group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tools {
    /// `HYDO_NIX_CACHE`: the binary cache hydo-nix-fetch reads. Default
    /// `https://cache.nixos.org`.
    pub nix_cache: String,
    /// `HYDO_GRPC_ARCH`: the default of `hydo-grpc-gen -arch` (unset or empty: `amd64`).
    pub grpc_arch: String,
    /// `HYDO_GRPC_CLIENT`: the default of `hydo-grpc-gen -client`. Default empty.
    pub grpc_client: String,
    /// `DOCKER_CONFIG`: the folder of Docker's `config.json`, where hydo compose and
    /// hydo-fetch-image find a registry's credential (Milestone 320). Unset or empty:
    /// `~/.docker`; None without a home folder.
    pub docker_config: Option<PathBuf>,
}

impl Default for Tools {
    fn default() -> Tools {
        Tools::from_env(&Env::default())
    }
}

impl Tools {
    pub fn from_env(e: &Env) -> Tools {
        Tools {
            nix_cache: e
                .text(HYDO_NIX_CACHE)
                .unwrap_or_else(|| "https://cache.nixos.org".into()),
            grpc_arch: e.nonempty(HYDO_GRPC_ARCH).unwrap_or_else(|| "amd64".into()),
            grpc_client: e.text(HYDO_GRPC_CLIENT).unwrap_or_default(),
            docker_config: e.path_nonempty(DOCKER_CONFIG).or_else(|| {
                let h = e.home();
                (!h.as_os_str().is_empty()).then(|| h.join(".docker"))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_config() {
        let t = Tools::from_env(&Env::default().with_home("/h"));
        assert_eq!(t.docker_config, Some(PathBuf::from("/h/.docker")));
        let t = Tools::from_env(&Env::from_pairs([(DOCKER_CONFIG, "/d")]).with_home("/h"));
        assert_eq!(t.docker_config, Some(PathBuf::from("/d")));
        let t = Tools::from_env(&Env::from_pairs([(DOCKER_CONFIG, "")]).with_home("/h"));
        assert_eq!(t.docker_config, Some(PathBuf::from("/h/.docker")));
        assert_eq!(Tools::from_env(&Env::default()).docker_config, None);
    }
}
