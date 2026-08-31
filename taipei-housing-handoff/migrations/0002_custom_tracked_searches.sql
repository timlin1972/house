-- 讓使用者可以自己在網頁上新增追蹤清單（給名稱 + 591 搜尋網址），不再限定於一開始
-- 匯入的那 8 組固定組合。district/building_type/price_range 是原本 8 組（固定 regionid=1、
-- section=3/4、shape=2/5 的組合）才有意義的欄位，自訂搜尋不一定能套用同一套解析規則，
-- 顯示名稱改用這個新的 name 欄位為主，有值就優先顯示，沒有（原本匯入的 8 組）才 fallback
-- 回 district+building_type+price_range 組出來的字串。
ALTER TABLE tracked_searches ADD COLUMN name TEXT;
