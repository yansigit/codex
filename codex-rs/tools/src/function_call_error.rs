use thiserror::Error;

/// Error returned while executing a model-visible tool invocation.
#[derive(Debug, Error, PartialEq)]
pub enum FunctionCallError {
    #[error("{0}")]
    RespondToModel(String),
    /// The host refused execution. The message remains ordinary model-visible output.
    #[error("{0}")]
    Rejected(String),
    #[error("Fatal error: {0}")]
    Fatal(String),
}

#[cfg(test)]
mod tests {
    use super::FunctionCallError;

    #[test]
    fn rejection_preserves_model_visible_display() {
        let message = "execution refused by host";
        assert_eq!(
            FunctionCallError::Rejected(message.into()).to_string(),
            message
        );
        assert_eq!(
            FunctionCallError::Rejected(message.into()).to_string(),
            FunctionCallError::RespondToModel(message.into()).to_string()
        );
    }
}
