//! GUI code.
//!
//! I'm using two threads, one for the GTK+ GUI and another one that starts when
//! decoding/resampling.
//!
//! GTK+ is not thread safe so everything GUI related is on the GTK+ thread that
//! is also the main thread. When pressing the Start button, a temporary thread
//! starts for decoding/resampling.
//!
//! I'm using a `WidgetList` struct for keeping track of every Widget I'm
//! interested in. This struct is wrapped on the `Rc` smart pointer to allow
//! multiple ownership of the struct. Previously I wrapped inside `Rc` and
//! `RefCell` too to allow mutable access to everyone, but AFAIK having mutable
//! access to a Widget is not neccesary.
//!
//! When doing a callback from another thread I use `ThreadGuard`, lets you
//! `Send` the Widgets to another thread but you cant use them there (panics in
//! that case). So I use `glib::idle_add()` to execute code on the main thread
//! from another thread. In the end, we send the widgets to another thread and
//! back.

use std::env::args;
use std::rc::Rc;
use std::cell::RefCell;

use gtk;
use gio;
use glib;
use gio::prelude::*;
use gtk::prelude::*;
use gtk::Builder;

use err;
use noaa_apt::{self, Contrast};
use context::Context;
use misc;
use dsp::Rate;
use misc::ThreadGuard;


/// Defined by Cargo.toml
const VERSION: &str = env!("CARGO_PKG_VERSION");

/// If the user wants to decode or resample.
#[derive(Debug, Clone, Copy)]
enum Mode {
    Decode,
    Resample,
}

// Stores the WidgetList.
//
// Use the functions below when accesing it. Only available from the GUI thread.
// Option because it's none before building the GUI
// RefCell because I need mutable references
thread_local!(static GLOBAL: RefCell<Option<WidgetList>> = RefCell::new(None));


/// Work with reference to WidgetList.
///
/// Panics if called from a thread different than the GUI one. Also panics if
/// the GUI is not built yet.
fn borrow_widgets<F, R>(f: F) -> R
where F: FnOnce(&WidgetList) -> R
{
    GLOBAL.with(|global| {
        if let Some(ref widgets) = *global.borrow() {
            (f)(widgets)
        } else {
            panic!("Can't get WidgetList. Tried to borrow from another thread \
                    or tried to borrow before building the GUI")
        }
    })
}

/// Set the WidgetList.
///
/// Called when building the GUI.
fn set_widgets(widget_list: WidgetList) {
    GLOBAL.with(|global| {
        *global.borrow_mut() = Some(widget_list);
    });
}


/// Contains references to widgets, so I can pass them together around.
#[derive(Debug, Clone)]
struct WidgetList {
    mode:                  Mode,
    window:                gtk::ApplicationWindow,
    outer_box:              gtk::Box,
    main_box:              gtk::Box,
    progress_bar:          gtk::ProgressBar,
    start_button:          gtk::Button,
    info_bar:              gtk::InfoBar,
    info_label:            gtk::Label,
    info_revealer:         gtk::Revealer,
    output_entry:          gtk::Entry,
    rate_spinner:          Option<gtk::SpinButton>,
    input_file_chooser:    gtk::FileChooserButton,
    sync_check:            Option<gtk::CheckButton>,
    wav_steps_check:       gtk::CheckButton,
    resample_step_check:   gtk::CheckButton,
    contrast_combo:        Option<gtk::ComboBoxText>,
}

