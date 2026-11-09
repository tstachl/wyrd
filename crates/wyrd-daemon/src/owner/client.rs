//! Daemon view of the drive-owner client: a re-export of the shared
//! client in `wyrd_owner_client`. The transport lives in exactly one
//! place; this module only renames it into the daemon's historical
//! path so existing importers keep working.

pub use wyrd_owner_client::client::*;
