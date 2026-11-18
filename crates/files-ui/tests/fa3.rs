// SPDX-License-Identifier: GPL-3.0-only
// The FA3 boards (files2-browse with Favourites and Recent, -recent, -types, -type-docs, -search, -search-filters,
// -bin, -cleanup, -details), each as `snapshots/files2-<board>.png` with the board's sample data, plus the two
// screens no board draws: a Free up space review and Favourites › Edit.
mod common;
use common::*;
use files_core::{self as model, Item};
use files_ui::{CleanCard, Detail, FileRow, file_row, hit_row, space_row, type_tile, used_bar};

const GB: u64 = 1 << 30;

/// A row the way Recent, search and the bin write it: `size · when · where`.
fn placed(item: &Item, middle: &str, place: &str) -> FileRow {
    let mut r = file_row(item, day(TODAY), false);
    r.detail = if item.is_dir {
        format!("{} · {place}", model::detail_text(item, day(TODAY))).into()
    } else if middle.is_empty() {
        format!("{} · {place}", model::size_text(item.size)).into()
    } else {
        format!("{} · {middle} · {place}", model::size_text(item.size)).into()
    };
    r
}

fn section(mut r: FileRow, s: &str) -> FileRow {
    r.section = s.into();
    r
}

#[test]
fn fa3_boards() {
    let s = app();
    let ui = &s.ui;

    // files2-browse: the drives, then Favourites and Recent
    ui.set_drives(model_of(drives()));
    ui.set_favs(model_of(vec![
        file_row(&folder("Downloads", 9), day(TODAY), false),
        file_row(&folder("Notes", 7), day(TODAY), false),
        file_row(&folder("Projects", 3), day(TODAY), false),
    ]));
    ui.set_recent_strip(rows(
        &[
            file("sunset.jpg", 3174 * KB, "16 Nov"),
            file("boarding-pass.pdf", 212 * KB, "16 Nov"),
            file("main.rs", 3 * KB, "16 Nov"),
            file("Yerevan Nights.mp3", 4300 * KB, "15 Nov"),
        ],
        &[],
    ));
    ui.set_view("browse".into());
    s.shoot("files2-browse-full");

    // files2-recent
    ui.set_tab_index(1);
    ui.set_view("recent".into());
    ui.set_rows(model_of(vec![
        section(
            placed(
                &file("sunset.jpg", 3174 * KB, "16 Nov"),
                "10:12",
                "E:\\Images\\Camera",
            ),
            "TODAY",
        ),
        placed(
            &file("boarding-pass.pdf", 212 * KB, "16 Nov"),
            "09:40",
            "E:\\Downloads",
        ),
        placed(
            &file("main.rs", 3277, "16 Nov"),
            "08:55",
            "E:\\Projects\\neoui",
        ),
        section(
            placed(
                &file("Yerevan Nights.mp3", 4198 * KB, "15 Nov"),
                "21:30",
                "E:\\Music",
            ),
            "YESTERDAY",
        ),
        placed(&file("notes.md", 2150, "15 Nov"), "18:02", "E:\\Notes"),
        section(
            placed(
                &file("Бюджет 2026.xlsx", 96 * KB, "15 Nov"),
                "Sun",
                "E:\\Documents",
            ),
            "THIS WEEK",
        ),
        placed(
            &file("Ծրագիր.docx", 48 * KB, "14 Nov"),
            "Sat",
            "E:\\Documents",
        ),
        placed(
            &file("clip-0412.mp4", 18 * MB, "13 Nov"),
            "Fri",
            "E:\\Videos",
        ),
    ]));
    s.shoot("files2-recent");

    // files2-types
    ui.set_tab_index(2);
    ui.set_view("types".into());
    let groups: [(u32, u64); 8] = [
        (1038, 5_583_457_485),
        (26, 3_650_722_201),
        (212, 2_254_857_830),
        (58, 410 * MB),
        (3, 1_288_490_188),
        (6, 18 * MB),
        (9, 1_181_116_006),
        (96, 4 * MB),
    ];
    let labels = [
        "Images",
        "Videos",
        "Audio",
        "Documents",
        "Archives",
        "Apps",
        "Downloads",
        "Code & text",
    ];
    ui.set_types(model_of(
        (0..8)
            .map(|k| type_tile(k, labels[k], groups[k].0, groups[k].1))
            .collect(),
    ));
    let bytes: [u64; 8] = core::array::from_fn(|k| groups[k].1);
    ui.set_types_used("12.4 GB".into());
    ui.set_types_of("used of 41.8 GB on E:".into());
    ui.set_types_bar(model_of(used_bar(&bytes, 41 * GB + 819 * MB)));
    s.shoot("files2-types");

    // files2-type-docs
    ui.set_view("type".into());
    ui.set_name("Documents".into());
    ui.set_type_subs(strings(&["All", "PDF", "Word", "Sheets", "Slides"]));
    ui.set_type_sub(0);
    ui.set_rows(model_of(vec![
        placed(
            &file("boarding-pass.pdf", 212 * KB, "1 Nov"),
            "1 Nov",
            "E:\\Downloads",
        ),
        placed(
            &file("Ծրագիր.docx", 48 * KB, "30 Oct"),
            "30 Oct",
            "E:\\Documents",
        ),
        placed(
            &file("Бюджет 2026.xlsx", 96 * KB, "28 Oct"),
            "28 Oct",
            "E:\\Documents",
        ),
        placed(
            &file("slides-neosymbian.pptx", 2458 * KB, "26 Oct"),
            "26 Oct",
            "E:\\Documents",
        ),
        placed(
            &file("Symbian^3 book.epub", 1843 * KB, "20 Oct"),
            "20 Oct",
            "E:\\Documents",
        ),
        placed(
            &file("trip-notes.txt", 4 * KB, "3 Nov"),
            "3 Nov",
            "E:\\Notes",
        ),
        placed(&file("notes.md", 2150, "16 Nov"), "16 Nov", "E:\\Notes"),
        placed(
            &file("budget.csv", 6 * KB, "14 Nov"),
            "14 Nov",
            "E:\\Documents",
        ),
    ]));
    s.shoot("files2-type-docs");

    // files2-search: "song" on all drives
    ui.set_type_subs(strings(&[]));
    ui.set_view("browse".into());
    ui.set_tab_index(0);
    ui.set_searching(true);
    ui.set_query("song".into());
    ui.set_can_search_folder(true);
    ui.set_search_info("8 results · C:, E: and Z:".into());
    let hits = [
        (file("song.mp3", 5734 * KB, "30 Oct"), "E:\\Music"),
        (
            file("song.mp3", 5325 * KB, "1 Oct"),
            "E:\\Backup\\Old phone",
        ),
        (folder("songs", 48), "E:\\Backup\\Old phone"),
        (
            file("Songbook.pdf", 1126 * KB, "4 Oct"),
            "E:\\Documents\\Scans",
        ),
        (file("song-ideas.md", 3 * KB, "4 Oct"), "E:\\Notes"),
        (
            file("ringtone-song.amr", 38 * KB, "4 Oct"),
            "C:\\Data\\Ringtones",
        ),
        (
            file("Song of Sasun.epub", 2355 * KB, "4 Oct"),
            "E:\\Downloads",
        ),
    ];
    ui.set_hits(model_of(
        hits.iter()
            .map(|(it, at)| hit_row(placed(it, "", at), "song"))
            .collect(),
    ));
    s.shoot("files2-search");

    // files2-search-filters
    ui.set_hits(model_of(vec![hit_row(
        placed(&hits[0].0, "", hits[0].1),
        "song",
    )]));
    ui.set_search_info("1 result · Audio, 1–100 MB, this month".into());
    ui.set_filter_count(3);
    ui.set_f_type(2);
    ui.set_f_size(2);
    ui.set_f_modified(3);
    ui.set_filter_results(1);
    ui.set_filters_open(true);
    s.shoot("files2-search-filters");
    ui.set_filters_open(false);
    ui.set_searching(false);
    ui.set_query("".into());

    // files2-bin, two picked
    let now = day(TODAY);
    let gone = |name: &str, size: u64, days: i64, from: &str, dir: bool| {
        let it = if dir {
            folder(name, 4)
        } else {
            file(name, size, "1 Oct")
        };
        let mut r = file_row(&it, now, false);
        r.detail = format!(
            "{} · from {from}",
            model::deleted_text(now - days * model::DAY, now)
        )
        .into();
        if let Some(left) = model::left_text(now - days * model::DAY, now) {
            r.detail = model::deleted_text(now - days * model::DAY, now).into();
            r.note = left.into();
        }
        r
    };
    let mut bin = vec![
        gone("song (1).mp3", 0, 2, "E:\\Music", false),
        gone("IMG_2033.jpg", 0, 2, "E:\\Images", false),
        gone("old-draft.md", 0, 5, "E:\\Notes", false),
        gone("Projects-old", 0, 9, "E:\\Projects", true),
        gone("invoice-oct.pdf", 0, 18, "E:\\Notes", false),
        gone("XPlore_1.57.sisx", 0, 27, "E:\\Downloads", false),
    ];
    bin[0].checked = true;
    bin[1].checked = true;
    ui.set_view("bin".into());
    ui.set_name("Recycle bin".into());
    ui.set_info_title("Kept for 30 days".into());
    ui.set_info_text(
        "Then deleted for good · 6 items · 96 MB in C:\\.trash\\ and E:\\.trash\\".into(),
    );
    ui.set_rows(model_of(bin));
    ui.set_selected(2);
    s.shoot("files2-bin");
    ui.set_selected(0);

    // files2-cleanup
    ui.set_view("cleanup".into());
    ui.set_drives(model_of(vec![
        space_row(
            'E',
            "Mass memory",
            "mass",
            41 * GB + 819 * MB,
            29 * GB + 410 * MB,
        ),
        space_row('C', "Phone memory", "phone", 8 * GB, 2 * GB + 922 * MB),
    ]));
    ui.set_can_free("4.5 GB".into());
    let card = |title: &str, size: &str, sub: &str, kind: i32| CleanCard {
        title: title.into(),
        size: size.into(),
        sub: sub.into(),
        kind,
        enabled: true,
    };
    ui.set_clean(model_of(vec![
        card("Large files", "3.4 GB", "12 files", 0),
        card("Old downloads", "820 MB", "6 old files", 1),
        card("Duplicates", "210 MB", "14 copies", 2),
        card("Recycle bin", "96 MB", "6 items", 3),
    ]));
    s.shoot("files2-cleanup");

    // a review (no board): the large files, two picked
    ui.set_view("review".into());
    ui.set_name("Large files".into());
    ui.set_info_title("".into());
    let mut large = vec![
        placed(
            &file("backup-2026.zip", 1229 * MB, "27 Oct"),
            "27 Oct",
            "E:\\Backup",
        ),
        placed(
            &file("N8-backup.nbu", 640 * MB, "1 Oct"),
            "1 Oct",
            "E:\\Backup\\Old phone",
        ),
        placed(
            &file("clip-0101.mp4", 512 * MB, "4 Oct"),
            "4 Oct",
            "E:\\Videos",
        ),
        placed(
            &file("live-set.flac", 410 * MB, "12 Oct"),
            "12 Oct",
            "E:\\Music",
        ),
    ];
    large[0].checked = true;
    large[1].checked = true;
    ui.set_rows(model_of(large));
    ui.set_selected(2);
    s.shoot("files2-review");
    ui.set_selected(0);

    // files2-details over the Documents folder
    let docs = vec![
        folder("Letters", 4),
        folder("Scans", 6),
        file("boarding-pass.pdf", 212 * KB, "1 Nov"),
        file("Ծրագիր.docx", 48 * KB, "30 Oct"),
        file("Бюджет 2026.xlsx", 96 * KB, "28 Oct"),
        file("slides-neosymbian.pptx", 2458 * KB, "26 Oct"),
        file("budget.csv", 6 * KB, "14 Nov"),
    ];
    ui.set_view("folder".into());
    ui.set_name("Documents".into());
    ui.set_crumbs(strings(&["E:", "Documents"]));
    ui.set_rows(rows(&docs, &[]));
    let pass = file("boarding-pass.pdf", 217_088, "1 Nov");
    ui.set_details_item(file_row(&pass, now, false));
    ui.set_details_kind("PDF document".into());
    let d = |k: &str, v: String, mono: bool| Detail {
        key: k.into(),
        value: v.into(),
        mono,
    };
    ui.set_details(model_of(vec![
        d("Type", "PDF document (.pdf)".into(), false),
        d("Size", model::bytes_text(217_088), false),
        d("Location", "E:\\Documents\\".into(), true),
        d(
            "Modified",
            model::full_date_text(dated("1 Nov") + 8 * 3600 * 1_000_000 + 40 * 60 * 1_000_000),
            false,
        ),
        d("Opens with", "PDF viewer (built in)".into(), false),
    ]));
    ui.set_details_attributes(true);
    ui.set_details_open(true);
    s.shoot("files2-details");
    ui.set_details_open(false);

    // Favourites › Edit (no board)
    ui.set_view("favs".into());
    let mut favs = vec![
        file_row(&folder("Downloads", 9), now, false),
        file_row(&folder("Notes", 7), now, false),
        file_row(&folder("Projects", 3), now, false),
    ];
    for (f, at) in favs
        .iter_mut()
        .zip(["E:\\Downloads\\", "E:\\Notes\\", "E:\\Projects\\"])
    {
        f.detail = at.into();
    }
    ui.set_favs(model_of(favs));
    s.shoot("files2-favs-edit");

    // hit_row splits around the match in any case and script
    let r = hit_row(
        file_row(&file("Ноктюрн.opus", 1, "1 Nov"), now, false),
        "ТЮР",
    );
    assert_eq!(
        (r.name.as_str(), r.hit.as_str(), r.post.as_str()),
        ("Нок", "тюр", "н.opus")
    );
}
