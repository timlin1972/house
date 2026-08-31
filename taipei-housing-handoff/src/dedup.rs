//! 「同一戶被不同仲介重複刊登」的判斷邏輯（PROJECT_SPEC.md 核心業務邏輯②）。
//!
//! 這裡只回答一個問題：兩筆刊登是不是同一戶？不負責決定誰是代表刊登、
//! 也不碰下架/代表轉移——那些是拿到「是不是同一戶」的答案之後，在更上層
//! （比對當次抓取結果 vs 資料庫既有紀錄時）才要做的決定。

/// 判斷「同一戶」用得到的欄位。刻意只放這幾個而不是整個 ScrapedCard/Listing，
/// 讓這個函式的輸入需求一眼就看得出來，也方便兩邊型別（新抓到的 vs 資料庫existing）
/// 各自轉成這個共同形狀來比較。
#[derive(Debug, Clone, Copy)]
pub struct HouseholdKey<'a> {
    pub community: Option<&'a str>,
    pub address: &'a str,
    pub floor: &'a str,
    pub area: f64,
    pub main_area: Option<f64>,
    pub age: Option<i64>,
}

const AREA_TOLERANCE: f64 = 0.1;
const MAIN_AREA_TOLERANCE: f64 = 0.01;

fn close(a: f64, b: f64, tolerance: f64) -> bool {
    (a - b).abs() <= tolerance
}

/// mainArea + age 完全一致：spec 說這是很強的同戶訊號，社區名或地址寫法不一致時特別有用。
/// 兩邊都要有值才算數——None 對 None 不能算「一致」，那什麼都證明不了。
fn main_area_and_age_match(a: &HouseholdKey, b: &HouseholdKey) -> bool {
    match (a.main_area, b.main_area, a.age, b.age) {
        (Some(ma_a), Some(ma_b), Some(age_a), Some(age_b)) => {
            close(ma_a, ma_b, MAIN_AREA_TOLERANCE) && age_a == age_b
        }
        _ => false,
    }
}

/// 地址或社區名兩者有一個相符即可（591 同一棟樓有時候被不同仲介登記成不同社區名，
/// 所以社區名不同不能直接排除；但兩邊都是 None 也不能算相符，等於沒比較到任何東西）。
fn address_or_community_match(a: &HouseholdKey, b: &HouseholdKey) -> bool {
    if a.address == b.address {
        return true;
    }
    matches!((a.community, b.community), (Some(x), Some(y)) if x == y)
}

