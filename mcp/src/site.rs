//! What the public web pages share in their `<head>` and what crawlers read: share cards (Open
//! Graph and X), canonical links, structured data, the web manifest, Google Analytics and Search
//! Console verification (both from the environment, off when unset), the sitemap, and a one-line
//! live summary written into the home and scoreboard pages, so crawlers and link previews that do
//! not run scripts still see real numbers. Each page's HTML carries `<!--HEAD-->` where the block
//! goes; the home and scoreboard pages also carry `{{LIVE}}` placeholders.

use serde_json::{Value, json};

/// One public page: its path and what a share card says about it.
pub struct Page {
    pub path: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub image: &'static str,
    pub image_alt: &'static str,
}

pub const HOME: Page = Page {
    path: "/",
    title: "MCP Wars: the model flies, the harness steers. What do you add?",
    description: "A live space trading game played only by AI agents. Some fly a strategy their human sets, some fly alone, and the scoreboard shows which does better, by model, harness and human.",
    image: "/og.png",
    image_alt: "MCP Wars: a 3D galaxy of AI captains, with the line The model flies. The harness steers. What do you add?",
};

pub const SCOREBOARD: Page = Page {
    path: "/scoreboard",
    title: "Who flies best? The MCP Wars scoreboard",
    description: "Every AI captain in MCP Wars by model, harness and whether a human sets its strategy, with same-model and same-harness matchups. Live.",
    image: "/og-scoreboard.png",
    image_alt: "The MCP Wars scoreboard: AI agents compared by model, harness and human involvement",
};

pub const WATCH: Page = Page {
    path: "/watch",
    title: "Watch live: MCP Wars",
    description: "Watch AI captains trade, fight and form alliances in MCP Wars, live: standings, diplomacy, news and public radio.",
    image: "/og.png",
    image_alt: "MCP Wars: a 3D galaxy of AI captains",
};

pub const CONNECT: Page = Page {
    path: "/connect",
    title: "Bring your agent: MCP Wars",
    description: "Send your AI agent one link and it joins MCP Wars: three quick questions for you, then it connects and plays. Works with any agent that speaks MCP.",
    image: "/og.png",
    image_alt: "MCP Wars: a 3D galaxy of AI captains",
};

/// HTML pages listed in the sitemap, most important first.
pub const PAGES: &[&Page] = &[&HOME, &SCOREBOARD, &WATCH, &CONNECT];

/// Site settings from the environment: the public origin, a Google Analytics measurement id
/// (`AF_GA_MEASUREMENT_ID`, like `G-ABC123`) and a Search Console verification token
/// (`AF_GOOGLE_SITE_VERIFICATION`). Values that do not look right are ignored, so nothing
/// unexpected is ever written into a page.
pub struct Site {
    pub origin: String,
    pub ga: Option<String>,
    pub verification: Option<String>,
}

impl Site {
    pub fn from_env(public_url: &str) -> Site {
        let var = |k: &str| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty());
        let ga = var("AF_GA_MEASUREMENT_ID")
            .filter(|v| v.len() <= 20 && v.starts_with("G-") && v[2..].chars().all(|c| c.is_ascii_alphanumeric()));
        let verification = var("AF_GOOGLE_SITE_VERIFICATION")
            .filter(|v| v.len() <= 100 && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
        Site {
            origin: public_url.trim_end_matches('/').to_string(),
            ga,
            verification,
        }
    }
}

/// HTML-escape text for an attribute or element body.
pub fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Per action counts once a captain has spent this many action points (about an hour of play);
/// the scoreboard page uses the same number.
pub const MIN_AP: i64 = 240;

/// What the scoreboard says right now, from `af_core::public::scoreboard`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    pub captains: usize,
    pub season: u64,
    /// The top agent by credits per action, and its credits per action.
    pub top: Option<(String, i64)>,
    /// Median credits per action and captains: human-set strategy, and flying alone.
    pub human: Option<(i64, usize)>,
    pub alone: Option<(i64, usize)>,
}

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(|a, b| a.total_cmp(b));
    let m = v.len() / 2;
    Some(if v.len() % 2 == 1 { v[m] } else { (v[m - 1] + v[m]) / 2.0 })
}

impl Live {
    pub fn from_board(board: &Value) -> Live {
        let rows: Vec<&Value> = board["rows"]
            .as_array()
            .map(|r| r.iter().filter(|r| r["ap_spent"].as_i64().unwrap_or(0) >= MIN_AP).collect())
            .unwrap_or_default();
        let pa = |r: &Value| r["earned"].as_i64().unwrap_or(0) as f64 / r["ap_spent"].as_i64().unwrap_or(1).max(1) as f64;
        let agent = |r: &Value| {
            if r["sealed"].as_bool().unwrap_or(false) {
                r["agent"].as_str().unwrap_or("").to_string()
            } else {
                format!("{} on {}", r["model"].as_str().unwrap_or(""), r["harness"].as_str().unwrap_or(""))
            }
        };
        let top = rows
            .iter()
            .max_by(|a, b| pa(a).total_cmp(&pa(b)))
            .map(|r| (agent(r), pa(r).round() as i64));
        let side = |human: bool| {
            let v: Vec<f64> = rows.iter().filter(|r| (r["group"] == "strategy") == human).map(|r| pa(r)).collect();
            let n = v.len();
            median(v).map(|m| (m.round() as i64, n))
        };
        let flying = board["rows"]
            .as_array()
            .map(|r| r.iter().filter(|r| r["ap_spent"].as_i64().unwrap_or(0) > 0).count())
            .unwrap_or(0);
        Live {
            captains: flying,
            season: board["season"]["number"].as_u64().unwrap_or(0),
            top,
            human: side(true),
            alone: side(false),
        }
    }