// impl WidgetList {
    // /// Create and load widgets from `gtk::Builder`.
    // fn create(
        // mode: Mode,
        // window: gtk::ApplicationWindow,
        // builder: &gtk::Builder
    // ) -> Self {
        // Self {
            // mode
            // window:              builder.get_object("window"             ).expect("Couldn't get window"             ),
            // progress_bar:        builder.get_object("progress_bar"       ).expect("Couldn't get progress_bar"       ),
            // start_button:        builder.get_object("start_button"       ).expect("Couldn't get start_button"       ),
            // info_bar:            builder.get_object("info_bar"           ).expect("Couldn't get info_bar"           ),
            // info_label:          builder.get_object("info_label"         ).expect("Couldn't get info_label"         ),
            // info_revealer:       builder.get_object("info_revealer"      ).expect("Couldn't get info_revealer"      ),
            // output_entry:        builder.get_object("output_entry"       ).expect("Couldn't get output_entry"       ),
            // rate_spinner:        builder.get_object("rate_spinner"       ).expect("Couldn't get rate_spinner"       ),
            // input_file_chooser:  builder.get_object("input_file_chooser" ).expect("Couldn't get input_file_chooser" ),
            // sync_check:          builder.get_object("sync_check"         ).expect("Couldn't get sync_check"         ),
            // wav_steps_check:     builder.get_object("wav_steps_check"    ).expect("Couldn't get wav_steps_check"    ),
            // resample_step_check: builder.get_object("resample_step_check").expect("Couldn't get resample_step_check"),
            // contrast_combo:      builder.get_object("contrast_combo"     ).expect("Couldn't get contrast_combo"     ),
        // }
    // }
// }

/// Start GUI.
///
/// Build the window.
pub fn main() {
    let application = gtk::Application::new(
        "ar.com.mbernardi.noaa-apt",
        gio::ApplicationFlags::empty(),
    ).expect("Initialization failed");

    application.connect_startup(move |app| {
        create_window(app);
    });
    application.connect_activate(|_| {});

    application.run(&args().collect::<Vec<_>>());
}

