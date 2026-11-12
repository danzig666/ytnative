//! LG voice search ("search for cats" on the Magic Remote while ytnative is in front): registers
//! with `com.webos.service.voiceconductor` as the foreground interactor, offers the `SearchContent`
//! intent when asked for the context, and turns a matching `performAction` into a search, through
//! the same queue as launch links (`{"query": …}`).

use crate::ls2::Luna;

pub fn start() {
    plx_base::task::spawn("voice", || {
        let luna = match Luna::shared(plx_base::paths::app_id()) {
            Ok(l) => l,
            Err(e) => return crate::log(&format!("voice: no bus: {e}")),
        };
        let ok = luna.subscribe(
            "luna://com.webos.service.voiceconductor/interactor/register",
            r#"{"type":"foreground","subscribe":true}"#,
            // Replies are calls on the same bus: never block the bus thread that delivers this.
            move |msg| {
                let msg = msg.to_string();
                std::thread::spawn(move || handle(luna, &msg));
            },
        );
        crate::log(&format!("voice: interactor/register -> {ok}"));
    });
}

fn handle(luna: &'static Luna, msg: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else { return };
    let ticket = v["voiceTicket"].clone();
    match v["command"].as_str() {
        Some("setContext") => {
            let p = serde_json::json!({
                "voiceTicket": ticket,
                "inAppIntents": [{ "intent": "SearchContent", "supportAsrOnly": true }],
            });
            let r = luna.call("luna://com.webos.service.voiceconductor/interactor/setContext", &p.to_string());
            crate::log(&format!("voice: setContext -> {r}"));
        }
        Some("performAction") => {
            let a = &v["action"];
            let query = ["value", "itemId", "query", "utterance"]
                .iter()
                .find_map(|k| a[*k].as_str().or_else(|| v[*k].as_str()))
                .map(str::trim)
                .filter(|q| !q.is_empty())
                .map(String::from);
            crate::log(&format!("voice: performAction intent={} query={query:?}", a["intent"]));
            let done = match query {
                Some(q) => {
                    plx_platform::tv::launch::offer(&serde_json::json!({ "query": q }).to_string());
                    plx_machine::present::wake_from_worker();
                    true
                }
                None => false,
            };
            let p = serde_json::json!({ "voiceTicket": ticket, "result": done });
            luna.call("luna://com.webos.service.voiceconductor/interactor/reportActionResult", &p.to_string());
        }
        _ => crate::log(&format!("voice: {}", msg.chars().take(200).collect::<String>())),
    }
}
