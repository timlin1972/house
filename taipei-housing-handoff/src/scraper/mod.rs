//! 591 抓取層。591 沒有可直接呼叫的公開 JSON API（見待辦②的研究結論），
//! 用 headless_chrome 直接讀 `.ware-item` 卡片的 DOM。
//!
//! 每次翻頁都用一段 JS 把當頁所有卡片一次性序列化成 JSON 抓回來，
//! 避免對每張卡片、每個欄位各發一次 CDP round-trip。

use std::time::Duration;

use anyhow::{Context, Result};
use headless_chrome::Tab;
use serde::Deserialize;

/// 從 DOM 抓出來、尚未做任何去重/過濾判斷的原始卡片。
#[derive(Debug, Clone)]
pub struct ScrapedCard {
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
}

#[derive(Debug, Deserialize)]
struct RawCardJs {
    id: Option<String>,
    title: Option<String>,
    attrs: Vec<String>,
    community: Option<String>,
    section: String,
    address: String,
    agent: Option<String>,
    price_value: String,
    price_section_text: String,
}

const EXTRACT_CARDS_JS: &str = r#"
(() => {
  // 591 會混入「熱銷建案/好康推薦」卡片，也用 .ware-item 這個 class，但內部結構完全不同
  // （沒有 .ware-item__attrs，class 命名是單破折號 ware-item-xxx 而非 BEM 的 ware-item__xxx）。
  // 用是否存在 .ware-item__attrs 判斷是不是真正的中古屋搜尋結果卡片，直接在這裡排除掉。
  const cards = Array.from(document.querySelectorAll('.ware-item'))
    .filter(card => card.querySelector('.ware-item__attrs'));
  return JSON.stringify(cards.map(card => {
    const attrs = Array.from(card.querySelectorAll('.ware-item__attr')).map(el => el.textContent.trim());
    const communityRaw = card.querySelector('.ware-item__community');
    const community = communityRaw ? communityRaw.textContent.trim() : null;
    const section = card.querySelector('.ware-item__section');
    const address = card.querySelector('.ware-item__address');
    const agentEl = card.querySelector('.user-info__name');
    const priceValueEl = card.querySelector('.ware-item__price-value');
    const priceSectionEl = card.querySelector('.ware-item__price-section');
    return {
      id: card.getAttribute('data-id'),
      title: card.getAttribute('title'),
      attrs,
      community,
      section: section ? section.textContent.trim() : '',
      address: address ? address.textContent.trim() : '',
      agent: agentEl ? agentEl.textContent.trim() : null,
      price_value: priceValueEl ? priceValueEl.textContent.trim() : '',
      price_section_text: priceSectionEl ? priceSectionEl.textContent.trim() : '',
    };
  }));
})()
"#;

const TOTAL_COUNT_JS: &str =
    r#"document.querySelector('.ware-count-box .number')?.textContent.trim() ?? ''"#;

/// 591 目前不會出現社區名稱時,用這個佔位字串代替(「依現場名稱」= 依現場招牌,沒有登記社區名)。
const COMMUNITY_PLACEHOLDER: &str = "依現場名稱";

fn parse_attrs(attrs: &[String], card_id: &str) -> Result<(String, f64, Option<f64>, Option<i64>, String)> {
    let mut rooms = None;
    let mut area = None;
    let mut main_area = None;
    let mut age = None;
    let mut floor = None;

    for attr in attrs {
        if let Some(rest) = attr.strip_prefix("權狀") {
            area = Some(
                rest.trim_end_matches('坪')
                    .parse::<f64>()
                    .with_context(|| format!("area 格式錯誤 {attr:?}（{card_id}）"))?,
            );
        } else if let Some(rest) = attr.strip_prefix("主建") {
            main_area = Some(
                rest.trim_end_matches('坪')
                    .parse::<f64>()
                    .with_context(|| format!("mainArea 格式錯誤 {attr:?}（{card_id}）"))?,
            );
        } else if attr.contains('房') {
            rooms = Some(attr.clone());
        } else if attr.contains('F') {
            floor = Some(attr.clone());
        } else if let Some(rest) = attr.strip_suffix('年') {
            if rest.chars().all(|c| c.is_ascii_digit()) && !rest.is_empty() {
                age = Some(rest.parse::<i64>().unwrap());
            }
        }
        // 其餘（例如第一個「華廈」/「電梯大樓」建物類型標籤）不需要，tracked_search 本身已經知道。
    }

    Ok((
        rooms.with_context(|| format!("attrs 找不到 rooms（{card_id}）: {attrs:?}"))?,
        area.with_context(|| format!("attrs 找不到 area（{card_id}）: {attrs:?}"))?,
        main_area,
        age,
        floor.with_context(|| format!("attrs 找不到 floor（{card_id}）: {attrs:?}"))?,
    ))
}