/// Set contents depending on mode.
///
/// Loads GUI from glade file depending if decoding or resampling
fn build_ui(mode: Mode, application: &gtk::Application, window: &gtk::ApplicationWindow) {

    if let Some(previous_outer_box) = window.get_child() {
        window.remove(&previous_outer_box);
    }

    let glade_src = match mode {
        Mode::Decode => include_str!("decode.glade"),
        Mode::Resample => include_str!("resample.glade"),
    };

    let builder = Builder::new_from_string(glade_src);

    let rate_spinner;
    let sync_check;
    let contrast_combo;
    match mode {
        Mode::Decode => {
            rate_spinner = None;
            sync_check = Some(builder.get_object("sync_check")
                .expect("Couldn't get sync_check"));
            contrast_combo = Some(builder.get_object("contrast_combo")
                .expect("Couldn't get contrast_combo"));
        },
        Mode::Resample => {
            rate_spinner = Some(builder.get_object("rate_spinner")
                .expect("Couldn't get sync_check"));
            sync_check = None;
            contrast_combo = None;
        },
    };

    let widgets = WidgetList {
        mode,
        window:              window.clone(),
        outer_box:           gtk::Box::new(gtk::Orientation::Vertical, 0),
        main_box:            builder.get_object("main_box"           ).expect("Couldn't get main_box"           ),
        progress_bar:        builder.get_object("progress_bar"       ).expect("Couldn't get progress_bar"       ),
        start_button:        builder.get_object("start_button"       ).expect("Couldn't get start_button"       ),
        info_bar:            gtk::InfoBar::new(),
        info_label:          gtk::Label::new(None),
        info_revealer:       gtk::Revealer::new(),
        output_entry:        builder.get_object("output_entry"       ).expect("Couldn't get output_entry"       ),
        rate_spinner,
        input_file_chooser:  builder.get_object("input_file_chooser" ).expect("Couldn't get input_file_chooser" ),
        sync_check,
        wav_steps_check:     builder.get_object("wav_steps_check"    ).expect("Couldn't get wav_steps_check"    ),
        resample_step_check: builder.get_object("resample_step_check").expect("Couldn't get resample_step_check"),
        contrast_combo,
    };

    widgets.info_revealer.add(&widgets.info_bar);
    widgets.info_bar.set_show_close_button(true);
    let info_content_area = widgets
        .info_bar
        .get_content_area()
        .expect("Couldn't get info_content_area (is None)")
        .downcast::<gtk::Box>()
        .expect("Couldn't get info_content_area (not a gtk::Box)");
    info_content_area.add(&widgets.info_label);

    widgets.outer_box.pack_start(&widgets.main_box, true, true, 0);
    widgets.outer_box.pack_end(&widgets.info_revealer, false, false, 0);

    widgets.window.add(&widgets.outer_box);

    set_widgets(widgets.clone());

    info!("GUI opened");

    // Set progress_bar and start_button to ready

    widgets.progress_bar.set_text("Ready");
    widgets.start_button.set_sensitive(true);

    check_updates();

    // Configure output_entry file chooser

    widgets.output_entry.connect_icon_press(|_, _, _| {
        borrow_widgets(|widgets| {
            let file_chooser = gtk::FileChooserDialog::new(
                Some("Save file as"),
                Some(&widgets.window),
                gtk::FileChooserAction::Save
            );

            file_chooser.add_buttons(&[
                ("Ok", gtk::ResponseType::Ok.into()),
                ("Cancel", gtk::ResponseType::Cancel.into()),
            ]);

            if file_chooser.run() == Into::<i32>::into(gtk::ResponseType::Ok) {
                let filename = file_chooser.get_filename()
                    .expect("Couldn't get filename");

                widgets.output_entry.set_text(filename.to_str().unwrap());
            }

            file_chooser.destroy();
        });
    });

    // Connect start button

    /*
    widgets.start_button.connect_clicked(|_| {
        borrow_widgets(|widgets| {
            widgets.info_revealer.set_reveal_child(false);

            // Check if we are decoding or resampling
            match widgets.options_stack.get_visible_child_name()
                .expect("Stack has no visible child").as_str()
            {

                "decode_page" => run_noaa_apt(Mode::Decode),
                "resample_page" => run_noaa_apt(Mode::Resample),

                x => panic!("Unexpected stack child name {}", x),

            }.unwrap_or_else(|error| {
                show_info(&widgets, gtk::MessageType::Error, error.to_string().as_str());
                error!("{}", error);
            });
        });
    });

    */
    // Connect info_bar close button

    widgets.info_bar.connect_response(|_, response| {
        if gtk::ResponseType::Close == response {
            borrow_widgets(|widgets| {
                widgets.info_revealer.set_reveal_child(false);
            });
        }
    });

    // Finish and show

    widgets.window.connect_delete_event(|_, _| {
        borrow_widgets(|widgets| {
            widgets.window.destroy();
            Inhibit(false)
        })
    });


    build_system_menu(mode, application, &window);

    widgets.window.show_all();
}

/// Add widgets from .glade file and get everything ready.
///
/// Connect signals to Widgets.
fn create_window(
    application: &gtk::Application
) {

    let window = gtk::ApplicationWindow::new(application);

    let mode = Mode::Resample;

    window.set_title("noaa-apt");
    window.set_default_size(450, -1);

    // Set WM_CLASS property. Without it, on KDE the taskbar icon is correct,
    // but for some reason the window has a stock X11 icon on the top-left
    // corner. When I set WM_CLASS the window gets the correct icon.
    // GTK docs say that this option is deprecated?
    // https://gtk-rs.org/docs/gtk/trait.GtkWindowExt.html#tymethod.set_wmclass
    window.set_wmclass("noaa-apt", "noaa-apt");

    build_ui(mode, &application, &window);
}

