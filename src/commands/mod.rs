//! Command implementations, one module per Redis type family. Every command
//! takes a `&Connection` that is already inside the transaction opened by
//! `cli::dispatch` and returns a typed `Reply` (or a recoverable `CmdError`).

pub(crate) mod hashes;
pub(crate) mod keys;
pub(crate) mod lists;
pub(crate) mod sets;
pub(crate) mod strings;
pub(crate) mod ttl;
