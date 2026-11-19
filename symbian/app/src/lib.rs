// SPDX-License-Identifier: GPL-3.0-only
//! The Files app (`NEOFILES.EXE`, v2: docs files-app.md FA1 + FA2): a neoui app process ([`neoapp`]) on Symbian's
//! file server ([`fs_e32`] over [`e32::fs`]). The Browse tab lists the drives; a folder shows as a list or a grid with
//! its breadcrumb, sorted and filtered as the user chose (kept as `files.*` settings), and lists itself again when the
//! file server says it changed. A long press picks rows: Copy and Move go through the paste bar and run on a worker
//! ([`job`]) with a progress toast and a shade entry (Cancel); Delete moves into the drive's Recycle bin
//! (`files_ops::trash`) with Undo; Rename keeps the extension. Z: and other read-only drives open and copy only.
//! FA3 ([`fa3`]): Favourites and Recent, the Types tab and search over a file index kept by a worker ([`scan`]), the
//! Recycle bin, Free up space and Details. FA4 ([`edit`]): the text viewer and the editors (code, Markdown, CSV).
//! FA5 ([`image`], [`pics`]): the image viewer and editor, and thumbnails, decoded on a picture worker.
#![no_std]

extern crate alloc;

mod archive;
mod edit;
mod fa3;
mod fa7;
mod image;
mod job;
mod pics;
mod scan;

use alloc::collections::VecDeque;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;
use core::cell::RefCell;
use core::pin::pin;
use e32::fs::notify;
use e32::ipc::TIpcArgs;
use e32::rt::{self, Either};
use e32::sys::TPtr8;
use e32::{dprintln, time};
use files_core::{self as model, Item, Sort, SortKey};
use files_ops::{Choice, Kind, trash};
use fs_e32::E32Fs;
use neo_fs::{Fs as _, Open};
use neoapp::{App, AppEvent, Page, Query};
use neoui_proto::ipc as proto;
use neoui_proto::page::Notification;
use neoui_proto::settings::Value;
use neoui_proto::{InputEvent, Rect};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use files_ui::{FileRow, FilesApp, KeyboardFocus, Kit, Scroll, Theme};

e32::main!(main);

fn main() -> i32 {
    match run() {
        Ok(()) => 0,
        Err(e) => {
            dprintln!("files: failed {}", e);
            e
        }
    }
}

/// How often the UI looks at a running job.
const JOB_POLL_US: i32 = 250_000;
/// How often the shade entry is posted again while a job runs (each post is an IPC and a shell redraw).
const NOTIFY_MS: u64 = 1_000;
/// The shade entry of a running job.
const JOB_NOTE: u32 = 1;
/// Folders with more subfolders than this show "Folder" instead of counting each one's items.
const COUNT_MAX: usize = 64;
/// How often the picture worker is polled while it works.
const PICS_POLL_US: i32 = 100_000;

/// What the UI asked for (its callbacks only queue; the loop acts with the file server).
enum Action {
    Drive(usize),
    Row(usize),
    Toggle(usize),
    Crumb(usize),
    Up,
    Tab(usize),
    Menu(String),
    Pick(usize),
    PickMenu(String),
    ClearSelection,
    NewFolder,
    NewFile,
    Refresh,
    SortKey(usize),
    Ascending(bool),
    FoldersFirst(bool),
    PasteCancel,
    PasteGo,
    JobHide,
    Clash(usize),
    FormCancel,
    FormSave,
    Fav(usize),
    FavRemove(usize),
    FavsEdit,
    RecentOpen(usize),
    SeeAll,
    TypeOpen(usize),
    TypeDrive(usize),
    TypeSub(usize),
    Searched(String),
    SearchScope(bool),
    Hit(usize),
    FilterType(usize),
    FilterSize(usize),
    FilterModified(usize),
    FiltersReset,
    Review(usize),
    DetailsReadOnly(bool),
    DetailsHidden(bool),
    TextScrolled(usize),
    TextTapped(usize, usize),
    TextHeld(usize, usize),
    HandleMoved(i32, usize, usize),
    Edit,
    UndoEdit,
    Save,
    CodeKey(String),
    WrapToggled,
    FindEdited(String),
    FindStep(bool),
    Replace(bool),
    MdPicked(usize),
    TaskToggled(usize),
    CellPicked(usize, usize),
    CellDone,
    NewType(usize),
    // FA5
    ImgMenu,
    ImgStrip(usize),
    ImgTapped,
    ImgDouble(f32, f32),
    ImgDragged(f32, f32),
    ImgReleased(f32, f32),
    ImgEditMenu(usize),
    ImgRedo,
    ImgRotate,
    ImgCropMode,
    ImgResize,
    ImgFlip,
    ImgSave,
    CropRatio(usize),
    CropStart(i32),
    CropDrag(i32, f32, f32),
    CropDone,
    ResizeChosen(usize),
    GroupPicked(usize, usize),
    // FA6
    ExtractWay(usize),
    ExtractBar(bool),
    JobCancel,
    CompressFormat(usize),
    SaveIn,
    PickerCancel,
    PickerDone,
    PickerDrive(usize),
    PickerCrumb(usize),
    PickerOpen(usize),
    PickerNewFolder,
    // FA7
    Fa7(fa7::Act),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Browse,
    Folder,
    Text,
    Recent,
    Types,
    Type,
    Bin,
    Cleanup,
    Review,
    Favs,
    Edit,
    Image,
    ImageEdit,
    Archive,
    PickFolder,
    /// FA7's viewers (cards, events, font, certificate, package, bytes).
    Fa7,
}

/// The form page over the folder.
enum Form {
    NewFolder,
    NewFile,
    /// The item and its extension, kept (files2-rename).
    Rename(Item, String),
}

/// The shell's dialog that is open, and what its first button does.
enum Ask {
    /// Leave the form without its name.
    Discard,
    Trash(Vec<String>),
    /// Recycle bin items to delete for good.
    Purge(Vec<String>),
    /// Leave the editor: Save / Discard / Cancel.
    Unsaved,
    /// Leave the image editor: Save copy / Discard / Cancel.
    ImageEdits,
    /// Leave a contact's or an event's form: Save / Discard / Cancel.
    Fa7Unsaved,
}

/// Files picked for Copy or Move, waiting in the paste bar for a folder.
struct Clip {
    kind: Kind,
    paths: Vec<String>,
    from: String,
}

/// The job running on the worker, as the UI shows it.
struct Running {
    kind: Kind,
    /// Deleting from the bin for good; `quiet` for the 30-day clean-up at start (no toast).
    purge: bool,
    quiet: bool,
    dest: String,
    hidden: bool,
    asked: bool,
    noted_at: u64,
    /// FA6: an archive job (Extract, Compress, List, Unpack) in place of a copy or move.
    arc: Option<archive::ArcJob>,
    /// When it started (ms), for the time left.
    started: u64,
}

struct State {
    fs: Rc<E32Fs>,
    slot: &'static job::Slot,
    view: View,
    /// Drive letters in card order, and whether each is read-only.
    drives: Vec<(char, bool)>,
    /// The open folder (`X:\dir\`), its entries as shown, which are picked, and whether its drive is read-only.
    dir: String,
    items: Vec<Item>,
    checked: Vec<bool>,
    read_only: bool,
    /// The open file (the text viewer).
    file: String,
    sort: Sort,
    grid: bool,
    hidden: bool,
    form: Option<Form>,
    asking: Option<Ask>,
    clip: Option<Clip>,
    running: Option<Running>,
    /// The last trash, for the toast's Undo.
    undo: Option<trash::Batch>,
    /// A folder whose change notification failed (a ROM path): not watched again.
    unwatched: String,
    /// The page last described to the shell.
    page: Page,
    // FA3
    /// The footer's tab (0 Browse, 1 Recent, 2 Types).
    tab: usize,
    scanner: &'static scan::Scanner,
    index: files_ops::index::Index,
    dupes: Option<Vec<Vec<usize>>>,
    /// Another scan is wanted when this one ends (and with duplicates).
    scan_again: Option<bool>,
    want_dupes: bool,
    /// The view the text viewer was opened from (Back returns there).
    text_from: View,
    recent: model::Recent,
    favs: Vec<String>,
    /// The rows of Recent, a type page, the bin or a review (`checked` is theirs there).
    listed: Vec<fa3::Listed>,
    group: usize,
    type_drive: usize,
    type_sub: usize,
    review: fa3::Review,
    /// Where the bin goes back to (the tab, or Free up space).
    bin_from: View,
    search: fa3::Search,
    /// What Details shows.
    details: String,
    /// The keyboard last asked of the UI server: (open, KeyboardFocus.seq, mode), as Contacts keeps it.
    kb: (bool, i32, i32),
    /// The file open in the viewer or an editor.
    open: Option<edit::Open>,
    /// New file's type (files2-new-file): 0 .txt, 1 .md, 2 .csv, 3 .json, 4 Code… (the name carries it).
    new_type: usize,
    // FA5
    pics: &'static pics::Pics,
    thumbs: image::Thumbs,
    viewer: Option<image::Viewer>,
    /// The Images type page's folders and their rows in `listed`.
    img_groups: Vec<(String, Vec<usize>)>,
    /// What the thumbnails were last fitted to: (view, scroll in rows, rows, groups, viewer's image).
    thumb_mark: (u8, i32, usize, usize, usize),
    // FA6
    sheet: Option<archive::Sheet>,
    archive: Option<archive::Browse>,
    picker: Option<archive::Picker>,
    compress: Option<archive::CompressForm>,
    // FA7
    fa7: Option<fa7::Open7>,
}

