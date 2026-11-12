//! Log paging (owned by G4 after G0b; spec 21 §4.2): drives `RepoModel::log_pager`, appends pages with
//! `ChunkedRows::appended` and drops pages whose snapshot generation is stale.
