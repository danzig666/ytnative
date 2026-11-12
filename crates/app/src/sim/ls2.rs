//! Windows simulator stand-in for `ls2.rs`: there is no luna-service2 bus off the TV. `connect`
//! always fails, so nothing ever holds a `Luna`.
#![allow(dead_code)]

pub struct Luna;

impl Luna {
    pub fn connect(_app_id: &str) -> Result<&'static Luna, String> {
        Err("no LS2 bus in the simulator".into())
    }

    pub fn call(&self, _uri: &str, _payload: &str) -> String {
        "<no LS2 bus in the simulator>".into()
    }

    pub fn subscribe(&self, _uri: &str, _payload: &str, _f: impl FnMut(&str) + Send + 'static) -> bool {
        false
    }
}
