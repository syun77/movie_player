// Provide a GUI implementation behind the "gui" feature. When the feature is
// disabled the crate compiles to a lightweight CLI stub that instructs how to
// enable GUI mode. This avoids pkg-config / system GTK failures on machines
// without GTK installed.

#[cfg(feature = "gui")]
mod gui {
    use gio::prelude::*;
    use gst::bus::BusWatchGuard;
    use gst::prelude::*;
    use gstreamer as gst;
    use glib::{self, ControlFlow};
    use gtk::gdk;
    use gtk::prelude::*;
    use gtk4 as gtk;
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::rc::Rc;
    use std::time::Duration;

    const SEEK_STEP_SECONDS: i64 = 10;
    const MAX_HISTORY_ITEMS: usize = 50;

    #[derive(Clone, Debug)]
    struct PersistedState {
        loop_enabled: bool,
        history: Vec<PathBuf>,
    }

    impl Default for PersistedState {
        fn default() -> Self {
            Self {
                loop_enabled: false,
                history: Vec::new(),
            }
        }
    }

    impl PersistedState {
        fn state_file_path() -> Option<PathBuf> {
            let home = std::env::var_os("HOME")?;
            let mut dir = PathBuf::from(home);
            if cfg!(target_os = "macos") {
                dir.push("Library");
                dir.push("Application Support");
            } else {
                dir.push(".config");
            }
            dir.push("movie_player");
            Some(dir.join("state.txt"))
        }

        fn load() -> Self {
            let Some(path) = Self::state_file_path() else {
                return Self::default();
            };

            let Ok(content) = fs::read_to_string(&path) else {
                return Self::default();
            };

            let mut state = Self::default();
            for line in content.lines() {
                if let Some(value) = line.strip_prefix("loop=") {
                    state.loop_enabled = value == "1";
                } else if let Some(value) = line.strip_prefix("history=") {
                    if !value.is_empty() {
                        state.history.push(PathBuf::from(value));
                    }
                }
            }

            state.history.retain(|p| p.exists());
            state.history.truncate(MAX_HISTORY_ITEMS);
            state
        }

        fn save(&self) {
            let Some(path) = Self::state_file_path() else {
                return;
            };

            let Some(parent) = path.parent() else {
                return;
            };

            if let Err(err) = fs::create_dir_all(parent) {
                eprintln!("[state] create_dir_all failed: {}", err);
                return;
            }

            let mut lines = Vec::with_capacity(1 + self.history.len());
            lines.push(format!("loop={}", if self.loop_enabled { 1 } else { 0 }));
            for path in self.history.iter().take(MAX_HISTORY_ITEMS) {
                lines.push(format!("history={}", path.to_string_lossy()));
            }

            if let Err(err) = fs::write(path, lines.join("\n")) {
                eprintln!("[state] save failed: {}", err);
            }
        }

        fn record_recent_file(&mut self, path: PathBuf) {
            self.history.retain(|p| p != &path);
            self.history.insert(0, path);
            self.history.retain(|p| p.exists());
            self.history.truncate(MAX_HISTORY_ITEMS);
        }
    }

    fn history_label(index: usize, path: &Path) -> String {
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("(unknown)");
        format!("{}. {}", index + 1, filename)
    }

    struct PlayerUi {
        playbin: gst::Element,
        play_pause_button: gtk::Button,
        seek_scale: gtk::Scale,
        time_label: gtk::Label,
        history_menu: gio::Menu,
        state: RefCell<PersistedState>,
        loop_enabled: Cell<bool>,
        suppress_seek_signal: Cell<bool>,
        user_seeking: Cell<bool>,
        was_playing_before_seek: Cell<bool>,
        seek_resume_timer: RefCell<Option<glib::SourceId>>,
        bus_watch_guard: RefCell<Option<BusWatchGuard>>,
    }

    impl PlayerUi {
        fn is_playing(&self) -> bool {
            let (_, current, pending) = self.playbin.state(gst::ClockTime::from_seconds(0));
            current == gst::State::Playing || pending == gst::State::Playing
        }

        fn duration_seconds(&self) -> f64 {
            self.playbin
                .query_duration::<gst::ClockTime>()
                .map(|d| d.nseconds() as f64 / 1_000_000_000.0)
                .unwrap_or(0.0)
        }

