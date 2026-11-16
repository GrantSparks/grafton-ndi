//! Parts supplied by crate code still need the audited descriptor contract.
use super::*;

struct UnvalidatedParts;

fn unvalidated_parts(guard: Guard<'_, VideoKind>) {
    let _: Result<ValidatedFrame<_, UnvalidatedParts>> = ValidatedFrame::validate(guard);
}

// A real parts type cannot be reused with another descriptor family either.
fn foreign_parts(guard: Guard<'_, AudioKind>) {
    let _: Result<ValidatedFrame<_, (ValidatedVideoLayout, VideoHeader)>> =
        ValidatedFrame::validate(guard);
}
