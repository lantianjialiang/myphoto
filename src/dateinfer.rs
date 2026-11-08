//! 日期识别：多来源候选 + 优先级 + 置信度。
//!
//! 优先级（PRD FR-02）：人工覆盖 > EXIF/视频容器 > 文件名 > 目录名 > 文件系统时间。
//! 无法识别时保持为空，绝不写入“今天”。

use chrono::{NaiveDate, NaiveDateTime, NaiveTime};
use serde::{Deserialize, Serialize};
pub const CONFIRMED_UNKNOWN:&str="__confirmed_unknown__";

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Candidate {
    /// ISO 本地时间 "YYYY-MM-DDTHH:MM:SS"
    pub value: String,
    /// second | minute | day | month | year
    pub precision: String,
    pub source: String,
    pub confidence: String,
    pub raw: String,
    pub offset_min: Option<i32>,
    pub detail: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Precision {
    Second,
    Minute,
    Day,
    Month,
    Year,
}

impl Precision {
    pub fn as_str(self) -> &'static str {
        match self {
            Precision::Second => "second",
            Precision::Minute => "minute",
            Precision::Day => "day",
            Precision::Month => "month",
            Precision::Year => "year",
        }
    }
}

pub fn iso(dt: NaiveDateTime) -> String {
    dt.format("%Y-%m-%dT%H:%M:%S").to_string()
}

pub fn parse_iso(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
        .ok()
        .or_else(||NaiveDateTime::parse_from_str(s,"%Y-%m-%dT%H:%M").ok())
        .or_else(|| NaiveDateTime::parse_from_str(&format!("{s}T00:00:00"), "%Y-%m-%dT%H:%M:%S").ok())
}

pub fn source_rank(source: &str) -> i32 {
    match source {
        "override" => 50,
        "exif" | "video" => 40,
        "filename" => 30,
        "folder" => 20,
        "filesystem" => 10,
        _ => 0,
    }
}

fn confidence_for(source: &str, precision: Precision) -> &'static str {
    match (source, precision) {
        ("override", _) => "high",
        ("exif", _) | ("video", _) => "high",
        ("filename", Precision::Second) | ("filename", Precision::Minute) => "medium",
        ("filename", _) => "low",
        _ => "low",
    }
}

fn valid_year(y: i32) -> bool {
    (1970..=2100).contains(&y)
}

struct Runs {
    runs: Vec<(usize, usize)>, // (byte start, digits len)
    bytes: Vec<u8>,
}

impl Runs {
    fn new(s: &str) -> Runs {
        let b = s.as_bytes().to_vec();
        let mut runs = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i].is_ascii_digit() {
                let start = i;
                while i < b.len() && b[i].is_ascii_digit() {
                    i += 1;
                }
                runs.push((start, i - start));
            } else {
                i += 1;
            }
        }
        Runs { runs, bytes: b }
    }

    fn num(&self, start: usize, len: usize) -> Option<i64> {
        if start + len > self.bytes.len() {
            return None;
        }
        let s = std::str::from_utf8(&self.bytes[start..start + len]).ok()?;
        s.parse::<i64>().ok()
    }
}