        fn position_seconds(&self) -> f64 {
            self.playbin
                .query_position::<gst::ClockTime>()
                .map(|p| p.nseconds() as f64 / 1_000_000_000.0)
                .unwrap_or(0.0)
        }

        fn set_media_file(&self, path: PathBuf) {
            let file = gio::File::for_path(&path);
            let uri = file.uri();
            eprintln!("Loading file: {:?}", path);

            let _ = self.playbin.set_state(gst::State::Ready);
            self.playbin.set_property("uri", &uri);

            if self.playbin.set_state(gst::State::Playing).is_err() {
                let msg = "動画を再生できませんでした。\nGStreamer のプラグイン/コーデックを確認してください。";
                eprintln!("{}", msg);
                show_error_dialog(msg);
                self.play_pause_button.set_label("▶");
            } else {
                self.play_pause_button.set_label("⏸");
            }

            self.record_recent_file(path);
            self.refresh_seek_ui();
        }

        fn set_loop_enabled(&self, enabled: bool) {
            self.loop_enabled.set(enabled);
            self.state.borrow_mut().loop_enabled = enabled;
            self.state.borrow().save();
        }

        fn record_recent_file(&self, path: PathBuf) {
            {
                let mut state = self.state.borrow_mut();
                state.record_recent_file(path);
                state.save();
            }
            self.refresh_history_menu();
        }

        fn refresh_history_menu(&self) {
            self.history_menu.remove_all();

            let state = self.state.borrow();
            for (index, path) in state.history.iter().enumerate() {
                let action = format!("win.open-history-{}", index);
                let label = history_label(index, path);
                self.history_menu.append(Some(&label), Some(&action));
            }
        }

        fn open_history_index(&self, index: usize) {
            let maybe_path = self.state.borrow().history.get(index).cloned();
            if let Some(path) = maybe_path {
                if path.exists() {
                    self.set_media_file(path);
                } else {
                    show_error_dialog("履歴のファイルが見つかりませんでした。");
                    {
                        let mut state = self.state.borrow_mut();
                        state.history.retain(|p| p.exists());
                        state.save();
                    }
                    self.refresh_history_menu();
                }
            }
        }

        fn toggle_play_pause(&self) {
            if self.is_playing() {
                let _ = self.playbin.set_state(gst::State::Paused);
                self.play_pause_button.set_label("▶");
            } else {
                let _ = self.playbin.set_state(gst::State::Playing);
                self.play_pause_button.set_label("⏸");
            }
        }

        fn seek_relative_seconds(&self, seconds: i64) {
            let current = self.position_seconds() as i64;
            let duration = self.duration_seconds() as i64;
            let mut target = current.saturating_add(seconds);
            if target < 0 {
                target = 0;
            }
            if duration > 0 {
                target = target.min(duration);
            }

            let _ = self.playbin.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT,
                gst::ClockTime::from_seconds(target as u64),
            );
            self.refresh_seek_ui();
        }

