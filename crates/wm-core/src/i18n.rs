//! 界面语言与双语消息。
//!
//! 所有面向用户的文本都以 [`Msg`]（中文 + 英文）保存，**序列化时按当前界面语言输出**。
//! 这样后台任务中产生的提示、错误与路由原因，在用户切换语言后也会以新语言显示，
//! 无需重新扫描。默认语言为英文。

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum Language {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "zh-CN")]
    ZhCn,
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn set_language(l: Language) {
    LANG.store(if l == Language::ZhCn { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn language() -> Language {
    if LANG.load(Ordering::Relaxed) == 1 {
        Language::ZhCn
    } else {
        Language::En
    }
}

pub fn is_zh() -> bool {
    language() == Language::ZhCn
}

/// 双语消息。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Msg {
    pub zh: Cow<'static, str>,
    pub en: Cow<'static, str>,
}

impl Msg {
    pub fn new(zh: impl Into<Cow<'static, str>>, en: impl Into<Cow<'static, str>>) -> Self {
        Self { zh: zh.into(), en: en.into() }
    }

    /// 当前语言下的文本。
    pub fn get(&self) -> &str {
        if is_zh() {
            &self.zh
        } else {
            &self.en
        }
    }

    /// 拼接两段消息（各语言分别拼接）。
    pub fn join(&self, sep_zh: &str, sep_en: &str, other: &Msg) -> Msg {
        Msg::new(format!("{}{}{}", self.zh, sep_zh, other.zh), format!("{}{}{}", self.en, sep_en, other.en))
    }

    pub fn contains(&self, s: &str) -> bool {
        self.zh.contains(s) || self.en.contains(s)
    }
}

impl fmt::Display for Msg {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.get())
    }
}

impl Serialize for Msg {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(self.get())
    }
}

impl<'de> Deserialize<'de> for Msg {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Ok(Msg::new(s.clone(), s))
    }
}

/// 构造双语消息：`msg!("中文", "English")`，也可传入 `format!` 结果。
#[macro_export]
macro_rules! msg {
    ($zh:expr, $en:expr $(,)?) => {
        $crate::i18n::Msg::new($zh, $en)
    };
}

/// 按当前语言选择：`tr!("中文", "English")`。
#[macro_export]
macro_rules! tr {
    ($zh:expr, $en:expr $(,)?) => {
        if $crate::i18n::is_zh() {
            $zh
        } else {
            $en
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn msg_serializes_in_current_language() {
        let m = Msg::new("你好", "Hello");
        set_language(Language::En);
        assert_eq!(serde_json::to_string(&m).unwrap(), "\"Hello\"");
        set_language(Language::ZhCn);
        assert_eq!(serde_json::to_string(&m).unwrap(), "\"你好\"");
        set_language(Language::En);
    }

    #[test]
    fn language_serde_names() {
        assert_eq!(serde_json::to_string(&Language::ZhCn).unwrap(), "\"zh-CN\"");
        assert_eq!(serde_json::from_str::<Language>("\"en\"").unwrap(), Language::En);
    }
}
