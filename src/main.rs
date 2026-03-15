// Provide a GUI implementation behind the "gui" feature. When the feature is
// disabled the crate compiles to a lightweight CLI stub that instructs how to
// enable GUI mode. This avoids pkg-config / system GTK failures on machines
// without GTK installed.

#[cfg(feature = "gui")]
mod gui {
    use gio::prelude::*;
    use gst::prelude::*;
    use gstreamer as gst;
    use glib::{self, ControlFlow};
    use gtk::gdk;
    use gtk::prelude::*;
    use gtk4 as gtk;
    use std::cell::Cell;
    use std::path::PathBuf;
    use std::rc::Rc;

    const SEEK_STEP_SECONDS: i64 = 10;

    struct PlayerUi {
        playbin: gst::Element,
        play_pause_button: gtk::Button,
        seek_scale: gtk::Scale,
        time_label: gtk::Label,
        suppress_seek_signal: Cell<bool>,
    }

    impl PlayerUi {
        fn is_playing(&self) -> bool {
            self.playbin.current_state() == gst::State::Playing
        }

        fn duration_seconds(&self) -> f64 {
            self.playbin
                .query_duration::<gst::ClockTime>()
                .map(|d| d.seconds() as f64)
                .unwrap_or(0.0)
        }

        fn position_seconds(&self) -> f64 {
            self.playbin
                .query_position::<gst::ClockTime>()
                .map(|p| p.seconds() as f64)
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

            self.refresh_seek_ui();
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
            let duration = self.duration_seconds() as i64;
            let mut target = seconds as i64;
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
        }

        fn refresh_seek_ui(&self) {
            let position_seconds = self.position_seconds();
            let duration_seconds = self.duration_seconds();

            self.seek_scale.set_range(0.0, duration_seconds.max(0.1));
            self.suppress_seek_signal.set(true);
            self.seek_scale.set_value(position_seconds.min(duration_seconds));
            self.suppress_seek_signal.set(false);

            self.time_label.set_text(&format!(
                "{} / {}",
                format_time(position_seconds as i64),
                format_time(duration_seconds as i64)
            ));

            if !self.is_playing() && duration_seconds > 0.0 && position_seconds >= duration_seconds {
                self.play_pause_button.set_label("▶");
            }
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

        let seek_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 1.0, 0.1);
        seek_scale.set_hexpand(true);
        seek_scale.set_draw_value(false);

        let time_label = gtk::Label::new(Some("00:00 / 00:00"));

        controls.append(&rewind_button);
        controls.append(&play_pause_button);
        controls.append(&forward_button);
        controls.append(&seek_scale);
        controls.append(&time_label);

        root.append(&controls);
        window.set_child(Some(&root));

        let ui = Rc::new(PlayerUi {
            playbin: playbin.clone(),
            play_pause_button: play_pause_button.clone(),
            seek_scale: seek_scale.clone(),
            time_label: time_label.clone(),
            suppress_seek_signal: Cell::new(false),
        });

        if let Some(bus) = playbin.bus() {
            let ui = ui.clone();
            let playbin_obj: gst::Object = playbin.clone().upcast();
            let _ = bus.add_watch_local(move |_, msg| {
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
                        ui.play_pause_button.set_label("▶");
                    }
                    MessageView::Eos(..) => {
                        ui.play_pause_button.set_label("▶");
                    }
                    MessageView::StateChanged(changed) => {
                        if changed
                            .src()
                            .is_some_and(|src| src.as_ptr() == playbin_obj.as_ptr())
                        {
                            if changed.current() == gst::State::Playing {
                                ui.play_pause_button.set_label("⏸");
                            }
                        }
                    }
                    _ => {}
                }
                ControlFlow::Continue
            });
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
            seek_scale.connect_value_changed(move |scale| {
                if ui.suppress_seek_signal.get() {
                    return;
                }
                ui.seek_to_seconds(scale.value());
                ui.refresh_seek_ui();
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
                std::env::set_var("GSK_RENDERER", "cairo");
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
