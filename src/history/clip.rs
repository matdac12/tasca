//! A Clip: one entry in the History (see `CONTEXT.md`). Plain data, shared by
//! the state machine, the drawing and (later) the store.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClipId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Clip {
    pub id: ClipId,
    /// The copied text. Two Clips with the same text are the same Clip.
    pub text: String,
}
