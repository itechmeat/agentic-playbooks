//! Single integration-test binary for apb-core. Cargo treats every file
//! directly under `tests/` as its own test binary; each fresh binary costs a
//! macOS security-scan on first spawn. Consolidating the 23 former
//! `tests/*.rs` files into modules under `tests/suite/` and driving them from
//! this one `main.rs` collapses that to a single binary. No content was
//! changed in the moved files - only their location and this file are new,
//! except: `include_str!` relative paths adjusted for the extra directory
//! depth (registry_test.rs, validate_semantics_test.rs, migration_test.rs,
//! validate_structure_test.rs, provenance_test.rs, promotion_test.rs,
//! schema_test.rs, versioning_test.rs - each loads
//! `tests/fixtures/valid.yaml`, which is left in place since cargo only
//! auto-targets `tests/*.rs`, not subdirectories), `detect_test.rs`'s former
//! `#![cfg(unix)]` inner attribute converted to the outer `#[cfg(unix)]`
//! below on its `mod` line, and every former private per-file `static
//! ENV_LOCK` (models_table_test.rs, profile_resolve_test.rs, doctor_test.rs,
//! schema_migrate_config_test.rs, detect_test.rs) replaced with calls into
//! the single shared lock in `suite/common.rs` (see that file for why:
//! running as modules of one process means separate per-file locks no
//! longer prevent races between files). `config_test.rs`'s
//! `global_config_load_paths` mutated `APB_CONFIG_DIR` with no lock at all
//! before this change (safe only because it had its own test binary); it now
//! takes the shared lock too.

#[path = "suite/common.rs"]
mod common;

#[path = "suite/bundle_test.rs"]
mod bundle_test;
#[path = "suite/config_test.rs"]
mod config_test;
#[path = "suite/content_snapshot_test.rs"]
mod content_snapshot_test;
#[cfg(unix)]
#[path = "suite/detect_test.rs"]
mod detect_test;
#[path = "suite/doctor_test.rs"]
mod doctor_test;
#[path = "suite/fsutil_test.rs"]
mod fsutil_test;
#[path = "suite/instruction_draft_test.rs"]
mod instruction_draft_test;
#[path = "suite/migration_test.rs"]
mod migration_test;
#[path = "suite/models_table_test.rs"]
mod models_table_test;
#[path = "suite/overrides_test.rs"]
mod overrides_test;
#[path = "suite/profile_resolve_test.rs"]
mod profile_resolve_test;
#[path = "suite/promotion_test.rs"]
mod promotion_test;
#[path = "suite/provenance_test.rs"]
mod provenance_test;
#[path = "suite/registry_test.rs"]
mod registry_test;
#[path = "suite/schema_migrate_config_test.rs"]
mod schema_migrate_config_test;
#[path = "suite/schema_migrate_test.rs"]
mod schema_migrate_test;
#[path = "suite/schema_test.rs"]
mod schema_test;
#[path = "suite/validate_duration_test.rs"]
mod validate_duration_test;
#[path = "suite/validate_profiles_test.rs"]
mod validate_profiles_test;
#[path = "suite/validate_semantics_test.rs"]
mod validate_semantics_test;
#[path = "suite/validate_structure_test.rs"]
mod validate_structure_test;
#[path = "suite/versioning_test.rs"]
mod versioning_test;
#[path = "suite/versions_provenance_test.rs"]
mod versions_provenance_test;
