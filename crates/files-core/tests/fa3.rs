// SPDX-License-Identifier: Apache-2.0
use files_core::*;

/// 2026-11-18 (a Wednesday) 10:12 UTC as a `TTime`.
const NOW: i64 = ((20_775 + 719_540) * 86_400 + 10 * 3600 + 12 * 60) * 1_000_000;

#[test]
fn now_is_what_the_tests_think() {
    assert_eq!(date_text(NOW), "2026-11-18 10:12");
}

#[test]
fn ages_and_when_texts() {
    assert_eq!(age(NOW, NOW), Age::Today);
    assert_eq!(age(NOW - DAY, NOW), Age::Yesterday);
    assert_eq!(age(NOW - 3 * DAY, NOW), Age::ThisWeek);
    assert_eq!(age(NOW - 7 * DAY, NOW), Age::Earlier);
    assert_eq!(when_text(NOW, NOW), "10:12");
    assert_eq!(when_text(NOW - 3 * DAY, NOW), "Sun");
    assert_eq!(when_text(NOW - 15 * DAY, NOW), "3 Nov");
}

#[test]
fn bin_texts() {
    assert_eq!(deleted_text(NOW, NOW), "Deleted today");
    assert_eq!(deleted_text(NOW - DAY, NOW), "Deleted 1 day ago");
    assert_eq!(deleted_text(NOW - 9 * DAY, NOW), "Deleted 9 days ago");
    assert_eq!(left_text(NOW - 26 * DAY, NOW), None);
    assert_eq!(
        left_text(NOW - 27 * DAY, NOW).as_deref(),
        Some("3 days left")
    );
    assert_eq!(
        left_text(NOW - 29 * DAY, NOW).as_deref(),
        Some("1 day left")
    );
}

#[test]
fn details_texts() {
    assert_eq!(bytes_text(217_088), "212 KB (217,088 bytes)");
    assert_eq!(bytes_text(512), "512 bytes");
    assert_eq!(bytes_text(1), "1 byte");
    assert_eq!(bytes_text(1_234_567_890), "1.1 GB (1,234,567,890 bytes)");
    assert_eq!(full_date_text(NOW), "18 Nov 2026, 10:12");
}

#[test]
fn modified_bands_start_at_midnight_monday_the_first_and_new_year() {
    let since = |m: Modified| date_text(m.since(NOW).unwrap());
    assert_eq!(Modified::Any.since(NOW), None);
    assert_eq!(since(Modified::Today), "2026-11-18 00:00");
    assert_eq!(since(Modified::Week), "2026-11-16 00:00");
    assert_eq!(since(Modified::Month), "2026-11-01 00:00");
    assert_eq!(since(Modified::Year), "2026-01-01 00:00");
}

#[test]
fn recent_dedupes_caps_and_round_trips() {
    let mut r = Recent::default();
    for i in 0..60 {
        r.add(&format!("E:\\f{i}.txt"), i);
    }
    r.add("e:\\F59.TXT", 99);
    assert_eq!(r.items.len(), RECENT_MAX);
    assert_eq!(r.items[0], ("e:\\F59.TXT".to_string(), 99));
    assert_eq!(r.items[1].0, "E:\\f58.txt");
    assert_eq!(Recent::decode(&r.encode()), r);
    assert_eq!(
        Recent::decode("x\tE:\\a\n5\t\n7\tE:\\b.txt\n").items,
        [("E:\\b.txt".to_string(), 7)]
    );
    let favs = vec!["E:\\Downloads\\".to_string(), "C:\\Data\\Привет\\".into()];
    assert_eq!(favs_decode(&favs_encode(&favs)), favs);
}