fn run() -> Result<(), i32> {
    let app = App::connect()?;
    let ui = FilesApp::new().map_err(|_| -2)?;
    ui.global::<Theme>().set_dp(app.dp);
    let actions: Rc<RefCell<VecDeque<Action>>> = Rc::default();
    bind(&ui, &actions);
    ui.show().map_err(|_| -2)?;
    let weak = ui.as_weak();
    app.set_scrolled(move || {
        weak.upgrade()
            .map_or(((0, 0), Rect::default()), |ui| scrolled(&ui))
    });

    let fs = Rc::new(E32Fs::connect().map_err(|e| e.code())?);
    let mut st = State {
        fs: fs.clone(),
        slot: job::Slot::new()?,
        view: View::Browse,
        drives: Vec::new(),
        dir: String::new(),
        items: Vec::new(),
        checked: Vec::new(),
        read_only: false,
        file: String::new(),
        sort: Sort::default(),
        grid: false,
        hidden: false,
        form: None,
        asking: None,
        clip: None,
        running: None,
        undo: None,
        unwatched: String::new(),
        page: Page::default(),
        tab: 0,
        scanner: scan::Scanner::new()?,
        index: files_ops::index::Index::default(),
        dupes: None,
        scan_again: None,
        want_dupes: false,
        text_from: View::Folder,
        recent: model::Recent::default(),
        favs: Vec::new(),
        listed: Vec::new(),
        group: 0,
        type_drive: 0,
        type_sub: 0,
        review: fa3::Review::Large,
        bin_from: View::Browse,
        search: fa3::Search::default(),
        details: String::new(),
        kb: (false, 0, 0),
        open: None,
        new_type: 0,
        pics: pics::Pics::new()?,
        thumbs: image::Thumbs::default(),
        viewer: None,
        img_groups: Vec::new(),
        thumb_mark: (0, 0, 0, 0, 0),
        sheet: None,
        archive: None,
        picker: None,
        compress: None,
        fa7: None,
    };
    load_settings(&app, &ui, &mut st);
    fa3::load(&mut st);
    archive::clear_cache(&st);
    #[cfg(feature = "test-files")]
    {
        test_files(&st);
        test_photos(&st);
        test_zips(&st);
        fa7::test_files(&st);
    }
    show_drives(&app, &ui, &mut st);
    // a trash cut short by a power cut is finished or rolled back (files-ops' journal)
    for &(d, ro) in &st.drives {
        if !ro {
            let _ = trash::recover(&*st.fs, d);
        }
    }
    // kept 30 days, then deleted for good, quietly
    let expired = fa3::expired(&st);
    if !expired.is_empty() {
        start_purge(&app, &ui, &mut st, expired, true);
    }
    rescan(&mut st, false);
    describe(&app, &ui, &mut st)?;
    app.draw()?;

    rt::block_on(async {
        loop {
            let mut ev = [0u8; proto::EVENT_LEN];
            let got = {
                let mut des = TPtr8::new(&mut ev);
                let mut args = TIpcArgs::new();
                args.des8(0, &mut des);
                // one NEXT_EVENT stays outstanding while the folder's watch and the job's poll come and go
                let mut next = pin!(app.server.send(proto::NEXT_EVENT, &args));
                loop {
                    let mut wait = app.wake_us().unwrap_or(10_000_000).min(10_000_000) as i32;
                    if st.running.is_some() || st.scanner.running() {
                        wait = wait.min(JOB_POLL_US);
                    }
                    if st.pics.busy() {
                        wait = wait.min(PICS_POLL_US);
                    }
                    let watch_dir =
                        (st.view == View::Folder && st.dir != st.unwatched).then(|| st.dir.clone());
                    let watch = async {
                        match &watch_dir {
                            Some(d) => match fs.0.notify_change(notify::ENTRY, d) {
                                Ok(n) => n.await,
                                Err(e) => Err(e),
                            },
                            None => core::future::pending().await,
                        }
                    };
                    match rt::select(
                        next.as_mut(),
                        rt::select(watch, time::delay(wait.max(1000))),
                    )
                    .await
                    {
                        Either::Left(r) => break r,
                        Either::Right(Either::Left(Ok(()))) => {
                            if st.view == View::Folder && st.form.is_none() {
                                relist(&app, &ui, &mut st);
                            }
                        }
                        Either::Right(Either::Left(Err(e))) => {
                            dprintln!("files: no watch on {} ({})", st.dir, e);
                            st.unwatched = st.dir.clone();
                        }
                        Either::Right(Either::Right(())) => {
                            poll_job(&app, &ui, &mut st);
                            poll_scan(&app, &ui, &mut st);
                            image::poll(&app, &ui, &mut st);
                            // a flick goes on without events: its scroll callbacks wait here
                            loop {
                                let next = actions.borrow_mut().pop_front();
                                match next {
                                    Some(a) => act(&app, &ui, &mut st, a),
                                    None => break,
                                }
                            }
                        }
                    }
                    thumbs_tick(&app, &ui, &mut st);
                    describe(&app, &ui, &mut st)?;
                    sync_keyboard(&app, &ui, &mut st);
                    app.redraw()?;
                }
            };
            got?;
            let Some(e) = AppEvent::decode(&ev) else {
                continue;
            };
            // the editor reads keys itself (as Terminal does); fields (find, a cell) get them through Slint
            match e {
                AppEvent::Input(InputEvent::Key { code, down: true })
                    if editor_has_keys(&ui, &st) =>
                {
                    editor_key(&app, &ui, &mut st, code)
                }
                // the viewer reads a second finger itself (a pinch)
                AppEvent::Input(InputEvent::Touch { id, x, y, phase })
                    if st.view == View::Image
                        && image::touch(&app, &ui, &mut st, id, x, y, phase) => {}
                _ => app.handle(&e),
            }
            match e {
                AppEvent::Quit => break,
                AppEvent::Back if neoapp::kit_back!(ui) => {}
                AppEvent::Back => back(&app, &ui, &mut st),
                AppEvent::Answer { i } => answer(&app, &ui, &mut st, i as usize),
                AppEvent::Keyboard { h } => {
                    // 0 while a field keeps the focus: the user put it away; a tap on the field asks again
                    let kf = ui.global::<KeyboardFocus>();
                    kf.set_dismissed(h == 0 && kf.get_open());
                    kf.set_text_h(h as f32);
                    ui.set_kb_h(h as f32);
                }
                AppEvent::NoteAction => undo(&app, &ui, &mut st),
                // back from NEOCONT's IMPORT page: the cards it has now are marked
                AppEvent::Visible(true) if st.view == View::Fa7 => fa7::visible(&ui, &mut st),
                AppEvent::Notified {
                    id: JOB_NOTE,
                    action: 1,
                } => st.slot.cancel(),
                _ => {}
            }
            loop {
                let next = actions.borrow_mut().pop_front();
                match next {
                    Some(a) => act(&app, &ui, &mut st, a),
                    None => break,
                }
            }
            image::poll(&app, &ui, &mut st);
            thumbs_tick(&app, &ui, &mut st);
            describe(&app, &ui, &mut st)?;
            sync_keyboard(&app, &ui, &mut st);
            app.redraw()?;
        }
        Ok::<(), i32>(())
    })?;
    if let Some(o) = st.open.as_ref().filter(|o| o.dirty()) {
        // what was typed survives a Quit (a later phase offers it back)
        o.keep_copy(&st.fs);
        dprintln!("files: kept unsaved {}", o.path);
    }
    if st.running.is_some() {
        // the worker dies with the process: let it stop between steps and remove its unfinished file first
        st.slot.cancel();
        let start = time::millis();
        while !st.slot.idle() && time::millis() - start < 5_000 {
            time::sleep(50_000);
        }
        let _ = app.cancel_notify(JOB_NOTE);
    }
    dprintln!("files: quit");
    Ok(())
}

/// The UI's callbacks queue what was asked.
fn bind(ui: &FilesApp, actions: &Rc<RefCell<VecDeque<Action>>>) {
    let index = |a: fn(usize) -> Action| {
        let q = actions.clone();
        move |i: i32| q.borrow_mut().push_back(a(i.max(0) as usize))
    };
    ui.on_drive(index(Action::Drive));
    ui.on_open_row(index(Action::Row));
    ui.on_toggle(index(Action::Toggle));
    ui.on_crumb(index(Action::Crumb));
    ui.on_tab(index(Action::Tab));
    ui.on_fav(index(Action::Fav));
    ui.on_fav_remove(index(Action::FavRemove));
    ui.on_recent_open(index(Action::RecentOpen));
    ui.on_type_open(index(Action::TypeOpen));
    ui.on_type_drive_picked(index(Action::TypeDrive));
    ui.on_type_sub_picked(index(Action::TypeSub));
    ui.on_hit_open(index(Action::Hit));
    ui.on_filter_type(index(Action::FilterType));
    ui.on_filter_size(index(Action::FilterSize));
    ui.on_filter_modified(index(Action::FilterModified));
    ui.on_review(index(Action::Review));
    ui.on_text_scrolled(index(Action::TextScrolled));
    ui.on_md_picked(index(Action::MdPicked));
    ui.on_task_toggled(index(Action::TaskToggled));
    ui.on_new_type_picked(index(Action::NewType));
    let pair = |a: fn(usize, usize) -> Action| {
        let q = actions.clone();
        move |x: i32, y: i32| {
            q.borrow_mut()
                .push_back(a(x.max(0) as usize, y.max(0) as usize))
        }
    };
    ui.on_text_tapped(pair(Action::TextTapped));
    ui.on_text_held(pair(Action::TextHeld));
    ui.on_cell_picked(pair(Action::CellPicked));
    {
        let q = actions.clone();
        ui.on_handle_moved(move |w, r, x| {
            q.borrow_mut()
                .push_back(Action::HandleMoved(w, r.max(0) as usize, x.max(0) as usize))
        });
    }
    ui.on_selection_action(index(Action::Pick));
    ui.on_sort_chosen(index(Action::SortKey));
    ui.on_clash_answer(index(Action::Clash));
    let text = |a: fn(String) -> Action| {
        let q = actions.clone();
        move |s: SharedString| q.borrow_mut().push_back(a(s.to_string()))
    };
    ui.on_menu(text(Action::Menu));
    ui.on_selection_menu(text(Action::PickMenu));
    ui.on_searched(text(Action::Searched));
    ui.on_code_key(text(Action::CodeKey));
    ui.on_find_edited(text(Action::FindEdited));
    let flag = |a: fn(bool) -> Action| {
        let q = actions.clone();
        move |v: bool| q.borrow_mut().push_back(a(v))
    };
    ui.on_ascending_changed(flag(Action::Ascending));
    ui.on_folders_first_changed(flag(Action::FoldersFirst));
    ui.on_search_scope(flag(Action::SearchScope));
    ui.on_details_read_only(flag(Action::DetailsReadOnly));
    ui.on_details_hidden(flag(Action::DetailsHidden));
    let plain = |a: fn() -> Action| {
        let q = actions.clone();
        move || q.borrow_mut().push_back(a())
    };
    ui.on_up(plain(|| Action::Up));
    ui.on_clear_selection(plain(|| Action::ClearSelection));
    ui.on_new_folder(plain(|| Action::NewFolder));
    ui.on_new_file(plain(|| Action::NewFile));
    ui.on_refresh(plain(|| Action::Refresh));
    ui.on_paste_cancel(plain(|| Action::PasteCancel));
    ui.on_paste_go(plain(|| Action::PasteGo));
    ui.on_job_hide(plain(|| Action::JobHide));
    ui.on_form_cancel(plain(|| Action::FormCancel));
    ui.on_form_save(plain(|| Action::FormSave));
    ui.on_favs_edit(plain(|| Action::FavsEdit));
    ui.on_see_all(plain(|| Action::SeeAll));
    ui.on_filters_reset(plain(|| Action::FiltersReset));
    ui.on_edit(plain(|| Action::Edit));
    ui.on_undo(plain(|| Action::UndoEdit));
    ui.on_save(plain(|| Action::Save));
    ui.on_wrap_toggled(plain(|| Action::WrapToggled));
    ui.on_find_next(plain(|| Action::FindStep(false)));
    ui.on_find_prev(plain(|| Action::FindStep(true)));
    ui.on_replace_one(plain(|| Action::Replace(false)));
    ui.on_replace_all(plain(|| Action::Replace(true)));
    ui.on_cell_done(plain(|| Action::CellDone));
    // a handle let go: the selection stays as it was dragged
    ui.on_handle_done(|| {});
    // FA5
    {
        let q = actions.clone();
        ui.on_img_menu_picked(move |_| q.borrow_mut().push_back(Action::ImgMenu));
    }
    ui.on_img_strip_picked(index(Action::ImgStrip));
    ui.on_img_edit_menu(index(Action::ImgEditMenu));
    ui.on_crop_ratio_picked(index(Action::CropRatio));
    ui.on_resize_chosen(index(Action::ResizeChosen));
    ui.on_img_tapped(plain(|| Action::ImgTapped));
    ui.on_img_redo(plain(|| Action::ImgRedo));
    ui.on_img_rotate(plain(|| Action::ImgRotate));
    ui.on_img_crop_mode(plain(|| Action::ImgCropMode));
    ui.on_img_resize(plain(|| Action::ImgResize));
    ui.on_img_flip(plain(|| Action::ImgFlip));
    ui.on_img_save(plain(|| Action::ImgSave));
    ui.on_crop_done(plain(|| Action::CropDone));
    let lengths = |f: fn(f32, f32) -> Action| {
        let q = actions.clone();
        move |x: f32, y: f32| q.borrow_mut().push_back(f(x, y))
    };
    ui.on_img_double_tapped(lengths(Action::ImgDouble));
    ui.on_img_dragged(lengths(Action::ImgDragged));
    ui.on_img_released(lengths(Action::ImgReleased));
    {
        let q = actions.clone();
        ui.on_crop_start(move |h| q.borrow_mut().push_back(Action::CropStart(h)));
    }
    {
        let q = actions.clone();
        ui.on_crop_drag(move |h, x, y| q.borrow_mut().push_back(Action::CropDrag(h, x, y)));
    }
    // FA6
    ui.on_extract_way(index(Action::ExtractWay));
    ui.on_extract_to(plain(|| Action::ExtractBar(true)));
    ui.on_extract_all(plain(|| Action::ExtractBar(false)));
    ui.on_job_cancel(plain(|| Action::JobCancel));
    ui.on_compress_format_picked(index(Action::CompressFormat));
    ui.on_save_in(plain(|| Action::SaveIn));
    ui.on_picker_cancel(plain(|| Action::PickerCancel));
    ui.on_picker_done(plain(|| Action::PickerDone));
    ui.on_picker_drive_picked(index(Action::PickerDrive));
    ui.on_picker_crumb(index(Action::PickerCrumb));
    ui.on_picker_open(index(Action::PickerOpen));
    ui.on_picker_new_folder(plain(|| Action::PickerNewFolder));
    fa7::bind(ui, actions);
    {
        let q = actions.clone();
        ui.on_group_picked(move |g, i| {
            q.borrow_mut()
                .push_back(Action::GroupPicked(g.max(0) as usize, i.max(0) as usize))
        });
    }
}

