//! The app's view of the remote config service: feature flags, the published JS bundles, the
//! newest app build. Plus bundle authenticity and the cached-above-embedded bundle source.

mod bundle;
mod config;

pub use bundle::{BundleError, BundleSource, VerifiedBundle, verify_signature};
pub use config::{ApiIdentity, AppUpdate, BundleVersion, RelayError, RemoteConfig, RemoteConfigData};