        fn seek_to_seconds(&self, seconds: f64) {
            let duration_ns = self
                .playbin
                .query_duration::<gst::ClockTime>()
                .map(|d| d.nseconds() as i128)
                .unwrap_or(0);

            let mut target_ns = (seconds.max(0.0) * 1_000_000_000.0) as i128;
            if duration_ns > 0 {
                target_ns = target_ns.min(duration_ns);
            }

            let _ = self.playbin.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::KEY_UNIT | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(target_ns as u64),
            );
        }

        fn refresh_seek_ui(&self) {
            let position_seconds = self.position_seconds();
            let duration_seconds = self.duration_seconds();

            self.seek_scale.set_range(0.0, duration_seconds.max(0.1));
            if !self.user_seeking.get() {
                self.suppress_seek_signal.set(true);
                self.seek_scale.set_value(position_seconds.min(duration_seconds));
                self.suppress_seek_signal.set(false);
            }

            self.time_label.set_text(&format!(
                "{} / {}",
                format_time(position_seconds as i64),
                format_time(duration_seconds as i64)
            ));

            if !self.is_playing() && duration_seconds > 0.0 && position_seconds >= duration_seconds {
                self.play_pause_button.set_label("▶");
            }
        }

        fn restart_from_beginning(&self) {
            eprintln!("[loop] restart_from_beginning: start");
            self.user_seeking.set(false);
            if let Some(source_id) = self.seek_resume_timer.borrow_mut().take() {
                source_id.remove();
                eprintln!("[loop] canceled pending seek resume timer");
            }

            match self.playbin.set_state(gst::State::Paused) {
                Ok(ret) => eprintln!("[loop] set_state(Paused) -> {:?}", ret),
                Err(err) => eprintln!("[loop] set_state(Paused) failed: {:?}", err),
            }

            let seek_result = self.playbin.seek_simple(
                gst::SeekFlags::FLUSH | gst::SeekFlags::ACCURATE,
                gst::ClockTime::from_nseconds(0),
            );
            eprintln!("[loop] seek_simple(0ns) -> {:?}", seek_result);

            if seek_result.is_err() {
                // Some short clips may reject seek at EOS timing; re-preroll from READY.
                match self.playbin.set_state(gst::State::Ready) {
                    Ok(ret) => eprintln!("[loop] fallback set_state(Ready) -> {:?}", ret),
                    Err(err) => eprintln!("[loop] fallback set_state(Ready) failed: {:?}", err),
                }
            }

            match self.playbin.set_state(gst::State::Playing) {
                Ok(ret) => eprintln!("[loop] set_state(Playing) -> {:?}", ret),
                Err(err) => eprintln!("[loop] set_state(Playing) failed: {:?}", err),
            }

            let (change_ret, current, pending) = self.playbin.state(gst::ClockTime::from_mseconds(100));
            eprintln!(
                "[loop] state after restart: ret={:?}, current={:?}, pending={:?}",
                change_ret, current, pending
            );

            self.play_pause_button.set_label("⏸");
            self.refresh_seek_ui();
        }
    }

    fn format_time(total_seconds: i64) -> String {
        let total_seconds = total_seconds.max(0) as u64;
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        if hours > 0 {
            format!("{hours:02}:{minutes:02}:{seconds:02}")
        } else {
            format!("{minutes:02}:{seconds:02}")
        }
    }

    fn open_file_dialog() -> Option<PathBuf> {
        rfd::FileDialog::new()
            .add_filter("Video", &["mp4", "mkv", "webm", "mov", "avi", "m4v"])
            .pick_file()
    }

    fn show_error_dialog(message: &str) {
        if gtk::is_initialized() {
            let dialog = gtk::MessageDialog::builder()
                .message_type(gtk::MessageType::Error)
                .text(message)
                .build();
            dialog.present();
        } else {
            eprintln!("ERROR: {}", message);
        }
    }

    fn build_ui(app: &gtk::Application) {
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Rust Movie Player")
            .default_width(1100)
            .default_height(700)
            .build();

        let playbin = match gst::ElementFactory::make("playbin").build() {
            Ok(element) => element,
            Err(_) => {
                show_error_dialog("GStreamer の playbin を作成できませんでした。`brew install gstreamer` を確認してください。");
                return;
            }
        };

        let video_sink = match gst::ElementFactory::make("gtk4paintablesink").build() {
            Ok(element) => element,
            Err(_) => {
                show_error_dialog(
                    "`gtk4paintablesink` が見つかりません。\n`brew install gstreamer` の再実行後に再起動してください。",
                );
                return;
            }
        };

        playbin.set_property("video-sink", &video_sink);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

        let menu_root = gio::Menu::new();
        let file_menu = gio::Menu::new();
        file_menu.append(Some("開く"), Some("win.open"));
        let history_menu = gio::Menu::new();
        file_menu.append_submenu(Some("履歴"), &history_menu);
        menu_root.append_submenu(Some("ファイル"), &file_menu);
        let help_menu = gio::Menu::new();
        help_menu.append(Some("このアプリについて"), Some("win.about"));
        menu_root.append_submenu(Some("ヘルプ"), &help_menu);
        let menubar = gtk::PopoverMenuBar::from_model(Some(&menu_root));
        root.append(&menubar);

        let picture = gtk::Picture::new();
        picture.set_hexpand(true);
        picture.set_vexpand(true);
        picture.set_can_shrink(true);
        let paintable = video_sink.property::<gdk::Paintable>("paintable");
        picture.set_paintable(Some(&paintable));
        root.append(&picture);

        let controls = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        controls.set_margin_start(12);
        controls.set_margin_end(12);
        controls.set_margin_top(8);
        controls.set_margin_bottom(12);

        let rewind_button = gtk::Button::with_label("⏪ 10秒");
        let play_pause_button = gtk::Button::with_label("▶");
        let forward_button = gtk::Button::with_label("10秒 ⏩");
        let loop_check = gtk::CheckButton::with_label("ループ");

        let seek_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.1);
        seek_scale.set_hexpand(true);
        seek_scale.set_draw_value(false);

        let time_label = gtk::Label::new(Some("00:00 / 00:00"));

        controls.append(&rewind_button);
        controls.append(&play_pause_button);
        controls.append(&forward_button);
        controls.append(&loop_check);
        controls.append(&seek_scale);
        controls.append(&time_label);

        root.append(&controls);
        window.set_child(Some(&root));

        let initial_state = PersistedState::load();

        let ui = Rc::new(PlayerUi {
            playbin: playbin.clone(),
            play_pause_button: play_pause_button.clone(),
            seek_scale: seek_scale.clone(),
            time_label: time_label.clone(),
            history_menu: history_menu.clone(),
            state: RefCell::new(initial_state.clone()),
            loop_enabled: Cell::new(initial_state.loop_enabled),
            suppress_seek_signal: Cell::new(false),
            user_seeking: Cell::new(false),
            was_playing_before_seek: Cell::new(false),
            seek_resume_timer: RefCell::new(None),
            bus_watch_guard: RefCell::new(None),
        });

        loop_check.set_active(initial_state.loop_enabled);
        ui.refresh_history_menu();

        if let Some(bus) = playbin.bus() {
            let ui_for_messages = ui.clone();
            let playbin_obj: gst::Object = playbin.clone().upcast();
            match bus.add_watch_local(move |_, msg| {
                use gst::MessageView;
                match msg.view() {
                    MessageView::Error(err) => {
                        let msg = format!(
                            "動画を再生できませんでした: {} ({})",
                            err.error(),
                            err.debug().unwrap_or_default()
                        );
                        eprintln!("{}", msg);
                        show_error_dialog(&msg);
                        ui_for_messages.play_pause_button.set_label("▶");
                    }
                    MessageView::Eos(..) => {
                        eprintln!(
                            "[bus] EOS received, loop_enabled={}",
                            ui_for_messages.loop_enabled.get()
                        );
                        if ui_for_messages.loop_enabled.get() {
                            ui_for_messages.restart_from_beginning();
                        } else {
                            ui_for_messages.play_pause_button.set_label("▶");
                        }
                    }
                    MessageView::StateChanged(changed) => {
                        if changed
                            .src()
                            .is_some_and(|src| src.as_ptr() == playbin_obj.as_ptr())
                        {
                            eprintln!(
                                "[bus] state changed: old={:?} current={:?} pending={:?}",
                                changed.old(),
                                changed.current(),
                                changed.pending()
                            );
                            if changed.current() == gst::State::Playing {
                                ui_for_messages.play_pause_button.set_label("⏸");
                            }
                        }
                    }
                    _ => {}
                }
                ControlFlow::Continue
            }) {
                Ok(guard) => {
                    eprintln!("[bus] add_watch_local attached");
                    *ui.bus_watch_guard.borrow_mut() = Some(guard);
                }
                Err(err) => {
                    eprintln!("[bus] add_watch_local failed: {}", err);
                }
            }
        }

        {
            let ui = ui.clone();
            let btn = play_pause_button.clone();
            btn.connect_clicked(move |_| ui.toggle_play_pause());
        }

        {
            let ui = ui.clone();
            rewind_button.connect_clicked(move |_| ui.seek_relative_seconds(-SEEK_STEP_SECONDS));
        }

        {
            let ui = ui.clone();
            forward_button.connect_clicked(move |_| ui.seek_relative_seconds(SEEK_STEP_SECONDS));
        }

        {
            let ui = ui.clone();
            loop_check.connect_toggled(move |check| {
                ui.set_loop_enabled(check.is_active());
            });
        }

        for index in 0..MAX_HISTORY_ITEMS {
            let action = gio::SimpleAction::new(&format!("open-history-{}", index), None);
            let ui_for_history = ui.clone();
            action.connect_activate(move |_, _| {
                ui_for_history.open_history_index(index);
            });
            window.add_action(&action);
        }

        {
            let ui = ui.clone();
            seek_scale.connect_value_changed(move |scale| {
                if ui.suppress_seek_signal.get() {
                    return;
                }

                if !ui.user_seeking.get() {
                    ui.user_seeking.set(true);
                    let was_playing = ui.is_playing();
                    ui.was_playing_before_seek.set(was_playing);
                    if was_playing {
                        let _ = ui.playbin.set_state(gst::State::Paused);
                        ui.play_pause_button.set_label("▶");
                    }
                }

                ui.seek_to_seconds(scale.value());

                ui.time_label.set_text(&format!(
                    "{} / {}",
                    format_time(scale.value() as i64),
                    format_time(ui.duration_seconds() as i64)
                ));

                if let Some(source_id) = ui.seek_resume_timer.borrow_mut().take() {
                    source_id.remove();
                }

                let ui_for_finalize = ui.clone();
                let source_id = glib::timeout_add_local(Duration::from_millis(180), move || {
                    ui_for_finalize.user_seeking.set(false);
                    if ui_for_finalize.was_playing_before_seek.get() {
                        let _ = ui_for_finalize.playbin.set_state(gst::State::Playing);
                        ui_for_finalize.play_pause_button.set_label("⏸");
                    } else {
                        ui_for_finalize.play_pause_button.set_label("▶");
                    }
                    ui_for_finalize.refresh_seek_ui();
                    *ui_for_finalize.seek_resume_timer.borrow_mut() = None;
                    ControlFlow::Break
                });
                *ui.seek_resume_timer.borrow_mut() = Some(source_id);
            });
        }

        {
            let ui = ui.clone();
            glib::timeout_add_local(std::time::Duration::from_millis(250), move || {
                ui.refresh_seek_ui();
                ControlFlow::Continue
            });
        }

        let open_action = gio::SimpleAction::new("open", None);
        {
            let ui = ui.clone();
            open_action.connect_activate(move |_, _| {
                if let Some(path) = open_file_dialog() {
                    ui.set_media_file(path);
                }
            });
        }
        window.add_action(&open_action);

        let about_action = gio::SimpleAction::new("about", None);
        {
            let weak_window = window.downgrade();
            about_action.connect_activate(move |_, _| {
                if let Some(window) = weak_window.upgrade() {
                    let dialog = gtk::AboutDialog::builder()
                        .transient_for(&window)
                        .modal(true)
                        .program_name("Rust Movie Player")
                        .comments("ローカル動画をシンプルに再生する Rust 製プレイヤー")
                        .build();
                    dialog.present();
                }
            });
        }
        window.add_action(&about_action);

        let key_controller = gtk::EventControllerKey::new();
        {
            let ui = ui.clone();
            key_controller.connect_key_pressed(move |_, key, _, _| match key {
                gdk::Key::space => {
                    ui.toggle_play_pause();
                    glib::Propagation::Stop
                }
                gdk::Key::Left => {
                    ui.seek_relative_seconds(-SEEK_STEP_SECONDS);
                    glib::Propagation::Stop
                }
                gdk::Key::Right => {
                    ui.seek_relative_seconds(SEEK_STEP_SECONDS);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            });
        }
        window.add_controller(key_controller);

        {
            let playbin = playbin.clone();
            window.connect_close_request(move |_| {
                let _ = playbin.set_state(gst::State::Null);
                glib::Propagation::Proceed
            });
        }

        window.present();
    }

    pub fn run() {
        if let Err(err) = gst::init() {
            let msg = format!("GStreamer 初期化に失敗しました: {}", err);
            show_error_dialog(&msg);
            return;
        }

        if cfg!(target_os = "macos") && std::env::var_os("GSK_RENDERER").is_none() {
            unsafe {
                // On macOS, cairo fallback can make video colors look washed out.
                // Prefer the GPU renderer unless the user explicitly overrides it.
                std::env::set_var("GSK_RENDERER", "gl");
            }
        }

        let app = gtk::Application::builder()
            .application_id("com.syun77.movie_player")
            .build();
        app.connect_activate(build_ui);
        app.run();
    }
}

#[cfg(not(feature = "gui"))]
fn main() {
    println!(
        "GUI feature not enabled. To build with the GTK4 GUI, install GTK4 and run:\n  brew install gtk4 pkg-config\n  export PKG_CONFIG_PATH=...\n  cargo run --features gui"
    );
}

#[cfg(feature = "gui")]
fn main() {
    gui::run();
}