/// The page: Back stays in the app below the Browse tab, in a form, with a dialog, or with a kit layer open.
fn describe(app: &App, ui: &FilesApp, st: &mut State) -> Result<(), i32> {
    // search starts in the open folder (files2-search: in folder as built), all drives elsewhere
    ui.set_can_search_folder(st.view == View::Folder);
    if !ui.get_searching() {
        st.search.in_folder = st.view == View::Folder;
        ui.set_search_folder(st.search.in_folder);
    }
    let page = Page {
        title: "Files".into(),
        can_back: !matches!(st.view, View::Browse | View::Recent | View::Types)
            || st.form.is_some()
            || st.compress.is_some()
            || ui.get_clash_open()
            || ui.global::<Kit>().get_layers() > 0,
        ..Page::default()
    };
    if page != st.page {
        app.page(&page)?;
        st.page = page;
    }
    Ok(())
}

/// A field took the focus (the search header, a form's name) or lost it: ask the UI server for its keyboard, or hide
/// it (the kit's KeyboardFocus, as Contacts does).
fn sync_keyboard(app: &App, ui: &FilesApp, st: &mut State) {
    let kf = ui.global::<KeyboardFocus>();
    let want = (kf.get_open(), kf.get_seq(), kf.get_mode());
    let had = st.kb;
    if want.0 != had.0 || (want.0 && (want.1, want.2) != (had.1, had.2)) {
        st.kb = want;
        let mode = if want.0 {
            want.2.max(1)
        } else {
            proto::KEYBOARD_HIDDEN
        };
        let h = app.keyboard(mode).unwrap_or(0);
        kf.set_text_h(h as f32);
        ui.set_kb_h(h as f32);
    }
}

/// What the lists scrolled since the last frame (smooth-ui S2d), cleared for the next.
fn scrolled(ui: &FilesApp) -> ((i32, i32), Rect) {
    let sc = ui.global::<Scroll>();
    let moved = (sc.get_dy(), sc.get_lists());
    sc.set_dy(0);
    sc.set_lists(0);
    let list = Rect::new(
        sc.get_x() as i32,
        sc.get_y() as i32,
        sc.get_width() as u32,
        sc.get_height() as u32,
    );
    (moved, list)
}

// ── settings (files.*: the app's own namespace, no registry row needed) ───────────────────────────────────────────

pub(crate) fn flag(app: &App, key: &str, default: bool) -> bool {
    match app.setting_raw(key) {
        Ok(Value::Bool(b)) => b,
        _ => default,
    }
}

fn load_settings(app: &App, ui: &FilesApp, st: &mut State) {
    let key = match app.setting_raw("files.sort") {
        Ok(Value::Int(i)) => SortKey::from_index(i),
        _ => SortKey::Name,
    };
    st.sort = Sort {
        key,
        ascending: flag(app, "files.asc", true),
        folders_first: flag(app, "files.folders_first", true),
    };
    st.grid = flag(app, "files.grid", false);
    st.hidden = flag(app, "files.hidden", false);
    show_settings(ui, st);
}

fn show_settings(ui: &FilesApp, st: &State) {
    ui.set_sort_key(st.sort.key.index());
    ui.set_ascending(st.sort.ascending);
    ui.set_folders_first(st.sort.folders_first);
    ui.set_grid(st.grid);
    ui.set_show_hidden(st.hidden);
}

pub(crate) fn save(app: &App, key: &str, v: Value) {
    if let Err(e) = app.set_setting_raw(key, &v) {
        dprintln!("files: setting {} {}", key, e);
    }
}

// ── actions ──────────────────────────────────────────────────────────────────────────────────────────────────────

