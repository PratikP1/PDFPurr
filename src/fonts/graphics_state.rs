//! Graphics state tracking for PDF content stream processing.
//!
//! Tracks the text state parameters that affect text rendering and
//! extraction, particularly the current font. The graphics state stack
//! supports `q` (save) and `Q` (restore) operators.
//!
//! ISO 32000-2:2020, Section 8.4 (Graphics State) and Section 9.3 (Text State).

/// Text state parameters that affect text rendering.
///
/// These are set by text state operators within a content stream.
#[derive(Debug, Clone)]
pub struct TextState {
    /// Current font name (set by `Tf` operator, e.g., "F1").
    pub font_name: Option<String>,
    /// Current font size in text space units (set by `Tf` operator).
    pub font_size: f64,
    /// Character spacing in text space units (set by `Tc` operator).
    pub character_spacing: f64,
    /// Word spacing in text space units (set by `Tw` operator).
    pub word_spacing: f64,
    /// Text leading in text space units (set by `TL` operator).
    pub leading: f64,
    /// Horizontal scaling as a percentage (set by `Tz` operator, default 100).
    pub horizontal_scaling: f64,
    /// Text rise in text space units (set by `Ts` operator).
    pub rise: f64,
    /// Text rendering mode (set by `Tr` operator, default 0 = fill).
    ///
    /// 0 = Fill, 1 = Stroke, 2 = Fill then stroke, 3 = Invisible,
    /// 4–7 = same but also add to clipping path.
    pub rendering_mode: u8,
}

impl Default for TextState {
    fn default() -> Self {
        Self {
            font_name: None,
            font_size: 0.0,
            character_spacing: 0.0,
            word_spacing: 0.0,
            leading: 0.0,
            horizontal_scaling: 100.0,
            rise: 0.0,
            rendering_mode: 0,
        }
    }
}

/// Graphics state encompassing text state and other rendering parameters.
#[derive(Debug, Clone, Default)]
pub struct GraphicsState {
    /// Text-specific state parameters.
    pub text_state: TextState,
}

/// A stack of graphics states supporting `q`/`Q` (save/restore) operations.
///
/// The current state is always accessible. `save()` pushes a copy onto
/// the stack, and `restore()` pops the most recent saved state.
#[derive(Debug)]
pub struct GraphicsStateStack {
    /// Stack of saved states (most recent on top).
    stack: Vec<GraphicsState>,
    /// The current active graphics state.
    current: GraphicsState,
}

impl GraphicsStateStack {
    /// Creates a new graphics state stack with default state.
    pub fn new() -> Self {
        Self {
            stack: Vec::new(),
            current: GraphicsState::default(),
        }
    }

    /// Saves the current graphics state (pushes onto stack).
    /// Corresponds to the `q` operator.
    pub fn save(&mut self) {
        self.stack.push(self.current.clone());
    }

    /// Restores the most recently saved graphics state (pops from stack).
    /// Corresponds to the `Q` operator.
    /// If the stack is empty, resets to default state.
    pub fn restore(&mut self) {
        self.current = self.stack.pop().unwrap_or_default();
    }

    /// Returns a reference to the current graphics state.
    pub fn current(&self) -> &GraphicsState {
        &self.current
    }

    /// Returns a mutable reference to the current graphics state.
    pub fn current_mut(&mut self) -> &mut GraphicsState {
        &mut self.current
    }

    /// Returns the current stack depth (number of saved states).
    pub fn depth(&self) -> usize {
        self.stack.len()
    }
}

impl Default for GraphicsStateStack {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_state() {
        let ts = TextState::default();
        assert_eq!(ts.font_name, None);
        assert_eq!(ts.font_size, 0.0);
        assert_eq!(ts.character_spacing, 0.0);
        assert_eq!(ts.word_spacing, 0.0);
        assert_eq!(ts.leading, 0.0);
        assert_eq!(ts.horizontal_scaling, 100.0);
        assert_eq!(ts.rise, 0.0);
    }

    #[test]
    fn save_and_restore() {
        let mut stack = GraphicsStateStack::new();
        stack.current_mut().text_state.font_name = Some("F1".to_string());
        stack.current_mut().text_state.font_size = 12.0;

        stack.save();
        assert_eq!(stack.depth(), 1);

        // Modify current state
        stack.current_mut().text_state.font_name = Some("F2".to_string());
        stack.current_mut().text_state.font_size = 24.0;
        assert_eq!(stack.current().text_state.font_name, Some("F2".to_string()));

        // Restore previous state
        stack.restore();
        assert_eq!(stack.depth(), 0);
        assert_eq!(stack.current().text_state.font_name, Some("F1".to_string()));
        assert_eq!(stack.current().text_state.font_size, 12.0);
    }

    #[test]
    fn restore_empty_stack_resets_to_default() {
        let mut stack = GraphicsStateStack::new();
        stack.current_mut().text_state.font_size = 42.0;

        stack.restore(); // stack is empty
        assert_eq!(stack.current().text_state.font_size, 0.0);
        assert_eq!(stack.current().text_state.font_name, None);
    }

    #[test]
    fn nested_save_restore() {
        let mut stack = GraphicsStateStack::new();

        stack.current_mut().text_state.font_name = Some("F1".to_string());
        stack.save();
        stack.current_mut().text_state.font_name = Some("F2".to_string());
        stack.save();
        stack.current_mut().text_state.font_name = Some("F3".to_string());

        assert_eq!(stack.depth(), 2);
        assert_eq!(stack.current().text_state.font_name, Some("F3".to_string()));

        stack.restore();
        assert_eq!(stack.current().text_state.font_name, Some("F2".to_string()));

        stack.restore();
        assert_eq!(stack.current().text_state.font_name, Some("F1".to_string()));
    }

    #[test]
    fn text_state_operators() {
        let mut stack = GraphicsStateStack::new();
        let ts = &mut stack.current_mut().text_state;

        // Simulate Tf operator: /F1 12 Tf
        ts.font_name = Some("F1".to_string());
        ts.font_size = 12.0;

        // Simulate Tc operator: 0.5 Tc
        ts.character_spacing = 0.5;

        // Simulate Tw operator: 1.0 Tw
        ts.word_spacing = 1.0;

        // Simulate TL operator: 14 TL
        ts.leading = 14.0;

        assert_eq!(stack.current().text_state.font_name, Some("F1".to_string()));
        assert_eq!(stack.current().text_state.font_size, 12.0);
        assert_eq!(stack.current().text_state.character_spacing, 0.5);
        assert_eq!(stack.current().text_state.word_spacing, 1.0);
        assert_eq!(stack.current().text_state.leading, 14.0);
    }
}
