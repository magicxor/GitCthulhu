//! Forge accounts (D0.1; owned by X4 after G0b; spec 21 §5.4, §2.8). Milestone 1: no account offers, no Accounts
//! settings page.

use gpui_kit::{App, SharedString};

use crate::backend::CredentialRequest;

/// A connected account the HTTPS credential dialog can offer ("Use GitHub account alice").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountOffer {
    /// "GitHub", "GitLab", "Bitbucket".
    pub forge: SharedString,
    pub user: SharedString,
}

/// The account to offer for an HTTPS username/password prompt. Milestone 1: `None`.
pub fn https_offer(_req: &CredentialRequest, _cx: &App) -> Option<AccountOffer> {
    None
}

/// Whether the settings dialog shows the Accounts page. Milestone 1: `false`.
pub fn available(_cx: &App) -> bool {
    false
}