fn act(app: &App, ui: &FilesApp, st: &mut State, a: Action) {
    match a {
        Action::Drive(i) => {
            if let Some(&(d, _)) = st.drives.get(i) {
                open_dir(app, ui, st, format!("{d}:\\"));
            }
        }
        Action::Row(i) if st.view == View::Archive => archive::row(app, ui, st, i),
        Action::Toggle(i) if st.view == View::Archive => archive::toggle(ui, st, i),
        Action::Crumb(i) if st.view == View::Archive => archive::crumb(ui, st, i),
        Action::Pick(_) if st.view == View::Archive => archive::extract_picked(app, ui, st),
        Action::PickMenu(_) if st.view == View::Archive => archive::select_all(ui, st, true),
        Action::ClearSelection if st.view == View::Archive => archive::select_all(ui, st, false),
        Action::Menu(m) if st.view == View::Archive => match m.as_str() {
            "Select all" => archive::select_all(ui, st, true),
            _ => archive::details(app, ui, st),
        },
        Action::Searched(t) if st.view == View::Archive => archive::searched(ui, st, &t),
        Action::FormCancel if st.form.is_none() && st.compress.is_some() => {
            archive::compress_cancel(ui, st)
        }
        Action::FormSave if st.form.is_none() && st.compress.is_some() => {
            archive::compress_save(app, ui, st)
        }
        Action::ExtractWay(i) => archive::way(app, ui, st, i),
        Action::ExtractBar(to) => archive::bar(app, ui, st, to),
        Action::JobCancel => st.slot.cancel(),
        Action::CompressFormat(i) => archive::compress_format(ui, st, i),
        Action::SaveIn => archive::save_in(app, ui, st),
        Action::PickerCancel => archive::picker_cancel(app, ui, st),
        Action::PickerDone => archive::picker_done(app, ui, st),
        Action::PickerDrive(i) => archive::picker_drive(app, ui, st, i),
        Action::PickerCrumb(i) => archive::picker_crumb(app, ui, st, i),
        Action::PickerOpen(i) => archive::picker_open(app, ui, st, i),
        Action::PickerNewFolder => archive::picker_new_folder(app, ui, st),
        Action::Fa7(a) => fa7::act(app, ui, st, a),
        Action::Edit if st.view == View::Fa7 => fa7::edit(ui, st),
        Action::Row(i) if st.view == View::Folder => {
            let Some(item) = st.items.get(i).cloned() else {
                return;
            };
            if item.is_dir {
                open_dir(app, ui, st, model::join_dir(&st.dir, &item.name));
            } else {
                view_file(app, ui, st, &item);
            }
        }
        Action::Row(i) => {
            // the bin's rows are in the bin: they open after Restore
            if st.view != View::Bin
                && let Some(l) = st.listed.get(i)
            {
                let p = l.path.clone();
                open_path(app, ui, st, &p);
            }
        }
        Action::Toggle(i) => {
            if let Some(c) = st.checked.get_mut(i) {
                *c = !*c;
                show_any(ui, st);
            }
        }
        Action::Crumb(i) => {
            let crumbs = model::crumbs(&st.dir);
            if let Some((_, dir)) = crumbs.get(i)
                && (st.view == View::Text || i + 1 < crumbs.len())
            {
                open_dir(app, ui, st, dir.clone());
            }
        }
        Action::Up => back(app, ui, st),
        Action::Tab(i) => show_tab(app, ui, st, i),
        Action::Menu(m) => menu(app, ui, st, &m),
        Action::Pick(i) if st.view == View::Bin => {
            if i == 0 {
                fa3::restore(app, ui, st);
            } else {
                fa3::ask_purge(app, st);
            }
        }
        Action::Pick(_) if st.view == View::Review => {
            let paths = fa3::picked_paths(st);
            ask_trash_paths(app, st, paths);
        }
        Action::Pick(0) => pick_clip(ui, st, Kind::Copy),
        Action::Pick(_) => ask_trash(app, st),
        Action::PickMenu(m) => match m.as_str() {
            "Move" => pick_clip(ui, st, Kind::Move),
            "Rename" => start_rename(app, ui, st),
            "Compress" => {
                let paths = picked(st).iter().map(|i| path_of(st, i)).collect();
                clear_selection(ui, st);
                archive::compress_open(app, ui, st, paths);
            }
            "Add to Favourites" => {
                if let [one] = picked(st).as_slice()
                    && one.is_dir
                {
                    let dir = model::join_dir(&st.dir, &one.name);
                    clear_selection(ui, st);
                    fa3::add_fav(app, st, dir);
                }
            }
            "Details" => {
                if let [one] = picked(st).as_slice() {
                    let p = path_of(st, one);
                    fa3::details(app, ui, st, &p);
                }
            }
            "Select all" => {
                st.checked.iter_mut().for_each(|c| *c = true);
                show_any(ui, st);
            }
            _ => {}
        },
        Action::ClearSelection => {
            ui.set_picking(false);
            clear_selection(ui, st);
        }
        Action::NewFolder => open_form(app, ui, st, Form::NewFolder),
        Action::NewFile => open_form(app, ui, st, Form::NewFile),
        Action::Refresh => relist(app, ui, st),
        Action::SortKey(i) => {
            st.sort.key = SortKey::from_index(i as i32);
            save(app, "files.sort", Value::Int(st.sort.key.index()));
            ui.set_sort_open(false);
            resort(ui, st);
        }
        Action::Ascending(v) => {
            st.sort.ascending = v;
            save(app, "files.asc", Value::Bool(v));
            resort(ui, st);
        }
        Action::FoldersFirst(v) => {
            st.sort.folders_first = v;
            save(app, "files.folders_first", Value::Bool(v));
            resort(ui, st);
        }
        Action::PasteCancel => {
            st.clip = None;
            show_clip(ui, st);
        }
        Action::PasteGo => paste(app, ui, st),
        Action::JobHide => {
            if let Some(r) = st.running.as_mut() {
                r.hidden = true;
                ui.set_job_text(SharedString::new());
            }
        }
        Action::Clash(i) => {
            let choice = [Choice::Replace, Choice::KeepBoth, Choice::Skip];
            ui.set_clash_open(false);
            if let Some(r) = st.running.as_mut() {
                r.asked = false;
            }
            st.slot.answer(choice[i.min(2)], ui.get_clash_all());
        }
        Action::FormCancel => cancel_form(app, ui, st),
        Action::FormSave => save_form(app, ui, st),
        Action::Fav(i) => {
            if let Some(d) = fa3::fav_path(st, i) {
                open_dir(app, ui, st, d);
            }
        }
        Action::FavRemove(i) => fa3::remove_fav(ui, st, i),
        Action::FavsEdit => fa3::show_favs(ui, st),
        Action::RecentOpen(i) => {
            if let Some(p) = fa3::recent_path(st, i) {
                open_path(app, ui, st, &p);
            }
        }
        Action::SeeAll => show_tab(app, ui, st, 1),
        Action::TypeOpen(i) => {
            st.group = i;
            st.type_drive = 0;
            st.type_sub = 0;
            fa3::show_type(ui, st);
        }
        Action::TypeDrive(i) => {
            st.type_drive = i;
            fa3::show_type(ui, st);
        }
        Action::TypeSub(i) => {
            st.type_sub = i;
            fa3::show_type(ui, st);
        }
        Action::Searched(t) => {
            st.search.text = t;
            fa3::search(ui, st);
        }
        Action::SearchScope(f) => {
            st.search.in_folder = f;
            fa3::search(ui, st);
        }
        Action::Hit(i) => {
            if let Some((p, _)) = st.search.hits.get(i).cloned() {
                ui.set_searching(false);
                ui.set_query(SharedString::new());
                st.search.text.clear();
                open_path(app, ui, st, &p);
            }
        }
        Action::FilterType(i) => {
            st.search.kind = i;
            fa3::search(ui, st);
        }
        Action::FilterSize(i) => {
            st.search.size = i;
            fa3::search(ui, st);
        }
        Action::FilterModified(i) => {
            st.search.modified = i;
            fa3::search(ui, st);
        }
        Action::FiltersReset => {
            fa3::reset_filters(st);
            fa3::search(ui, st);
        }
        Action::Review(i) => match i {
            0 => fa3::show_review(ui, st, fa3::Review::Large),
            1 => fa3::show_review(ui, st, fa3::Review::Old),
            2 => fa3::show_review(ui, st, fa3::Review::Dupes),
            _ => {
                st.bin_from = View::Cleanup;
                fa3::show_bin(ui, st);
            }
        },
        Action::DetailsReadOnly(on) => fa3::set_attribute(app, ui, st, neo_fs::att::READ_ONLY, on),
        Action::DetailsHidden(on) => fa3::set_attribute(app, ui, st, neo_fs::att::HIDDEN, on),
        Action::TextScrolled(r) => {
            if let Some(o) = st.open.as_mut() {
                o.scrolled(ui, r);
            }
        }
        Action::TextTapped(r, x) => {
            if let Some(o) = st.open.as_mut()
                && st.view == View::Edit
            {
                // a tap in the text leaves the find bar (its field had the keys)
                if ui.get_find_open() {
                    ui.set_find_open(false);
                    o.find(ui, "");
                }
                o.tapped(ui, r, x);
                editor_keyboard(ui);
            }
        }
        Action::TextHeld(r, x) => {
            if let Some(o) = st.open.as_mut()
                && st.view == View::Edit
            {
                o.held(ui, r, x);
            }
        }
        Action::HandleMoved(w, r, x) => {
            if let Some(o) = st.open.as_mut() {
                o.handle_moved(ui, w, r, x);
            }
        }
        Action::Edit if st.view == View::Image => image::edit(ui, st),
        Action::Edit => {
            if let Some(o) = st.open.take() {
                open_text(app, ui, st, &o.path, true);
            }
        }
        Action::UndoEdit if st.view == View::ImageEdit => image::undo(ui, st, false),
        Action::UndoEdit => {
            if let Some(o) = st.open.as_mut() {
                o.undo(ui);
            }
        }
        Action::Save => save_open(app, ui, st),
        Action::CodeKey(k) => {
            if let Some(o) = st.open.as_mut() {
                o.code_key(ui, &k);
            }
        }
        Action::WrapToggled => {
            if let Some(o) = st.open.as_mut() {
                o.toggle_wrap(ui);
                save(app, "files.wrap", Value::Bool(o.wrap));
            }
        }
        Action::FindEdited(t) => {
            if let Some(o) = st.open.as_mut() {
                o.find(ui, &t);
            }
        }
        Action::FindStep(back) => {
            if let Some(o) = st.open.as_mut() {
                o.find_step(ui, back);
            }
        }
        Action::Replace(all) => {
            if let Some(o) = st.open.as_mut() {
                o.replace(
                    ui,
                    all,
                    ui.get_replace_text().as_str(),
                    ui.get_find_text().as_str(),
                );
            }
        }
        Action::MdPicked(i) => {
            if let Some(o) = st.open.as_mut() {
                o.preview = i == 1;
                o.show(ui);
                if o.preview {
                    ui.global::<KeyboardFocus>().invoke_close();
                } else {
                    editor_keyboard(ui);
                }
            }
        }
        Action::TaskToggled(i) => {
            if let Some(o) = st.open.as_mut() {
                o.toggle_task(ui, i);
                o.show(ui);
            }
        }
        Action::CellPicked(r, c) => {
            if let Some(o) = st.open.as_mut() {
                o.cell_picked(ui, r, c);
                // the cell bar sits on the keyboard
                editor_keyboard(ui);
            }
        }
        Action::CellDone => {
            if let Some(o) = st.open.as_mut() {
                o.cell_done(ui);
                ui.global::<KeyboardFocus>().invoke_close();
            }
        }
        Action::NewType(i) => {
            st.new_type = i.min(4);
            ui.set_new_type(st.new_type as i32);
            ui.set_name_suffix([".txt", ".md", ".csv", ".json", ""][st.new_type].into());
        }
        Action::ImgMenu => {
            let p = st.file.clone();
            fa3::details(app, ui, st, &p);
        }
        Action::ImgStrip(i) => image::strip_picked(ui, st, i),
        Action::ImgTapped => image::tapped(ui, st),
        Action::ImgDouble(x, y) => image::double_tapped(app, ui, st, x, y),
        Action::ImgDragged(x, y) => image::dragged(app, ui, st, x, y),
        Action::ImgReleased(x, y) => image::released(app, ui, st, x, y),
        Action::ImgEditMenu(0) => image::reset(ui, st),
        Action::ImgEditMenu(_) => {
            let p = st.file.clone();
            fa3::details(app, ui, st, &p);
        }
        Action::ImgRedo => image::undo(ui, st, true),
        Action::ImgRotate => image::rotate(ui, st, false),
        Action::ImgFlip => image::rotate(ui, st, true),
        Action::ImgCropMode => image::crop_mode(ui, st),
        Action::ImgResize => ui.set_resize_open(true),
        Action::ResizeChosen(i) => image::resize_chosen(ui, st, i),
        Action::ImgSave => image::save(app, ui, st),
        Action::CropRatio(i) => image::ratio_picked(ui, st, i),
        Action::CropStart(h) => image::crop_start(st, h),
        Action::CropDrag(h, x, y) => image::crop_drag(ui, st, h, x, y),
        Action::CropDone => image::crop_done(ui, st),
        Action::GroupPicked(g, i) => {
            let Some((dir, idx)) = st.img_groups.get(g).cloned() else {
                return;
            };
            let more = ui
                .get_img_groups()
                .row_data(g)
                .is_some_and(|r| !r.more.is_empty());
            if more && i + 1 == idx.len() {
                open_dir(app, ui, st, dir);
            } else if let Some(p) = idx
                .get(i)
                .and_then(|&k| st.listed.get(k))
                .map(|l| l.path.clone())
            {
                image::open(app, ui, st, &p);
            }
        }
    }
}

/// Thumbnails follow the rows: refitted when the rows, the scroll (by a row) or the viewer's image change.
fn thumbs_tick(app: &App, ui: &FilesApp, st: &mut State) {
    let top = if st.view == View::Folder {
        ui.get_folder_top()
    } else {
        ui.get_list_top()
    };
    let row = 64.0 * app.dp;
    let mark = (
        st.view as u8,
        (top / row) as i32,
        ui.get_rows().row_count() + st.dir.len() * 1000 + st.listed.len() * 7,
        st.img_groups.len(),
        st.viewer.as_ref().map_or(0, |v| v.at),
    );
    if mark != st.thumb_mark {
        st.thumb_mark = mark;
        image::refresh(app, ui, st);
    }
}

/// A tab of the footer, at its root.
fn show_tab(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    st.tab = i.min(2);
    ui.set_tab_index(st.tab as i32);
    match st.tab {
        1 => fa3::show_recent(ui, st),
        2 => fa3::show_types(ui, st),
        _ => show_drives(app, ui, st),
    }
}

/// A file opens in the viewer, a folder (ends with `\`) as a folder.
fn open_path(app: &App, ui: &FilesApp, st: &mut State, p: &str) {
    if p.ends_with('\\') {
        return open_dir(app, ui, st, p.to_string());
    }
    let Some(dir) = neo_fs::path::parent(p) else {
        return;
    };
    let name = neo_fs::path::name_of(p).to_string();
    if image::is_image(&name) {
        return image::open(app, ui, st, p);
    }
    if archive::is_archive(&name) {
        return archive::sheet(app, ui, st, p);
    }
    if !is_text(&name) {
        // FA7: cards, events, fonts, certificates, packages; anything else as bytes
        return fa7::open(app, ui, st, p);
    }
    st.dir = dir;
    view_file(
        app,
        ui,
        st,
        &Item {
            name,
            ..Item::default()
        },
    );
}

/// The rows of whichever list is showing.
fn show_any(ui: &FilesApp, st: &State) {
    if st.view == View::Folder {
        show_rows(ui, st);
    } else {
        fa3::show_listed(ui, st);
    }
}

/// Walk the drives again on the worker (now, or when the running walk ends); `dupes` also looks for duplicates.
fn rescan(st: &mut State, dupes: bool) {
    // once Free up space has asked, every walk looks for duplicates (a walk without them would drop them)
    st.want_dupes |= dupes;
    let dupes = st.want_dupes;
    if st.scanner.running() {
        st.scan_again = Some(st.scan_again.unwrap_or(false) || dupes);
        return;
    }
    let drives = st.drives.clone();
    if let Err(e) = st.scanner.start(drives, st.hidden, dupes) {
        dprintln!("files: index failed {}", e);
    }
}