/// Build menu bar
fn build_system_menu(mode: Mode, application: &gtk::Application, window: &gtk::ApplicationWindow) {
    // let menu = gio::Menu::new();
    let menu_bar = gio::Menu::new();
    let help_menu = gio::Menu::new();
    let tools_menu = gio::Menu::new();

    // The first argument is the label of the menu item whereas the second is the action name. It'll
    // makes more sense when you'll be reading the "add_actions" function.
    // menu.append("Quit", "app.quit");

    tools_menu.append("_Decode", "app.decode");
    tools_menu.append("_Resample WAV", "app.resample");
    menu_bar.append_submenu("_Tools", &tools_menu);


    help_menu.append("_Usage", "app.usage");
    help_menu.append("_Guide", "app.guide");
    help_menu.append("_About", "app.about");
    menu_bar.append_submenu("_Help", &help_menu);

    application.set_menubar(&menu_bar);


    let decode = gio::SimpleAction::new("decode", None);
    let w = window.clone();
    let a = application.clone();
    decode.connect_activate(move |_, _| {
        build_ui(Mode::Decode, &a, &w);
    });
    application.add_action(&decode);

    let resample = gio::SimpleAction::new("resample", None);
    let w = window.clone();
    let a = application.clone();
    resample.connect_activate(move |_, _| {
        build_ui(Mode::Resample, &a, &w);
    });
    application.add_action(&resample);

    let about = gio::SimpleAction::new("about", None);
    about.connect_activate(|_, _| {
        let dialog = gtk::AboutDialog::new();
        dialog.set_program_name("noaa-apt");
        dialog.set_version(VERSION);
        dialog.set_authors(&["Martín Bernardi <martin@mbernardi.com.ar>"]);
        dialog.set_website_label(Some("noaa-apt website"));
        dialog.set_website(Some("https://noaa-apt.mbernardi.com.ar/"));
        dialog.set_license_type(gtk::License::Gpl30);
        dialog.set_title("About noaa-apt");
        // dialog.set_transient_for(Some(&window));
        dialog.run();
        dialog.destroy();
    });
    application.add_action(&about);
}

/// Set progress of ProgressBar
fn set_progress(fraction: f32, description: String) {
    borrow_widgets(|widgets| {
        widgets.progress_bar.set_fraction(fraction as f64);
        widgets.progress_bar.set_text(description.as_str());
    });
}

