//! 每次抓取後的對帳邏輯（PROJECT_SPEC.md 核心業務邏輯③④：下架偵測 + 代表刊登轉移 + 重新上架），
//! 外加使用者提醒的重要一點：**591 的 id 本身有時候會換，即使該戶從沒真的從頁面消失過**。
//!
//! 所以「資料庫沒看過的新 id」永遠不能直接當成新戶——一律先用 dedup::same_household 跟這組
//! 追蹤清單裡「今天沒出現」的既有物件比對內容，比對到才判定是新戶。這跟 spec 原本寫的
//! 「先下架、之後用新id重新上架」共用同一套代表轉移機制，差別只在觸發時機：
//! 換id可以在同一天內發生、不需要真的中間消失過。
//!
//! 下架判斷用「連續兩次都沒看到」而不是 spec 字面上的單次比對：591 的列表是即時排序，
//! 分頁之間會 drift，單次抓取本來就可能漏掉該組 20-25% 的在架物件（見抓取層的驗證結果），
//! 單次沒看到就判定下架風險太高。

use std::collections::{HashMap, HashSet};

use anyhow::Result;
use chrono::NaiveDate;
use sqlx::sqlite::SqlitePool;

use crate::dedup::{same_household, HouseholdKey};
use crate::scraper::ScrapedCard;

/// 對帳用的既有物件快照——只放比對/決策需要的欄位，用擁有型別（不是引用）
/// 避免這個結構被到處借用，操作起來綁手綁腳。
#[derive(Debug, Clone)]
struct ExistingListing {
    id: String,
    area: f64,
    main_area: Option<f64>,
    age: Option<i64>,
    floor: String,
    community: Option<String>,
    address: String,
    duplicate_of: Option<String>,
    delisted: bool,
    last_seen_check_run_id: Option<i64>,
}

impl ExistingListing {
    fn household_key(&self) -> HouseholdKey<'_> {
        HouseholdKey {
            community: self.community.as_deref(),
            address: &self.address,
            floor: &self.floor,
            area: self.area,
            main_area: self.main_area,
            age: self.age,
        }
    }
}

fn card_household_key(c: &ScrapedCard) -> HouseholdKey<'_> {
    HouseholdKey {
        community: c.community.as_deref(),
        address: &c.address,
        floor: &c.floor,
        area: c.area,
        main_area: c.main_area,
        age: c.age,
    }
}

#[derive(Debug, Default)]
pub struct ReconcileSummary {
    pub check_run_id: i64,
    /// 資料庫沒看過的 id（不論最後判定是新戶、多一筆重複刊登、還是同一戶換id）。
    pub new_ids: Vec<String>,
    /// 這次確定下架（連續兩次都沒看到）的代表物件 id。
    pub delisted_ids: Vec<String>,
    /// (舊id, 新id)：判定為「同一戶只是換了id」而做的代表/重複刊登轉移。
    pub id_changes: Vec<(String, String)>,
    /// 目前這組清單有效（非下架、代表）物件總數，寫進 check_runs.total_count。
    pub total_active_representatives: i64,
}