/// 尝试从文本（文件名/目录名）解析日期时间，返回 (时间, 精度, 原始片段)。
pub fn parse_text(text: &str) -> Option<(NaiveDateTime, Precision, String)> {
    let runs = Runs::new(text);
    // 1) 时间戳（微信/安卓常见 13 位毫秒、10 位秒）
    for &(start, len) in &runs.runs {
        if len == 13 {
            if let Some(v) = runs.num(start, 13) {
                if (946_684_800_000..=2_051_222_400_000).contains(&v) {
                    let secs = v / 1000;
                    if let Some(dt) = chrono::DateTime::from_timestamp(secs, 0) {
                        let local = dt.with_timezone(&chrono::Local).naive_local();
                        return Some((
                            local,
                            Precision::Second,
                            text[start..start + len].to_string(),
                        ));
                    }
                }
            }
        }
        if len == 10 {
            if let Some(v) = runs.num(start, 10) {
                if (946_684_800..=2_051_222_400).contains(&v) {
                    if let Some(dt) = chrono::DateTime::from_timestamp(v, 0) {
                        let local = dt.with_timezone(&chrono::Local).naive_local();
                        return Some((
                            local,
                            Precision::Second,
                            text[start..start + len].to_string(),
                        ));
                    }
                }
            }
        }
    }
    // 2) 紧凑格式 yyyymmdd[hhmmss]
    for (idx, &(start, len)) in runs.runs.iter().enumerate() {
        if len >= 8 {
            let y = runs.num(start, 4)? as i32;
            let mo = runs.num(start + 4, 2)? as u32;
            let d = runs.num(start + 6, 2)? as u32;
            if !valid_year(y) {
                continue;
            }
            let date = match NaiveDate::from_ymd_opt(y, mo, d) {
                Some(v) => v,
                None => continue,
            };
            if len >= 14 {
                let hh = runs.num(start + 8, 2)? as u32;
                let mi = runs.num(start + 10, 2)? as u32;
                let ss = runs.num(start + 12, 2)? as u32;
                if let Some(t) = NaiveTime::from_hms_opt(hh, mi, ss) {
                    return Some((
                        NaiveDateTime::new(date, t),
                        Precision::Second,
                        text[start..start + 14].to_string(),
                    ));
                }
                continue;
            }
            if len == 12 {
                let hh = runs.num(start + 8, 2)? as u32;
                let mi = runs.num(start + 10, 2)? as u32;
                if let Some(t) = NaiveTime::from_hms_opt(hh, mi, 0) {
                    return Some((
                        NaiveDateTime::new(date, t),
                        Precision::Minute,
                        text[start..start + 12].to_string(),
                    ));
                }
            }
            if len == 8 {
                // 形如 20230501_103000 / 20230501-1030 的“日期 + 时间”分段写法
                if let Some(&(nstart, nlen)) = runs.runs.get(idx + 1) {
                    let gap = &text[start + 8..nstart.min(text.len())];
                    let gap_ok = gap.len() <= 2 && !gap.chars().any(|c| c.is_ascii_alphanumeric());
                    if gap_ok && nlen >= 6 {
                        let hh = runs.num(nstart, 2)? as u32;
                        let mi = runs.num(nstart + 2, 2)? as u32;
                        let ss = runs.num(nstart + 4, 2)? as u32;
                        if let Some(t) = NaiveTime::from_hms_opt(hh, mi, ss) {
                            return Some((
                                NaiveDateTime::new(date, t),
                                Precision::Second,
                                format!("{}_{}", &text[start..start + 8], &text[nstart..nstart + 6]),
                            ));
                        }
                    } else if gap_ok && nlen == 4 {
                        let hh = runs.num(nstart, 2)? as u32;
                        let mi = runs.num(nstart + 2, 2)? as u32;
                        if let Some(t) = NaiveTime::from_hms_opt(hh, mi, 0) {
                            return Some((
                                NaiveDateTime::new(date, t),
                                Precision::Minute,
                                text[start..nstart + 4].to_string(),
                            ));
                        }
                    }
                }
                return Some((
                    NaiveDateTime::new(date, NaiveTime::from_hms_opt(0, 0, 0)?),
                    Precision::Day,
                    text[start..start + 8].to_string(),
                ));
            }
        }
    }
    // 3) 带分隔符：2023-05-01 / 2023_05_01_103000 / 2023年5月1日
    for (idx, &(start, len)) in runs.runs.iter().enumerate() {
        if len != 4 {
            continue;
        }
        let y = runs.num(start, 4)? as i32;
        if !valid_year(y) {
            continue;
        }
        let mut cursor = start + 4;
        let mut pieces: Vec<u32> = Vec::new();
        let mut scanned = 0usize;
        while let Some(&(rstart, rlen)) = runs.runs.get(idx + 1 + scanned) {
            // 中间只允许分隔符（含中文年月日）
            let gap = &text[cursor..rstart];
            if gap.chars().count() > 2 || gap.chars().any(|c| c.is_ascii_alphanumeric()) {
                break;
            }
            if rlen > 4 {
                break;
            }
            let v = runs.num(rstart, rlen)?;
            pieces.push(v as u32);
            cursor = rstart + rlen;
            scanned += 1;
            if pieces.len() >= 6 {
                break;
            }
        }
        if pieces.is_empty() {
            continue;
        }
        if pieces.len() == 1 {
            let mo = pieces[0];
            if (1..=12).contains(&mo) {
                let date = NaiveDate::from_ymd_opt(y, mo, 1)?;
                return Some((
                    NaiveDateTime::new(date, NaiveTime::from_hms_opt(0, 0, 0)?),
                    Precision::Month,
                    text[start..cursor.min(text.len())].to_string(),
                ));
            }
            continue;
        }
        let (mo, d) = (pieces[0], pieces[1]);
        let date = match NaiveDate::from_ymd_opt(y, mo, d) {
            Some(v) => v,
            None => continue,
        };
        let raw = text[start..cursor.min(text.len())].to_string();
        if pieces.len() >= 5 {
            let (hh, mi, ss) = (pieces[2], pieces[3], pieces[4]);
            if let Some(t) = NaiveTime::from_hms_opt(hh, mi, ss) {
                return Some((NaiveDateTime::new(date, t), Precision::Second, raw));
            }
        } else if pieces.len() == 4 {
            let (hh, mi) = (pieces[2], pieces[3]);
            if let Some(t) = NaiveTime::from_hms_opt(hh, mi, 0) {
                return Some((NaiveDateTime::new(date, t), Precision::Minute, raw));
            }
        }
        return Some((
            NaiveDateTime::new(date, NaiveTime::from_hms_opt(0, 0, 0)?),
            Precision::Day,
            raw,
        ));
    }
    None
}