/// A finished walk: the new index replaces the old, and the screen that shows it is drawn again.
fn poll_scan(app: &App, ui: &FilesApp, st: &mut State) {
    let (index, dupes, ms) = {
        let mut f = st.scanner.found.lock();
        if f.running {
            return;
        }
        (f.index.take(), f.dupes.take(), f.ms)
    };
    let Some(index) = index else {
        if let Some(d) = st.scan_again.take() {
            rescan(st, d);
        }
        return;
    };
    dprintln!("files: index {} entries in {} ms", index.items.len(), ms);
    st.index = index;
    // groups point into the index they were found in; a later walk without them drops them
    st.dupes = dupes;
    match st.view {
        View::Types => fa3::show_types(ui, st),
        View::Type => fa3::show_type(ui, st),
        View::Cleanup => fa3::show_cleanup(ui, st),
        _ => {}
    }
    if ui.get_searching() {
        fa3::search(ui, st);
    }
    if let Some(d) = st.scan_again.take() {
        rescan(st, d);
    }
    let _ = app;
}

/// The ⋮ menus (files2-folder-menu, files2-spec-headers).
fn menu(app: &App, ui: &FilesApp, st: &mut State, m: &str) {
    match m {
        "New folder" => open_form(app, ui, st, Form::NewFolder),
        "New file" => open_form(app, ui, st, Form::NewFile),
        "Sort by" => ui.set_sort_open(true),
        "Grid view" | "List view" => {
            st.grid = m == "Grid view";
            save(app, "files.grid", Value::Bool(st.grid));
            ui.set_grid(st.grid);
        }
        "Recycle bin" => {
            st.bin_from = st.view;
            fa3::show_bin(ui, st);
        }
        "Free up space" => {
            fa3::show_cleanup(ui, st);
            if st.dupes.is_none() {
                rescan(st, true);
            }
        }
        "Details" => {
            let target = match (&st.open, st.view) {
                (Some(o), View::Text | View::Edit) => o.path.clone(),
                _ => st.dir.clone(),
            };
            fa3::details(app, ui, st, &target);
        }
        "Find and replace" => {
            ui.set_find_open(true);
        }
        "Wrap lines" | "No wrap" => act(app, ui, st, Action::WrapToggled),
        "Add row" | "Add column" | "Delete row" => {
            if let Some(o) = st.open.as_mut() {
                o.table_menu(ui, m);
            }
        }
        "Empty bin" => fa3::ask_empty(app, st),
        "Select" => ui.set_picking(true),
        "Select all" => {
            st.checked.iter_mut().for_each(|c| *c = true);
            show_any(ui, st);
        }
        "Show hidden" | "Hide hidden" | "Show hidden files" | "Hide hidden files" => {
            st.hidden = m.starts_with("Show");
            save(app, "files.hidden", Value::Bool(st.hidden));
            ui.set_show_hidden(st.hidden);
            if st.view == View::Folder {
                relist(app, ui, st);
            }
        }
        _ => {}
    }
}

/// Back: the clash dialog (Skip), the form (asking when a name was typed), the viewer, then up a level; the Browse
/// tab doesn't keep Back, so the shell leaves the app. The kit's layers (⋮, the Sort sheet, selection) went first
/// ([`neoapp::kit_back!`]). The paste bar stays: it waits for a folder while the user moves about.
fn back(app: &App, ui: &FilesApp, st: &mut State) {
    if ui.get_clash_open() {
        act(app, ui, st, Action::Clash(2));
    } else if st.form.is_some() {
        cancel_form(app, ui, st);
    } else if st.view == View::PickFolder {
        archive::picker_cancel(app, ui, st);
    } else if st.compress.is_some() {
        archive::compress_cancel(ui, st);
    } else if st.view == View::Fa7 {
        fa7::back(app, ui, st);
    } else if st.view == View::Archive {
        archive::up(app, ui, st);
    } else if st.view == View::Edit {
        if ui.get_find_open() {
            ui.set_find_open(false);
            if let Some(o) = st.open.as_mut() {
                o.find(ui, "");
            }
        } else if let Some(o) = st.open.as_ref().filter(|o| o.dirty()) {
            let text = format!(
                "Save changes to {}? You edited {} line{}. Discard drops them; the file stays as it was.",
                o.name,
                o.edited(),
                if o.edited() == 1 { "" } else { "s" }
            );
            let q = Query {
                text: cut(&text, neoui_proto::page::TEXT_MAX),
                buttons: alloc::vec!["Save".into(), "Discard".into(), "Cancel".into()],
                destructive: false,
            };
            if app.query(&q).is_ok() {
                st.asking = Some(Ask::Unsaved);
            }
        } else {
            leave_editor(app, ui, st);
        }
    } else if st.view == View::ImageEdit {
        if image::edited(st) {
            let q = Query {
                text:
                    "Save a copy with your edits? Discard drops them; the picture stays as it was."
                        .into(),
                buttons: alloc::vec!["Save copy".into(), "Discard".into(), "Cancel".into()],
                destructive: false,
            };
            if app.query(&q).is_ok() {
                st.asking = Some(Ask::ImageEdits);
            }
        } else {
            image::leave_editor(ui, st);
        }
    } else if st.view == View::Image {
        let from = st.viewer.take().map_or(View::Folder, |v| v.from);
        st.text_from = from;
        return_to(app, ui, st, from);
    } else if st.view == View::Text {
        let from = st.text_from;
        return_to(app, ui, st, from);
    } else if st.view == View::Review {
        fa3::show_cleanup(ui, st);
    } else if st.view == View::Bin && st.bin_from == View::Cleanup {
        st.bin_from = View::Browse;
        fa3::show_cleanup(ui, st);
    } else if st.view == View::Folder
        && let Some(up) = model::parent(&st.dir)
    {
        open_dir(app, ui, st, up);
    } else {
        // Back order 6: the tab's root
        show_tab(app, ui, st, st.tab);
    }
}

/// Back from a viewer to the page it was opened from.
fn return_to(app: &App, ui: &FilesApp, st: &mut State, from: View) {
    match from {
        View::Recent => fa3::show_recent(ui, st),
        View::Type => fa3::show_type(ui, st),
        View::Review => {
            let k = st.review;
            fa3::show_review(ui, st, k);
        }
        View::Browse | View::Types => show_tab(app, ui, st, st.tab),
        View::Archive if st.archive.is_some() => {
            st.view = View::Archive;
            archive::show(ui, st);
        }
        _ => {
            let dir = st.dir.clone();
            open_dir(app, ui, st, dir);
        }
    }
}

fn picked(st: &State) -> Vec<Item> {
    st.items
        .iter()
        .zip(&st.checked)
        .filter(|(_, c)| **c)
        .map(|(i, _)| i.clone())
        .collect()
}

fn path_of(st: &State, item: &Item) -> String {
    if item.is_dir {
        model::join_dir(&st.dir, &item.name)
    } else {
        model::join_file(&st.dir, &item.name)
    }
}

fn clear_selection(ui: &FilesApp, st: &mut State) {
    st.checked.iter_mut().for_each(|c| *c = false);
    show_any(ui, st);
}

fn ask(app: &App, st: &mut State, text: String, yes: &str, what: Ask) {
    let q = Query {
        text: cut(&text, neoui_proto::page::TEXT_MAX),
        buttons: alloc::vec![yes.into(), "Cancel".into()],
        destructive: true,
    };
    if app.query(&q).is_ok() {
        st.asking = Some(what);
    }
}

/// The shell's dialog was answered (`yes`: its first button). Back answers the last, Cancel.
fn answer(app: &App, ui: &FilesApp, st: &mut State, i: usize) {
    let yes = i == 0;
    match st.asking.take() {
        Some(Ask::Unsaved) => match i {
            0 => {
                save_open(app, ui, st);
                if !st.open.as_ref().is_some_and(edit::Open::dirty) {
                    leave_editor(app, ui, st);
                }
            }
            1 => {
                if let Some(o) = &st.open {
                    dprintln!("files: discard {}", o.path);
                }
                leave_editor(app, ui, st);
            }
            _ => {}
        },
        Some(Ask::Fa7Unsaved) => fa7::answer(app, ui, st, i),
        Some(Ask::ImageEdits) => match i {
            0 => image::save(app, ui, st),
            1 => image::leave_editor(ui, st),
            _ => {}
        },
        Some(Ask::Discard) if yes => close_form(app, ui, st),
        Some(Ask::Purge(items)) if yes => {
            ui.set_picking(false);
            start_purge(app, ui, st, items, false);
        }
        Some(Ask::Trash(paths)) if yes => {
            clear_selection(ui, st);
            let r = trash::trash(&*st.fs, &paths, now());
            dprintln!(
                "files: trash {} {}",
                paths.first().map_or("", String::as_str),
                r.as_ref().err().map_or(0, |e| e.code())
            );
            if st.view == View::Review {
                // the files are gone from the index too: walk again, then show the review without them
                if r.is_ok() {
                    st.listed.retain(|l| !paths.contains(&l.path));
                }
                st.checked = alloc::vec![false; st.listed.len()];
                fa3::show_listed(ui, st);
            } else {
                relist(app, ui, st);
            }
            rescan(st, st.dupes.is_some());
            match r {
                Ok(batch) => {
                    let n = batch.items.len();
                    st.undo = Some(batch);
                    let text = if n == 1 {
                        "1 item moved to Recycle bin".to_string()
                    } else {
                        format!("{n} items moved to Recycle bin")
                    };
                    note_with(app, &text, "Undo");
                }
                Err(e) => toast(app, "", e.code()),
            }
        }
        _ => {}
    }
}

/// Delete from a Free up space review: into the bin, after the shell's dialog.
fn ask_trash_paths(app: &App, st: &mut State, paths: Vec<String>) {
    let text = match paths.len() {
        0 => return,
        1 => format!("Move {} to Recycle bin?", neo_fs::path::name_of(&paths[0])),
        n => format!("Move {n} items to Recycle bin?"),
    };
    ask(app, st, text, "Move to bin", Ask::Trash(paths));
}

/// Delete (files2-delete): into the drive's Recycle bin, after the shell's dialog.
fn ask_trash(app: &App, st: &mut State) {
    let picked = picked(st);
    if picked.is_empty() || st.read_only {
        return;
    }
    let drive = st.dir.chars().next().unwrap_or('C');
    // one line for one item (the shell's dialog keeps its size); the names and size for more (files2-delete)
    let text = match picked.as_slice() {
        [] => return,
        [one] => format!("Move {} to Recycle bin?", one.name),
        [a, b, rest @ ..] => {
            let names = match rest.len() {
                0 => format!("{} and {}", a.name, b.name),
                n => format!("{}, {} and {n} more", a.name, b.name),
            };
            let bytes: u64 = picked.iter().map(|i| i.size).sum();
            format!(
                "Move {} items to Recycle bin?\n{names} ({}). Kept in {drive}:\\.trash\\.",
                picked.len(),
                model::size_text(bytes)
            )
        }
    };
    let paths = picked.iter().map(|i| path_of(st, i)).collect();
    ask(app, st, text, "Move to bin", Ask::Trash(paths));
}

