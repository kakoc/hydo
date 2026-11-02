//! Helper programs: hydo-nix-fetch and hydo-grpc-gen.

use crate::registry::*;
use crate::Env;

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
        }
    }
}
