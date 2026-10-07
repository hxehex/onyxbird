//! Compatibility and convenience methods for working with [serenity].
//! Requires the `"serenity"` feature.
//!
//! [serenity]: https://crates.io/crates/serenity

use crate::{Config, Onyxbird};
use serenity::{
    client::{ClientBuilder, Context},
    prelude::TypeMapKey,
};
use std::sync::Arc;

/// Zero-size type used to retrieve the registered [`Onyxbird`] instance
/// from serenity's inner [`TypeMap`].
///
/// [`Onyxbird`]: Onyxbird
/// [`TypeMap`]: serenity::prelude::TypeMap
pub struct OnyxbirdKey;

impl TypeMapKey for OnyxbirdKey {
    type Value = Arc<Onyxbird>;
}

/// Installs a new onyxbird instance into the serenity client.
///
/// This should be called after any uses of `ClientBuilder::type_map`.
pub fn register(client_builder: ClientBuilder) -> ClientBuilder {
    let voice = Onyxbird::serenity();
    register_with(client_builder, voice)
}

/// Installs a given onyxbird instance into the serenity client.
///
/// This should be called after any uses of `ClientBuilder::type_map`.
pub fn register_with(client_builder: ClientBuilder, voice: Arc<Onyxbird>) -> ClientBuilder {
    client_builder
        .voice_manager_arc(voice.clone())
        .type_map_insert::<OnyxbirdKey>(voice)
}

/// Installs a given onyxbird instance into the serenity client.
///
/// This should be called after any uses of `ClientBuilder::type_map`.
pub fn register_from_config(client_builder: ClientBuilder, config: Config) -> ClientBuilder {
    let voice = Onyxbird::serenity_from_config(config);
    register_with(client_builder, voice)
}

/// Retrieve the Onyxbird voice client from a serenity context's
/// shared key-value store.
pub async fn get(ctx: &Context) -> Option<Arc<Onyxbird>> {
    let data = ctx.data.read().await;

    data.get::<OnyxbirdKey>().cloned()
}

/// Helper trait to add installation/creation methods to serenity's
/// `ClientBuilder`.
///
/// These install the client to receive gateway voice events, and
/// store an easily accessible reference to Onyxbird's managers.
pub trait SerenityInit {
    /// Registers a new Onyxbird voice system with serenity, storing it for easy
    /// access via [`get`].
    ///
    /// [`get`]: get
    #[must_use]
    fn register_onyxbird(self) -> Self;
    /// Registers a given Onyxbird voice system with serenity, as above.
    #[must_use]
    fn register_onyxbird_with(self, voice: Arc<Onyxbird>) -> Self;
    /// Registers a Onyxbird voice system serenity, based on the given configuration.
    #[must_use]
    fn register_onyxbird_from_config(self, config: Config) -> Self;
}

impl SerenityInit for ClientBuilder {
    fn register_onyxbird(self) -> Self {
        register(self)
    }

    fn register_onyxbird_with(self, voice: Arc<Onyxbird>) -> Self {
        register_with(self, voice)
    }

    fn register_onyxbird_from_config(self, config: Config) -> Self {
        register_from_config(self, config)
    }
}