/// The toast's Undo: the last trash goes back.
fn undo(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(batch) = st.undo.take() else {
        return;
    };
    let r = trash::undo(&*st.fs, &batch);
    dprintln!(
        "files: undo {} {}",
        batch.items.len(),
        r.as_ref().err().map_or(0, |e| e.code())
    );
    relist(app, ui, st);
    rescan(st, st.dupes.is_some());
    if let Err(e) = r {
        toast(app, "", e.code());
    }
}

/// Copy (the header) or Move (⋮): the picked rows wait in the paste bar (files2-paste-bar).
fn pick_clip(ui: &FilesApp, st: &mut State, kind: Kind) {
    let picked = picked(st);
    if picked.is_empty() || (kind == Kind::Move && st.read_only) {
        return;
    }
    st.clip = Some(Clip {
        kind,
        paths: picked.iter().map(|i| path_of(st, i)).collect(),
        from: st.dir.clone(),
    });
    clear_selection(ui, st);
    show_clip(ui, st);
}

fn show_clip(ui: &FilesApp, st: &State) {
    let Some(c) = &st.clip else {
        ui.set_paste_action(SharedString::new());
        return;
    };
    let n = c.paths.len();
    let what = if n == 1 {
        "1 item".to_string()
    } else {
        format!("{n} items")
    };
    let (verb, action) = match c.kind {
        Kind::Move => ("to move", "Move here"),
        Kind::Copy => ("to copy", "Paste here"),
    };
    ui.set_paste_title(format!("{what} {verb}").into());
    ui.set_paste_from(format!("from {}", c.from.trim_end_matches('\\')).into());
    ui.set_paste_action(action.into());
}

/// Move here / Paste here: the job starts on the worker.
fn paste(app: &App, ui: &FilesApp, st: &mut State) {
    if st.running.is_some() || st.view != View::Folder || st.read_only {
        return;
    }
    let Some(clip) = st.clip.take() else {
        return;
    };
    show_clip(ui, st);
    let dest = st.dir.clone();
    dprintln!(
        "files: {} {} items to {}",
        verb(clip.kind),
        clip.paths.len(),
        dest
    );
    if let Err(e) = st
        .slot
        .start(job::Work::Transfer(clip.kind, clip.paths, dest.clone()))
    {
        return toast(app, "", e);
    }
    st.running = Some(Running {
        kind: clip.kind,
        purge: false,
        quiet: false,
        dest,
        hidden: false,
        asked: false,
        noted_at: 0,
        arc: None,
        started: time::millis(),
    });
    ui.set_job_text(format!("{} …", title_verb(clip.kind)).into());
    ui.set_job_progress(0.0);
}

/// Delete Recycle bin items for good on the worker (`quiet`: the 30-day clean-up, no toast).
fn start_purge(app: &App, ui: &FilesApp, st: &mut State, items: Vec<String>, quiet: bool) {
    if st.running.is_some() {
        if !quiet {
            note_with(app, "Wait for the copy to finish", "");
        }
        return;
    }
    dprintln!("files: purge {} items", items.len());
    // out of the bin's journal here, on the UI thread (the only one that writes it); the worker deletes what that
    // leaves (`.~del`), one item at a time so a failure part-way still hands over what was taken out
    let mut doomed = Vec::new();
    for item in &items {
        match trash::purge(&*st.fs, core::slice::from_ref(item)) {
            Ok(d) => doomed.extend(d),
            Err(e) => {
                dprintln!("files: purge {} {}", item, e.code());
                if !quiet {
                    toast(app, "", e.code());
                }
                break;
            }
        }
    }
    if doomed.is_empty() {
        return;
    }
    let n = doomed.len();
    if let Err(e) = st.slot.start(job::Work::Purge(doomed, n)) {
        return toast(app, "", e);
    }
    st.running = Some(Running {
        kind: Kind::Move,
        purge: true,
        quiet,
        dest: String::new(),
        hidden: quiet,
        asked: false,
        noted_at: 0,
        arc: None,
        started: time::millis(),
    });
    if !quiet {
        ui.set_job_text("Deleting …".into());
        ui.set_job_progress(0.0);
    }
}

fn verb(kind: Kind) -> &'static str {
    match kind {
        Kind::Copy => "copy",
        Kind::Move => "move",
    }
}

fn title_verb(kind: Kind) -> &'static str {
    match kind {
        Kind::Copy => "Copying",
        Kind::Move => "Moving",
    }
}

/// While a job runs (every [`JOB_POLL_US`]): its toast, its shade entry (with a bar, protocol 1.11), a clash to ask,
/// and its end.
fn poll_job(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(r) = st.running.as_mut() else {
        return;
    };
    let (progress, clash, done) = {
        let mut s = st.slot.shared.lock();
        (s.progress.clone(), s.clash.clone(), s.done.take())
    };
    if let Some(done) = done {
        let kind = r.kind;
        let dest = r.dest.clone();
        let (purge, quiet) = (r.purge, r.quiet);
        let arc = r.arc.take();
        st.running = None;
        ui.set_job_text(SharedString::new());
        ui.set_clash_open(false);
        let _ = app.cancel_notify(JOB_NOTE);
        if let Some(a) = arc {
            if matches!(
                a,
                archive::ArcJob::Extract { .. } | archive::ArcJob::Compress { .. }
            ) {
                rescan(st, st.dupes.is_some());
            }
            return archive::done(app, ui, st, a, done);
        }
        if purge {
            let n = done.as_ref().map_or(0, |s| s.copied);
            dprintln!(
                "files: purge {} {}",
                n,
                done.as_ref().err().map_or(0, |e| e.code())
            );
            match done {
                Ok(_) if !quiet => note_with(
                    app,
                    &if n == 1 {
                        "1 item deleted for good".to_string()
                    } else {
                        format!("{n} items deleted for good")
                    },
                    "",
                ),
                Err(e) if e != neo_fs::Error::Cancelled => toast(app, "", e.code()),
                _ => {}
            }
            match st.view {
                View::Bin => fa3::show_bin(ui, st),
                View::Cleanup => fa3::show_cleanup(ui, st),
                _ => {}
            }
            return;
        }
        rescan(st, st.dupes.is_some());
        match done {
            Ok(s) => {
                dprintln!(
                    "files: {} {} files {} bytes to {} done",
                    verb(kind),
                    s.copied,
                    s.bytes,
                    dest
                );
                let what = if s.copied == 1 {
                    "1 file".to_string()
                } else {
                    format!("{} files", s.copied)
                };
                let skipped = if s.skipped > 0 {
                    format!(", {} skipped", s.skipped)
                } else {
                    String::new()
                };
                let did = match kind {
                    Kind::Copy => "copied",
                    Kind::Move => "moved",
                };
                note_with(app, &format!("{what} {did}{skipped}"), "");
            }
            Err(e) => {
                dprintln!("files: {} to {} failed {}", verb(kind), dest, e.code());
                if e != neo_fs::Error::Cancelled {
                    toast(app, "", e.code());
                }
            }
        }
        relist(app, ui, st);
        return;
    }
    if let Some(c) = clash
        && !r.asked
    {
        r.asked = true;
        ui.set_clash_title(format!("{} is already here", c.name).into());
        let here = format!(
            "The one here: {}, {}.",
            model::size_text(c.here.size),
            model::day_text(c.here.modified, now())
        );
        let new = format!(
            "The new one: {}, {}.",
            model::size_text(c.new.size),
            model::day_text(c.new.modified, now())
        );
        ui.set_clash_text(format!("{here}\n{new}").into());
        ui.set_clash_count(c.left as i32);
        ui.set_clash_all(c.left > 1);
        ui.set_clash_open(true);
    }
    let Some(p) = progress else {
        return;
    };
    let pct = p
        .bytes_done
        .saturating_mul(100)
        .checked_div(p.bytes_total)
        .map_or(0, |v| v.min(100) as u8);
    if r.purge {
        if !r.hidden {
            ui.set_job_text(format!("Deleting · {} removed", p.files_done).into());
        }
        return;
    }
    let verb = r.arc.as_ref().map_or(title_verb(r.kind), |a| a.verb());
    let arc_text = r
        .arc
        .as_ref()
        .map(|a| archive::progress_text(a, &p, r.started));
    let now = time::millis();
    let due = now.saturating_sub(r.noted_at) >= NOTIFY_MS;
    // an archive job's sheet and toast change once a second: redrawn on every poll, the sheet over the whole
    // surface cost a 1 GB extract 396 s instead of 70 s on the Pixel
    if arc_text.is_some() && !due {
        return;
    }
    if let Some(t) = &arc_text {
        // the extract sheet's card
        ui.set_job_title(format!("{verb}…").into());
        ui.set_job_detail(t.as_str().into());
        ui.set_job_progress(f32::from(pct) / 100.0);
    }
    if !r.hidden {
        let files = match p.files_total {
            0 => String::new(),
            1 => " 1 file".to_string(),
            n => format!(" {n} files"),
        };
        ui.set_job_text(format!("{verb}{files} · {pct} %").into());
        ui.set_job_progress(f32::from(pct) / 100.0);
    }
    if due {
        r.noted_at = now;
        let n = Notification {
            id: JOB_NOTE,
            title: cut(
                &if arc_text.is_some() {
                    format!("Files · {verb}")
                } else {
                    format!(
                        "Files · {verb} file {} of {}",
                        (p.files_done + 1).min(p.files_total),
                        p.files_total
                    )
                },
                proto::TITLE_MAX,
            ),
            text: cut(
                &match &arc_text {
                    Some(t) => format!("{t} · {pct} %"),
                    None => format!(
                        "{} · {} of {} · {pct} %",
                        p.current,
                        model::size_text(p.current_done),
                        model::size_text(p.current_size)
                    ),
                },
                neoui_proto::page::TEXT_MAX,
            ),
            actions: alloc::vec!["Cancel".into()],
            progress: Some(pct),
            ..Notification::default()
        };
        if let Err(e) = app.notify(&n) {
            dprintln!("files: notify {}", e);
        }
    }
}

// ── listing ──────────────────────────────────────────────────────────────────────────────────────────────────────

fn show_drives(app: &App, ui: &FilesApp, st: &mut State) {
    let list = match st.fs.drives() {
        Ok(l) => l,
        Err(e) => return toast(app, "drives", e.code()),
    };
    let mut rows = Vec::new();
    st.drives.clear();
    let mut log = String::new();
    for d in &list {
        let att = if d.read_only {
            0x02
        } else if d.removable {
            0x20
        } else {
            0
        };
        let label = model::drive_label(d.letter, att, 0);
        let kind = files_ui::drive_kind(d.letter, d.read_only, d.removable, 0);
        rows.push(files_ui::drive_row(d.letter, label, kind, d.size, d.free));
        st.drives.push((d.letter, d.read_only));
        log.push_str(&format!(" {}({}/{})", d.letter, d.free, d.size));
    }
    dprintln!("files: drives{}", log);
    ui.set_drives(ModelRc::from(Rc::new(VecModel::from(rows))));
    fa3::browse_extras(ui, st);
    st.tab = 0;
    ui.set_tab_index(0);
    st.view = View::Browse;
    st.dir.clear();
    ui.set_view("browse".into());
}

