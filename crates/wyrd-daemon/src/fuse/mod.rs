//! FUSE presentation backend, split by concern: [`inode`] owns the
//! inode table and open-state tracking, and [`backend`] maps kernel
//! ops onto the daemon's [`DriveView`](wyrd_fuse::DriveView) — itself
//! split by area (`setup`, `projection`, `handles`, `read`, `write`,
//! `fold`, `attr`, `ops`). Behavior tests live alongside, one file
//! per theme.
//!
//! The composed view type is re-exported at the crate root
//! (`crate::DriveView`) so hosts build over the same view without
//! depending on the view crate directly and without needing the
//! `mount` feature.

mod backend;
mod inode;

#[cfg(test)]
mod tests_budget;
#[cfg(test)]
mod tests_conflicts;
#[cfg(test)]
mod tests_errors;
#[cfg(test)]
mod tests_handles;
#[cfg(test)]
mod tests_harness;
#[cfg(test)]
mod tests_inode;
#[cfg(test)]
mod tests_readonly;
#[cfg(test)]
mod tests_want;
#[cfg(test)]
mod tests_write;

pub use backend::FuseBackend;
