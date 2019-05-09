use crate::state_types::{EnvFuture, Environment, Request};
use crate::types::addons::*;
use futures::future;
use std::marker::PhantomData;

mod legacy;
pub use self::legacy::AddonLegacyTransport;

pub const MANIFEST_PATH: &str = "/manifest.json";
pub const LEGACY_PATH: &str = "/stremio/v1";

pub trait AddonInterface {
    fn get(&self, resource_ref: &ResourceRef) -> EnvFuture<ResourceResponse>;
    fn manifest(&self) -> EnvFuture<Manifest>;
}

#[derive(Default)]
pub struct AddonHTTPTransport<'a, T: Environment> {
    env: PhantomData<T>,
    transport_url: &'a str,
}
impl<'a, T: Environment> AddonHTTPTransport<'a, T> {
    pub fn from_url(transport_url: &'a str) -> Self {
        AddonHTTPTransport {
            env: PhantomData,
            transport_url,
        }
    }
}
impl<'a, T: Environment> AddonInterface for AddonHTTPTransport<'a, T> {
    fn get(&self, resource_ref: &ResourceRef) -> EnvFuture<ResourceResponse> {
        let url = self
            .transport_url
            .replace(MANIFEST_PATH, &resource_ref.to_string());
        match Request::get(&url).body(()) {
            Ok(r) => T::fetch_serde::<_, ResourceResponse>(r),
            Err(e) => Box::new(future::err(e.into())),
        }
    }
    fn manifest(&self) -> EnvFuture<Manifest> {
        match Request::get(self.transport_url).body(()) {
            Ok(r) => T::fetch_serde::<_, Manifest>(r),
            Err(e) => Box::new(future::err(e.into())),
        }
    }
}
