//! 引擎层：拼音切分、候选召回、打分排序。
//!
//! 全部在同步路径上，预算 50ms。不得做 IO、不得调网络、不得调大模型。

pub mod lexicon;
pub mod pinyin;
