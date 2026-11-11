//! 用户可调配置（P3）：候选窗外观、每页候选数、撤回窗口、学习开关。
//!
//! ## 为什么放注册表、为什么不缓存
//!
//! 理由和 `state.rs` 完全一样（ADR-0007）：输入法 DLL 在每个宿主进程里各有一份，
//! 进程内缓存一旦和真实值不同步，用户看到的就是"改了没反应，重启才好"。
//! `RegQueryValueExW` 是微秒级调用，一次按键读五个值完全吃得消；
//! `RegNotifyChangeKeyValue` 要一条常驻线程，而 TSF DLL 是薄壳、不起线程。
//!
//! ## 为什么不是 `ui.yaml`
//!
//! 这些值**在按键路径上**要读。解析 YAML 得引依赖、处理编码与 BOM、还要管解析失败
//! 时退回默认值；注册表读取是原子的，也没有"文件被另一个进程写到一半"的问题。
//! 等 P2 设置台做出来，它只是这个注册表键的编辑器，读写仍然走这里。

use windows_sys::Win32::System::Registry::{
    KEY_READ, KEY_WRITE, REG_DWORD, RegCloseKey, RegSetValueExW,
};

use crate::state;

/// 配置键。P2 的设置台以后写这里，DLL 只读。
pub const KEY_PATH: &str = r"Software\inklink\Config";

/// 候选窗暗色主题：1 = 暗色，0/缺失 = 浅色。
pub const VALUE_DARK: &str = "Dark";
/// 候选窗列数（3–9）。
pub const VALUE_COLS: &str = "Cols";
/// 展开时行数（1–9）；收起时固定 1 行。
pub const VALUE_ROWS: &str = "Rows";
/// 误选撤回窗口（毫秒，0–10000）；0 表示关掉撤回。
pub const VALUE_UNDO_MS: &str = "UndoMs";
/// 是否记词（1/缺失 = 记，0 = 只查询不学习）。
pub const VALUE_LEARN: &str = "Learn";
/// 联想条开关（`Hint`，DWORD）：缺失=开，写 0=关。
pub const VALUE_HINT: &str = "Hint";

/// 列数合法区间。小于 3 一行放不下「序号 + 词」，大于 9 一屏糊成一片。
pub const COLS_RANGE: (usize, usize) = (3, 9);
/// 行数合法区间。
pub const ROWS_RANGE: (usize, usize) = (1, 9);
/// 撤回窗口上限：再长就把正常编辑也算成"选错了"。
pub const UNDO_MS_MAX: u64 = 10_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Config {
    pub dark: bool,
    pub cols: usize,
    pub rows: usize,
    pub undo_ms: u64,
    pub learn: bool,
    /// 联想条（上屏词后给后接词）开关
    pub hint: bool,
}

impl Config {
    /// 出厂默认：浅色、5 列 5 行（收起 5 个 / 展开 25 个）、撤回 2 秒、记词开。
    /// 这些和 1.1.20 之前的硬编码值一致 —— 升级不该改变任何人的使用感受。
    pub const DEFAULT: Config = Config {
        dark: false,
        cols: 5,
        rows: 5,
        undo_ms: 2000,
        learn: true,
        hint: true,
    };

    /// 收起时一页几个：一行。
    pub fn collapsed(&self) -> usize {
        self.cols
    }

    /// 展开时一页几个：列 × 行。
    pub fn expanded(&self) -> usize {
        self.cols * self.rows
    }

    /// 误选撤回窗口；`None` = 用户关掉了撤回。
    pub fn undo_window(&self) -> Option<std::time::Duration> {
        if self.undo_ms == 0 {
            None
        } else {
            Some(std::time::Duration::from_millis(self.undo_ms))
        }
    }
}

fn clamp_u32(raw: Option<u32>, default: u64, lo: u64, hi: u64) -> u64 {
    raw.map(u64::from).unwrap_or(default).clamp(lo, hi)
}

/// 把注册表里的原始值折成配置（纯函数，便于单测）。
///
/// 两个开关的缺失语义**刻意不对称**：
/// - `Dark` 缺失 = 浅色，跟随主流输入法的默认观感；
/// - `Learn` 缺失 = 开，学习是这个产品的主功能，不能因为没写过值就悄悄关掉。
pub fn from_raw(
    dark: Option<u32>,
    cols: Option<u32>,
    rows: Option<u32>,
    undo_ms: Option<u32>,
    learn: Option<u32>,
    hint: Option<u32>,
) -> Config {
    Config {
        dark: matches!(dark, Some(v) if v != 0),
        cols: clamp_u32(
            cols,
            Config::DEFAULT.cols as u64,
            COLS_RANGE.0 as u64,
            COLS_RANGE.1 as u64,
        ) as usize,
        rows: clamp_u32(
            rows,
            Config::DEFAULT.rows as u64,
            ROWS_RANGE.0 as u64,
            ROWS_RANGE.1 as u64,
        ) as usize,
        undo_ms: clamp_u32(undo_ms, Config::DEFAULT.undo_ms, 0, UNDO_MS_MAX),
        learn: !matches!(learn, Some(0)),
        hint: !matches!(hint, Some(0)),
    }
}

