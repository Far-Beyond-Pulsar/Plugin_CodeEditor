//! AI assistance inside the code editor.
//!
//! - [`prompt`] -- what is asked of the model and how the reply is read.
//! - [`inline`] -- the prompt box that opens in the text (Ctrl+I), runs the
//!   request and hands edits to Mockaco's inline review.
//! - [`tools`] -- the editor's tools for the agent chat panel, so the chat
//!   can read the open buffers and propose edits that land as the same
//!   inline review.

pub mod inline;
pub mod prompt;
pub mod tools;

pub use inline::{InlineAssist, InlineAssistEvent};
pub use prompt::AssistKind;
