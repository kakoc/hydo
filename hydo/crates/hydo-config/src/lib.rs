//! hydo-config: one typed configuration, read in one place (M144).
//!
//! hydo is configured by its command lines, by a project's files (`hydo.yaml`, the `hydo/`
//! folder), by the schedule string and, for what belongs to a machine, an installation or a
//! developer's session, by environment variables. This crate owns those variables:
//!
//! - [`registry`] is the inventory: every `HYDO_*`, `HVX_*` and `HVM_*` name the Rust crates use,
//!   with its owner, kind (user-facing, developer-only, test switch, guest interface, image
//!   variable, set for another program), type, default, readers and documentation, plus the
//!   names removed in M144. It is printed as `INVENTORY.md` next to this crate.
//! - [`Env`] holds the values: [`Env::process`] reads them from the process environment once, at
//!   a program's start (`hydo`, `hydo-build`, `hydo-hvx`, `hydo-explore`, `hydo-bench`,
//!   `hydo-grpc-gen`, `hydo-nix-fetch`). It is the only code that reads hydo's variables from the
//!   environment.
//! - [`Config`] is the typed configuration, grouped by owner ([`engine::Engine`],
//!   [`hvx::Options`], [`hvm::Engine`], [`hvm::Options`], [`run::Paths`], [`run::Pool`], [`run::Control`],
//!   [`run::Cli`], [`build::Build`], [`tools::Tools`]); each field's doc comment names its
//!   variable and default. A program builds it from its [`Env`] and passes the groups down
//!   explicitly; a library never reads the environment.
//!
//! The variables keep their names and meanings; a command-line flag that sets the same thing
//! wins where one exists (for example the explorer's `--turn-margin`).
//!
//! # No configuration file
//!
//! There is no hydo configuration file, on purpose. What a project tests is in its files and
//! schedules, which are versioned and travel with it. What is left is per machine or per
//! session: the release fills its own paths in (the `hydo` command sets the build inputs and the
//! wrapper from the release layout), a shared machine's quiet window or result cache is set for
//! one exploration, and the rest are developer diagnostics switched on for a single run. A file
//! would add a second place for each of these and a precedence rule between the two, for no
//! option anyone sets once and keeps.
//!
//! # How to add an option
//!
//! 1. Add the variable to the `variables!` list in `src/registry.rs`, under its group: a doc
//!    comment (what it does), then `NAME: Group, Kind, "type", "default", "readers";`. The macro
//!    makes the constant `registry::NAME` and the inventory entry.
//! 2. Add a field to its group's struct (`src/engine.rs`, `hvx.rs`, `run.rs`, `build.rs`,
//!    `tools.rs`), with a doc comment naming the variable and its default, and read it in that
//!    struct's `from_env` with an [`Env`] accessor that says how a value is read (`text`,
//!    `nonempty`, `path`, `is_set`, `set_not_0`, `not_0`, `is`). A new group gets its own struct
//!    and a field of [`Config`].
//! 3. Use the field where it is needed, reached through the arguments the program's `main`
//!    passes down (add a parameter or a struct field on the way; never call
//!    `std::env::var` or [`Env::process`] inside a library).
//! 4. Run `HYDO_BLESS=1 cargo test -p hydo-config` to rewrite `INVENTORY.md`, and review its
//!    diff. `tests/inventory.rs` fails while a crate reads a `HYDO_*`/`HVX_*`/`HVM_*` variable
//!    outside this crate, names one the inventory lacks, or the inventory lists a name nothing
//!    uses.
//!
//! A switch only tests read goes into the list too, with kind `Test`, and stays in the test's
//! code. A name hydo hands to the guest (kind `Guest`), writes into an image's manifest.sh
//! (`Image`) or sets for a program it starts (`Passed`) is listed for completeness; hydo never
//! reads those from its own environment.

pub mod build;
pub mod engine;
mod env;
pub mod hvm;
pub mod hvx;
pub mod registry;
pub mod run;
pub mod tools;

pub use env::Env;

/// The whole configuration of one program run, read from one [`Env`].
#[derive(Clone, Debug)]
pub struct Config {
    /// The values the groups were read from (the hvx engine reads its [`hvx::Options`] from
    /// them with one engine's overrides on top).
    pub env: Env,
    pub engine: engine::Engine,
    /// The Mac engine's choice (the engine's own options: [`hvm::Options`], read from `env`).
    pub hvm: hvm::Engine,
    pub paths: run::Paths,
    pub pool: run::Pool,
    pub control: run::Control,
    pub cli: run::Cli,
    pub build: build::Build,
    pub tools: tools::Tools,
}

impl Config {
    pub fn from_env(e: &Env) -> Config {
        Config {
            env: e.clone(),
            engine: engine::Engine::from_env(e),
            hvm: hvm::Engine::from_env(e),
            paths: run::Paths::from_env(e),
            pool: run::Pool::from_env(e),
            control: run::Control::from_env(e),
            cli: run::Cli::from_env(e),
            build: build::Build::from_env(e),
            tools: tools::Tools::from_env(e),
        }
    }

    /// From the process environment: for a program's `main` and for tests.
    pub fn process() -> Config {
        Config::from_env(&Env::process())
    }
}

impl Default for Config {
    /// No variable set, no home directory.
    fn default() -> Config {
        Config::from_env(&Env::default())
    }
}
