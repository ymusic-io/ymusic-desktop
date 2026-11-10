//! Signing in without a browser engine: session cookies arrive from the phone app (an encrypted
//! one-shot handoff through the YMusic service, started by a QR the desktop shows) or from a paste
//! (a browser's Copy as cURL, a Cookie header, or a cookie export), and are kept sealed at rest.

mod link;
mod paste;
mod vault;

pub use link::{DeviceLinkOffer, LinkError, await_cookies, close_offer, create_offer, decrypt, encrypt, post_cookies};
pub use paste::{SessionCookie, looks_like_curl, looks_signed_in, parse_cookie_paste, to_set_cookie};
pub use vault::{CookieVault, PasswordSeal, VaultError, os_cookie_key, random_key};
