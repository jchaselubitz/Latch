//! Resize authority: decision D4.
//!
//! Termius on a phone is roughly 40 columns; iTerm is roughly 200. For this
//! product that is the ordinary configuration and not an edge case, so the
//! policy is explicit:
//!
//! > The session's size is the current controller's size. When a controller
//! > detaches, the size reverts to what was in effect before it took control.
//! > Watchers never resize the session.
//!
//! The state is a stack rather than a single previous value, because control can
//! nest: the desk terminal holds control, the phone steals it, a tablet steals it
//! from the phone. Each departure must return to the size that was in effect when
//! that controller arrived, in order. A single "previous size" gets the first
//! unwind right and every subsequent one wrong, and the failure looks like a desk
//! session that mysteriously stays at 40 columns.
//!
//! An explicit `latch resize --pin` overrides the whole mechanism and survives
//! controller changes, because a user who pinned a size said something about
//! their intent that a phone attaching should not undo.

use latch_protocol::control::Size;

use crate::worker::registry::AttachmentId;

/// One controller's claim on the session's size.
///
/// Held even after the controller is stolen from, because the stack is what
/// makes a nested unwind return the right size at each step. The entry is
/// removed when that attachment actually leaves.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Claim {
    who: AttachmentId,
    size: Size,
}

/// The session's authoritative size, and how it changes hands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResizeAuthority {
    /// The size in effect with no controller attached.
    base: Size,
    /// Controllers in the order they took control, current one last.
    claims: Vec<Claim>,
    /// An explicit `latch resize --pin`, which outranks the stack entirely.
    pinned: Option<Size>,
}

impl ResizeAuthority {
    /// Starts with `base` in effect and nobody holding control.
    ///
    /// `base` is the session's creation size — the desk terminal's geometry in
    /// the ordinary case, and what every unwind eventually returns to.
    pub fn new(base: Size) -> Self {
        Self {
            base,
            claims: Vec::new(),
            pinned: None,
        }
    }

    /// The size the PTY is currently set to.
    pub fn effective(&self) -> Size {
        if let Some(pinned) = self.pinned {
            return pinned;
        }
        self.claims.last().map_or(self.base, |claim| claim.size)
    }

    /// The size in effect with no controller attached.
    pub fn base(&self) -> Size {
        self.base
    }

    /// How many controllers are stacked, deepest last.
    ///
    /// Exposed because "the stack unwound in order" is otherwise only observable
    /// through a sequence of sizes that happen to differ, and a test that relies
    /// on that passes for the wrong reason when two controllers share a size.
    pub fn depth(&self) -> usize {
        self.claims.len()
    }

    /// Whether a size has been pinned against controller changes.
    pub fn is_pinned(&self) -> bool {
        self.pinned.is_some()
    }

    /// Records `id` taking control at `size`, pushing what was in effect.
    ///
    /// Returns the new effective size when it changed, and `None` when it did
    /// not — because the sizes matched, or because a pin is in force. Callers use
    /// that to decide whether to touch the PTY at all: an unnecessary `SIGWINCH`
    /// makes a full-screen application redraw for no reason.
    pub fn take_control(&mut self, id: &AttachmentId, size: Size) -> Option<Size> {
        self.changed(|authority| {
            // An attachment appears at most once. Taking control twice without
            // leaving in between is a client repeating itself, not a second
            // claim to unwind through.
            authority.claims.retain(|claim| &claim.who != id);
            authority.claims.push(Claim {
                who: id.clone(),
                size,
            });
        })
    }

    /// Records the current controller's terminal changing size.
    ///
    /// Ignored, returning `None`, when `id` is not the current controller. That
    /// covers both a watcher sending `resize` and a demoted controller whose
    /// window resizes before it notices it lost control — neither may move the
    /// session.
    pub fn controller_resize(&mut self, id: &AttachmentId, size: Size) -> Option<Size> {
        match self.claims.last() {
            Some(claim) if &claim.who == id => {}
            _ => return None,
        }
        self.changed(|authority| {
            if let Some(claim) = authority.claims.last_mut() {
                claim.size = size;
            }
        })
    }

    /// Records `id` releasing control, by detaching or by being stolen from.
    ///
    /// Releasing the top of the stack reverts to the size beneath it. Releasing
    /// an entry further down removes it without changing the effective size:
    /// a controller that already lost control to a steal cannot move the session
    /// on its way out.
    pub fn release(&mut self, id: &AttachmentId) -> Option<Size> {
        self.changed(|authority| {
            authority.claims.retain(|claim| &claim.who != id);
        })
    }

    /// Sets the size that applies with no controller attached.
    ///
    /// This is `latch resize` without `--pin`: it changes the geometry the
    /// session returns to, and takes effect immediately only when nobody holds
    /// control.
    pub fn set_base(&mut self, size: Size) -> Option<Size> {
        self.changed(|authority| authority.base = size)
    }

    /// Pins `size`, overriding controllers until it is unpinned.
    pub fn pin(&mut self, size: Size) -> Option<Size> {
        self.changed(|authority| authority.pinned = Some(size))
    }

    /// Releases a pin, restoring whatever the stack says is in effect.
    ///
    /// The stack kept tracking while the pin was in force, so this lands on the
    /// current controller rather than on whatever was in effect when the pin was
    /// set.
    pub fn unpin(&mut self) -> Option<Size> {
        self.changed(|authority| authority.pinned = None)
    }

    /// Applies a mutation and reports the effective size only if it moved.
    ///
    /// Every mutator funnels through here so that "did this change the PTY?" is
    /// answered one way. Deriving the answer per-operation is how a release from
    /// the middle of the stack ends up reported as a resize.
    fn changed(&mut self, mutate: impl FnOnce(&mut Self)) -> Option<Size> {
        let before = self.effective();
        mutate(self);
        let after = self.effective();
        (after != before).then_some(after)
    }
}