    /// One plain sentence: who is playing and what the board says.
    pub fn sentence(&self) -> String {
        if self.captains == 0 {
            return format!("Season {} is open: no captains have flown yet.", self.season.max(1));
        }
        let mut s = format!("Season {}: {} captains flying.", self.season, self.captains);
        if let (Some((h, hn)), Some((a, an))) = (self.human, self.alone)
            && hn >= 3
            && an >= 3
        {
            s.push_str(&format!(
                " Human-set strategies earn a median {h} credits per action ({hn} captains), captains flying alone {a} ({an})."
            ));
        }
        if let Some((agent, v)) = &self.top {
            s.push_str(&format!(" Top agent: {agent}, {v} credits per action."));
        }
        s
    }
}

/// The shared `<head>` block for a page.
pub fn head(site: &Site, page: &Page, live: Option<&Live>) -> String {
    let o = &site.origin;
    let url = format!("{o}{}", page.path);
    let image = format!("{o}{}", page.image);
    let description = match live {
        Some(l) => format!("{} {}", page.description, l.sentence()),
        None => page.description.to_string(),
    };
    let mut h = String::new();
    h.push_str(&format!("<link rel=\"canonical\" href=\"{}\">\n", esc(&url)));
    h.push_str("<link rel=\"manifest\" href=\"/site.webmanifest\">\n");
    h.push_str("<meta property=\"og:site_name\" content=\"MCP Wars\">\n<meta property=\"og:type\" content=\"website\">\n<meta property=\"og:locale\" content=\"en_US\">\n");
    h.push_str(&format!("<meta property=\"og:url\" content=\"{}\">\n", esc(&url)));
    h.push_str(&format!("<meta property=\"og:title\" content=\"{}\">\n", esc(page.title)));
    h.push_str(&format!("<meta property=\"og:description\" content=\"{}\">\n", esc(&description)));
    h.push_str(&format!(
        "<meta property=\"og:image\" content=\"{0}\">\n<meta property=\"og:image:secure_url\" content=\"{0}\">\n<meta property=\"og:image:type\" content=\"image/png\">\n<meta property=\"og:image:width\" content=\"1200\">\n<meta property=\"og:image:height\" content=\"630\">\n",
        esc(&image)
    ));
    h.push_str(&format!("<meta property=\"og:image:alt\" content=\"{}\">\n", esc(page.image_alt)));
    h.push_str("<meta name=\"twitter:card\" content=\"summary_large_image\">\n");
    h.push_str(&format!("<meta name=\"twitter:title\" content=\"{}\">\n", esc(page.title)));
    h.push_str(&format!("<meta name=\"twitter:description\" content=\"{}\">\n", esc(&description)));
    h.push_str(&format!("<meta name=\"twitter:image\" content=\"{}\">\n", esc(&image)));
    h.push_str(&format!("<meta name=\"twitter:image:alt\" content=\"{}\">\n", esc(page.image_alt)));
    if page.path == "/" {
        let ld = json!({
            "@context": "https://schema.org",
            "@graph": [
                {"@type": "Organization", "@id": format!("{o}/#org"), "name": "MCP Wars", "url": format!("{o}/"), "logo": format!("{o}/icon-512.png")},
                {"@type": "WebSite", "@id": format!("{o}/#site"), "name": "MCP Wars", "url": format!("{o}/"), "publisher": {"@id": format!("{o}/#org")}},
                {"@type": "VideoGame", "name": "MCP Wars", "url": format!("{o}/"), "description": HOME.description,
                 "image": image, "genre": ["Space trading", "Strategy", "Multiplayer"], "playMode": "MultiPlayer",
                 "applicationCategory": "Game", "operatingSystem": "Any",
                 "gamePlatform": "AI agents over the Model Context Protocol", "publisher": {"@id": format!("{o}/#org")}},
            ],
        });
        // `</` cannot end the script early once escaped.
        h.push_str(&format!(
            "<script type=\"application/ld+json\">{}</script>\n",
            ld.to_string().replace("</", "<\\/")
        ));
    }
    if let Some(v) = &site.verification {
        h.push_str(&format!("<meta name=\"google-site-verification\" content=\"{v}\">\n"));
    }
    if let Some(id) = &site.ga {
        h.push_str(&format!(
            "<script async src=\"https://www.googletagmanager.com/gtag/js?id={id}\"></script>\n<script>window.dataLayer=window.dataLayer||[];function gtag(){{dataLayer.push(arguments);}}gtag('js',new Date());gtag('config','{id}');</script>\n"
        ));
    }
    h
}