fn parse_price(price_value: &str, price_section_text: &str, card_id: &str) -> Result<(i64, f64)> {
    let price: i64 = price_value
        .replace(',', "")
        .parse()
        .with_context(|| format!("price 格式錯誤 {price_value:?}（{card_id}）"))?;

    // price_section_text 形如 "2,688萬 87.33萬/坪"，取「萬/坪」前面那個數字。
    let unit_price = price_section_text
        .split_whitespace()
        .find_map(|tok| tok.strip_suffix("萬/坪"))
        .with_context(|| format!("unitPrice 解析失敗 {price_section_text:?}（{card_id}）"))?
        .parse::<f64>()
        .with_context(|| format!("unitPrice 數字格式錯誤（{card_id}）"))?;

    Ok((price, unit_price))
}

fn parse_card(raw: RawCardJs) -> Result<ScrapedCard> {
    let id = raw.id.context("卡片缺少 data-id")?;
    let title = raw.title.with_context(|| format!("卡片缺少 title（id={id}）"))?;

    let (rooms, area, main_area, age, floor) = parse_attrs(&raw.attrs, &id)?;
    let (price, unit_price) = parse_price(&raw.price_value, &raw.price_section_text, &id)?;

    let community = raw
        .community
        .filter(|c| !c.is_empty() && c != COMMUNITY_PLACEHOLDER);
    let address = format!("{}{}", raw.section, raw.address);
    let agent = raw.agent.filter(|a| !a.is_empty());

    Ok(ScrapedCard {
        id,
        title,
        price,
        unit_price,
        rooms,
        area,
        main_area,
        age,
        floor,
        community,
        address,
        agent,
    })
}

/// 實測目前的列表卡片一次到位、不需要捲動就能拿到內容（一開始以為要小幅度捲動才會
/// hydrate，後來發現「內容缺失」的卡片其實是熱銷推薦卡片,永遠不會有 data-id，跟捲動無關）。
/// 591 的列表是即時排序，掃過頁面越久、翻頁之間的資料 drift 越嚴重（見 dedup_by_id 的說明），
/// 所以這裡只做一次快速捲到底，不做小步慢捲——真的又出現虛擬化列表問題時再加回來。
fn scroll_through_list(tab: &Tab) -> Result<()> {
    tab.evaluate("window.scrollTo(0, document.body.scrollHeight);", false)?;
    std::thread::sleep(Duration::from_millis(300));
    Ok(())
}

fn extract_raw_cards(tab: &Tab) -> Result<Vec<RawCardJs>> {
    let json = tab
        .evaluate(EXTRACT_CARDS_JS, false)?
        .value
        .context("抓取卡片的 JS 沒有回傳值")?;
    let json_str = json.as_str().context("抓取卡片的 JS 回傳值不是字串")?;
    Ok(serde_json::from_str(json_str)?)
}

/// 捲動一輪後抓取；EXTRACT_CARDS_JS 已經把「熱銷建案/好康推薦」那類假卡片濾掉了，
/// 所以正常情況下不會再有 id 缺失的卡片。如果真的還有（591 頁面結構變了、或真的遇到
/// 尚未 hydrate 完的情況），重試兩次，仍然失敗就記警告後跳過，不讓單一頁面卡住整組抓取。
fn extract_cards_on_current_page(tab: &Tab) -> Result<Vec<ScrapedCard>> {
    tab.evaluate("window.scrollTo(0, 0);", false)?;
    std::thread::sleep(Duration::from_millis(200));
    scroll_through_list(tab)?;
    let mut raw_cards = extract_raw_cards(tab)?;

    for attempt in 1..=2 {
        let empty = raw_cards.iter().filter(|c| c.id.is_none()).count();
        if empty == 0 {
            break;
        }
        tracing::warn!(attempt, empty, "有卡片缺 id（非熱銷推薦卡片），重試捲動+抓取");
        scroll_through_list(tab)?;
        raw_cards = extract_raw_cards(tab)?;
    }

    let empty = raw_cards.iter().filter(|c| c.id.is_none()).count();
    if empty > 0 {
        tracing::warn!(empty, "重試後仍有卡片缺 id，略過這些卡片");
    }

    raw_cards
        .into_iter()
        .filter(|c| c.id.is_some())
        .map(parse_card)
        .collect()
}

fn read_declared_total(tab: &Tab) -> Result<i64> {
    let value = tab
        .evaluate(TOTAL_COUNT_JS, false)?
        .value
        .context("讀取總筆數的 JS 沒有回傳值")?;
    let text = value.as_str().unwrap_or("");
    text.parse().with_context(|| format!("無法解析總筆數 {text:?}"))
}