/// 兩筆刊登是否為同一戶。價格刻意不是這個函式的輸入之一——spec 明確說「價格不同不代表
/// 不是同一戶」（同一戶降價後由不同仲介重新刊登非常常見），比價格完全是另一層的事。
pub fn same_household(a: &HouseholdKey, b: &HouseholdKey) -> bool {
    if !close(a.area, b.area, AREA_TOLERANCE) {
        return false;
    }

    if address_or_community_match(a, b) {
        if a.floor == b.floor {
            return true;
        }
        // 樓層標示格式不一致（如「1F~2F/8F」vs「1F/8F」）但其實是同一複層/樓中樓單位。
        if main_area_and_age_match(a, b) {
            return true;
        }
    }

    // 地址/社區名寫法完全對不上時，mainArea+age 完全一致仍然是夠強的同戶訊號。
    main_area_and_age_match(a, b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key<'a>(
        community: Option<&'a str>,
        address: &'a str,
        floor: &'a str,
        area: f64,
        main_area: Option<f64>,
        age: Option<i64>,
    ) -> HouseholdKey<'a> {
        HouseholdKey {
            community,
            address,
            floor,
            area,
            main_area,
            age,
        }
    }

    #[test]
    fn identical_listing_is_same_household() {
        let a = key(Some("建成花園廣場"), "松山區-民生東路五段", "15F/20F", 36.3, Some(26.09), Some(37));
        let b = a;
        assert!(same_household(&a, &b));
    }

    #[test]
    fn area_within_0_1_tolerance_still_matches() {
        let a = key(Some("建成花園廣場"), "松山區-民生東路五段", "15F/20F", 36.30, Some(26.09), Some(37));
        let b = key(Some("建成花園廣場"), "松山區-民生東路五段", "15F/20F", 36.39, Some(26.09), Some(37));
        assert!(same_household(&a, &b));
    }

    #[test]
    fn area_beyond_tolerance_does_not_match() {
        let a = key(Some("建成花園廣場"), "松山區-民生東路五段", "15F/20F", 36.3, Some(26.09), Some(37));
        let b = key(Some("建成花園廣場"), "松山區-民生東路五段", "15F/20F", 36.5, Some(26.09), Some(37));
        assert!(!same_household(&a, &b));
    }

    /// spec 案例：community 欄位文字不同、但 address/floor/area/mainArea 都吻合 → 視為同一戶。
    #[test]
    fn different_community_name_same_address_floor_area_matches() {
        let a = key(Some("僑福新村"), "松山區-健康路", "10F/13F", 38.14, None, Some(48));
        let b = key(Some("僑福新邨"), "松山區-健康路", "10F/13F", 38.14, None, Some(48));
        assert!(same_household(&a, &b));
    }

    /// spec 案例：樓層標示格式不一致（1F~2F/8F vs 1F/8F）但實際是同一複層單位，
    /// 用 mainArea + age 完全一致來確認。
    #[test]
    fn inconsistent_floor_format_confirmed_by_main_area_and_age() {
        let a = key(Some("南京新貴族"), "松山區-八德路四段245巷", "1F~2F/8F", 32.77, Some(25.76), Some(35));
        let b = key(Some("南京新貴族"), "松山區-八德路四段245巷", "1F/8F", 32.77, Some(25.76), Some(35));
        assert!(same_household(&a, &b));
    }

    /// 樓層不同、且 mainArea/age 對不上 → 真的是不同戶（同一棟樓不同樓層）。
    #[test]
    fn different_floor_without_main_area_age_confirmation_is_different_household() {
        let a = key(Some("南京新貴族"), "松山區-八德路四段245巷", "1F/8F", 32.77, Some(25.76), Some(35));
        let b = key(Some("南京新貴族"), "松山區-八德路四段245巷", "3F/8F", 32.77, Some(20.10), Some(35));
        assert!(!same_household(&a, &b));
    }

    /// spec 案例：mainArea + age 完全相同是很強的同戶訊號，社區名或地址寫法都不一致時特別有用。
    #[test]
    fn main_area_and_age_alone_overrides_mismatched_address_and_community() {
        let a = key(Some("建成花園廣場"), "松山區-民生東路五段", "15F/20F", 36.3, Some(26.09), Some(37));
        let b = key(Some("民生社區"), "松山區-民生東路5段", "15F/20F", 36.3, Some(26.09), Some(37));
        assert!(same_household(&a, &b));
    }

    /// mainArea 或 age 缺一個值就不能單獨當作同戶訊號（None 不能拿來確認任何事）。
    #[test]
    fn missing_main_area_or_age_cannot_confirm_alone() {
        let a = key(None, "松山區-某路", "5F/10F", 30.0, None, Some(40));
        let b = key(None, "松山區-某路", "6F/10F", 30.0, None, Some(40));
        assert!(!same_household(&a, &b));
    }

    /// 價格不是輸入之一：就算兩筆刊登價格差很多，只要其他條件吻合仍視為同一戶
    /// （降價後由不同仲介重新刊登很常見）——這裡直接體現在 HouseholdKey 沒有 price 欄位。
    #[test]
    fn household_key_has_no_price_field_by_design() {
        let a = key(Some("X"), "Y", "1F/1F", 30.0, None, None);
        let b = key(Some("X"), "Y", "1F/1F", 30.0, None, None);
        assert!(same_household(&a, &b));
    }
}