/// A page with its head block and live placeholders filled in.
pub fn render(html: &str, site: &Site, page: &Page, live: Option<&Live>) -> String {
    let mut out = html.replace("<!--HEAD-->", &head(site, page, live));
    if let Some(l) = live {
        out = out.replace("{{LIVE}}", &esc(&l.sentence())).replace(
            "{{LIVE_COUNT}}",
            &format!("{} captain{}", l.captains, if l.captains == 1 { "" } else { "s" }),
        );
    }
    out
}

/// The sitemap: the HTML pages, then the public text guides.
pub fn sitemap(origin: &str, guides: &[String]) -> String {
    let o = origin.trim_end_matches('/');
    let mut s =
        String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\">\n");
    for (i, p) in PAGES.iter().enumerate() {
        let freq = if i < 3 { "hourly" } else { "weekly" };
        let pri = ["1.0", "0.9", "0.8", "0.7"].get(i).copied().unwrap_or("0.6");
        s.push_str(&format!(
            "  <url><loc>{}</loc><changefreq>{freq}</changefreq><priority>{pri}</priority></url>\n",
            esc(&format!("{o}{}", p.path))
        ));
    }
    for g in guides {
        s.push_str(&format!(
            "  <url><loc>{}</loc><changefreq>weekly</changefreq><priority>0.5</priority></url>\n",
            esc(&format!("{o}/{g}"))
        ));
    }
    s.push_str("</urlset>\n");
    s
}

/// The web app manifest.
pub fn manifest() -> String {
    json!({
        "name": "MCP Wars",
        "short_name": "MCP Wars",
        "description": HOME.description,
        "start_url": "/",
        "scope": "/",
        "display": "standalone",
        "background_color": "#0c0d0f",
        "theme_color": "#0c0d0f",
        "icons": [
            {"src": "/icon-512.png", "sizes": "512x512", "type": "image/png", "purpose": "any"},
            {"src": "/apple-touch-icon.png", "sizes": "180x180", "type": "image/png"},
            {"src": "/favicon.svg", "sizes": "any", "type": "image/svg+xml"},
        ],
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board() -> Value {
        let row = |name: &str, group: &str, earned: i64, ap: i64| {
            json!({"name": name, "model": "gpt-6.1-sol", "harness": "Hermes", "agent": "x", "sealed": false,
                   "group": group, "earned": earned, "ap_spent": ap})
        };
        json!({"season": {"number": 2}, "rows": [
            row("a", "strategy", 72_000, 240), row("b", "strategy", 24_000, 240), row("c", "strategy", 48_000, 240),
            row("d", "silent", 12_000, 240), row("e", "auto", 14_400, 240), row("f", "silent", 16_800, 240),
            row("g", "silent", 0, 0), row("h", "silent", -5_000, 10),
        ]})
    }

    #[test]
    fn live_summary_reads_the_board() {
        let l = Live::from_board(&board());
        assert_eq!(l.captains, 7, "captains that have spent no action points are left out");
        assert_eq!(l.human, Some((200, 3)));
        assert_eq!(l.alone, Some((60, 3)));
        assert_eq!(l.top, Some(("gpt-6.1-sol on Hermes".into(), 300)));
        assert!(
            l.sentence()
                .contains("median 200 credits per action (3 captains), captains flying alone 60 (3)")
        );
        assert_eq!(Live::default().sentence(), "Season 1 is open: no captains have flown yet.");
    }

    #[test]
    fn head_escapes_and_includes_only_valid_settings() {
        let site = Site {
            origin: "https://mcpwars.com".into(),
            ga: Some("G-ABC123".into()),
            verification: None,
        };
        let mut l = Live::from_board(&board());
        l.top = Some(("<script>x</script>".into(), 1));
        let h = head(&site, &HOME, Some(&l));
        assert!(h.contains("<link rel=\"canonical\" href=\"https://mcpwars.com/\">"));
        assert!(h.contains("og:image\" content=\"https://mcpwars.com/og.png\""));
        assert!(h.contains("twitter:card\" content=\"summary_large_image\""));
        assert!(h.contains("gtag/js?id=G-ABC123"));
        assert!(h.contains("application/ld+json"));
        assert!(!h.contains("<script>x</script>"), "player text is escaped");
        assert!(!h.contains("google-site-verification"));
        let h = head(&site, &SCOREBOARD, None);
        assert!(!h.contains("ld+json"), "structured data only on the home page");
        assert!(h.contains("og-scoreboard.png"));
    }

    #[test]
    fn sitemap_lists_pages_then_guides() {
        let s = sitemap("https://mcpwars.com/", &["join".into(), "llms.txt".into()]);
        assert!(s.contains("<loc>https://mcpwars.com/</loc>"));
        assert!(s.contains("<loc>https://mcpwars.com/scoreboard</loc>"));
        assert!(s.contains("<loc>https://mcpwars.com/llms.txt</loc>"));
        assert!(s.starts_with("<?xml"));
    }
}
