use automerge::AutomergeError;

#[derive(Debug)]
pub enum RuntimeStateError {
    MissingScaffold(&'static str),
    InvalidProgressShape,
    Automerge(AutomergeError),
    LockPoisoned,
}

impl std::fmt::Display for RuntimeStateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingScaffold(name) => {
                write!(f, "scaffold map '{name}' missing — doc may be corrupt")
            }
            Self::InvalidProgressShape => {
                write!(f, "env progress phase must serialize as an object")
            }
            Self::Automerge(e) => write!(f, "automerge: {e}"),
            Self::LockPoisoned => write!(f, "RuntimeStateDoc mutex poisoned"),
        }
    }
}

impl std::error::Error for RuntimeStateError {}

impl From<AutomergeError> for RuntimeStateError {
    fn from(e: AutomergeError) -> Self {
        Self::Automerge(e)
    }
}