fn relist(app: &App, ui: &FilesApp, st: &mut State) {
    if !st.dir.is_empty() && matches!(st.view, View::Folder | View::Text) {
        let (dir, view) = (st.dir.clone(), st.view);
        open_dir(app, ui, st, dir);
        if view == View::Text {
            // the viewer stays; only the folder under it was listed again
            st.view = View::Text;
            ui.set_view("text".into());
        }
    }
}

fn open_dir(app: &App, ui: &FilesApp, st: &mut State, dir: String) {
    ui.set_refreshing(false);
    let entries = match st.fs.read_dir(&dir) {
        Ok(e) => e,
        Err(e) => {
            dprintln!("files: list {} {}", dir, e.code());
            return toast(app, "", e.code());
        }
    };
    let mut items: Vec<Item> = entries
        .into_iter()
        .map(|e| Item {
            is_dir: e.is_dir(),
            name: e.name,
            size: e.size,
            modified: e.modified,
            att: e.att,
            count: None,
        })
        .collect();
    items = model::visible(items, st.hidden);
    // "186 items" per folder, while there are few enough to count
    if items.iter().filter(|i| i.is_dir).count() <= COUNT_MAX {
        for i in items.iter_mut().filter(|i| i.is_dir) {
            let sub = model::join_dir(&dir, &i.name);
            i.count = st.fs.read_dir(&sub).ok().map(|v| {
                model::visible(
                    v.into_iter()
                        .map(|e| Item {
                            name: e.name,
                            att: e.att,
                            ..Item::default()
                        })
                        .collect(),
                    st.hidden,
                )
                .len() as u32
            });
        }
    }
    model::sort(&mut items, st.sort);
    if dir != st.dir || st.view != View::Folder {
        dprintln!("files: list {} {}", dir, items.len());
    }
    let letter = dir.chars().next().unwrap_or('C');
    st.read_only = st.drives.iter().any(|&(d, ro)| d == letter && ro);
    // the same folder again (a change notification, a finished job): what was picked stays picked
    let kept: Vec<String> = if dir == st.dir && st.view == View::Folder {
        picked(st).into_iter().map(|i| i.name).collect()
    } else {
        Vec::new()
    };
    st.checked = items.iter().map(|i| kept.contains(&i.name)).collect();
    st.items = items;
    st.dir = dir;
    st.view = View::Folder;
    ui.set_name(model::folder_name(&st.dir).into());
    ui.set_crumbs(crumbs(&st.dir, None));
    ui.set_read_only(st.read_only);
    show_rows(ui, st);
    ui.set_view("folder".into());
}

/// The breadcrumb's labels: the drive, then the folders, then the open file.
fn crumbs(dir: &str, file: Option<&str>) -> ModelRc<SharedString> {
    let mut v: Vec<SharedString> = model::crumbs(dir)
        .into_iter()
        .map(|(l, _)| l.as_str().into())
        .collect();
    if let Some(f) = file {
        v.push(f.into());
    }
    ModelRc::from(Rc::new(VecModel::from(v)))
}

fn resort(ui: &FilesApp, st: &mut State) {
    show_settings(ui, st);
    let mut pairs: Vec<(Item, bool)> = st.items.drain(..).zip(st.checked.drain(..)).collect();
    let mut items: Vec<Item> = pairs.iter().map(|(i, _)| i.clone()).collect();
    model::sort(&mut items, st.sort);
    st.checked = items
        .iter()
        .map(|i| {
            pairs
                .iter()
                .find(|(p, _)| p.name == i.name)
                .is_some_and(|(_, c)| *c)
        })
        .collect();
    pairs.clear();
    st.items = items;
    show_rows(ui, st);
}

fn show_rows(ui: &FilesApp, st: &State) {
    let now = now();
    let rows: Vec<FileRow> = st
        .items
        .iter()
        .zip(&st.checked)
        .map(|(i, c)| files_ui::file_row(i, now, *c))
        .collect();
    let picked: Vec<&Item> = st
        .items
        .iter()
        .zip(&st.checked)
        .filter(|(_, c)| **c)
        .map(|(i, _)| i)
        .collect();
    ui.set_one_item(picked.len() == 1);
    ui.set_one_folder(matches!(picked.as_slice(), [one] if one.is_dir));
    ui.set_selected(picked.len() as i32);
    ui.set_rows(ModelRc::from(Rc::new(VecModel::from(rows))));
    // new rows: the thumbnails already made go straight back in
    image::patch_rows(ui, st);
}

/// Text-like types open in the text viewer (FA4 brings the editors and the other viewers).
pub(crate) fn is_text(name: &str) -> bool {
    use neo_view::{Editor, Family};
    neo_view::type_of(name).is_some_and(|t| {
        matches!(t.editor, Editor::Text | Editor::Markdown | Editor::Table)
            || matches!(t.family, Family::Text | Family::Code)
    }) || ["bat", "cfg", "inf", "rss", "htm"]
        .iter()
        .any(|e| neo_view::ext_of(name).eq_ignore_ascii_case(e))
}

fn view_file(app: &App, ui: &FilesApp, st: &mut State, item: &Item) {
    let path = model::join_file(&st.dir, &item.name);
    if image::is_image(&item.name) {
        return image::open(app, ui, st, &path);
    }
    if archive::is_archive(&item.name) {
        return archive::sheet(app, ui, st, &path);
    }
    if !is_text(&item.name) {
        return fa7::open(app, ui, st, &path);
    }
    open_text(app, ui, st, &path, false);
}

/// A text file in the viewer (`edit` false) or its editor; the folder under it stays `st.dir`.
fn open_text(app: &App, ui: &FilesApp, st: &mut State, path: &str, edit: bool) {
    let letter = path.chars().next().unwrap_or('C');
    // a file opened from an archive is a copy in the cache: read-only
    let ro_drive = st.drives.iter().any(|&(d, ro)| d == letter && ro) || archive::in_cache(path);
    let mut o = match edit::Open::new(&st.fs, path, edit, ro_drive) {
        Ok(o) => o,
        Err(msg) => return note_with(app, &msg, ""),
    };
    o.wrap = flag(app, "files.wrap", true);
    dprintln!(
        "files: {} {} {}",
        if edit { "edit" } else { "view" },
        path,
        o.ed.doc.lines.len()
    );
    fa3::opened(st, path);
    if let Some(dir) = neo_fs::path::parent(path) {
        st.dir = dir;
    }
    st.file = String::from(path);
    if !matches!(st.view, View::Text | View::Edit) {
        st.text_from = st.view;
    }
    st.view = if edit { View::Edit } else { View::Text };
    ui.set_find_open(false);
    ui.set_view(if edit { "edit" } else { "text" }.into());
    // the pane takes its new header's size before it is measured for wrapping
    let _ = app.redraw();
    o.relayout(ui);
    ui.invoke_text_top();
    o.show(ui);
    if edit && o.mode != edit::Mode::Table {
        editor_keyboard(ui);
    } else {
        ui.global::<KeyboardFocus>().invoke_close();
    }
    st.open = Some(o);
}

/// Back out of the editor: the viewer of the same file (a table goes back to its folder).
fn leave_editor(app: &App, ui: &FilesApp, st: &mut State) {
    ui.global::<KeyboardFocus>().invoke_close();
    ui.set_find_open(false);
    let Some(o) = st.open.take() else { return };
    if o.mode == edit::Mode::Table || !is_text(&o.name) {
        let dir = o.path_dir();
        open_dir(app, ui, st, dir);
    } else {
        open_text(app, ui, st, &o.path, false);
    }
}

fn save_open(app: &App, ui: &FilesApp, st: &mut State) {
    let Some(o) = st.open.as_mut() else { return };
    let r = o.save(&st.fs);
    dprintln!(
        "files: save {} {} {}",
        o.path,
        r.as_ref().map_or(0, |n| *n),
        r.as_ref().err().map_or(0, |e| *e)
    );
    match r {
        Ok(_) => {
            o.show(ui);
            note_with(app, &format!("Saved {}", o.name), "");
        }
        Err(-21) => note_with(app, "This file is read-only", ""),
        Err(e) => toast(app, "", e),
    }
}

/// Test images only (`--features test-files`): a 5 MB log to open on the phone (files-app.md FA4's gate).
#[cfg(feature = "test-files")]
fn test_files(st: &State) {
    const BIG: &str = "C:\\Data\\big.log";
    if st.fs.entry(BIG).is_ok() {
        return;
    }
    let mut s = String::with_capacity(5 << 20);
    let mut i = 0u32;
    while s.len() < 5 << 20 {
        s.push_str(&format!(
            "10:{:02}:{:02}.{:03} EFILE line {i}: Mounted E: FAT32, 41.8 GB, label MASS\r\n",
            (i / 3600) % 60,
            (i / 60) % 60,
            i % 1000
        ));
        i += 1;
    }
    let r = files_ops::store::write(&*st.fs, BIG, s.as_bytes());
    dprintln!(
        "files: test big.log {} lines {}",
        i,
        r.err().map_or(0, |e| e.code())
    );
}

/// Test images only: `C:\Data\photos\` with three landscapes, a PNG and a 12 MP JPEG (FA5's gate), drawn here.
#[cfg(feature = "test-files")]
fn test_photos(st: &State) {
    use neo_format_image::encode;
    use neo_format_image::shrink::Rows;
    const DIR: &str = "C:\\Data\\photos\\";
    if st.fs.entry(&format!("{DIR}big.jpg")).is_ok() {
        return;
    }
    let land = |sky: &str, low: &str, hill: &str| {
        format!(
            r##"<svg viewBox="0 0 400 300"><rect width="400" height="300" fill="{sky}"/><rect y="90" width="400" height="120" fill="{low}" opacity=".75"/><circle cx="236" cy="130" r="22" fill="#ffe1a0"/><path d="M-40 230 Q80 150 200 210 T440 200 V300 H-40 Z" fill="{hill}"/><rect y="250" width="400" height="50" fill="#1b0d2e"/></svg>"##
        )
    };
    let pics = [
        ("a.jpg", land("#f6c46a", "#d2607a", "#4a2466")),
        ("b.jpg", land("#3aa0d8", "#8fd0f0", "#0b3c5d")),
        ("c.jpg", land("#9fd88a", "#2f9e44", "#0b3d1e")),
        ("d.png", land("#e88fb0", "#f4c6d6", "#5a1d3a")),
    ];
    for (name, svg) in pics {
        let Ok(p) = neo_format_image::svg::render(svg.as_bytes(), 1024) else {
            continue;
        };
        let b = if name.ends_with(".png") {
            encode::png(&p)
        } else {
            encode::jpeg(&p, 90).unwrap_or_default()
        };
        let _ = files_ops::store::write(&*st.fs, &format!("{DIR}{name}"), &b);
    }
    /// A 4032 × 3024 gradient with a disc, made a row at a time (no 36 MB buffer).
    struct Big;
    impl Rows for Big {
        fn w(&self) -> u32 {
            4032
        }
        fn h(&self) -> u32 {
            3024
        }
        fn ch(&self) -> u8 {
            3
        }
        fn row(&self, y: u32, out: &mut [u8]) {
            for x in 0..4032u32 {
                let (dx, dy) = (x as i32 - 2600, y as i32 - 1100);
                let sun = dx * dx + dy * dy < 300 * 300;
                let o = x as usize * 3;
                out[o] = if sun {
                    255
                } else {
                    (240 - y * 160 / 3024) as u8
                };
                out[o + 1] = if sun {
                    225
                } else {
                    (190 - y * 150 / 3024) as u8
                };
                out[o + 2] = if sun {
                    160
                } else {
                    (110 + x * 80 / 4032) as u8
                };
            }
        }
    }
    let t = e32::time::millis();
    let b = encode::jpeg(&Big, 90).unwrap_or_default();
    let r = files_ops::store::write(&*st.fs, &format!("{DIR}big.jpg"), &b);
    dprintln!(
        "files: test photos big.jpg {} KB {} ms {}",
        b.len() / 1024,
        e32::time::millis() - t,
        r.err().map_or(0, |e| e.code())
    );
}