/// 读当前配置。键不存在或读失败一律用出厂默认（不会因为读不到就崩）。
pub fn snapshot() -> Config {
    unsafe {
        let Some(key) = state::open_key_path(KEY_PATH, KEY_READ) else {
            return Config::DEFAULT;
        };
        let cfg = from_raw(
            state::query_dword(key, VALUE_DARK),
            state::query_dword(key, VALUE_COLS),
            state::query_dword(key, VALUE_ROWS),
            state::query_dword(key, VALUE_UNDO_MS),
            state::query_dword(key, VALUE_LEARN),
            state::query_dword(key, VALUE_HINT),
        );
        RegCloseKey(key);
        cfg
    }
}

/// 写 `Dark`。浮条上那个太阳/月亮格子用它。
///
/// 失败不报错：写不进去时下一次 `snapshot()` 仍读到旧值，
/// 表现出来就是"点了没反应"，和 `state::set_chinese` 那边的降级一致。
pub fn set_dark(on: bool) {
    unsafe {
        let Some(key) = state::open_key_path(KEY_PATH, KEY_READ | KEY_WRITE) else {
            return;
        };
        let value = if on { 1u32 } else { 0u32 };
        let name: Vec<u16> = VALUE_DARK
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        RegSetValueExW(
            key,
            name.as_ptr(),
            0,
            REG_DWORD,
            &value as *const u32 as *const u8,
            4,
        );
        RegCloseKey(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 什么都没写就是出厂默认() {
        let cfg = from_raw(None, None, None, None, None, None);
        assert_eq!(cfg, Config::DEFAULT);
        assert!(!cfg.dark, "默认浅色");
        assert!(cfg.learn, "默认记词");
        assert_eq!(cfg.collapsed(), 5);
        assert_eq!(cfg.expanded(), 25);
        assert_eq!(cfg.undo_ms, 2000);
    }

    #[test]
    fn 每页候选数等于列乘行() {
        let cfg = from_raw(Some(1), Some(4), Some(3), None, None, None);
        assert_eq!(cfg.collapsed(), 4, "收起时就是一行");
        assert_eq!(cfg.expanded(), 12);
    }

    #[test]
    fn 脏值被夹到合法区间() {
        // 太小
        let cfg = from_raw(None, Some(1), Some(0), Some(0), None, None);
        assert_eq!(cfg.cols, COLS_RANGE.0);
        assert_eq!(cfg.rows, ROWS_RANGE.0);
        assert_eq!(cfg.undo_ms, 0, "0 是合法值：表示关掉撤回");
        // 太大
        let cfg = from_raw(None, Some(99), Some(99), Some(999_999), None, None);
        assert_eq!(cfg.cols, COLS_RANGE.1);
        assert_eq!(cfg.rows, ROWS_RANGE.1);
        assert_eq!(cfg.undo_ms, UNDO_MS_MAX);
    }

    #[test]
    fn 暗色只看非零() {
        assert!(!from_raw(Some(0), None, None, None, None, None).dark);
        assert!(from_raw(Some(1), None, None, None, None, None).dark);
        assert!(
            from_raw(Some(7), None, None, None, None, None).dark,
            "脏值按开处理"
        );
    }

    #[test]
    fn 学习开关只有明确写零才关() {
        assert!(from_raw(None, None, None, None, None, None).learn);
        assert!(from_raw(None, None, None, None, Some(3), None).learn);
        assert!(!from_raw(None, None, None, None, Some(0), None).learn);
    }

    #[test]
    fn 撤回窗口零表示关掉() {
        assert_eq!(
            from_raw(None, None, None, Some(0), None, None).undo_window(),
            None
        );
        assert_eq!(
            from_raw(None, None, None, Some(1500), None, None).undo_window(),
            Some(std::time::Duration::from_millis(1500))
        );
    }

    #[test]
    fn 读不到注册表也不崩() {
        // 机器上可能已经有配置，这里只断言"能拿到一份合法配置"
        let cfg = snapshot();
        assert!((COLS_RANGE.0..=COLS_RANGE.1).contains(&cfg.cols));
        assert!((ROWS_RANGE.0..=ROWS_RANGE.1).contains(&cfg.rows));
        assert!(cfg.undo_ms <= UNDO_MS_MAX);
    }
}
