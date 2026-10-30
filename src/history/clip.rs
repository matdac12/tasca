//! A Clip: one entry in the History (see `CONTEXT.md`). Plain data, shared by
//! the state machine, the drawing and (later) the store.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ClipId(pub u64);

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Clip {
    pub id: ClipId,
    /// The copied text. Two Clips with the same text are the same Clip.
    pub text: String,
    /// When it was last copied (or pasted back), in ms since the Unix epoch.
    /// A plain number so the store can keep it as it is.
    pub last_copied_ms: u64,
}