/// FA6's Pixel gate: C:\Data\zips\ with a 1 GB zip written by our own writer (63 stored members of 16 MB, one
/// deflated text of 16 MB), a small zip with UTF-8 names and a tar.gz (both made by other tools).
#[cfg(feature = "test-files")]
fn test_zips(st: &State) {
    use neo_format_archive::zip::ZipWriter;
    use neo_format_archive::{Patch, Result as ArResult, Sink};
    use neo_fs::File as _;
    const DIR: &str = "C:\\Data\\zips\\";
    let _ = files_ops::store::write(
        &*st.fs,
        &format!("{DIR}names.zip"),
        include_bytes!("../../../crates/neo-format-archive/tests/data/infozip.zip"),
    );
    let _ = files_ops::store::write(
        &*st.fs,
        &format!("{DIR}data.tar.gz"),
        include_bytes!("../../../crates/neo-format-archive/tests/data/data.tar.gz"),
    );
    let big = format!("{DIR}big.zip");
    if st.fs.entry(&big).is_ok() {
        return;
    }
    struct Out<'a> {
        f: fs_e32::E32File<'a>,
        at: u64,
    }
    impl Sink for Out<'_> {
        fn write(&mut self, d: &[u8]) -> ArResult<()> {
            self.f
                .write_at(self.at, d)
                .map_err(|e| neo_format_archive::Error::Io(e.code()))?;
            self.at += d.len() as u64;
            Ok(())
        }
    }
    impl Patch for Out<'_> {
        fn pos(&self) -> u64 {
            self.at
        }
        fn patch(&mut self, at: u64, d: &[u8]) -> ArResult<()> {
            self.f
                .write_at(at, d)
                .map_err(|e| neo_format_archive::Error::Io(e.code()))
        }
    }
    let t = e32::time::millis();
    let part = format!("{big}.~part");
    let Ok(f) = st.fs.open(&part, Open::Replace) else {
        return;
    };
    let mut z = ZipWriter::new(Out { f, at: 0 });
    let mut buf = alloc::vec![0u8; 1 << 20];
    let mut seed = 0x9e37_79b9u32;
    let mut r = Ok(());
    for k in 0..64 {
        let text = k == 0;
        let name = if text {
            String::from("notes/long.txt")
        } else {
            format!("media/part-{k:02}.bin")
        };
        r = r.and_then(|_| z.start(&name, 1_763_548_800, text));
        for mb in 0..16 {
            if text {
                for (i, b) in buf.iter_mut().enumerate() {
                    *b = b"Files FA6 extracts a gigabyte. "[(i + mb) % 31];
                }
            } else {
                for b in buf.as_chunks_mut::<4>().0 {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    b.copy_from_slice(&seed.to_le_bytes());
                }
            }
            r = r.and_then(|_| z.write(&buf));
        }
    }
    let r = r.and_then(|_| z.finish()).map(|o| drop(o.f));
    let r = r.map_err(|e| match e {
        neo_format_archive::Error::Io(c) => c,
        _ => -20,
    });
    let r = r.and_then(|_| st.fs.rename(&part, &big).map_err(|e| e.code()));
    if r.is_err() {
        let _ = st.fs.delete(&part);
    }
    dprintln!(
        "files: test zips big.zip {} ms {}",
        e32::time::millis() - t,
        r.err().unwrap_or(0)
    );
}

/// The editor wants the system keyboard: the text one, for its Select all · Copy · Paste bar.
fn editor_keyboard(ui: &FilesApp) {
    let kf = ui.global::<KeyboardFocus>();
    kf.set_mode(proto::KEYBOARD_TEXT);
    kf.set_dismissed(false);
    kf.set_open(true);
    kf.set_seq(kf.get_seq() + 1);
}

/// Keys go to the editor's text (not to a find field or a table cell).
fn editor_has_keys(ui: &FilesApp, st: &State) -> bool {
    st.view == View::Edit
        && !ui.get_find_open()
        && st
            .open
            .as_ref()
            .is_some_and(|o| o.mode == edit::Mode::Code || (o.mode == edit::Mode::Md && !o.preview))
}

fn editor_key(app: &App, ui: &FilesApp, st: &mut State, code: u32) {
    let Some(o) = st.open.as_mut() else { return };
    match o.key(ui, code) {
        edit::Clip::None => {}
        edit::Clip::Copy(t) => {
            let mut end = t.len().min(proto::CLIP_MAX);
            while !t.is_char_boundary(end) {
                end -= 1;
            }
            app.set_clipboard(&t[..end]);
            if end < t.len() {
                note_with(app, "Copied the first 2 KB", "");
            }
        }
        edit::Clip::Paste => {
            if let Some(t) = app.clipboard() {
                o.paste(ui, &t);
            }
        }
    }
}

// ── forms ────────────────────────────────────────────────────────────────────────────────────────────────────────

fn start_rename(app: &App, ui: &FilesApp, st: &mut State) {
    let picked = picked(st);
    let [one] = picked.as_slice() else {
        return;
    };
    if st.read_only {
        return;
    }
    let one = one.clone();
    clear_selection(ui, st);
    let ext = if one.is_dir {
        String::new()
    } else {
        model::split_name(&one.name).1.to_string()
    };
    open_form(app, ui, st, Form::Rename(one, ext));
}

fn open_form(app: &App, ui: &FilesApp, st: &mut State, form: Form) {
    if st.read_only {
        return;
    }
    let kb = app.keyboard(proto::KEYBOARD_TEXT).unwrap_or(0);
    ui.set_kb_h(kb as f32);
    let names: Vec<&str> = st.items.iter().map(|i| i.name.as_str()).collect();
    let (kind, name, suffix) = match &form {
        Form::NewFolder => ("folder", String::new(), String::new()),
        // files2-new-file: the name, its type's extension after it (Text .txt to start)
        Form::NewFile => {
            st.new_type = 0;
            ui.set_new_type(0);
            let free = model::free_name(&names, "New file", ".txt");
            (
                "file",
                free.trim_end_matches(".txt").to_string(),
                String::from(".txt"),
            )
        }
        Form::Rename(item, ext) => (
            "rename",
            item.name[..item.name.len() - ext.len()].to_string(),
            ext.clone(),
        ),
    };
    ui.set_form_dir(st.dir.as_str().into());
    if let Form::Rename(item, _) = &form {
        let row = files_ui::file_row(item, now(), false);
        ui.set_form_dir(st.dir.trim_end_matches('\\').into());
        ui.set_form_detail(row.detail);
        ui.set_form_badge(if item.is_dir {
            SharedString::new()
        } else {
            row.badge
        });
        ui.set_form_colour(row.colour);
    }
    ui.set_name_text(name.into());
    ui.set_name_suffix(suffix.into());
    ui.set_form(kind.into());
    st.form = Some(form);
}

/// ✕ or Back on the form: leave at once when nothing was typed, else ask (D14: the app asks, not the kit).
fn cancel_form(app: &App, ui: &FilesApp, st: &mut State) {
    let typed = ui.get_name_text();
    let unchanged = match &st.form {
        Some(Form::NewFolder) => typed.is_empty(),
        Some(Form::NewFile) => true,
        Some(Form::Rename(i, ext)) => typed.as_str() == &i.name[..i.name.len() - ext.len()],
        None => return,
    };
    if unchanged {
        close_form(app, ui, st);
    } else {
        ask(app, st, "Discard the name?".into(), "Discard", Ask::Discard);
    }
}

fn close_form(app: &App, ui: &FilesApp, st: &mut State) {
    st.form = None;
    let _ = app.keyboard(proto::KEYBOARD_HIDDEN);
    // the name field goes with its `if` and never reports losing the focus
    ui.global::<KeyboardFocus>().invoke_close();
    st.kb = (false, 0, 0);
    ui.set_kb_h(0.0);
    ui.set_form(SharedString::new());
}

/// ✓: make or rename, then show the folder again (a new file opens in the viewer).
fn save_form(app: &App, ui: &FilesApp, st: &mut State) {
    let suffix = ui.get_name_suffix();
    let mut name = format!("{}{}", ui.get_name_text().trim(), suffix);
    // Code… takes the extension in the name; none typed makes a text file
    if matches!(st.form, Some(Form::NewFile)) && neo_view::ext_of(&name).is_empty() {
        name.push_str(".txt");
    }
    if !model::valid_name(&name) {
        return note_with(app, "That name can't be used", "");
    }
    let Some(form) = st.form.take() else { return };
    close_form(app, ui, st);
    let (what, path, r) = match &form {
        Form::NewFolder => {
            let p = model::join_dir(&st.dir, &name);
            let r = st.fs.mkdir(&p);
            ("mkdir", p, r)
        }
        Form::NewFile => {
            let p = model::join_file(&st.dir, &name);
            let r = st.fs.open(&p, Open::Create).map(drop);
            ("newfile", p, r)
        }
        Form::Rename(i, _) => {
            let (from, to) = (
                model::join_file(&st.dir, &i.name),
                model::join_file(&st.dir, &name),
            );
            let r = st.fs.rename(&from, &to);
            ("rename", from, r)
        }
    };
    dprintln!(
        "files: {} {} {}",
        what,
        path,
        r.as_ref().err().map_or(0, |e| e.code())
    );
    relist(app, ui, st);
    if st.view == View::PickFolder {
        // a folder made in the picker shows up in it
        archive::picker_list(app, ui, st);
    }
    match r {
        Err(e) => toast(app, "", e.code()),
        Ok(()) if matches!(form, Form::NewFile) => {
            let p = model::join_file(&st.dir, &name);
            open_text(app, ui, st, &p, true);
        }
        Ok(()) => {}
    }
}

// ── toasts ───────────────────────────────────────────────────────────────────────────────────────────────────────

/// The time now as a `TTime` (UTC), as the file server dates entries.
fn now() -> i64 {
    time::ttime_from_unix(time::utc_now().0)
}

/// `s` cut to at most `max` bytes, on a character boundary.
fn cut(s: &str, max: usize) -> String {
    let mut end = s.len().min(max);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    String::from(&s[..end])
}

/// A short message as the shell's toast (NOTE), with an action (Undo) or none.
fn note_with(app: &App, text: &str, action: &str) {
    if let Err(e) = app.note(&cut(text, neoui_proto::page::NOTE_TEXT_MAX), action) {
        dprintln!("files: note {}", e);
    }
}

fn toast(app: &App, what: &str, code: i32) {
    let msg = model::error_text(code);
    if what.is_empty() {
        note_with(app, &msg, "");
    } else {
        note_with(app, &format!("{what}: {msg}"), "");
    }
}