/*
/// Start decoding or resampling.
///
/// Starts another working thread and updates the `status_label` when finished.
/// Also sets the button as not sensitive and then as sensitive again.
fn run_noaa_apt(action: Mode) -> err::Result<()> {

    // input_filename has to be a String instead of GString because I need to
    // move it to another thread
    let input_filename: String =
        borrow_widgets(|widgets| {
            widgets
            .input_file_chooser
            .get_filename() // Option<std::path::PathBuf>
            .ok_or_else(|| err::Error::Internal("Select input file".to_string()))
            .and_then(|path: std::path::PathBuf| {
                 path.to_str()
                     .ok_or_else(|| err::Error::Internal("Invalid character on input path".to_string()))
                     .map(|s: &str| s.to_string())
            })
        })?;


    // output_filename has to be a String instead of GString because I need to
    // move to another thread
    let output_filename = match action {
        Mode::Decode => borrow_widgets(|w| w.decode_output_entry.get_text())
            .expect("Couldn't get decode_output_entry text").as_str().to_string(),
        Mode::Resample => borrow_widgets(|w| w.resample_output_entry.get_text())
            .expect("Couldn't get resample_output_entry text").as_str().to_string(),
    };

    if output_filename == "" {
        return Err(err::Error::Internal("Select output filename".to_string()))
    }

    let callback = move |result| {
        glib::idle_add(move || {
            borrow_widgets(|widgets| {
                widgets.start_button.set_sensitive(true);
                match result {
                    Ok(()) => {
                        // widgets.status_label.set_markup("Finished");
                        set_progress(1., "Finished".to_string());
                    },
                    Err(ref e) => {
                        set_progress(1., "Error".to_string());
                        show_info(&widgets, gtk::MessageType::Error, format!("{}", e).as_str());

                        error!("{}", e);
                    },
                }
            });
            gtk::Continue(false)
        });
    };
    let progress_callback = |progress, description: String| {
        glib::idle_add(move || {
            set_progress(progress, description.clone());
            gtk::Continue(false)
        });
    };

    borrow_widgets(|widgets| {
        match action {
            Mode::Decode => {
                let sync = widgets.decode_sync_check.get_active();
                let wav_steps = widgets.decode_wav_steps_check.get_active();
                let resample_step = widgets.decode_resample_step_check.get_active();

                // See https://stackoverflow.com/questions/48034119/rust-matching-a-optionstring
                let contrast_adjustment: Contrast = match widgets
                    .decode_contrast_combo
                    .get_active_text()
                    .as_ref()
                    .map(|s| s.as_str())
                {
                    Some("Keep 98 percent") => Ok(Contrast::Percent(0.98)),
                    Some("From telemetry") => Ok(Contrast::Telemetry),
                    Some("Disable") => Ok(Contrast::MinMax),
                    Some(id) => Err(err::Error::Internal(
                        format!("Unknown contrast adjustment \"{}\"", id)
                    )),
                    None => Err(err::Error::Internal(
                        "Select contrast adjustment".to_string()
                    )),
                }?;
                debug!("Decode {} to {}", input_filename, output_filename);

                widgets.start_button.set_sensitive(false);
                std::thread::spawn(move || {
                    let context = Context::decode(
                        progress_callback,
                        Rate::hz(noaa_apt::WORK_RATE),
                        Rate::hz(noaa_apt::FINAL_RATE),
                        wav_steps,
                        resample_step,
                    );

                    callback(noaa_apt::decode(
                        context,
                        input_filename.as_str(),
                        output_filename.as_str(),
                        contrast_adjustment,
                        sync,
                    ));
                });
            },
            Mode::Resample => {
                let rate = widgets.resample_rate_spinner.get_value_as_int() as u32;
                let wav_steps = widgets.resample_wav_steps_check.get_active();
                let resample_step = widgets.resample_resample_step_check.get_active();
                debug!("Resample {} as {} to {}", input_filename, rate, output_filename);

                widgets.start_button.set_sensitive(false);
                std::thread::spawn(move || {
                    let context = Context::resample(
                        progress_callback,
                        wav_steps,
                        resample_step,
                    );

                    callback(noaa_apt::resample_wav(
                        context,
                        input_filename.as_str(),
                        output_filename.as_str(),
                        Rate::hz(rate),
                    ));
                });
            },
        };

        Ok(())
    })
}
*/

/// Show InfoBar with custom message and type.
fn show_info(widgets: &WidgetList, message_type: gtk::MessageType, text: &str) {
    match message_type {
        gtk::MessageType::Info =>
            widgets.info_label.set_markup(
                text
            ),
        gtk::MessageType::Warning =>
            widgets.info_label.set_markup(
                format!("<b>Warning: {}</b>", text).as_str()
            ),
        gtk::MessageType::Error =>
            widgets.info_label.set_markup(
                format!("<b>Error: {}</b>", text).as_str()
            ),
        _ =>
            unreachable!(),
    }

    widgets.info_bar.set_message_type(message_type);
    widgets.info_revealer.set_reveal_child(true);
}

/// Check for updates on another thread and show the result on the info_bar.
fn check_updates() {
    // Callback called when check_update ends. Inside calls glib::idle_add to
    // execute code om the GUI thread.
    let callback = move |result| {
        glib::idle_add(move || {
            borrow_widgets(|widgets| {
                match result {
                    Some((true, ref latest)) => {
                        show_info(
                            &widgets,
                            gtk::MessageType::Info,
                            format!("Version \"{}\" available for download!", latest).as_str(),
                        );
                    },
                    Some((false, _)) => {}, // Do nothing, already on latest version
                    None => {
                        show_info(
                            &widgets,
                            gtk::MessageType::Info,
                            format!("Error checking for updates, do you have an internet connection?").as_str(),
                        );
                    },
                }
            });
            gtk::Continue(false)
        });
    };

    std::thread::spawn(move || {
        callback(misc::check_updates(VERSION));
    });
}