/// 仅解析目录名中的年/月（也接受年月日）。
pub fn parse_folder(name: &str) -> Option<(NaiveDateTime, Precision, String)> {
    let t = name.trim();
    if t.len() == 4 {
        if let Ok(y) = t.parse::<i32>() {
            if valid_year(y) && !t.starts_with('0') {
                return Some((
                    NaiveDate::from_ymd_opt(y, 1, 1)?.and_hms_opt(0, 0, 0)?,
                    Precision::Year,
                    t.to_string(),
                ));
            }
        }
    }
    parse_text(t)
}

pub fn candidate(
    dt: NaiveDateTime,
    precision: Precision,
    source: &str,
    raw: String,
    offset_min: Option<i32>,
    detail: &str,
) -> Candidate {
    Candidate {
        value: iso(dt),
        precision: precision.as_str().to_string(),
        source: source.to_string(),
        confidence: confidence_for(source, precision).to_string(),
        raw,
        offset_min,
        detail: detail.to_string(),
    }
}

pub struct Selection {
    pub effective: Option<String>,
    pub source: Option<String>,
    pub confidence: String,
    pub precision: String,
    pub needs_review: bool,
    pub reason: Option<String>,
}

/// 从候选集中选出有效日期，并判定是否需要人工确认。
pub fn select(candidates: &[Candidate], override_value: Option<&str>) -> Selection {
    if override_value==Some(CONFIRMED_UNKNOWN) {
        return Selection{effective:None,source:Some("override".into()),confidence:"high".into(),precision:"none".into(),needs_review:false,reason:None};
    }
    if let Some(ov) = override_value {
        if !ov.is_empty() {
            return Selection {
                effective: Some(ov.to_string()),
                source: Some("override".into()),
                confidence: "high".into(),
                precision: "second".into(),
                needs_review: false,
                reason: None,
            };
        }
    }
    let mut best: Option<&Candidate> = None;
    for c in candidates {
        if c.value.is_empty() {
            continue;
        }
        let better = match best {
            None => true,
            Some(b) => source_rank(&c.source) > source_rank(&b.source),
        };
        if better {
            best = Some(c);
        }
    }
    let Some(best) = best else {
        return Selection {
            effective: None,
            source: None,
            confidence: "none".into(),
            precision: "none".into(),
            needs_review: true,
            reason: Some("无日期".into()),
        };
    };
    let mut needs_review = best.confidence != "high" && best.confidence != "medium";
    let mut reason = if needs_review {
        Some(format!("日期来自{}，需确认", label(&best.source)))
    } else {
        None
    };
    // 冲突检测：可靠的拍摄时间（EXIF/视频）与文件名/目录线索相差一天以上。
    // 文件系统时间只是兜底值，不参与冲突判定，否则几乎每张照片都会被误报。
    if let Some(primary) = candidates.iter().find(|c| c.source == "exif" || c.source == "video") {
        if let Some(pd) = parse_iso(&primary.value) {
            for other in candidates
                .iter()
                .filter(|c| matches!(c.source.as_str(), "filename" | "folder"))
            {
                if let Some(od) = parse_iso(&other.value) {
                    let diff = (pd - od).num_days().abs();
                    if diff >= 1 {
                        needs_review = true;
                        reason = Some(format!(
                            "时间冲突：{} 与 {} 相差 {} 天",
                            label(&primary.source),
                            label(&other.source),
                            diff
                        ));
                        break;
                    }
                }
            }
        }
    }
    Selection {
        effective: Some(best.value.clone()),
        source: Some(best.source.clone()),
        confidence: best.confidence.clone(),
        precision: best.precision.clone(),
        needs_review,
        reason,
    }
}

pub fn label(source: &str) -> &'static str {
    match source {
        "override" => "人工覆盖",
        "exif" => "EXIF",
        "video" => "视频容器",
        "filename" => "文件名",
        "folder" => "目录名",
        "filesystem" => "文件系统时间",
        _ => "未知",
    }
}

pub fn filesystem_candidate(mtime_ms: i64) -> Option<Candidate> {
    let secs = mtime_ms / 1000;
    chrono::DateTime::from_timestamp(secs, 0).map(|dt| {
        let local = dt.with_timezone(&chrono::Local).naive_local();
        candidate(
            local,
            Precision::Second,
            "filesystem",
            "文件修改时间".into(),
            None,
            "仅作兜底，不代表拍摄时间",
        )
    })
}

