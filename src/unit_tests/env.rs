use crate::state_types::{EnvFuture, Environment};
use chrono::{DateTime, Utc};
use futures::{future, Future};
use lazy_static::lazy_static;
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::collections::BTreeMap;
use std::collections::HashMap;
use std::sync::RwLock;
use tokio::executor::current_thread::spawn;

lazy_static! {
    pub static ref REQUESTS: RwLock<Vec<Request>> = Default::default();
    pub static ref STORAGE: RwLock<BTreeMap<String, String>> = Default::default();
    pub static ref NOW: RwLock<DateTime<Utc>> = RwLock::new(Utc::now());
}

#[derive(Clone, Debug)]
pub struct Request {
    pub url: String,
    pub method: String,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl<T: 'static + Serialize> From<http::Request<T>> for Request {
    fn from(request: http::Request<T>) -> Self {
        let (head, body) = request.into_parts();
        Request {
            url: head.uri.to_string(),
            method: head.method.as_str().to_owned(),
            headers: head
                .headers
                .iter()
                .map(|(key, value)| (key.as_str().to_owned(), value.to_str().unwrap().to_owned()))
                .collect::<HashMap<_, _>>(),
            body: serde_json::to_string(&body).unwrap(),
        }
    }
}

pub struct Env {}

impl Env {
    pub fn reset() {
        *REQUESTS.write().unwrap() = vec![];
        *STORAGE.write().unwrap() = BTreeMap::new();
        *NOW.write().unwrap() = Utc::now();
    }
}

impl Environment for Env {
    fn fetch_serde<IN, OUT>(request: http::Request<IN>) -> EnvFuture<OUT>
    where
        IN: 'static + Serialize,
        OUT: 'static + DeserializeOwned,
    {
        let request = Request::from(request);
        REQUESTS.write().unwrap().push(request.to_owned());
        Env::unit_test_fetch(request)
    }
    fn get_storage<T: 'static + DeserializeOwned>(key: &str) -> EnvFuture<Option<T>> {
        Box::new(future::ok(
            STORAGE
                .read()
                .unwrap()
                .get(key)
                .map(|data| serde_json::from_str(&data).unwrap()),
        ))
    }
    fn set_storage<T: Serialize>(key: &str, value: Option<&T>) -> EnvFuture<()> {
        let mut storage = STORAGE.write().unwrap();
        match value {
            Some(v) => storage.insert(key.to_string(), serde_json::to_string(v).unwrap()),
            None => storage.remove(key),
        };
        Box::new(future::ok(()))
    }
    fn now() -> DateTime<Utc> {
        *NOW.read().unwrap()
    }
    fn exec(fut: Box<dyn Future<Item = (), Error = ()>>) {
        spawn(fut);
    }
}