pub async fn reconcile_tracked_search(
    pool: &SqlitePool,
    tracked_search_id: i64,
    checked_date: NaiveDate,
    scraped: Vec<ScrapedCard>,
) -> Result<ReconcileSummary> {
    let mut tx = pool.begin().await?;
    sqlx::query("PRAGMA defer_foreign_keys = ON;")
        .execute(&mut *tx)
        .await?;

    let rows: Vec<(
        String,
        f64,
        Option<f64>,
        Option<i64>,
        String,
        Option<String>,
        String,
        Option<String>,
        bool,
        Option<i64>,
    )> = sqlx::query_as(
        "SELECT id, area, main_area, age, floor, community, address,
                duplicate_of, delisted, last_seen_check_run_id
         FROM listings WHERE tracked_search_id = ?",
    )
    .bind(tracked_search_id)
    .fetch_all(&mut *tx)
    .await?;

    let mut by_id: HashMap<String, ExistingListing> = rows
        .into_iter()
        .map(|r| {
            let existing = ExistingListing {
                id: r.0.clone(),
                area: r.1,
                main_area: r.2,
                age: r.3,
                floor: r.4,
                community: r.5,
                address: r.6,
                duplicate_of: r.7,
                delisted: r.8,
                last_seen_check_run_id: r.9,
            };
            (r.0, existing)
        })
        .collect();

    let previous_run_id: Option<i64> =
        sqlx::query_scalar("SELECT MAX(id) FROM check_runs WHERE tracked_search_id = ?")
            .bind(tracked_search_id)
            .fetch_one(&mut *tx)
            .await?;

    let scraped_ids: HashSet<String> = scraped.iter().map(|c| c.id.clone()).collect();

    let mut new_ids = Vec::new();
    let mut id_changes = Vec::new();

    // PASS 1：代表本身今天沒出現，但該戶「已知」的其他重複刊登還在架上 → 代表轉移。
    // 這一步完全不需要任何新卡片，純粹是既有紀錄之間的重新指派，spec 原文的
    // 「代表刊登轉移」講的就是這個情況（選現存 id 裡數字最小的一筆當新代表）。
    // 一定要在處理新卡片之前做，這樣後面同一戶如果剛好也冒出一個全新 id，
    // 才會正確比對到「已經轉移過的代表」而不是舊代表。
    let representative_ids: Vec<String> = by_id
        .values()
        .filter(|l| l.duplicate_of.is_none())
        .map(|l| l.id.clone())
        .collect();

    for rep_id in representative_ids {
        if scraped_ids.contains(&rep_id) {
            continue;
        }
        let mut present_members: Vec<String> = by_id
            .values()
            .filter(|l| l.duplicate_of.as_deref() == Some(rep_id.as_str()) && scraped_ids.contains(&l.id))
            .map(|l| l.id.clone())
            .collect();
        present_members.sort_by_key(|id| id.parse::<u64>().unwrap_or(u64::MAX));
        let Some(new_rep_id) = present_members.into_iter().next() else {
            continue;
        };

        transfer_representative_among_known(&mut tx, tracked_search_id, &rep_id, &new_rep_id).await?;

        if let Some(old_mut) = by_id.get_mut(&rep_id) {
            old_mut.duplicate_of = Some(new_rep_id.clone());
        }
        for l in by_id.values_mut() {
            if l.duplicate_of.as_deref() == Some(rep_id.as_str()) {
                l.duplicate_of = Some(new_rep_id.clone());
            }
        }
        if let Some(new_mut) = by_id.get_mut(&new_rep_id) {
            new_mut.duplicate_of = None;
        }
        id_changes.push((rep_id, new_rep_id));
    }

    // PASS 2：處理這次抓取結果，逐筆決定「已知/新戶/多一筆重複/換id」。
    for card in &scraped {
        if by_id.contains_key(&card.id) {
            update_known_listing(&mut tx, tracked_search_id, card).await?;
            by_id.get_mut(&card.id).unwrap().delisted = false;
            continue;
        }

        new_ids.push(card.id.clone());

        // 比對對象是「所有已知物件」，不分今天還在不在架上——這裡要同時涵蓋兩種情況：
        // (a) 該戶原本的 id 今天還在架上，這筆新 id 只是同一戶多一個仲介刊登（今天的其他
        //     新卡片也算在內，因為處理完的新卡片會立刻插進 by_id，同一批裡好幾筆同戶的
        //     卡片才能互相配對成同一戶，而不是各自變成獨立的代表）。
        // (b) 該戶原本的 id 今天沒出現，這是換id/重新上架，要走代表轉移。
        // 優先找「今天還在架上」的既有物件——PASS 1 已經把能轉移的代表都轉移給現存已知
        // id 了，這裡如果隨便比對到一個「今天沒出現」的舊 id（其實已經在 PASS 1 轉移掉、
        // 只是還留著一筆歷史紀錄指向新代表），會多做一次沒必要、甚至指向錯誤的轉移。
        // 只有在真的沒有任何「還在架上」的match時，才考慮「今天沒出現」的match（代表換id）。
        let card_key = card_household_key(card);
        let matched = by_id
            .values()
            .filter(|l| scraped_ids.contains(&l.id))
            .find(|l| same_household(&card_key, &l.household_key()))
            .map(|l| (l.id.clone(), l.duplicate_of.clone(), true))
            .or_else(|| {
                by_id
                    .values()
                    .filter(|l| !scraped_ids.contains(&l.id))
                    .find(|l| same_household(&card_key, &l.household_key()))
                    .map(|l| (l.id.clone(), l.duplicate_of.clone(), false))
            });

        match matched {
            Some((matched_id, matched_duplicate_of, matched_present_today)) if matched_present_today => {
                // 代表（或這戶其他已知刊登）今天還在架上：單純多一筆重複刊登，不算換id。
                let rep_id = matched_duplicate_of.unwrap_or(matched_id);
                insert_new_duplicate(&mut tx, tracked_search_id, card, &rep_id, checked_date).await?;
                by_id.insert(
                    card.id.clone(),
                    ExistingListing {
                        id: card.id.clone(),
                        area: card.area,
                        main_area: card.main_area,
                        age: card.age,
                        floor: card.floor.clone(),
                        community: card.community.clone(),
                        address: card.address.clone(),
                        duplicate_of: Some(rep_id),
                        delisted: false,
                        last_seen_check_run_id: None,
                    },
                );
            }
            Some((old_id, _, _)) => {
                let old = by_id.get(&old_id).unwrap().clone();
                transfer_to_new_id(&mut tx, tracked_search_id, &old, card, checked_date).await?;

                // 記憶體裡的快照也要同步更新，後面判斷「今天還在架的舊有物件」才會正確：
                // 舊 id 現在改指向新代表；如果舊的本身就是代表，所有原本掛在它底下的重複刊登
                // 也要一起轉過去（duplicate_of 永遠只有一層）。
                let new_rep = if old.duplicate_of.is_none() {
                    card.id.clone()
                } else {
                    old.duplicate_of.clone().unwrap()
                };
                if let Some(old_mut) = by_id.get_mut(&old_id) {
                    old_mut.duplicate_of = Some(new_rep.clone());
                }
                if old.duplicate_of.is_none() {
                    for l in by_id.values_mut() {
                        if l.duplicate_of.as_deref() == Some(old_id.as_str()) {
                            l.duplicate_of = Some(new_rep.clone());
                        }
                    }
                }
                by_id.insert(
                    card.id.clone(),
                    ExistingListing {
                        id: card.id.clone(),
                        area: card.area,
                        main_area: card.main_area,
                        age: card.age,
                        floor: card.floor.clone(),
                        community: card.community.clone(),
                        address: card.address.clone(),
                        duplicate_of: if new_rep == card.id { None } else { Some(new_rep) },
                        delisted: false,
                        last_seen_check_run_id: None, // 下面統一回填成這次的 check_run id
                    },
                );
                id_changes.push((old_id, card.id.clone()));
            }
            None => {
                insert_new_household(&mut tx, tracked_search_id, card, checked_date).await?;
                by_id.insert(
                    card.id.clone(),
                    ExistingListing {
                        id: card.id.clone(),
                        area: card.area,
                        main_area: card.main_area,
                        age: card.age,
                        floor: card.floor.clone(),
                        community: card.community.clone(),
                        address: card.address.clone(),
                        duplicate_of: None,
                        delisted: false,
                        last_seen_check_run_id: None,
                    },
                );
            }
        }
    }

    // 下架判斷：只看「代表物件」，且該戶所有已知重複刊登也都要缺席才算——
    // 用 by_id 裡目前的 duplicate_of 快照抓出每個代表底下所有成員的 id 集合。
    let mut members_of: HashMap<String, Vec<String>> = HashMap::new();
    for l in by_id.values() {
        let rep = l.duplicate_of.clone().unwrap_or_else(|| l.id.clone());
        members_of.entry(rep).or_default().push(l.id.clone());
    }

    let mut delisted_ids = Vec::new();
    let mut relisted_ids = Vec::new();

    for l in by_id.values() {
        if l.duplicate_of.is_some() {
            continue; // 只對代表物件做下架判斷
        }
        let household_seen_today = members_of
            .get(&l.id)
            .map(|members| members.iter().any(|m| scraped_ids.contains(m)))
            .unwrap_or(false);

        if household_seen_today {
            if l.delisted {
                relisted_ids.push(l.id.clone());
            }
            continue;
        }

        if l.delisted {
            continue; // 已經是下架狀態，維持原樣
        }

        // 上一次 check_run 就已經沒看到了（last_seen_check_run_id 落後上一輪）→ 連續兩次沒看到，確定下架。
        // 上一次還看得到（last_seen_check_run_id == previous_run_id）→ 這是第一次沒看到，先不下架。
        let missed_last_time = match (l.last_seen_check_run_id, previous_run_id) {
            (_, None) => false, // 這是這組清單第一次跑，沒有「上一次」可比，不能判下架
            (Some(last_seen), Some(prev)) => last_seen < prev,
            (None, Some(_)) => true, // 從沒記錄過看到過，等同一直缺席
        };

        if missed_last_time {
            delisted_ids.push(l.id.clone());
        }
    }

    for id in &delisted_ids {
        sqlx::query(
            "UPDATE listings SET delisted = 1, delisted_date = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE tracked_search_id = ? AND id = ?",
        )
        .bind(checked_date.to_string())
        .bind(tracked_search_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    for id in &relisted_ids {
        sqlx::query(
            "UPDATE listings SET delisted = 0, delisted_date = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE tracked_search_id = ? AND id = ?",
        )
        .bind(tracked_search_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }

    // 上面已經把 delisted_ids/relisted_ids 的 UPDATE 寫進這個 transaction 了，
    // 直接數目前「代表 + 非下架」的筆數就是最新狀態，不需要額外加減。
    let total_active_representatives: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM listings WHERE tracked_search_id = ? AND duplicate_of IS NULL AND delisted = 0",
    )
    .bind(tracked_search_id)
    .fetch_one(&mut *tx)
    .await?;

    let note = format!(
        "新增 {} 筆、下架 {} 筆、換id轉移 {} 筆",
        new_ids.len(),
        delisted_ids.len(),
        id_changes.len()
    );

    let check_run_id: i64 = sqlx::query_scalar(
        "INSERT INTO check_runs (tracked_search_id, checked_date, new_count, total_count, note)
         VALUES (?, ?, ?, ?, ?)
         RETURNING id",
    )
    .bind(tracked_search_id)
    .bind(checked_date.to_string())
    .bind(new_ids.len() as i64)
    .bind(total_active_representatives)
    .bind(&note)
    .fetch_one(&mut *tx)
    .await?;

    // 這次抓取結果裡出現過的每個 id（不管是既有還是新/換id）都回填 last_seen_check_run_id。
    for id in &scraped_ids {
        sqlx::query(
            "UPDATE listings SET last_seen_check_run_id = ? WHERE tracked_search_id = ? AND id = ?",
        )
        .bind(check_run_id)
        .bind(tracked_search_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }

    for id in &new_ids {
        sqlx::query(
            "INSERT INTO check_run_events (check_run_id, tracked_search_id, listing_id, event_type)
             VALUES (?, ?, ?, 'new')",
        )
        .bind(check_run_id)
        .bind(tracked_search_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    for id in &delisted_ids {
        sqlx::query(
            "INSERT INTO check_run_events (check_run_id, tracked_search_id, listing_id, event_type)
             VALUES (?, ?, ?, 'delisted')",
        )
        .bind(check_run_id)
        .bind(tracked_search_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }

    sqlx::query("UPDATE tracked_searches SET last_checked = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now') WHERE id = ?")
        .bind(checked_date.to_string())
        .bind(tracked_search_id)
        .execute(&mut *tx)
        .await?;

    tx.commit().await?;

    Ok(ReconcileSummary {
        check_run_id,
        new_ids,
        delisted_ids,
        id_changes,
        total_active_representatives,
    })
}

async fn update_known_listing(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tracked_search_id: i64,
    card: &ScrapedCard,
) -> Result<()> {
    sqlx::query(
        "UPDATE listings SET
            title = ?, price = ?, unit_price = ?, rooms = ?, area = ?, main_area = ?, age = ?,
            floor = ?, community = ?, address = ?, agent = ?, delisted = 0, delisted_date = NULL,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(&card.title)
    .bind(card.price)
    .bind(card.unit_price)
    .bind(&card.rooms)
    .bind(card.area)
    .bind(card.main_area)
    .bind(card.age)
    .bind(&card.floor)
    .bind(&card.community)
    .bind(&card.address)
    .bind(&card.agent)
    .bind(tracked_search_id)
    .bind(&card.id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_new_household(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tracked_search_id: i64,
    card: &ScrapedCard,
    checked_date: NaiveDate,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO listings
            (tracked_search_id, id, title, price, unit_price, rooms, area, main_area, age,
             floor, community, address, agent, first_seen, duplicate_of, delisted)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL, 0)",
    )
    .bind(tracked_search_id)
    .bind(&card.id)
    .bind(&card.title)
    .bind(card.price)
    .bind(card.unit_price)
    .bind(&card.rooms)
    .bind(card.area)
    .bind(card.main_area)
    .bind(card.age)
    .bind(&card.floor)
    .bind(&card.community)
    .bind(&card.address)
    .bind(&card.agent)
    .bind(checked_date.to_string())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 該戶的代表（或其他既有刊登）今天還在架上，這筆新 id 只是多一個仲介的重複刊登。
async fn insert_new_duplicate(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tracked_search_id: i64,
    card: &ScrapedCard,
    representative_id: &str,
    checked_date: NaiveDate,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO listings
            (tracked_search_id, id, title, price, unit_price, rooms, area, main_area, age,
             floor, community, address, agent, first_seen, duplicate_of, delisted)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0)",
    )
    .bind(tracked_search_id)
    .bind(&card.id)
    .bind(&card.title)
    .bind(card.price)
    .bind(card.unit_price)
    .bind(&card.rooms)
    .bind(card.area)
    .bind(card.main_area)
    .bind(card.age)
    .bind(&card.floor)
    .bind(&card.community)
    .bind(&card.address)
    .bind(&card.agent)
    .bind(checked_date.to_string())
    .bind(representative_id)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// 同一戶換了 591 id：新 id 繼承舊代表/重複刊登的角色；如果舊的本身是代表，舊那筆改指向
/// 新代表，所有原本掛在舊代表底下的重複刊登也要一起轉過去；評價也要從舊id搬到新id
/// （不能直接改 PK，SQLite 要用「插入新的、刪掉舊的」）。
async fn transfer_to_new_id(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tracked_search_id: i64,
    old: &ExistingListing,
    card: &ScrapedCard,
    checked_date: NaiveDate,
) -> Result<()> {
    let old_was_representative = old.duplicate_of.is_none();
    let new_representative_id = if old_was_representative {
        card.id.clone()
    } else {
        old.duplicate_of.clone().unwrap()
    };
    let new_duplicate_of = if new_representative_id == card.id {
        None
    } else {
        Some(new_representative_id.clone())
    };

    sqlx::query(
        "INSERT INTO listings
            (tracked_search_id, id, title, price, unit_price, rooms, area, main_area, age,
             floor, community, address, agent, first_seen, duplicate_of, delisted, note)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, ?)",
    )
    .bind(tracked_search_id)
    .bind(&card.id)
    .bind(&card.title)
    .bind(card.price)
    .bind(card.unit_price)
    .bind(&card.rooms)
    .bind(card.area)
    .bind(card.main_area)
    .bind(card.age)
    .bind(&card.floor)
    .bind(&card.community)
    .bind(&card.address)
    .bind(&card.agent)
    .bind(checked_date.to_string())
    .bind(&new_duplicate_of)
    .bind(format!("由 {} 轉移而來（591 id 換了但內容判定為同一戶）", old.id))
    .execute(&mut **tx)
    .await?;

    if old_was_representative {
        sqlx::query(
            "UPDATE listings SET duplicate_of = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
             WHERE tracked_search_id = ? AND duplicate_of = ?",
        )
        .bind(&card.id)
        .bind(tracked_search_id)
        .bind(&old.id)
        .execute(&mut **tx)
        .await?;
    }
    // 舊 id 不管原本是不是代表，轉移後都不再是代表了——一律指向轉移後「目前的代表」，
    // 跟新那筆的 duplicate_of（new_duplicate_of）不是同一件事：old 是代表的情況下，
    // 新那筆的 duplicate_of 是 None（它自己就是代表），但舊那筆要指向新代表的 id。
    sqlx::query(
        "UPDATE listings SET duplicate_of = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(&new_representative_id)
    .bind(tracked_search_id)
    .bind(&old.id)
    .execute(&mut **tx)
    .await?;

    move_review(&mut *tx, tracked_search_id, &old.id, &card.id).await?;

    Ok(())
}

/// 代表本身今天沒出現，但該戶「已知」的其他重複刊登還在架上（不需要任何新id）：
/// 把代表資格轉移給現存的那筆——spec 原文：「通常選現存id中數字最小的一筆」。
async fn transfer_representative_among_known(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tracked_search_id: i64,
    old_rep_id: &str,
    new_rep_id: &str,
) -> Result<()> {
    // 其他原本掛在舊代表底下的重複刊登，一起轉到新代表底下（duplicate_of 永遠只有一層）。
    sqlx::query(
        "UPDATE listings SET duplicate_of = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE tracked_search_id = ? AND duplicate_of = ?",
    )
    .bind(new_rep_id)
    .bind(tracked_search_id)
    .bind(old_rep_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE listings SET duplicate_of = ?, note = ?, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(new_rep_id)
    .bind(format!(
        "代表轉移給 {new_rep_id}（原代表本身已從頁面消失，但同戶其他刊登仍在架）"
    ))
    .bind(tracked_search_id)
    .bind(old_rep_id)
    .execute(&mut **tx)
    .await?;

    sqlx::query(
        "UPDATE listings SET duplicate_of = NULL, updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')
         WHERE tracked_search_id = ? AND id = ?",
    )
    .bind(tracked_search_id)
    .bind(new_rep_id)
    .execute(&mut **tx)
    .await?;

    move_review(&mut *tx, tracked_search_id, old_rep_id, new_rep_id).await?;

    Ok(())
}

/// 評價跟著代表走：只有舊 id 本身就有評價時才需要搬（不能直接改 PK，用「插入新的、刪掉舊的」）。
async fn move_review(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    tracked_search_id: i64,
    old_id: &str,
    new_id: &str,
) -> Result<()> {
    let existing_review: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT decision, reason FROM reviews WHERE tracked_search_id = ? AND listing_id = ?",
    )
    .bind(tracked_search_id)
    .bind(old_id)
    .fetch_optional(&mut **tx)
    .await?;

    let Some((decision, reason)) = existing_review else {
        return Ok(());
    };

    sqlx::query(
        "INSERT INTO reviews (tracked_search_id, listing_id, decision, reason)
         VALUES (?, ?, ?, ?)
         ON CONFLICT(tracked_search_id, listing_id) DO UPDATE SET
            decision = excluded.decision, reason = excluded.reason,
            updated_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
    )
    .bind(tracked_search_id)
    .bind(new_id)
    .bind(&decision)
    .bind(&reason)
    .execute(&mut **tx)
    .await?;

    sqlx::query("DELETE FROM reviews WHERE tracked_search_id = ? AND listing_id = ?")
        .bind(tracked_search_id)
        .bind(old_id)
        .execute(&mut **tx)
        .await?;

    Ok(())
}
