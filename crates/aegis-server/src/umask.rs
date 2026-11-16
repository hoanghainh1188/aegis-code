//! The file mode mask of the process (#558). What the server and its
//! commands create (the workspace's `state/`, the repos' clones, the
//! index directory) must never be writable by everyone, whoever started
//! the process: a `docker exec` can start it with umask 0000, and
//! Codebase-Memory then refuses to start under such a directory, which
//! stops every job.

/// What the mask always holds: no write for the group or others.
pub const REQUIRED: u32 = 0o022;

/// Adds [`REQUIRED`] to the process's mask, keeping a stricter one (0027,
/// 0077); the mask now in force.
#[cfg(unix)]
pub fn tighten() -> u32 {
    let required = REQUIRED as libc::mode_t;
    // SAFETY: umask cannot fail and touches no memory; the mask is read by
    // setting it, then set again with REQUIRED added.
    let mask = unsafe {
        let old = libc::umask(required);
        libc::umask(old | required);
        old | required
    };
    mask.into()
}

#[cfg(not(unix))]
pub fn tighten() -> u32 {
    REQUIRED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mask_keeps_others_from_writing_and_never_loosens() {
        let mask = tighten();
        assert_eq!(mask & REQUIRED, REQUIRED);
        // Again: the same mask, nothing taken away.
        assert_eq!(tighten(), mask);
    }
}
