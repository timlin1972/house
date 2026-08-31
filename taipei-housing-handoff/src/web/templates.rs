use askama::Template;

pub struct ListingCard {
    pub tracked_search_id: i64,
    pub id: String,
    pub title: String,
    pub price: i64,
    pub unit_price: f64,
    pub rooms: String,
    pub area: f64,
    pub main_area: Option<f64>,
    pub age: Option<i64>,
    pub floor: String,
    pub community: Option<String>,
    pub address: String,
    pub agent: Option<String>,
    pub delisted: bool,
    pub note: Option<String>,
    pub decision: Option<String>,
    pub reason: Option<String>,
}

impl ListingCard {
    pub fn detail_url(&self) -> String {
        format!("https://sale.591.com.tw/home/house/detail/2/{}.html", self.id)
    }

    pub fn decision_is(&self, want: &str) -> bool {
        self.decision.as_deref() == Some(want)
    }
}

pub struct Group {
    pub tracked_search_id: i64,
    pub label: String,
    pub stats: Stats,
    pub listings: Vec<ListingCard>,
}

#[derive(Default)]
pub struct Stats {
    pub tracked_count: i64,
    pub new_today: i64,
    pub reviewed_count: i64,
    pub want_count: i64,
    pub delisted_count: i64,
}

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexTemplate {
    pub groups: Vec<Group>,
}
