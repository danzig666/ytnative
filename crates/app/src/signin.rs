//! Sign-in worker: TV device-code flow ("go to the URL, enter XXX-XXX-XXX"). Each successful
//! sign-in adds an account (multi-account) and makes it current. Reports each stage to the
//! screen through `post`; honours `cancel`.

use crate::host::SignIn;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use yt_core::auth::{self, Poll};

pub fn run(cancel: &AtomicBool, post: &dyn Fn(SignIn)) -> anyhow::Result<()> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let cfg = yt_core::bootstrap::fetch(&http)?;
    let code = auth::start_device_login(&http, &cfg)?;
    post(SignIn::Code { user_code: code.user_code.clone(), url: code.verification_url.clone() });
    let mut interval = code.interval.max(2);
    loop {
        for _ in 0..interval * 4 {
            if cancel.load(Ordering::SeqCst) {
                post(SignIn::Idle);
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(250));
        }
        match auth::poll_device_login(&http, &cfg, &code)? {
            Poll::Pending => {}
            Poll::SlowDown => interval += 2,
            Poll::Expired => anyhow::bail!("the code expired, try again"),
            Poll::Denied => anyhow::bail!("sign-in was declined"),
            Poll::Done(token) => {
                let account = yt_core::accounts::add(&http, &cfg, token)?;
                post(SignIn::Done(account.name));
                return Ok(());
            }
        }
    }
}