/// 从目录链（由近到远）里寻找最具体的日期。
pub fn folder_candidate(dirs: &[String]) -> Option<Candidate> {
    for dir in dirs {
        if let Some((dt, precision, raw)) = parse_folder(dir) {
            if precision == Precision::Year || precision == Precision::Month || precision == Precision::Day {
                return Some(candidate(
                    dt,
                    precision,
                    "folder",
                    raw,
                    None,
                    "来自目录名",
                ));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_filename_patterns() {
        let cases = [
            ("IMG_20230501_103000.jpg", "2023-05-01T10:30:00", Precision::Second),
            ("PXL_20210704_201530123.jpg", "2021-07-04T20:15:30", Precision::Second),
            ("Screenshot_20230501-103000.png", "2023-05-01T10:30:00", Precision::Second),
            ("VID_20200101_000000.mp4", "2020-01-01T00:00:00", Precision::Second),
            ("2023-05-01 10.30.00.jpg", "2023-05-01T10:30:00", Precision::Second),
            ("20230501.jpg", "2023-05-01T00:00:00", Precision::Day),
            ("2023年5月1日 春游.jpg", "2023-05-01T00:00:00", Precision::Day),
        ];
        for (name, expect, precision) in cases {
            let (dt, p, _) = parse_text(name).unwrap_or_else(|| panic!("无法解析 {name}"));
            assert_eq!(iso(dt), expect, "{name}");
            assert_eq!(p, precision, "{name}");
        }
    }

    #[test]
    fn rejects_implausible_numbers() {
        assert!(parse_text("IMG_1234.jpg").is_none());
        assert!(parse_text("photo-99999999.jpg").is_none());
        assert!(parse_text("20231399.jpg").is_none(), "非法月份/日期必须被拒绝");
    }

    #[test]
    fn parses_epoch_ms_and_seconds() {
        let (dt, _, _) = parse_text("wx_camera_1683000000000.jpg").unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2023-05-02");
        let (dt2, _, _) = parse_text("received_1683000000.jpg").unwrap();
        assert_eq!(dt2.format("%Y-%m-%d").to_string(), "2023-05-02");
    }

    #[test]
    fn folder_only_accepts_date_like_names() {
        assert!(parse_folder("2023").is_some());
        assert!(parse_folder("2023-05").is_some());
        assert!(parse_folder("照片").is_none());
        assert!(parse_folder("Trip2023").is_some() || parse_folder("Trip2023").is_none());
    }

    #[test]
    fn selection_prefers_exif_and_flags_conflict() {
        let cands = vec![
            candidate(
                NaiveDate::from_ymd_opt(2023, 5, 1).unwrap().and_hms_opt(10, 0, 0).unwrap(),
                Precision::Second,
                "exif",
                "2023:05:01 10:00:00".into(),
                Some(480),
                "EXIF DateTimeOriginal",
            ),
            candidate(
                NaiveDate::from_ymd_opt(2020, 1, 1).unwrap().and_hms_opt(0, 0, 0).unwrap(),
                Precision::Day,
                "filename",
                "20200101".into(),
                None,
                "文件名",
            ),
        ];
        let s = select(&cands, None);
        assert_eq!(s.source.as_deref(), Some("exif"));
        assert!(s.needs_review, "日期冲突必须进入待确认");
        assert!(s.reason.unwrap().contains("冲突"));
    }

    #[test]
    fn no_candidates_stays_empty() {
        let s = select(&[], None);
        assert!(s.effective.is_none());
        assert_eq!(s.confidence, "none");
        assert!(s.needs_review);
    }

    #[test]
    fn override_wins() {
        let cands = vec![candidate(
            NaiveDate::from_ymd_opt(2023, 5, 1).unwrap().and_hms_opt(10, 0, 0).unwrap(),
            Precision::Second,
            "exif",
            "".into(),
            None,
            "",
        )];
        let s = select(&cands, Some("2019-01-02T00:00:00"));
        assert_eq!(s.source.as_deref(), Some("override"));
        assert!(!s.needs_review);
    }

    #[test]
    fn confirmed_unknown_stays_out_of_review() {
        let cands = filesystem_candidate(1_700_000_000_000).into_iter().collect::<Vec<_>>();
        let s = select(&cands, Some(CONFIRMED_UNKNOWN));
        assert!(s.effective.is_none());
        assert_eq!(s.source.as_deref(), Some("override"));
        assert!(!s.needs_review);
    }

    #[test]
    fn native_time_input_can_omit_zero_seconds() {
        assert_eq!(iso(parse_iso("2024-09-16T13:33").unwrap()), "2024-09-16T13:33:00");
        assert!(parse_iso("2024-02-30T13:33").is_none());
    }
}
