/// Errors surfaced by the orchestrator core.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(transparent)]
    Db(#[from] am_db::DbError),
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Other(String),
}
