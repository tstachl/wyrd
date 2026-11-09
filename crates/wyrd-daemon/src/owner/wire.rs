//! Daemon view of the drive-owner wire: a re-export of the owned
//! protocol types in `wyrd_owner_client`, plus the directory for the
//! daemon's domain conversions (`mapping`). The types are defined
//! once, in the client crate; this module only renames them into the
//! daemon's historical paths so existing importers keep working.

pub use wyrd_owner_client::wire::*;
