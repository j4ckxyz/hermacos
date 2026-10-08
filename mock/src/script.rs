//! Canned conversations and replies for the mock dashboard.

use serde_json::{Value, json};

use crate::Session;

pub enum Step {
    Say(String),
    /// Reasoning the model streams before or between actions.
    Think(&'static str),
    Tool { name: &'static str, context: &'static str, summary: &'static str, millis: u64 },
    Approve { command: &'static str, description: &'static str },
}

fn say(text: &str) -> Step {
    Step::Say(text.to_owned())
}

const ROAD_TRIP: &str = "Here are stops worth building the Ohio leg around, grouped by what they're good for.

## Cities

1. **Cleveland**
   - **Rock and Roll Hall of Fame**: exhibits dedicated to the history of rock music.
   - **Cleveland Museum of Art**: a renowned museum with an extensive, *free* collection.
   - **West Side Market**: an iconic public market with local food.
2. **Columbus**
   - **Franklin Park Conservatory**: gardens and plant collections.
   - **Short North Arts District**: galleries, shops and restaurants.
3. **Cincinnati**
   - **Findlay Market**: Ohio's oldest continuously operated public market.

## Natural attractions

| Park | Best for | Drive from Columbus |
|:-----|:---------|--------------------:|
| Hocking Hills State Park | Waterfalls, caves, hiking | 1 h |
| Cuyahoga Valley National Park | Scenic railroad, towpath trail | 2 h |
| Cedar Point (Sandusky) | Roller coasters | 2 h 15 m |

> Hocking Hills gets crowded on autumn weekends. Arrive before 9 am or go midweek.

Park details are on the [National Park Service site](https://www.nps.gov/cuva/index.htm), and the trail map lives at https://www.alltrails.com/parks/us/ohio/hocking-hills-state-park.";

const CODE_REPLY: &str = "A debounce wraps a function so it only runs after calls have stopped for a while. In Rust with `tokio`:

```rust
use std::time::Duration;
use tokio::sync::mpsc;

/// Forward the latest value once `quiet` has passed without a newer one.
pub async fn debounce<T>(mut input: mpsc::Receiver<T>, quiet: Duration, mut emit: impl FnMut(T)) {
    let mut latest: Option<T> = None;
    loop {
        tokio::select! {
            value = input.recv() => match value {
                Some(v) => latest = Some(v),
                None => break, // sender dropped
            },
            _ = tokio::time::sleep(quiet), if latest.is_some() => {
                emit(latest.take().unwrap());
            }
        }
    }
}
```

Two things to notice:

- The `if latest.is_some()` guard keeps the timer branch **disabled** while there is nothing to emit.
- Every new value restarts the sleep, because `select!` builds a fresh future each iteration.

The same idea in Python is shorter but hides the cancellation:

```python
async def debounce(queue, quiet, emit):
    latest = None
    while True:
        try:
            latest = await asyncio.wait_for(queue.get(), quiet)
        except asyncio.TimeoutError:
            if latest is not None:
                emit(latest)
                latest = None
```

Call it with `quiet = 0.3` for search-as-you-type.";

const SHORT_REPLY: &str = "Hi! I'm your Hermes agent, running on the VPS. I can search the web, run commands on the host, manage scheduled jobs and remember things between conversations.

Try asking me for a **road trip plan**, some `code`, or to **deploy** something.";

const RESEARCH: &str = "The film has been restored in 4K from the original footage, with newly remixed audio from the original multitracks. Expect a concert rather than a documentary, with songs including “A Kind of Magic” and “Bohemian Rhapsody”.[1]

## What early viewers thought

The most useful firsthand review is from a journalist who saw the **London premiere at BFI IMAX**. They praised the crisp restoration and the remixed sound.[2] The band's own account of the Budapest premiere reported a strong audience response, though without detailed reviews.[1, 4]

**Bottom line:** early evidence is positive about the performance and the restoration. Showtimes are on the cinema's page.[3]

## Sources

[1] [QueenOnline: release details and set highlights](https://www.queenonline.com/news)
[2] [Forbes: review of the London IMAX premiere](https://www.forbes.com/sites/hughmcintyre/)
[3] [Cineworld Jersey listings](https://www.cineworld.co.uk/cinemas/jersey)
[4] [QueenOnline: Budapest premiere report](https://www.queenonline.com/news/budapest)";

pub fn turn_for(prompt: &str) -> Vec<Step> {
    let p = prompt.to_lowercase();
    if p.contains("queen") || p.contains("sources") || p.contains("research") {
        return vec![
            Step::Think("The user wants what is on tonight and what people who have seen it said. I should ground every claim in a source. "),
            Step::Tool { name: "skill_view", context: "grounded-citations", summary: "", millis: 300 },
            Step::Think("Search for the official release details first, then reviews from the premiere, then local listings. "),
            Step::Tool { name: "web_search", context: "Queen Budapest film Jersey tonight", summary: "Did 6 searches in 2.3s", millis: 1500 },
            Step::Tool { name: "web_search", context: "site:cineworld.co.uk Jersey 7 October 2026", summary: "Did 1 search in 2.3s", millis: 1300 },
            Step::Tool { name: "web_search", context: "Queen Budapest premiere October 2026 social reactions", summary: "Did 8 searches in 2.3s", millis: 1400 },
            Step::Tool { name: "web_search", context: "Queen Rock Montreal Cineworld UK official", summary: "Did 6 searches in 2.3s", millis: 1200 },
            Step::Think("Enough to answer. Separate the promotional reactions from the one independent review. "),
            say(RESEARCH),
        ];
    }
    if p.contains("deploy") || p.contains("approve") {
        vec![
            say("I'll check what's running first.\n\n"),
            Step::Tool { name: "terminal", context: "docker compose ps", summary: "3 services running", millis: 900 },
            say("All three services are up. Restarting the gateway needs your go-ahead.\n\n"),
            Step::Approve {
                command: "docker compose up -d --force-recreate gateway",
                description: "Recreates the gateway container. Active chats reconnect within a few seconds.",
            },
            Step::Tool { name: "terminal", context: "docker compose up -d --force-recreate gateway", summary: "Container gateway recreated", millis: 1400 },
            say("Done. The gateway came back **healthy** in 6 seconds and Discord reconnected.\n\n- Image: `hermes-agent:0.21.5`\n- Uptime: 6 s\n- Platforms: Discord ✓, Telegram ✓"),
        ]
    } else if p.contains("code") || p.contains("rust") || p.contains("debounce") {
        vec![say(CODE_REPLY)]
    } else if p.contains("trip") || p.contains("ohio") || p.contains("table") || p.contains("plan") {
        vec![
            say("Let me look up current opening times before I plan anything.\n\n"),
            Step::Tool { name: "web_search", context: "Ohio road trip stops Hocking Hills hours", summary: "8 results", millis: 1100 },
            Step::Tool { name: "web_extract", context: "https://www.nps.gov/cuva/index.htm", summary: "Read 2 pages", millis: 800 },
            say(ROAD_TRIP),
        ]
    } else {
        vec![say(SHORT_REPLY)]
    }
}

pub fn title_for(prompt: &str) -> String {
    let words: Vec<&str> = prompt.split_whitespace().filter(|w| !w.starts_with("@file:")).take(6).collect();
    let mut title = words.join(" ");
    if let Some(first) = title.get(..1) {
        title = first.to_uppercase() + &title[1..];
    }
    title.trim_end_matches(['?', '.', '!']).to_owned()
}

fn rows(pairs: &[(&str, Value)], at: f64) -> Vec<Value> {
    pairs
        .iter()
        .enumerate()
        .map(|(i, (role, body))| {
            let mut row = body.clone();
            if let Some(text) = row.as_str().map(str::to_owned) {
                row = json!({ "content": text });
            }
            row["role"] = json!(role);
            row["id"] = json!(i as i64 + 1);
            row["timestamp"] = json!(at + i as f64 * 20.0);
            row
        })
        .collect()
}

pub fn seed_sessions(now: f64, many: bool) -> Vec<Session> {
    let hour = 3600.0;
    let day = 24.0 * hour;
    let session = |id: &str, title: &str, source: &str, age: f64, pairs: &[(&str, Value)]| Session {
        id: id.to_owned(),
        title: title.to_owned(),
        source: source.to_owned(),
        started_at: now - age - 600.0,
        last_active: now - age,
        rows: rows(pairs, now - age - 600.0),
    };
    let mut sessions = vec![
        session(
            "20261007_091500_a1",
            "US Road Trip Stops",
            "desktop",
            2.0 * hour,
            &[
                ("user", json!("Plan the Ohio leg of my road trip. What are the stops worth making?")),
                ("assistant", json!({ "content": "Let me look up current opening times before I plan anything.", "tool_calls": [
                    { "id": "c1", "function": { "name": "web_search", "arguments": "{\"query\":\"Ohio road trip stops Hocking Hills hours\"}" } }
                ]})),
                ("tool", json!({ "tool_call_id": "c1", "tool_name": "web_search", "content": "{\"results\": 8}" })),
                ("assistant", json!(ROAD_TRIP)),
            ],
        ),
        session(
            "20261007_073000_b2",
            "Restart the gateway container",
            "discord",
            5.0 * hour,
            &[
                ("user", json!("the gateway keeps dropping discord, can you restart it")),
                ("assistant", json!({ "content": "", "tool_calls": [
                    { "id": "c1", "function": { "name": "terminal", "arguments": "{\"command\":\"docker compose restart gateway\"}" } }
                ]})),
                ("tool", json!({ "tool_call_id": "c1", "tool_name": "terminal", "content": "{\"exit_code\": 0}" })),
                ("assistant", json!("Restarted. Discord reconnected after **4 seconds** and the heartbeat is steady again.")),
            ],
        ),
        session(
            "20261006_180000_c3",
            "Debounce in Rust and Python",
            "cli",
            day + 3.0 * hour,
            &[("user", json!("show me a debounce in rust, then python")), ("assistant", json!(CODE_REPLY))],
        ),
        session(
            "20261006_080000_d4",
            "Morning digest",
            "cron",
            day + 9.0 * hour,
            &[
                ("user", json!("Run the morning digest.")),
                ("assistant", json!("**3 things for today**\n\n1. Renew the `ts.net` certificate (expires in 9 days).\n2. Two pull requests are waiting on review.\n3. Disk on the VPS is at 26%, no action needed.")),
            ],
        ),
        session(
            "20261003_120000_e5",
            "Tailscale ACL review",
            "telegram",
            4.0 * day,
            &[
                ("user", json!("can you check my tailscale ACLs for anything too open")),
                ("assistant", json!("One rule stands out: `\"*:*\"` for the `tag:server` group lets every server reach every port on every device. Narrow it to the ports you use:\n\n```json\n{\n  \"action\": \"accept\",\n  \"src\": [\"tag:server\"],\n  \"dst\": [\"tag:server:22,443,9119\"]\n}\n```")),
            ],
        ),
        session(
            "20260920_100000_f6",
            "Sourdough hydration maths",
            "desktop",
            17.0 * day,
            &[
                ("user", json!("if I have 450g flour and want 78% hydration how much water")),
                ("assistant", json!("**351 g** of water (450 × 0.78). If your starter is 100% hydration, subtract half its weight from both the flour and the water.")),
            ],
        ),
        session(
            "20260811_100000_g7",
            "Backup rotation script",
            "cli",
            57.0 * day,
            &[
                ("user", json!("write a backup rotation script keeping 7 daily and 4 weekly")),
                ("assistant", json!("```bash\n#!/usr/bin/env bash\nset -euo pipefail\n# Keep 7 daily and 4 weekly archives.\nfind /backups/daily -name '*.tar.zst' -mtime +7 -delete\nfind /backups/weekly -name '*.tar.zst' -mtime +28 -delete\n```")),
            ],
        ),
    ];
    if many {
        // A long history from other surfaces, to exercise paging.
        let sources = ["discord", "cli", "telegram", "cron", "desktop"];
        for n in 0..150 {
            let age = (60.0 + n as f64 * 0.6) * day;
            let id = format!("2026archive_{n:03}");
            let title = format!("Archived conversation {}", n + 1);
            sessions.push(session(&id, &title, sources[n % sources.len()], age, &[
                ("user", json!("an older question")),
                ("assistant", json!("An older answer.")),
            ]));
        }
    }
    sessions
}