/// 找目前分頁比「當前頁碼」大 1 的數字連結並點擊。找不到（已經是最後一頁）回傳 false。
fn click_next_page(tab: &Tab, current_page: u32) -> Result<bool> {
    let target = (current_page + 1).to_string();
    let links = tab.find_elements(".paginator-container a")?;
    for link in &links {
        if link.get_inner_text().map(|t| t.trim() == target).unwrap_or(false) {
            link.click()?;
            return Ok(true);
        }
    }
    Ok(false)
}

/// 591 偶爾會在結果裡混入「24開頭」的追蹤/重導連結：id每次抓都會變，但標題/價格/坪數
/// 跟頁面上其他既有物件完全相同——不是真實新刊登。用內容比對（title+price+area）抓出這種
/// 「一筆真的 + 一筆24開頭」的配對，捨棄24開頭那筆，只留正常id的那筆。
/// 這跟 dedup 模組要處理的「同一戶被不同仲介分別刊登」是兩個不同問題：這裡是同一張卡片
/// 被 591 自己重複序列化成兩個 id，那邊是真的有兩則刊登內容。
fn drop_tracking_redirects(cards: Vec<ScrapedCard>) -> Vec<ScrapedCard> {
    use std::collections::HashMap;

    let mut groups: HashMap<(String, i64, String), Vec<usize>> = HashMap::new();
    for (i, c) in cards.iter().enumerate() {
        let key = (c.title.clone(), c.price, format!("{:.2}", c.area));
        groups.entry(key).or_default().push(i);
    }

    let mut drop = vec![false; cards.len()];
    for idxs in groups.values() {
        if idxs.len() < 2 {
            continue;
        }
        let real: Vec<usize> = idxs.iter().copied().filter(|&i| !cards[i].id.starts_with("24")).collect();
        let fake: Vec<usize> = idxs.iter().copied().filter(|&i| cards[i].id.starts_with("24")).collect();
        if real.is_empty() || fake.is_empty() {
            continue;
        }
        for &i in &fake {
            tracing::info!(
                dropped_id = %cards[i].id,
                kept_id = %cards[real[0]].id,
                title = %cards[i].title,
                "捨棄24開頭追蹤連結"
            );
            drop[i] = true;
        }
    }

    cards
        .into_iter()
        .zip(drop)
        .filter_map(|(c, d)| (!d).then_some(c))
        .collect()
}

/// 抓一組追蹤清單的完整搜尋結果（所有分頁），已經濾掉24開頭追蹤連結。
/// 還沒跟資料庫既有資料比對「同一戶不同仲介重複刊登」——那是 dedup 模組的責任。
pub fn scrape_all_pages(tab: &Tab, search_url: &str) -> Result<(i64, Vec<ScrapedCard>)> {
    tab.navigate_to(search_url)?;
    tab.wait_until_navigated()?;
    tab.wait_for_element(".ware-item")
        .context("等不到 .ware-item 卡片，591 頁面結構可能變了")?;
    std::thread::sleep(Duration::from_millis(800));

    let declared_total = read_declared_total(tab)?;

    let mut all_cards = Vec::new();
    let mut page = 1u32;
    loop {
        let mut page_cards = extract_cards_on_current_page(tab)?;
        all_cards.append(&mut page_cards);

        if !click_next_page(tab, page)? {
            break;
        }
        page += 1;
        std::thread::sleep(Duration::from_millis(600));
        tab.wait_for_element(".ware-item")
            .with_context(|| format!("翻到第 {page} 頁後等不到 .ware-item"))?;
        std::thread::sleep(Duration::from_millis(400));

        if page > 50 {
            anyhow::bail!("分頁超過 50 頁，可能卡在同一頁重複點擊，中止避免無限迴圈");
        }
    }

    Ok((declared_total, drop_tracking_redirects(dedup_by_id(all_cards))))
}

/// 591 的列表是即時排序（例如「最新更新」），翻頁之間資料本身會變動：實測同一次抓取裡
/// 常有 id 同時出現在兩、三個不同分頁上（列表 drift）。一天只抓一次、資料變動不快，
/// 這裡用「保留第一次看到的那筆」處理即可，不需要為了消除 drift 去追求分頁一致性快照。
fn dedup_by_id(cards: Vec<ScrapedCard>) -> Vec<ScrapedCard> {
    use std::collections::HashSet;

    let mut seen = HashSet::new();
    let mut dropped = 0usize;
    let deduped: Vec<ScrapedCard> = cards
        .into_iter()
        .filter(|c| {
            let is_new = seen.insert(c.id.clone());
            if !is_new {
                dropped += 1;
            }
            is_new
        })
        .collect();

    if dropped > 0 {
        tracing::info!(dropped, "分頁間 id 重複（591列表即時排序造成的drift），已去重");
    }
    deduped
}
